// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 kaayzouee
// Author: https://github.com/kaayzouee

use std::{
    env,
    fmt::Write as FmtWrite,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const TEMP_CREATION_ATTEMPTS: usize = 64;
const RANDOM_TOKEN_BYTES: usize = 16;

pub fn check_privilege_helper() -> Result<PathBuf, String> {
    match find_privilege_helper() {
        Some(path) => {
            println!("[privilege]");
            println!("  ✓ found trusted helper: {}", path.display());
            Ok(path)
        }
        None => Err(
            "neither sudo nor doas was found as a trusted executable in PATH; \
             the helper must resolve to a root-owned, non-writable executable"
                .to_string(),
        ),
    }
}

fn find_privilege_helper() -> Option<PathBuf> {
    find_in_path("sudo")
        .filter(|path| is_trusted_privilege_helper(path))
        .or_else(|| find_in_path("doas").filter(|path| is_trusted_privilege_helper(path)))
}

pub fn find_in_path(command: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;

    for directory in env::split_paths(&path) {
        if !directory.is_absolute() {
            continue;
        }

        let candidate = directory.join(command);
        let canonical = fs::canonicalize(&candidate).ok()?;
        let metadata = fs::metadata(&canonical).ok()?;

        if !metadata.is_file() || !is_executable(&metadata) {
            continue;
        }

        return Some(canonical);
    }

    None
}

fn is_executable(metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o111 != 0
}

fn is_trusted_privilege_helper(path: &Path) -> bool {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return false,
    };

    if !metadata.is_file() || metadata.uid() != 0 {
        return false;
    }

    if metadata.permissions().mode() & 0o022 != 0 {
        return false;
    }

    let mut current = path.parent();

    while let Some(directory) = current {
        let metadata = match fs::symlink_metadata(directory) {
            Ok(metadata) => metadata,
            Err(_) => return false,
        };

        if !metadata.is_dir() || metadata.uid() != 0 {
            return false;
        }

        if metadata.permissions().mode() & 0o022 != 0 {
            return false;
        }

        if directory == Path::new("/") {
            break;
        }

        current = directory.parent();
    }

    true
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

pub fn run_privileged_command_output(
    privilege_helper: &Path,
    command: &str,
    args: &[&str],
) -> Result<String, String> {
    let output = Command::new(privilege_helper)
        .arg(command)
        .args(args)
        .output()
        .map_err(|error| {
            format!(
                "failed to run {} {}: {error}",
                privilege_helper.display(),
                command
            )
        })?;

    if !output.status.success() {
        return Err(format!(
            "{} {} exited with status {}",
            privilege_helper.display(),
            command,
            output.status
        ));
    }

    String::from_utf8(output.stdout)
        .map_err(|error| format!("privileged command returned invalid UTF-8: {error}"))
}

pub fn create_temp_directory(prefix: &str) -> Result<PathBuf, String> {
    let temp_root = env::temp_dir();

    for _ in 0..TEMP_CREATION_ATTEMPTS {
        let token = random_token()?;
        let path = temp_root.join(format!("{prefix}-{token}"));

        match fs::create_dir(&path) {
            Ok(()) => {
                let mut permissions = fs::metadata(&path)
                    .map_err(|error| {
                        format!(
                            "failed to inspect temporary directory {}: {error}",
                            path.display()
                        )
                    })?
                    .permissions();

                permissions.set_mode(0o700);

                if let Err(error) = fs::set_permissions(&path, permissions) {
                    fs::remove_dir(&path).ok();

                    return Err(format!(
                        "failed to secure temporary directory {}: {error}",
                        path.display()
                    ));
                }

                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "failed to create temporary directory {}: {error}",
                    path.display()
                ));
            }
        }
    }

    Err(format!(
        "failed to allocate a unique temporary directory after {TEMP_CREATION_ATTEMPTS} attempts"
    ))
}

pub fn create_temp_file(prefix: &str, contents: &str) -> Result<PathBuf, String> {
    let temp_root = env::temp_dir();

    for _ in 0..TEMP_CREATION_ATTEMPTS {
        let token = random_token()?;
        let path = temp_root.join(format!("{prefix}-{token}"));

        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "failed to create temporary file {}: {error}",
                    path.display()
                ));
            }
        };

        if let Err(error) = file.write_all(contents.as_bytes()) {
            drop(file);
            fs::remove_file(&path).ok();

            return Err(format!(
                "failed to write temporary file {}: {error}",
                path.display()
            ));
        }

        return Ok(path);
    }

    Err(format!(
        "failed to allocate a unique temporary file after {TEMP_CREATION_ATTEMPTS} attempts"
    ))
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0u8; RANDOM_TOKEN_BYTES];

    let mut random = File::open("/dev/urandom")
        .map_err(|error| format!("failed to open /dev/urandom: {error}"))?;

    random
        .read_exact(&mut bytes)
        .map_err(|error| format!("failed to read secure random bytes: {error}"))?;

    let mut token = String::with_capacity(RANDOM_TOKEN_BYTES * 2);

    for byte in bytes {
        FmtWrite::write_fmt(&mut token, format_args!("{byte:02x}"))
            .map_err(|_| "failed to format secure random token".to_string())?;
    }

    Ok(token)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_temp_directory_is_private() {
        let path = create_temp_directory("neodots-installer-test").unwrap();

        let metadata = fs::metadata(&path).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);

        fs::remove_dir(&path).unwrap();
    }

    #[test]
    fn secure_temp_file_is_private() {
        let path = create_temp_file("neodots-installer-test", "secret\n").unwrap();

        let metadata = fs::metadata(&path).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read_to_string(&path).unwrap(), "secret\n");

        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn secure_temp_names_are_unique() {
        let first = create_temp_file("neodots-installer-test", "one").unwrap();
        let second = create_temp_file("neodots-installer-test", "two").unwrap();

        assert_ne!(first, second);

        fs::remove_file(first).unwrap();
        fs::remove_file(second).unwrap();
    }
}
