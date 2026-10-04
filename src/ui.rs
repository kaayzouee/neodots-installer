// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 kaayzouee
// Author: https://github.com/kaayzouee

use std::io::{self, Write};

use crate::config::{
    MachineConfig, home_directory_for_username, render_machine_config, validate_absolute_path,
    validate_hostname, validate_username,
};
use crate::target::TargetRoot;

pub fn prompt_yes_no(prompt: &str) -> Result<bool, String> {
    prompt_yes_no_default(prompt, false)
}

fn prompt_yes_no_default(prompt: &str, default: bool) -> Result<bool, String> {
    loop {
        let suffix = if default { " [Y/n] " } else { " [y/N] " };
        print!("{prompt}{suffix}");

        io::stdout()
            .flush()
            .map_err(|error| format!("failed to flush stdout: {error}"))?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .map_err(|error| format!("failed to read input: {error}"))?;

        match input.trim().to_ascii_lowercase().as_str() {
            "" => return Ok(default),
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => println!("    ! enter y/yes or n/no"),
        }
    }
}

fn prompt_text(
    label: &str,
    current: &str,
    validator: impl Fn(&str) -> Result<(), String>,
) -> Result<String, String> {
    loop {
        print!("  {label} [{current}]: ");

        io::stdout()
            .flush()
            .map_err(|error| format!("failed to flush stdout: {error}"))?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .map_err(|error| format!("failed to read input: {error}"))?;

        let candidate = input.trim();
        let value = if candidate.is_empty() {
            current.to_string()
        } else {
            candidate.to_string()
        };

        match validator(&value) {
            Ok(()) => return Ok(value),
            Err(error) => println!("    ! {error}"),
        }
    }
}

pub fn select_machine_config(detected: &MachineConfig) -> Result<MachineConfig, String> {
    println!();
    println!("[configuration]");
    println!("  Press Enter to keep each detected value.");

    let mut selected = detected.clone();

    selected.username = prompt_text("username", &detected.username, validate_username)?;

    if selected.username != detected.username {
        selected.home_directory = home_directory_for_username(&selected.username);
        println!(
            "  homeDirectory → {} (derived from selected username)",
            selected.home_directory
        );
    }

    selected.hostname = prompt_text("hostname", &detected.hostname, validate_hostname)?;

    selected.personal_enable =
        prompt_yes_no_default("  Enable personal configuration?", detected.personal_enable)?;

    selected.persistence_enable =
        prompt_yes_no_default("  Enable persistence?", detected.persistence_enable)?;

    if selected.persistence_enable {
        selected.persistence_path =
            prompt_text("persistence path", &detected.persistence_path, |value| {
                validate_absolute_path(value, "persistence.path", true)
            })?;
    }

    println!();
    println!("Selected configuration:");
    println!("  username: {}", selected.username);
    println!("  hostname: {}", selected.hostname);
    println!("  home: {}", selected.home_directory);
    println!(
        "  personal: {}",
        if selected.personal_enable {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "  persistence: {} ({})",
        if selected.persistence_enable {
            "enabled"
        } else {
            "disabled"
        },
        selected.persistence_path
    );

    if !prompt_yes_no("Use this configuration?")? {
        return Err("configuration selection cancelled".to_string());
    }

    Ok(selected)
}

pub fn print_machine_summary(target: &TargetRoot, machine: &MachineConfig) {
    let path = target.machine_config();

    if path.is_file() {
        println!();
        println!("  Existing machine configuration: {}", path.display());
    } else {
        println!();
        println!("  No existing machine configuration: {}", path.display());
    }

    println!();
    println!("Generated machine.nix:");
    println!("----------------------------------------");
    print!("{}", render_machine_config(machine));
    println!("----------------------------------------");
}
