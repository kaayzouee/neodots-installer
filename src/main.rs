// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 kaayzouee
// Author: https://github.com/kaayzouee

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const HARDWARE_CONFIG: &str = "/etc/nixos/hardware-configuration.nix";
const GENERATE_CONFIG_COMMAND: &str = "nixos-generate-config";

#[derive(Debug, Clone, Copy)]
enum CheckStatus {
    Ok,
    Warning,
}

fn main() -> ExitCode {
    println!("Neodots installer preflight scanner\n");

    let mut status = CheckStatus::Ok;

    status = merge_status(status, check_hardware_configuration());
    status = merge_status(status, check_git());
    status = merge_status(status, check_privilege_helper());

    println!();
    match status {
        CheckStatus::Ok => {
            println!("All preflight checks passed.");
            ExitCode::SUCCESS
        }
        CheckStatus::Warning => {
            println!("Preflight completed with warnings.");
            ExitCode::from(1)
        }
    }
}

fn merge_status(current: CheckStatus, next: CheckStatus) -> CheckStatus {
    match (current, next) {
        (CheckStatus::Warning, _) | (_, CheckStatus::Warning) => CheckStatus::Warning,
        _ => CheckStatus::Ok,
    }
}

fn check_hardware_configuration() -> CheckStatus {
    println!("[hardware]");

    if Path::new(HARDWARE_CONFIG).is_file() {
        println!("  ✓ found: {HARDWARE_CONFIG}");
        return CheckStatus::Ok;
    }

    println!("  ! missing: {HARDWARE_CONFIG}");
    println!("  NixOS can generate the machine-specific hardware configuration.");

    let privilege = find_privilege_helper();
    let command = match privilege.as_deref() {
        Some(tool) => format!("{} {GENERATE_CONFIG_COMMAND}", tool.display()),
        None => GENERATE_CONFIG_COMMAND.to_string(),
    };

    println!("  Command: {command}");

    match prompt_yes_no("  Generate it now? [y/N] ") {
        Ok(true) => match run_generate_config(privilege.as_deref()) {
            Ok(()) => {
                if Path::new(HARDWARE_CONFIG).is_file() {
                    println!("  ✓ generated: {HARDWARE_CONFIG}");
                    CheckStatus::Ok
                } else {
                    eprintln!("  ! command completed, but {HARDWARE_CONFIG} was not created");
                    CheckStatus::Warning
                }
            }
            Err(error) => {
                eprintln!("  ! failed to generate hardware configuration: {error}");
                CheckStatus::Warning
            }
        },
        Ok(false) => {
            println!("  skipped.");
            CheckStatus::Warning
        }
        Err(error) => {
            eprintln!("  ! could not read your response: {error}");
            CheckStatus::Warning
        }
    }
}

fn check_git() -> CheckStatus {
    println!("[git]");

    match find_in_path("git") {
        Some(path) => {
            println!("  ✓ found: {}", path.display());
            CheckStatus::Ok
        }
        None => {
            println!("  ! git was not found in PATH");
            println!("  The installer will need git for repository operations.");
            CheckStatus::Warning
        }
    }
}

fn check_privilege_helper() -> CheckStatus {
    println!("[privilege]");

    match find_privilege_helper() {
        Some(path) => {
            println!("  ✓ found: {}", path.display());
            CheckStatus::Ok
        }
        None => {
            println!("  ! neither sudo nor doas was found in PATH");
            println!("  The installer needs a privilege escalation tool for system changes.");
            CheckStatus::Warning
        }
    }
}

fn find_privilege_helper() -> Option<PathBuf> {
    find_in_path("sudo").or_else(|| find_in_path("doas"))
}

fn find_in_path(program: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;

    for directory in env::split_paths(&path_var) {
        let candidate = directory.join(program);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }

    None
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    match fs::metadata(path) {
        Ok(metadata) => metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
}

fn run_generate_config(privilege: Option<&Path>) -> io::Result<()> {
    let mut command = match privilege {
        Some(tool) => Command::new(tool),
        None => Command::new(GENERATE_CONFIG_COMMAND),
    };

    if privilege.is_some() {
        command.arg(GENERATE_CONFIG_COMMAND);
    }

    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!("{command:?} exited with {status}"),
        ))
    }
}

fn prompt_yes_no(prompt: &str) -> io::Result<bool> {
    print!("{prompt}");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    Ok(matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}
