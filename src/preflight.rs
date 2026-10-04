// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 kaayzouee
// Author: https://github.com/kaayzouee

use std::{fs, path::Path};

use crate::{
    config::MachineConfig,
    privilege::{
        create_temp_file, find_in_path, path_to_string, remove_path_privileged,
        run_privileged_command,
    },
    target::TargetRoot,
    ui::prompt_yes_no,
};

const GENERATE_CONFIG_COMMAND: &str = "nixos-generate-config";
const NIX_COMMAND: &str = "nix";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwarePreparation {
    Existing,
    GenerateInStaging,
    Unavailable,
}

pub fn check_nixos(target: &TargetRoot) -> Result<(), String> {
    let contents = fs::read_to_string(target.os_release_path()).map_err(|error| {
        format!(
            "cannot read {}: {error}",
            target.os_release_path().display()
        )
    })?;

    let is_nixos = contents.lines().any(|line| {
        let line = line.trim();
        line == "ID=nixos" || line == "ID=\"nixos\""
    });

    if !is_nixos {
        return Err(format!(
            "target does not identify itself as NixOS: {}",
            target.os_release_path().display()
        ));
    }

    Ok(())
}

pub fn check_hardware_configuration(target: &TargetRoot) -> Result<HardwarePreparation, String> {
    let nixos_dir = target.nixos_config_dir();

    if !nixos_dir.is_dir() {
        return Err(format!(
            "target NixOS configuration directory is missing: {}",
            nixos_dir.display()
        ));
    }

    let hardware_config = target.hardware_config();

    println!("[hardware]");
    if hardware_config.is_file() {
        println!("  ✓ found: {}", hardware_config.display());
        return Ok(HardwarePreparation::Existing);
    }

    println!("  ! missing: {}", hardware_config.display());
    println!("  Hardware configuration can be generated in disposable validation staging.");

    if prompt_yes_no("  Generate hardware configuration during validation?")? {
        Ok(HardwarePreparation::GenerateInStaging)
    } else {
        Ok(HardwarePreparation::Unavailable)
    }
}

pub fn check_persistence(
    target: &TargetRoot,
    machine: &MachineConfig,
    privilege_helper: &Path,
) -> Result<(), String> {
    if !machine.persistence_enable {
        return Ok(());
    }

    let persistence_path = target.path(&machine.persistence_path);

    println!("[persistence]");
    println!("  path: {}", persistence_path.display());

    let metadata = fs::symlink_metadata(&persistence_path).map_err(|error| {
        format!(
            "configured persistence path does not exist: {}: {error}",
            persistence_path.display()
        )
    })?;

    if metadata.file_type().is_symlink() {
        return Err(format!(
            "configured persistence path must not be a symlink: {}",
            persistence_path.display()
        ));
    }

    if !metadata.is_dir() {
        return Err(format!(
            "configured persistence path is not a directory: {}",
            persistence_path.display()
        ));
    }

    let canonical_path = fs::canonicalize(&persistence_path).map_err(|error| {
        format!(
            "failed to resolve configured persistence path {}: {error}",
            persistence_path.display()
        )
    })?;

    if !is_mount_point(&canonical_path)? {
        return Err(format!(
            "configured persistence path is not a mounted filesystem: {}",
            canonical_path.display()
        ));
    }

    let probe_seed = create_temp_file("neodots-installer-persistence-probe", "")?;

    let probe_name = probe_seed
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "failed to obtain a valid persistence probe name from {}",
                probe_seed.display()
            )
        })?
        .to_owned();

    fs::remove_file(&probe_seed).map_err(|error| {
        format!(
            "failed to remove temporary persistence probe seed {}: {error}",
            probe_seed.display()
        )
    })?;

    let probe = canonical_path.join(probe_name);
    let probe_string = path_to_string(&probe)?;

    run_privileged_command(
        privilege_helper,
        "mkdir",
        &["-m", "0700", "--", probe_string.as_str()],
    )
    .map_err(|error| {
        format!("configured persistence path is not writable by the installation process: {error}")
    })?;

    if let Err(error) = remove_path_privileged(privilege_helper, &probe) {
        return Err(format!(
            "configured persistence path passed the write test but the test directory could not be removed: {error}"
        ));
    }

    println!(
        "  ✓ mounted and writable persistence path: {}",
        canonical_path.display()
    );

    Ok(())
}

fn is_mount_point(path: &Path) -> Result<bool, String> {
    let mounts = fs::read_to_string("/proc/self/mountinfo")
        .map_err(|error| format!("cannot read /proc/self/mountinfo: {error}"))?;

    Ok(mountinfo_contains_path(path, &mounts))
}

fn mountinfo_contains_path(path: &Path, mountinfo: &str) -> bool {
    let expected = path.to_string_lossy();

    mountinfo.lines().any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();

        fields
            .get(4)
            .map(|field| decode_mountinfo_path(field))
            .as_deref()
            == Some(expected.as_ref())
    })
}

fn decode_mountinfo_path(value: &str) -> String {
    value
        .replace(r"\134", r"\")
        .replace(r"\040", " ")
        .replace(r"\011", "\t")
}

pub fn check_git() -> Result<(), String> {
    match find_in_path("git") {
        Some(path) => {
            println!("[git]");
            println!("  ✓ found: {}", path.display());
            Ok(())
        }
        None => Err("git was not found in PATH".to_string()),
    }
}

pub fn check_nix() -> Result<(), String> {
    match find_in_path(NIX_COMMAND) {
        Some(path) => {
            println!("[nix]");
            println!("  ✓ found: {}", path.display());
            Ok(())
        }
        None => Err("nix was not found in PATH".to_string()),
    }
}

pub fn generate_hardware_configuration_in_staging(
    privilege_helper: &Path,
    generated_root: &Path,
) -> Result<(), String> {
    let nixos_dir = generated_root.join("etc").join("nixos");

    fs::create_dir_all(&nixos_dir).map_err(|error| {
        format!(
            "failed to create temporary NixOS directory {}: {error}",
            nixos_dir.display()
        )
    })?;

    let generated_root_string = path_to_string(generated_root)?;

    run_privileged_command(
        privilege_helper,
        GENERATE_CONFIG_COMMAND,
        &["--root", generated_root_string.as_str(), "--no-filesystems"],
    )
    .map_err(|error| format!("nixos-generate-config failed in disposable staging: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountinfo_path_matching_decodes_escaped_mount_points() {
        let mountinfo = "36 25 0:33 / /persist rw,relatime - ext4 /dev/sda rw\n\
             37 25 0:34 / /mnt/example\\040target rw,relatime - ext4 /dev/sdb rw";

        assert!(mountinfo_contains_path(Path::new("/persist"), mountinfo));
        assert!(mountinfo_contains_path(
            Path::new("/mnt/example target"),
            mountinfo
        ));
        assert!(!mountinfo_contains_path(Path::new("/missing"), mountinfo));
    }
}
