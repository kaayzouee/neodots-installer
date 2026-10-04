// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 kaayzouee
// Author: https://github.com/kaayzouee

use std::{fs, path::Path};

use crate::{
    privilege::{find_in_path, path_to_string, run_privileged_command},
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
