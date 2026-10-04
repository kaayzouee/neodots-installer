// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 kaayzouee
// Author: https://github.com/kaayzouee

use std::{
    env,
    fs::{File, OpenOptions},
    io,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
};

use crate::privilege::{path_to_string, run_privileged_command};

pub const TRANSACTION_ROOT: &str = "/etc/neodots-installer/transactions";
pub const TRANSACTION_LOCK: &str = "/etc/neodots-installer/installer.lock";

const LOCK_EX: i32 = 2;
const LOCK_UN: i32 = 8;
const LOCK_NB: i32 = 4;

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

#[derive(Debug, Clone)]
pub struct TargetRoot {
    root: PathBuf,
}

impl TargetRoot {
    pub fn from_environment() -> Result<Self, String> {
        let root = env::var_os("NEODOTS_NIXOS_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));

        Self::from_path(root)
    }

    pub fn from_path(root: PathBuf) -> Result<Self, String> {
        if !root.is_absolute() {
            return Err(format!(
                "target root must be an absolute path: {}",
                root.display()
            ));
        }

        if !root.is_dir() {
            return Err(format!(
                "target root does not exist or is not a directory: {}",
                root.display()
            ));
        }

        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn is_live_root(&self) -> bool {
        self.root == Path::new("/")
    }

    pub fn path(&self, suffix: &str) -> PathBuf {
        if suffix.is_empty() {
            self.root.clone()
        } else {
            self.root.join(suffix.trim_start_matches('/'))
        }
    }

    pub fn os_release_path(&self) -> PathBuf {
        self.path("/etc/os-release")
    }

    pub fn passwd_path(&self) -> PathBuf {
        self.path("/etc/passwd")
    }

    pub fn hostname_path(&self) -> PathBuf {
        self.path("/etc/hostname")
    }

    pub fn nixos_config_dir(&self) -> PathBuf {
        self.path("/etc/nixos")
    }

    pub fn transaction_root(&self) -> PathBuf {
        self.path(TRANSACTION_ROOT)
    }

    pub fn transaction_lock(&self) -> PathBuf {
        self.path(TRANSACTION_LOCK)
    }

    pub fn hardware_config(&self) -> PathBuf {
        self.nixos_config_dir().join("hardware-configuration.nix")
    }

    pub fn machine_config(&self) -> PathBuf {
        self.nixos_config_dir()
            .join("hosts")
            .join("nixos")
            .join("machine.nix")
    }
}

pub struct InstallerLock {
    file: File,
}

impl Drop for InstallerLock {
    fn drop(&mut self) {
        unsafe {
            let _ = flock(self.file.as_raw_fd(), LOCK_UN);
        }
    }
}

pub fn acquire_installer_lock(
    target: &TargetRoot,
    privilege_helper: &Path,
) -> Result<InstallerLock, String> {
    let lock_path = target.transaction_lock();
    let lock_parent = lock_path.parent().ok_or_else(|| {
        format!(
            "installer lock has no parent directory: {}",
            lock_path.display()
        )
    })?;

    let lock_parent_string = path_to_string(lock_parent)?;
    let lock_path_string = path_to_string(&lock_path)?;

    run_privileged_command(
        privilege_helper,
        "mkdir",
        &["-m", "0755", "-p", lock_parent_string.as_str()],
    )?;
    run_privileged_command(
        privilege_helper,
        "touch",
        &["--", lock_path_string.as_str()],
    )?;
    run_privileged_command(
        privilege_helper,
        "chmod",
        &["0644", "--", lock_path_string.as_str()],
    )?;

    let file = OpenOptions::new()
        .read(true)
        .open(&lock_path)
        .map_err(|error| {
            format!(
                "failed to open installer lock {}: {error}",
                lock_path.display()
            )
        })?;

    let result = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
    if result != 0 {
        let error = io::Error::last_os_error();
        return Err(format!(
            "another Neodots installer instance is already running or holds {}: {error}",
            lock_path.display()
        ));
    }

    println!("[lock]");
    println!("  ✓ acquired installer lock: {}", lock_path.display());
    Ok(InstallerLock { file })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_root_maps_paths_correctly() {
        let live = TargetRoot::from_path(PathBuf::from("/")).unwrap();

        assert_eq!(live.os_release_path(), PathBuf::from("/etc/os-release"));
        assert_eq!(live.passwd_path(), PathBuf::from("/etc/passwd"));
        assert_eq!(live.hostname_path(), PathBuf::from("/etc/hostname"));
        assert_eq!(live.nixos_config_dir(), PathBuf::from("/etc/nixos"));
        assert_eq!(
            live.machine_config(),
            PathBuf::from("/etc/nixos/hosts/nixos/machine.nix")
        );

        let mounted = TargetRoot {
            root: PathBuf::from("/mnt"),
        };

        assert_eq!(
            mounted.os_release_path(),
            PathBuf::from("/mnt/etc/os-release")
        );
        assert_eq!(mounted.passwd_path(), PathBuf::from("/mnt/etc/passwd"));
        assert_eq!(mounted.hostname_path(), PathBuf::from("/mnt/etc/hostname"));
        assert_eq!(mounted.nixos_config_dir(), PathBuf::from("/mnt/etc/nixos"));
        assert_eq!(
            mounted.machine_config(),
            PathBuf::from("/mnt/etc/nixos/hosts/nixos/machine.nix")
        );
        assert_eq!(
            mounted.transaction_root(),
            PathBuf::from("/mnt/etc/neodots-installer/transactions")
        );
        assert_eq!(
            mounted.transaction_lock(),
            PathBuf::from("/mnt/etc/neodots-installer/installer.lock")
        );
    }
}
