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

use std::{fs, path::Path, process::ExitCode};

use config::{
    MachineConfig, detect_machine_config, detect_password_state, render_machine_config,
    validate_machine_config, validate_primary_password_state, validate_username_for_target,
    verify_neodots_revision,
};
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

    if let Err(error) = verify_neodots_revision(&target) {
        return fail(error);
    }

    if let Err(error) = check_nix() {
        return fail(error);
    }

    let existing_machine_contents = match read_existing_machine_contents(&target) {
        Ok(contents) => contents,
        Err(error) => return fail(error),
    };

    let detected = match detect_machine_config(&target) {
        Ok(config) => config,
        Err(error) => return fail(error),
    };

    let selected = match select_machine_config(&target, &detected) {
        Ok(config) => config,
        Err(error) => return fail(error),
    };

    let password_state = match detect_password_state(&target, &selected.username, &privilege_helper)
    {
        Ok(state) => state,
        Err(error) => return fail(error),
    };

    println!();
    println!("[password]");
    println!("  ✓ primary user: {}", selected.username);
    println!("  ✓ password state: {}", password_state.as_str());

    if let Err(error) = validate_primary_password_state(password_state) {
        return fail(error);
    }

    let replacing_existing_machine =
        existing_machine_contents.is_some() && machine_config_changed(&detected, &selected);

    if replacing_existing_machine {
        println!();
        println!("  ! Existing machine.nix will be replaced.");
        println!(
            "  ! The installer only models its machine contract and cannot preserve \
arbitrary declarations outside that contract."
        );
        println!("  ! The replacement is intentional only after this separate confirmation.");

        match prompt_yes_no("Replace the existing machine.nix?") {
            Ok(true) => {}
            Ok(false) => {
                return fail("existing machine.nix replacement cancelled".to_string());
            }
            Err(error) => return fail(error),
        }
    }

    let machine_contents = match existing_machine_contents.as_deref() {
        Some(existing) if !replacing_existing_machine => existing.to_string(),
        _ => render_machine_config(&selected),
    };

    print_machine_summary(&target, &selected);

    match handle_machine_config(
        &target,
        &selected,
        &machine_contents,
        hardware,
        &privilege_helper,
    ) {
        Ok(()) => {
            println!();
            println!(
                "Installation completed successfully; the installed configuration was verified."
            );
            ExitCode::SUCCESS
        }
        Err(error) => fail(error),
    }
}

fn read_existing_machine_contents(target: &TargetRoot) -> Result<Option<String>, String> {
    let path = target.machine_config();

    if !path.is_file() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;

    Ok(Some(contents))
}

fn machine_config_changed(detected: &MachineConfig, selected: &MachineConfig) -> bool {
    detected.system != selected.system
        || detected.username != selected.username
        || detected.hostname != selected.hostname
        || detected.home_directory != selected.home_directory
        || detected.personal_enable != selected.personal_enable
        || detected.persistence_enable != selected.persistence_enable
        || detected.persistence_path != selected.persistence_path
}

fn handle_machine_config(
    target: &TargetRoot,
    machine: &MachineConfig,
    machine_contents: &str,
    hardware: HardwarePreparation,
    privilege_helper: &Path,
) -> Result<(), String> {
    validate_machine_config(machine)?;
    validate_username_for_target(target, &machine.username)?;

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

    validate_and_install_machine_config(
        target,
        machine,
        machine_contents,
        hardware,
        privilege_helper,
    )
}

fn fail(error: String) -> ExitCode {
    eprintln!("  ✗ {error}");
    ExitCode::FAILURE
}
