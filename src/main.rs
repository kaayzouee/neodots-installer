// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 kaayzouee
// Author: https://github.com/kaayzouee

mod config;
mod preflight;
mod privilege;
mod staging;
mod target;
mod transaction;
mod ui;

use std::process::ExitCode;

use config::{MachineConfig, detect_machine_config, validate_machine_config};
use preflight::{
    HardwarePreparation, check_git, check_hardware_configuration, check_nix, check_nixos,
};
use privilege::check_privilege_helper;
use staging::validate_and_install_machine_config;
use target::{TargetRoot, acquire_installer_lock};
use transaction::recover_pending_transactions;
use ui::{print_machine_summary, prompt_yes_no, select_machine_config};

fn main() -> ExitCode {
    println!("Neodots installer");
    println!();

    let target = match TargetRoot::from_environment() {
        Ok(target) => target,
        Err(error) => return fail(error),
    };

    println!("[target]");
    println!("  ✓ root: {}", target.root().display());

    if let Err(error) = check_nixos(&target) {
        return fail(error);
    }

    println!("[nixos]");
    println!("  ✓ target identifies itself as NixOS");

    let privilege_helper = match check_privilege_helper() {
        Ok(path) => path,
        Err(error) => return fail(error),
    };

    let _installer_lock = match acquire_installer_lock(&target, &privilege_helper) {
        Ok(lock) => lock,
        Err(error) => return fail(error),
    };

    if let Err(error) = recover_pending_transactions(&target, &privilege_helper) {
        return fail(error);
    }

    let hardware = match check_hardware_configuration(&target) {
        Ok(result) => result,
        Err(error) => return fail(error),
    };

    if let Err(error) = check_git() {
        return fail(error);
    }

    if let Err(error) = check_nix() {
        return fail(error);
    }

    let detected = match detect_machine_config(&target) {
        Ok(config) => config,
        Err(error) => return fail(error),
    };

    let selected = match select_machine_config(&detected) {
        Ok(config) => config,
        Err(error) => return fail(error),
    };

    print_machine_summary(&target, &selected);

    match handle_machine_config(&target, &selected, hardware, &privilege_helper) {
        Ok(()) => {
            println!();
            println!("Installer preflight completed.");
            ExitCode::SUCCESS
        }
        Err(error) => fail(error),
    }
}

fn handle_machine_config(
    target: &TargetRoot,
    machine: &MachineConfig,
    hardware: HardwarePreparation,
    privilege_helper: &std::path::Path,
) -> Result<(), String> {
    validate_machine_config(machine)?;

    if hardware == HardwarePreparation::Unavailable {
        return Err(
            "hardware-configuration.nix is missing and generation was declined".to_string(),
        );
    }

    if !prompt_yes_no(
        "Validate staged configuration and transactionally install the selected configuration?",
    )? {
        println!("  Installation skipped.");
        return Ok(());
    }

    validate_and_install_machine_config(target, machine, hardware, privilege_helper)
}

fn fail(error: String) -> ExitCode {
    eprintln!("  ✗ {error}");
    ExitCode::FAILURE
}
