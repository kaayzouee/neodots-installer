// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 kaayzouee
// Author: https://github.com/kaayzouee

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

const NIXOS_CONFIG_DIR: &str = "/etc/nixos";
const HARDWARE_CONFIG_FILE: &str = "hardware-configuration.nix";
const GENERATE_CONFIG_COMMAND: &str = "nixos-generate-config";
const NEODOTS_REPOSITORY: &str = "https://github.com/kaayzouee/neodots.git";

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

    if matches!(status, CheckStatus::Warning) {
        println!("\nPreflight completed with warnings; Neodots was not cloned.");
        return ExitCode::from(1);
    }

    status = merge_status(status, check_neodots_configuration());

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

    let config_dir = Path::new(NIXOS_CONFIG_DIR);
    let hardware_config = hardware_configuration_path(config_dir);

    if !config_dir.is_dir() {
        println!("  ! missing NixOS configuration directory: {NIXOS_CONFIG_DIR}");
        println!("  This installer must be run on a NixOS system with /etc/nixos available.");
        return CheckStatus::Warning;
    }

    if has_hardware_configuration(config_dir) {
        println!("  ✓ found: {}", hardware_config.display());
        return CheckStatus::Ok;
    }

    println!("  ! missing: {}", hardware_config.display());
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
                if hardware_config.is_file() {
                    println!("  ✓ generated: {}", hardware_config.display());
                    CheckStatus::Ok
                } else {
                    eprintln!(
                        "  ! command completed, but {} was not created",
                        hardware_config.display()
                    );
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

fn hardware_configuration_path(config_dir: &Path) -> PathBuf {
    config_dir.join(HARDWARE_CONFIG_FILE)
}

fn has_hardware_configuration(config_dir: &Path) -> bool {
    config_dir.is_dir() && hardware_configuration_path(config_dir).is_file()
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

fn check_neodots_configuration() -> CheckStatus {
    println!("[neodots]");
    println!("  Repository: {NEODOTS_REPOSITORY}");
    println!(
        "  The repository will be cloned into a temporary directory and checked without applying it."
    );

    match prompt_yes_no("  Clone and validate Neodots now? [y/N] ") {
        Ok(false) => {
            println!("  skipped.");
            CheckStatus::Warning
        }
        Err(error) => {
            eprintln!("  ! could not read your response: {error}");
            CheckStatus::Warning
        }
        Ok(true) => match clone_neodots_repository() {
            Ok(checkout) => {
                let result = copy_hardware_configuration(&checkout)
                    .and_then(|_| validate_neodots_configuration(&checkout));
                let cleanup_result = fs::remove_dir_all(&checkout);

                if let Err(error) = cleanup_result {
                    eprintln!(
                        "  ! could not remove temporary checkout {}: {error}",
                        checkout.display()
                    );
                }

                match result {
                    Ok(()) => {
                        println!("  ✓ Neodots configuration passed validation.");
                        CheckStatus::Ok
                    }
                    Err(error) => {
                        eprintln!("  ! Neodots validation failed: {error}");
                        CheckStatus::Warning
                    }
                }
            }
            Err(error) => {
                eprintln!("  ! could not clone Neodots: {error}");
                CheckStatus::Warning
            }
        },
    }
}

fn clone_neodots_repository() -> io::Result<PathBuf> {
    let checkout = temporary_checkout_directory()?;
    let status = Command::new("git")
        .args(["clone", "--depth", "1", NEODOTS_REPOSITORY])
        .arg(&checkout)
        .status()?;

    if status.success() {
        println!("  ✓ cloned: {}", checkout.display());
        Ok(checkout)
    } else {
        let clone_error = format!("git clone exited with {status}");
        if checkout.exists() {
            fs::remove_dir_all(&checkout).map_err(|cleanup_error| {
                io::Error::other(format!(
                    "{clone_error}; could not remove failed checkout {}: {cleanup_error}",
                    checkout.display()
                ))
            })?;
        }
        Err(io::Error::other(clone_error))
    }
}

fn temporary_checkout_directory() -> io::Result<PathBuf> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let checkout =
        env::temp_dir().join(format!("neodots-installer-{}-{unique}", std::process::id()));

    if checkout.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("temporary checkout already exists: {}", checkout.display()),
        ));
    }

    Ok(checkout)
}

fn copy_hardware_configuration(checkout: &Path) -> io::Result<()> {
    let hardware_config = hardware_configuration_path(Path::new(NIXOS_CONFIG_DIR));
    let destination = checkout.join(HARDWARE_CONFIG_FILE);

    fs::copy(&hardware_config, &destination)?;
    let status = Command::new("git")
        .args(["-C"])
        .arg(checkout)
        .args(["add", "--force", HARDWARE_CONFIG_FILE])
        .status()?;

    if !status.success() {
        return Err(io::Error::other(format!(
            "git add {HARDWARE_CONFIG_FILE} exited with {status}"
        )));
    }

    println!("  ✓ copied hardware configuration for validation.");
    Ok(())
}

fn validate_neodots_configuration(checkout: &Path) -> io::Result<()> {
    let status = Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "flake",
            "check",
            "--no-build",
        ])
        .current_dir(checkout)
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "nix flake check exited with {status}"
        )))
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
        Err(io::Error::other(format!(
            "{command:?} exited with {status}"
        )))
    }
}

fn prompt_yes_no(prompt: &str) -> io::Result<bool> {
    print!("{prompt}");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    Ok(matches!(
        input.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory() -> PathBuf {
        let path = temporary_checkout_directory().expect("create temporary test directory path");
        fs::create_dir_all(&path).expect("create temporary test directory");
        path
    }

    #[test]
    fn hardware_configuration_uses_the_nixos_configuration_directory() {
        let config_dir = Path::new(NIXOS_CONFIG_DIR);

        assert_eq!(
            hardware_configuration_path(config_dir),
            PathBuf::from("/etc/nixos/hardware-configuration.nix")
        );
    }

    #[test]
    fn hardware_configuration_is_detected_when_present() {
        let directory = temporary_directory();
        let hardware_config = hardware_configuration_path(&directory);

        assert!(!has_hardware_configuration(&directory));
        fs::write(&hardware_config, "# generated by nixos-generate-config\n")
            .expect("write hardware configuration");
        assert!(has_hardware_configuration(&directory));

        fs::remove_dir_all(directory).expect("remove temporary test directory");
    }

    #[test]
    fn warnings_are_preserved_when_checks_are_merged() {
        assert!(matches!(
            merge_status(CheckStatus::Ok, CheckStatus::Warning),
            CheckStatus::Warning
        ));
        assert!(matches!(
            merge_status(CheckStatus::Warning, CheckStatus::Ok),
            CheckStatus::Warning
        ));
    }
}
