// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 kaayzouee
// Author: https://github.com/kaayzouee

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn check_privilege_helper() -> Result<PathBuf, String> {
    match find_privilege_helper() {
        Some(path) => {
            println!("[privilege]");
            println!("  ✓ found: {}", path.display());
            Ok(path)
        }
        None => Err("neither sudo nor doas was found in PATH".to_string()),
    }
}

fn find_privilege_helper() -> Option<PathBuf> {
    find_in_path("sudo").or_else(|| find_in_path("doas"))
}

pub fn find_in_path(command: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;

    for directory in env::split_paths(&path) {
        let candidate = directory.join(command);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

pub fn run_privileged_command(
    privilege_helper: &Path,
    command: &str,
    args: &[&str],
) -> Result<(), String> {
    let status = Command::new(privilege_helper)
        .arg(command)
        .args(args)
        .status()
        .map_err(|error| {
            format!(
                "failed to run {} {}: {error}",
                privilege_helper.display(),
                command
            )
        })?;

    if !status.success() {
        return Err(format!(
            "{} {} exited with status {}",
            privilege_helper.display(),
            command,
            status
        ));
    }

    Ok(())
}

pub fn create_temp_directory(prefix: &str) -> Result<PathBuf, String> {
    let path = env::temp_dir().join(format!(
        "{}-{}-{}",
        prefix,
        std::process::id(),
        timestamp_nanos()
    ));

    fs::create_dir_all(&path).map_err(|error| {
        format!(
            "failed to create temporary directory {}: {error}",
            path.display()
        )
    })?;

    Ok(path)
}

pub fn create_temp_file(prefix: &str, contents: &str) -> Result<PathBuf, String> {
    let path = env::temp_dir().join(format!(
        "{}-{}-{}",
        prefix,
        std::process::id(),
        timestamp_nanos()
    ));

    fs::write(&path, contents).map_err(|error| {
        format!(
            "failed to create temporary file {}: {error}",
            path.display()
        )
    })?;

    Ok(path)
}

pub fn timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

pub fn path_to_string(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}

pub fn remove_path_privileged(privilege_helper: &Path, path: &Path) -> Result<(), String> {
    let path_string = path_to_string(path)?;
    run_privileged_command(privilege_helper, "rm", &["-rf", "--", path_string.as_str()])
}
