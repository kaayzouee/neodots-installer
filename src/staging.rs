// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 kaayzouee
// Author: https://github.com/kaayzouee

use std::{fs, os::unix::fs as unix_fs, path::Path, process::Command};

use crate::{
    config::{MachineConfig, render_machine_config, validate_machine_config},
    preflight::{HardwarePreparation, generate_hardware_configuration_in_staging},
    privilege::{create_temp_directory, create_temp_file, find_in_path},
    target::TargetRoot,
    transaction::{find_legacy_transaction, install_configuration_transactionally},
};

const HARDWARE_CONFIG_FILE: &str = "hardware-configuration.nix";
const NIX_COMMAND: &str = "nix";

pub fn validate_and_install_machine_config(
    target: &TargetRoot,
    machine: &MachineConfig,
    hardware: HardwarePreparation,
    privilege_helper: &Path,
) -> Result<(), String> {
    validate_machine_config(machine)?;

    let validation_dir = create_temp_directory("neodots-installer-validation")?;
    let machine_contents = render_machine_config(machine);

    let result = (|| {
        if let Some(legacy_transaction) = find_legacy_transaction(target)? {
            return Err(format!(
                "legacy installer transaction found under /etc/nixos: {}; recover or remove it before installing",
                legacy_transaction.display()
            ));
        }

        println!(
            "    Staging target configuration from {}...",
            target.nixos_config_dir().display()
        );

        let staged_nixos_dir = validation_dir.join("etc").join("nixos");
        fs::create_dir_all(&staged_nixos_dir).map_err(|error| {
            format!(
                "failed to create staged NixOS directory {}: {error}",
                staged_nixos_dir.display()
            )
        })?;

        copy_recursively(&target.nixos_config_dir(), &staged_nixos_dir)?;

        let generated_hardware_root = if hardware == HardwarePreparation::GenerateInStaging {
            let generated_root = create_temp_directory("neodots-installer-hardware")?;

            println!(
                "    Generating hardware configuration in {}...",
                generated_root.display()
            );

            generate_hardware_configuration_in_staging(privilege_helper, &generated_root)?;

            let generated_hardware = generated_root
                .join("etc")
                .join("nixos")
                .join(HARDWARE_CONFIG_FILE);

            if !generated_hardware.is_file() {
                return Err(format!(
                    "nixos-generate-config completed without producing {}",
                    generated_hardware.display()
                ));
            }

            fs::copy(
                &generated_hardware,
                staged_nixos_dir.join(HARDWARE_CONFIG_FILE),
            )
            .map_err(|error| {
                format!("failed to copy generated hardware configuration into staging: {error}")
            })?;

            Some(generated_root)
        } else {
            None
        };

        let staged_machine_path = staged_nixos_dir
            .join("hosts")
            .join("nixos")
            .join("machine.nix");

        if let Some(parent) = staged_machine_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create staged machine configuration directory {}: {error}",
                    parent.display()
                )
            })?;
        }

        fs::write(&staged_machine_path, &machine_contents).map_err(|error| {
            format!(
                "failed to write staged machine.nix {}: {error}",
                staged_machine_path.display()
            )
        })?;

        validate_staged_configuration(&staged_nixos_dir)?;
        println!("    ✓ staged configuration passed validation");

        let generated_hardware = generated_hardware_root
            .as_ref()
            .map(|root| root.join("etc").join("nixos").join(HARDWARE_CONFIG_FILE));

        let machine_temp = create_temp_file("neodots-installer-machine", &machine_contents)?;

        let install_result = install_configuration_transactionally(
            target,
            privilege_helper,
            &machine_temp,
            generated_hardware.as_deref(),
        );

        fs::remove_file(&machine_temp).ok();
        if let Some(root) = generated_hardware_root {
            fs::remove_dir_all(root).ok();
        }

        install_result
    })();

    if let Err(error) = &result {
        eprintln!("    ✗ {error}");
    }

    fs::remove_dir_all(validation_dir).ok();
    result
}

fn validate_staged_configuration(staged_nixos_dir: &Path) -> Result<(), String> {
    let nix_path =
        find_in_path(NIX_COMMAND).ok_or_else(|| "nix was not found in PATH".to_string())?;

    println!(
        "    Validating staged configuration with nix flake check at {}...",
        staged_nixos_dir.display()
    );

    let status = Command::new(&nix_path)
        .arg("flake")
        .arg("check")
        .arg("--no-build")
        .arg("--no-write-lock-file")
        .arg(staged_nixos_dir)
        .status()
        .map_err(|error| format!("failed to run nix flake check: {error}"))?;

    if !status.success() {
        return Err(format!(
            "staged nix flake check failed with status {}",
            status
        ));
    }

    Ok(())
}

fn copy_recursively(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("failed to inspect {}: {error}", source.display()))?;

    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source)
            .map_err(|error| format!("failed to read symlink {}: {error}", source.display()))?;

        if destination.exists() {
            remove_existing_path(destination)?;
        }

        unix_fs::symlink(&target, destination).map_err(|error| {
            format!(
                "failed to create symlink {} -> {}: {error}",
                destination.display(),
                target.display()
            )
        })?;

        return Ok(());
    }

    if metadata.is_dir() {
        fs::create_dir_all(destination).map_err(|error| {
            format!(
                "failed to create directory {}: {error}",
                destination.display()
            )
        })?;

        for entry in fs::read_dir(source)
            .map_err(|error| format!("failed to read directory {}: {error}", source.display()))?
        {
            let entry = entry.map_err(|error| {
                format!(
                    "failed to read directory entry in {}: {error}",
                    source.display()
                )
            })?;

            copy_recursively(&entry.path(), &destination.join(entry.file_name()))?;
        }

        return Ok(());
    }

    if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create parent directory {}: {error}",
                    parent.display()
                )
            })?;
        }

        fs::copy(source, destination).map_err(|error| {
            format!(
                "failed to copy {} to {}: {error}",
                source.display(),
                destination.display()
            )
        })?;

        return Ok(());
    }

    Err(format!(
        "unsupported filesystem object in target configuration: {}",
        source.display()
    ))
}

fn remove_existing_path(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;

    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(path)
            .map_err(|error| format!("failed to remove {}: {error}", path.display()))?;
    } else if metadata.is_dir() {
        fs::remove_dir_all(path)
            .map_err(|error| format!("failed to remove {}: {error}", path.display()))?;
    }

    Ok(())
}
