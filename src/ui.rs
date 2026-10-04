// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 kaayzouee
// Author: https://github.com/kaayzouee

use std::{
    env,
    io::{self, IsTerminal, Write},
};

use crate::config::{
    MachineConfig, PasswordState, home_directory_for_username, render_machine_config,
    validate_absolute_path, validate_hostname, validate_username_for_target,
};
use crate::target::TargetRoot;
use crate::tui;
use crate::wallpaper::{WallpaperAsset, WallpaperSelection, validate_wallpaper_id};

pub fn tui_enabled() -> bool {
    env::var_os("NEODOTS_PLAIN_UI").is_none()
        && io::stdin().is_terminal()
        && io::stdout().is_terminal()
}

pub fn prompt_yes_no(prompt: &str) -> Result<bool, String> {
    if tui_enabled() {
        return tui::confirm(prompt, false);
    }

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

fn select_machine_config_plain(
    target: &TargetRoot,
    detected: &MachineConfig,
) -> Result<MachineConfig, String> {
    println!();
    println!("[configuration]");
    println!("  Press Enter to keep each detected value.");

    let mut selected = detected.clone();

    selected.username = prompt_text("username", &detected.username, |value| {
        validate_username_for_target(target, value)
    })?;

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

    Ok(selected)
}

pub fn select_machine_config(
    target: &TargetRoot,
    detected: &MachineConfig,
) -> Result<MachineConfig, String> {
    if tui_enabled() {
        return tui::select_machine_config(target, detected);
    }

    select_machine_config_plain(target, detected)
}

pub fn show_password_state(state: PasswordState) -> Result<(), String> {
    if tui_enabled() {
        return tui::show_password_state(state);
    }

    println!();
    println!("[password]");
    println!("  ✓ password state: {}", state.as_str());

    Ok(())
}

fn select_wallpaper_plain() -> Result<WallpaperSelection, String> {
    println!();
    println!("[wallpaper]");
    println!("  n = none");
    println!("  r = random");
    println!("  s = specific wallpaper ID");

    loop {
        print!("  Wallpaper selection [n/r/s]: ");

        io::stdout()
            .flush()
            .map_err(|error| format!("failed to flush stdout: {error}"))?;

        let mut input = String::new();

        io::stdin()
            .read_line(&mut input)
            .map_err(|error| format!("failed to read input: {error}"))?;

        match input.trim().to_ascii_lowercase().as_str() {
            "" | "n" | "none" => return Ok(WallpaperSelection::None),

            "r" | "random" => return Ok(WallpaperSelection::Random),

            "s" | "specific" => {
                let id = prompt_text("wallpaper ID", "", validate_wallpaper_id)?;

                return Ok(WallpaperSelection::Specific(id));
            }

            _ => println!("    ! enter n/none, r/random, or s/specific"),
        }
    }
}

pub fn select_wallpaper() -> Result<WallpaperSelection, String> {
    if tui_enabled() {
        return tui::select_wallpaper();
    }

    select_wallpaper_plain()
}

pub fn print_machine_summary(
    target: &TargetRoot,
    machine: &MachineConfig,
    wallpaper: Option<&WallpaperAsset>,
) -> Result<(), String> {
    if tui_enabled() {
        return tui::show_review(target, machine, wallpaper);
    }

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

    match wallpaper {
        Some(asset) => {
            println!();
            println!("Wallpaper:");
            println!("  id: {}", asset.id);
            println!("  file: {}", asset.filename);
            println!("  sha256: {}", asset.sha256);
        }

        None => {
            println!();
            println!("Wallpaper: none");
        }
    }

    Ok(())
}

pub fn run_with_progress<T, F>(title: &str, detail: &str, operation: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String>,
{
    if tui_enabled() {
        return tui::run_with_progress(title, detail, operation);
    }

    println!();
    println!("[progress]");
    println!("  {title}");
    println!("  {detail}");

    operation()
}

pub fn show_error_screen(message: &str, recovery_hint: &str) -> Result<bool, String> {
    if tui_enabled() {
        return tui::show_error(message, recovery_hint);
    }

    eprintln!("  ✗ {message}");
    eprintln!("  recovery: {recovery_hint}");
    Ok(false)
}

pub fn show_done_screen(message: &str) -> Result<(), String> {
    if tui_enabled() {
        return tui::show_done(message);
    }

    println!();
    println!("[done]");
    println!("  {message}");

    Ok(())
}
