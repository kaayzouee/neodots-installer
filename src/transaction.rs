// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 kaayzouee
// Author: https://github.com/kaayzouee

use std::{
    fs::{self, File},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use crate::{
    privilege::{
        create_temp_file, path_to_string, remove_path_privileged, run_privileged_command,
        timestamp_nanos,
    },
    target::TargetRoot,
};

const TRANSACTION_PREFIX: &str = ".neodots-installer-txn-";
const LEGACY_TRANSACTION_DIR_PREFIX: &str = ".neodots-installer-txn-";
const TRANSACTION_STATE_FILE: &str = "state";
const TRANSACTION_STATE_STAGE: &str = "state.stage";
const TRANSACTION_MACHINE_STAGE: &str = "machine.nix.stage";
const TRANSACTION_MACHINE_EXPECTED: &str = "machine.nix.expected";
const TRANSACTION_MACHINE_BACKUP: &str = "machine.nix.backup";
const TRANSACTION_HARDWARE_STAGE: &str = "hardware-configuration.nix.stage";
const TRANSACTION_HARDWARE_EXPECTED: &str = "hardware-configuration.nix.expected";

const STATE_PREPARED: &str = "prepared";
const STATE_INSTALLING_HARDWARE: &str = "installing-hardware";
const STATE_HARDWARE_INSTALLED: &str = "hardware-installed";
const STATE_INSTALLING_MACHINE: &str = "installing-machine";
const STATE_MACHINE_INSTALLED: &str = "machine-installed";

pub fn install_configuration_transactionally(
    target: &TargetRoot,
    privilege_helper: &Path,
    machine_source: &Path,
    expected_machine_contents: Option<&str>,
    generated_hardware_source: Option<&Path>,
) -> Result<(), String> {
    let machine_destination = target.machine_config();
    let hardware_destination = target.hardware_config();
    let nixos_config_dir = target.nixos_config_dir();
    let transaction_root = target.transaction_root();

    let machine_parent = machine_destination.parent().ok_or_else(|| {
        format!(
            "machine configuration has no parent directory: {}",
            machine_destination.display()
        )
    })?;

    if !machine_parent.is_dir() {
        return Err(format!(
            "machine configuration parent directory does not exist: {}",
            machine_parent.display()
        ));
    }

    if !nixos_config_dir.is_dir() {
        return Err(format!(
            "NixOS configuration directory does not exist: {}",
            nixos_config_dir.display()
        ));
    }

    if let Some(generated_hardware) = generated_hardware_source {
        if hardware_destination.exists() {
            return Err(format!(
                "hardware configuration appeared during installation; refusing to overwrite {}",
                hardware_destination.display()
            ));
        }

        if !generated_hardware.is_file() {
            return Err(format!(
                "generated hardware configuration is missing: {}",
                generated_hardware.display()
            ));
        }
    }

    let transaction_id = format!("{}-{}", std::process::id(), timestamp_nanos());
    let transaction_dir = transaction_root.join(format!("{TRANSACTION_PREFIX}{transaction_id}"));
    let machine_stage = transaction_dir.join(TRANSACTION_MACHINE_STAGE);
    let machine_expected = transaction_dir.join(TRANSACTION_MACHINE_EXPECTED);
    let machine_backup = transaction_dir.join(TRANSACTION_MACHINE_BACKUP);
    let hardware_stage = transaction_dir.join(TRANSACTION_HARDWARE_STAGE);
    let hardware_expected = transaction_dir.join(TRANSACTION_HARDWARE_EXPECTED);

    let transaction_result = (|| -> Result<(), String> {
        let transaction_root_string = path_to_string(&transaction_root)?;
        let transaction_dir_string = path_to_string(&transaction_dir)?;

        run_privileged_command(
            privilege_helper,
            "mkdir",
            &["-m", "0755", "-p", transaction_root_string.as_str()],
        )?;

        run_privileged_command(
            privilege_helper,
            "mkdir",
            &["-m", "0755", "-p", transaction_dir_string.as_str()],
        )?;

        sync_path(&transaction_root)?;
        sync_path(&transaction_dir)?;

        verify_expected_machine_state(target, expected_machine_contents)?;

        write_transaction_state(privilege_helper, &transaction_dir, STATE_PREPARED)?;

        install_file_privileged(privilege_helper, machine_source, &machine_stage)?;
        install_file_privileged(privilege_helper, machine_source, &machine_expected)?;

        if machine_destination.exists() {
            install_file_privileged(privilege_helper, &machine_destination, &machine_backup)?;
        }

        if let Some(source) = generated_hardware_source {
            install_file_privileged(privilege_helper, source, &hardware_stage)?;
            install_file_privileged(privilege_helper, source, &hardware_expected)?;
        }

        sync_path(&machine_stage)?;
        sync_path(&machine_expected)?;

        if machine_backup.is_file() {
            sync_path(&machine_backup)?;
        }

        if generated_hardware_source.is_some() {
            sync_path(&hardware_stage)?;
            sync_path(&hardware_expected)?;
        }

        sync_path(&transaction_dir)?;

        if generated_hardware_source.is_some() {
            write_transaction_state(
                privilege_helper,
                &transaction_dir,
                STATE_INSTALLING_HARDWARE,
            )?;

            move_path_without_overwrite(privilege_helper, &hardware_stage, &hardware_destination)?;

            write_transaction_state(privilege_helper, &transaction_dir, STATE_HARDWARE_INSTALLED)?;
        }

        verify_expected_machine_state(target, expected_machine_contents)?;

        write_transaction_state(privilege_helper, &transaction_dir, STATE_INSTALLING_MACHINE)?;

        replace_path_atomically(privilege_helper, &machine_stage, &machine_destination)?;

        write_transaction_state(privilege_helper, &transaction_dir, STATE_MACHINE_INSTALLED)?;

        verify_completed_transaction(target, &transaction_dir)?;

        println!(
            "    ✓ machine.nix installed at {}",
            machine_destination.display()
        );

        if generated_hardware_source.is_some() {
            println!(
                "    ✓ generated hardware configuration installed at {}",
                hardware_destination.display()
            );
        }

        Ok(())
    })();

    match transaction_result {
        Ok(()) => {
            if let Err(error) = cleanup_transaction_directory(privilege_helper, &transaction_dir) {
                eprintln!("    ! installation succeeded, but transaction cleanup failed: {error}");
            }

            Ok(())
        }
        Err(error) => match recover_transaction(target, privilege_helper, &transaction_dir) {
            Ok(()) => Err(format!(
                "configuration installation failed; the previous configuration was restored: {error}"
            )),
            Err(recovery_error) => Err(format!(
                "configuration installation failed: {error}; automatic recovery also failed: {recovery_error}; transaction data was left at {} for recovery",
                transaction_dir.display()
            )),
        },
    }
}

pub fn recover_pending_transactions(
    target: &TargetRoot,
    privilege_helper: &Path,
) -> Result<(), String> {
    let transaction_root = target.transaction_root();

    if !transaction_root.exists() {
        return Ok(());
    }

    let entries = fs::read_dir(&transaction_root).map_err(|error| {
        format!(
            "failed to inspect transaction directory {}: {error}",
            transaction_root.display()
        )
    })?;

    let mut pending = Vec::new();

    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "failed to inspect an entry in {}: {error}",
                transaction_root.display()
            )
        })?;

        let path = entry.path();

        if path.is_dir()
            && entry
                .file_name()
                .to_string_lossy()
                .starts_with(TRANSACTION_PREFIX)
        {
            pending.push(path);
        }
    }

    for transaction_dir in pending {
        let state = read_transaction_state(&transaction_dir)?;

        println!("[transactions]");

        let Some(state) = state else {
            cleanup_transaction_directory(privilege_helper, &transaction_dir)?;

            println!(
                "  ✓ incomplete pre-mutation transaction cleaned up: {}",
                transaction_dir.display()
            );

            continue;
        };

        println!(
            "  ! found pending transaction: {} ({state})",
            transaction_dir.display()
        );

        match state.as_str() {
            STATE_PREPARED => {
                cleanup_transaction_directory(privilege_helper, &transaction_dir)?;
                println!("  ✓ stale transaction cleaned up");
            }

            STATE_MACHINE_INSTALLED => {
                verify_completed_transaction(target, &transaction_dir)?;
                cleanup_transaction_directory(privilege_helper, &transaction_dir)?;
                println!("  ✓ completed transaction verified and cleaned up");
            }

            STATE_INSTALLING_HARDWARE | STATE_HARDWARE_INSTALLED | STATE_INSTALLING_MACHINE => {
                recover_transaction(target, privilege_helper, &transaction_dir)?;
                println!("  ✓ interrupted transaction rolled back");
            }

            other => {
                return Err(format!(
                    "unknown transaction state `{other}` in {}",
                    transaction_dir.display()
                ));
            }
        }
    }

    Ok(())
}

pub fn recover_transaction(
    target: &TargetRoot,
    privilege_helper: &Path,
    transaction_dir: &Path,
) -> Result<(), String> {
    let state = read_transaction_state(transaction_dir)?;

    let Some(state) = state else {
        return cleanup_transaction_directory(privilege_helper, transaction_dir);
    };

    match state.as_str() {
        STATE_PREPARED => cleanup_transaction_directory(privilege_helper, transaction_dir),

        STATE_MACHINE_INSTALLED => {
            verify_completed_transaction(target, transaction_dir)?;
            cleanup_transaction_directory(privilege_helper, transaction_dir)
        }

        STATE_INSTALLING_HARDWARE | STATE_HARDWARE_INSTALLED => {
            rollback_hardware_transaction(target, privilege_helper, transaction_dir)?;
            cleanup_transaction_directory(privilege_helper, transaction_dir)
        }

        STATE_INSTALLING_MACHINE => {
            rollback_machine_transaction(target, privilege_helper, transaction_dir)?;
            rollback_hardware_transaction(target, privilege_helper, transaction_dir)?;
            cleanup_transaction_directory(privilege_helper, transaction_dir)
        }

        other => Err(format!(
            "cannot recover transaction {} with unknown state `{other}`",
            transaction_dir.display()
        )),
    }
}

fn rollback_machine_transaction(
    target: &TargetRoot,
    privilege_helper: &Path,
    transaction_dir: &Path,
) -> Result<(), String> {
    let machine_destination = target.machine_config();
    let machine_expected = transaction_dir.join(TRANSACTION_MACHINE_EXPECTED);
    let machine_backup = transaction_dir.join(TRANSACTION_MACHINE_BACKUP);

    if !machine_expected.is_file() {
        return Err(format!(
            "transaction is missing expected machine configuration: {}",
            machine_expected.display()
        ));
    }

    if machine_destination.exists() {
        if files_equal(&machine_destination, &machine_expected)? {
            remove_path_privileged(privilege_helper, &machine_destination)?;

            if let Some(parent) = machine_destination.parent() {
                sync_path(parent)?;
            }
        } else if machine_backup.is_file() && files_equal(&machine_destination, &machine_backup)? {
            return Ok(());
        } else {
            return Err(format!(
                "cannot safely roll back machine.nix because {} differs from both the expected new configuration and its backup",
                machine_destination.display()
            ));
        }
    }

    if machine_backup.is_file() {
        replace_path_atomically(privilege_helper, &machine_backup, &machine_destination)?;
    }

    Ok(())
}

fn rollback_hardware_transaction(
    target: &TargetRoot,
    privilege_helper: &Path,
    transaction_dir: &Path,
) -> Result<(), String> {
    let hardware_destination = target.hardware_config();
    let hardware_expected = transaction_dir.join(TRANSACTION_HARDWARE_EXPECTED);

    if !hardware_expected.is_file() || !hardware_destination.exists() {
        return Ok(());
    }

    if !files_equal(&hardware_destination, &hardware_expected)? {
        return Err(format!(
            "cannot safely roll back hardware configuration because {} differs from the transaction's generated configuration",
            hardware_destination.display()
        ));
    }

    remove_path_privileged(privilege_helper, &hardware_destination)?;

    if let Some(parent) = hardware_destination.parent() {
        sync_path(parent)?;
    }

    Ok(())
}

fn verify_completed_transaction(target: &TargetRoot, transaction_dir: &Path) -> Result<(), String> {
    let machine_destination = target.machine_config();
    let machine_expected = transaction_dir.join(TRANSACTION_MACHINE_EXPECTED);
    let hardware_destination = target.hardware_config();
    let hardware_expected = transaction_dir.join(TRANSACTION_HARDWARE_EXPECTED);

    if !machine_expected.is_file() {
        return Err(format!(
            "completed transaction is missing expected machine configuration: {}",
            machine_expected.display()
        ));
    }

    if !machine_destination.is_file() {
        return Err(format!(
            "completed transaction is missing installed machine configuration: {}",
            machine_destination.display()
        ));
    }

    if !files_equal(&machine_destination, &machine_expected)? {
        return Err(format!(
            "completed transaction cannot be finalized because {} differs from the expected machine configuration",
            machine_destination.display()
        ));
    }

    if hardware_expected.exists() {
        if !hardware_expected.is_file() {
            return Err(format!(
                "completed transaction has an invalid expected hardware configuration: {}",
                hardware_expected.display()
            ));
        }

        if !hardware_destination.is_file() {
            return Err(format!(
                "completed transaction is missing installed hardware configuration: {}",
                hardware_destination.display()
            ));
        }

        if !files_equal(&hardware_destination, &hardware_expected)? {
            return Err(format!(
                "completed transaction cannot be finalized because {} differs from the expected generated hardware configuration",
                hardware_destination.display()
            ));
        }
    }

    Ok(())
}

fn write_transaction_state(
    privilege_helper: &Path,
    transaction_dir: &Path,
    state: &str,
) -> Result<(), String> {
    let state_temp = create_temp_file("neodots-installer-state", &format!("{state}\n"))?;
    let state_path = transaction_dir.join(TRANSACTION_STATE_FILE);
    let state_stage = transaction_dir.join(TRANSACTION_STATE_STAGE);

    let result = (|| {
        install_file_privileged(privilege_helper, &state_temp, &state_stage)?;
        replace_path_atomically(privilege_helper, &state_stage, &state_path)
    })();

    fs::remove_file(&state_temp).ok();

    result
}

fn read_transaction_state(transaction_dir: &Path) -> Result<Option<String>, String> {
    let state_path = transaction_dir.join(TRANSACTION_STATE_FILE);

    let state = match fs::read_to_string(&state_path) {
        Ok(state) => state,

        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }

        Err(error) => {
            return Err(format!(
                "cannot read transaction state {}: {error}",
                state_path.display()
            ));
        }
    };

    let state = state.trim().to_string();

    if state.is_empty() {
        return Err(format!(
            "transaction state is empty: {}",
            state_path.display()
        ));
    }

    Ok(Some(state))
}

fn cleanup_transaction_directory(
    privilege_helper: &Path,
    transaction_dir: &Path,
) -> Result<(), String> {
    if !transaction_dir.exists() {
        return Ok(());
    }

    remove_path_privileged(privilege_helper, transaction_dir)?;

    if let Some(parent) = transaction_dir.parent() {
        sync_path(parent)?;
    }

    Ok(())
}

fn verify_expected_machine_state(
    target: &TargetRoot,
    expected_machine_contents: Option<&str>,
) -> Result<(), String> {
    let destination = target.machine_config();

    match expected_machine_contents {
        Some(expected) => {
            let current = fs::read_to_string(&destination).map_err(|error| {
                format!(
                    "cannot re-read {} before installation: {error}",
                    destination.display()
                )
            })?;

            if current != expected {
                return Err(format!(
                    "{} changed since validation; refusing to overwrite an externally modified configuration",
                    destination.display()
                ));
            }
        }

        None => {
            if destination.exists() {
                return Err(format!(
                    "{} appeared after validation; refusing to overwrite an externally created configuration",
                    destination.display()
                ));
            }
        }
    }

    Ok(())
}

fn sync_path(path: &Path) -> Result<(), String> {
    let file = File::open(path).map_err(|error| {
        format!(
            "failed to open {} for durability sync: {error}",
            path.display()
        )
    })?;

    file.sync_all().map_err(|error| {
        format!(
            "failed to sync {} to stable storage: {error}",
            path.display()
        )
    })
}

fn replace_path_atomically(
    privilege_helper: &Path,
    source: &Path,
    destination: &Path,
) -> Result<(), String> {
    ensure_same_filesystem(source, destination)?;

    let source_string = path_to_string(source)?;
    let destination_string = path_to_string(destination)?;

    run_privileged_command(
        privilege_helper,
        "mv",
        &["--", source_string.as_str(), destination_string.as_str()],
    )?;

    sync_path(destination)?;

    if let Some(parent) = destination.parent() {
        sync_path(parent)?;
    }

    if let Some(parent) = source.parent() {
        sync_path(parent)?;
    }

    Ok(())
}

fn move_path_without_overwrite(
    privilege_helper: &Path,
    source: &Path,
    destination: &Path,
) -> Result<(), String> {
    ensure_same_filesystem(source, destination)?;

    let source_string = path_to_string(source)?;
    let destination_string = path_to_string(destination)?;

    run_privileged_command(
        privilege_helper,
        "mv",
        &[
            "--no-clobber",
            "--",
            source_string.as_str(),
            destination_string.as_str(),
        ],
    )?;

    sync_path(destination)?;

    if let Some(parent) = destination.parent() {
        sync_path(parent)?;
    }

    if let Some(parent) = source.parent() {
        sync_path(parent)?;
    }

    if source.exists() {
        return Err(format!(
            "destination already exists; refusing to overwrite {}",
            destination.display()
        ));
    }

    Ok(())
}

fn ensure_same_filesystem(source: &Path, destination: &Path) -> Result<(), String> {
    let source_metadata = fs::metadata(source).map_err(|error| {
        format!(
            "failed to inspect transaction source {}: {error}",
            source.display()
        )
    })?;

    let destination_parent = destination.parent().ok_or_else(|| {
        format!(
            "destination has no parent directory: {}",
            destination.display()
        )
    })?;

    let destination_parent_metadata = fs::metadata(destination_parent).map_err(|error| {
        format!(
            "failed to inspect destination parent {}: {error}",
            destination_parent.display()
        )
    })?;

    if source_metadata.dev() != destination_parent_metadata.dev() {
        return Err(format!(
            "transaction source {} and destination {} are on different filesystems; refusing a non-atomic move",
            source.display(),
            destination.display()
        ));
    }

    Ok(())
}

fn files_equal(left: &Path, right: &Path) -> Result<bool, String> {
    let left_contents = fs::read(left).map_err(|error| {
        format!(
            "failed to read {} during transaction recovery: {error}",
            left.display()
        )
    })?;

    let right_contents = fs::read(right).map_err(|error| {
        format!(
            "failed to read {} during transaction recovery: {error}",
            right.display()
        )
    })?;

    Ok(left_contents == right_contents)
}

pub fn find_legacy_transaction(target: &TargetRoot) -> Result<Option<PathBuf>, String> {
    let entries = fs::read_dir(target.nixos_config_dir()).map_err(|error| {
        format!(
            "failed to inspect {} for legacy installer transactions: {error}",
            target.nixos_config_dir().display()
        )
    })?;

    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "failed to inspect an entry in {}: {error}",
                target.nixos_config_dir().display()
            )
        })?;

        let path = entry.path();

        if path.is_dir()
            && entry
                .file_name()
                .to_string_lossy()
                .starts_with(LEGACY_TRANSACTION_DIR_PREFIX)
        {
            return Ok(Some(path));
        }
    }

    Ok(None)
}

fn install_file_privileged(
    privilege_helper: &Path,
    source: &Path,
    destination: &Path,
) -> Result<(), String> {
    let destination_parent = destination.parent().ok_or_else(|| {
        format!(
            "destination has no parent directory: {}",
            destination.display()
        )
    })?;

    let destination_parent_string = path_to_string(destination_parent)?;
    let source_string = path_to_string(source)?;
    let destination_string = path_to_string(destination)?;

    run_privileged_command(
        privilege_helper,
        "mkdir",
        &["-p", destination_parent_string.as_str()],
    )?;

    run_privileged_command(
        privilege_helper,
        "install",
        &[
            "-m",
            "0644",
            source_string.as_str(),
            destination_string.as_str(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    const HARDWARE_CONFIG_FILE: &str = "hardware-configuration.nix";

    fn test_temp_dir(prefix: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            timestamp_nanos()
        ));

        fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }

        fs::write(path, contents).unwrap();
    }

    fn fake_privilege_helper() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "neodots-installer-test-helper-{}-{}",
            std::process::id(),
            timestamp_nanos()
        ));

        fs::write(&path, "#!/bin/sh\nexec \"$@\"\n").unwrap();

        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();

        path
    }

    #[test]
    fn transaction_root_is_outside_nixos_configuration() {
        let root = test_temp_dir("neodots-installer-transaction-root");
        let target = TargetRoot::from_path(root.clone()).unwrap();

        assert_eq!(
            target.transaction_root(),
            root.join("etc/neodots-installer/transactions")
        );

        assert!(
            !target
                .transaction_root()
                .starts_with(target.nixos_config_dir())
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn pending_legacy_transaction_is_detected() {
        let root = test_temp_dir("neodots-installer-legacy-transaction");
        let target_root = root.join("target");
        let nixos_dir = target_root.join("etc/nixos");
        let legacy_dir = nixos_dir.join(".neodots-installer-txn-legacy");

        fs::create_dir_all(&legacy_dir).unwrap();

        let target = TargetRoot::from_path(target_root).unwrap();

        assert_eq!(find_legacy_transaction(&target).unwrap(), Some(legacy_dir));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn atomic_replacement_preserves_new_contents() {
        let root = test_temp_dir("neodots-installer-atomic-replace");
        let helper = fake_privilege_helper();

        let source = root.join("machine.nix.stage");
        let destination = root.join("machine.nix");

        write_file(&source, "new configuration\n");
        write_file(&destination, "old configuration\n");

        replace_path_atomically(&helper, &source, &destination).unwrap();

        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            "new configuration\n"
        );

        assert!(!source.exists());

        fs::remove_file(helper).ok();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn transaction_state_replacement_preserves_new_state() {
        let root = test_temp_dir("neodots-installer-state-replace");
        let helper = fake_privilege_helper();
        let transaction_dir = root.join("transaction");

        fs::create_dir_all(&transaction_dir).unwrap();

        write_file(
            &transaction_dir.join(TRANSACTION_STATE_FILE),
            STATE_PREPARED,
        );

        write_transaction_state(&helper, &transaction_dir, STATE_INSTALLING_MACHINE).unwrap();

        assert_eq!(
            read_transaction_state(&transaction_dir).unwrap(),
            Some(STATE_INSTALLING_MACHINE.to_string())
        );

        assert!(!transaction_dir.join(TRANSACTION_STATE_STAGE).exists());

        fs::remove_file(helper).ok();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rollback_machine_restores_original_configuration() {
        let root = test_temp_dir("neodots-installer-rollback-machine");
        let helper = fake_privilege_helper();

        let target_root = root.join("target");
        let nixos_dir = target_root.join("etc/nixos");
        let machine_path = nixos_dir.join("hosts/nixos/machine.nix");
        let transaction_dir =
            target_root.join("etc/neodots-installer/transactions/.neodots-installer-txn-test");

        fs::create_dir_all(machine_path.parent().unwrap()).unwrap();
        fs::create_dir_all(&transaction_dir).unwrap();

        write_file(&machine_path, "new machine\n");

        write_file(
            &transaction_dir.join(TRANSACTION_MACHINE_EXPECTED),
            "new machine\n",
        );

        write_file(
            &transaction_dir.join(TRANSACTION_MACHINE_BACKUP),
            "old machine\n",
        );

        let target = TargetRoot::from_path(target_root).unwrap();

        rollback_machine_transaction(&target, &helper, &transaction_dir).unwrap();

        assert_eq!(fs::read_to_string(&machine_path).unwrap(), "old machine\n");

        fs::remove_file(helper).ok();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rollback_machine_refuses_unexpected_live_configuration() {
        let root = test_temp_dir("neodots-installer-rollback-refuse");
        let helper = fake_privilege_helper();

        let target_root = root.join("target");
        let nixos_dir = target_root.join("etc/nixos");
        let machine_path = nixos_dir.join("hosts/nixos/machine.nix");
        let transaction_dir =
            target_root.join("etc/neodots-installer/transactions/.neodots-installer-txn-test");

        fs::create_dir_all(machine_path.parent().unwrap()).unwrap();
        fs::create_dir_all(&transaction_dir).unwrap();

        write_file(&machine_path, "unexpected configuration\n");

        write_file(
            &transaction_dir.join(TRANSACTION_MACHINE_EXPECTED),
            "new machine\n",
        );

        write_file(
            &transaction_dir.join(TRANSACTION_MACHINE_BACKUP),
            "old machine\n",
        );

        let target = TargetRoot::from_path(target_root).unwrap();

        let error = rollback_machine_transaction(&target, &helper, &transaction_dir).unwrap_err();

        assert!(error.contains("differs from both the expected new configuration and its backup"));

        assert_eq!(
            fs::read_to_string(&machine_path).unwrap(),
            "unexpected configuration\n"
        );

        fs::remove_file(helper).ok();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn recover_missing_state_transaction_cleans_up_without_touching_configuration() {
        let root = test_temp_dir("neodots-installer-recover-missing-state");
        let helper = fake_privilege_helper();

        let target_root = root.join("target");
        let nixos_dir = target_root.join("etc/nixos");
        let machine_path = nixos_dir.join("hosts/nixos/machine.nix");
        let transaction_dir =
            target_root.join("etc/neodots-installer/transactions/.neodots-installer-txn-test");

        fs::create_dir_all(machine_path.parent().unwrap()).unwrap();
        fs::create_dir_all(&transaction_dir).unwrap();

        write_file(&machine_path, "existing machine\n");

        write_file(
            &transaction_dir.join(TRANSACTION_MACHINE_EXPECTED),
            "new machine\n",
        );

        write_file(
            &transaction_dir.join(TRANSACTION_MACHINE_BACKUP),
            "old machine\n",
        );

        write_file(
            &transaction_dir.join(TRANSACTION_MACHINE_STAGE),
            "new machine\n",
        );

        let target = TargetRoot::from_path(target_root).unwrap();

        recover_transaction(&target, &helper, &transaction_dir).unwrap();

        assert_eq!(
            fs::read_to_string(&machine_path).unwrap(),
            "existing machine\n"
        );

        assert!(!transaction_dir.exists());

        fs::remove_file(helper).ok();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn recover_interrupted_machine_transaction_restores_machine_and_removes_hardware() {
        let root = test_temp_dir("neodots-installer-recover-machine");
        let helper = fake_privilege_helper();

        let target_root = root.join("target");
        let nixos_dir = target_root.join("etc/nixos");
        let machine_path = nixos_dir.join("hosts/nixos/machine.nix");
        let hardware_path = nixos_dir.join(HARDWARE_CONFIG_FILE);
        let transaction_dir =
            target_root.join("etc/neodots-installer/transactions/.neodots-installer-txn-test");

        fs::create_dir_all(machine_path.parent().unwrap()).unwrap();
        fs::create_dir_all(&transaction_dir).unwrap();

        write_file(&machine_path, "new machine\n");
        write_file(&hardware_path, "generated hardware\n");

        write_file(
            &transaction_dir.join(TRANSACTION_MACHINE_EXPECTED),
            "new machine\n",
        );

        write_file(
            &transaction_dir.join(TRANSACTION_MACHINE_BACKUP),
            "old machine\n",
        );

        write_file(
            &transaction_dir.join(TRANSACTION_HARDWARE_EXPECTED),
            "generated hardware\n",
        );

        write_file(
            &transaction_dir.join(TRANSACTION_STATE_FILE),
            STATE_INSTALLING_MACHINE,
        );

        let target = TargetRoot::from_path(target_root).unwrap();

        recover_transaction(&target, &helper, &transaction_dir).unwrap();

        assert_eq!(fs::read_to_string(&machine_path).unwrap(), "old machine\n");

        assert!(!hardware_path.exists());
        assert!(!transaction_dir.exists());

        fs::remove_file(helper).ok();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn recover_interrupted_hardware_transaction_removes_generated_hardware() {
        let root = test_temp_dir("neodots-installer-recover-hardware");
        let helper = fake_privilege_helper();

        let target_root = root.join("target");
        let nixos_dir = target_root.join("etc/nixos");
        let hardware_path = nixos_dir.join(HARDWARE_CONFIG_FILE);
        let machine_path = nixos_dir.join("hosts/nixos/machine.nix");
        let transaction_dir =
            target_root.join("etc/neodots-installer/transactions/.neodots-installer-txn-test");

        fs::create_dir_all(machine_path.parent().unwrap()).unwrap();
        fs::create_dir_all(&transaction_dir).unwrap();

        write_file(&machine_path, "existing machine\n");
        write_file(&hardware_path, "generated hardware\n");

        write_file(
            &transaction_dir.join(TRANSACTION_HARDWARE_EXPECTED),
            "generated hardware\n",
        );

        write_file(
            &transaction_dir.join(TRANSACTION_STATE_FILE),
            STATE_HARDWARE_INSTALLED,
        );

        let target = TargetRoot::from_path(target_root).unwrap();

        recover_transaction(&target, &helper, &transaction_dir).unwrap();

        assert!(!hardware_path.exists());
        assert!(!transaction_dir.exists());

        fs::remove_file(helper).ok();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn recover_pending_machine_installed_transaction_verifies_and_cleans_up() {
        let root = test_temp_dir("neodots-installer-recover-completed");
        let helper = fake_privilege_helper();

        let target_root = root.join("target");
        let nixos_dir = target_root.join("etc/nixos");
        let machine_path = nixos_dir.join("hosts/nixos/machine.nix");
        let transaction_dir =
            target_root.join("etc/neodots-installer/transactions/.neodots-installer-txn-test");

        fs::create_dir_all(machine_path.parent().unwrap()).unwrap();
        fs::create_dir_all(&transaction_dir).unwrap();

        write_file(&machine_path, "new machine\n");

        write_file(
            &transaction_dir.join(TRANSACTION_MACHINE_EXPECTED),
            "new machine\n",
        );

        write_file(
            &transaction_dir.join(TRANSACTION_STATE_FILE),
            STATE_MACHINE_INSTALLED,
        );

        let target = TargetRoot::from_path(target_root).unwrap();

        recover_pending_transactions(&target, &helper).unwrap();

        assert!(!transaction_dir.exists());

        fs::remove_file(helper).ok();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn refuses_external_machine_change_before_installation() {
        let root = test_temp_dir("neodots-installer-concurrency-check");
        let helper = fake_privilege_helper();

        let target_root = root.join("target");
        let nixos_dir = target_root.join("etc/nixos");
        let machine_path = nixos_dir.join("hosts/nixos/machine.nix");
        let machine_source = root.join("machine-source.nix");

        fs::create_dir_all(machine_path.parent().unwrap()).unwrap();

        write_file(&machine_path, "externally changed\n");
        write_file(&machine_source, "new machine\n");

        let target = TargetRoot::from_path(target_root).unwrap();

        let error = install_configuration_transactionally(
            &target,
            &helper,
            &machine_source,
            Some("old validated machine\n"),
            None,
        )
        .unwrap_err();

        assert!(error.contains("changed since validation"));

        assert_eq!(
            fs::read_to_string(&machine_path).unwrap(),
            "externally changed\n"
        );

        fs::remove_file(helper).ok();
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn successful_transaction_installs_machine_and_generated_hardware() {
        let root = test_temp_dir("neodots-installer-transaction-success");
        let helper = fake_privilege_helper();

        let target_root = root.join("target");
        let nixos_dir = target_root.join("etc/nixos");
        let machine_path = nixos_dir.join("hosts/nixos/machine.nix");
        let hardware_path = nixos_dir.join(HARDWARE_CONFIG_FILE);
        let machine_source = root.join("machine-source.nix");
        let hardware_source = root.join("hardware-source.nix");

        fs::create_dir_all(machine_path.parent().unwrap()).unwrap();

        write_file(&machine_path, "old machine\n");
        write_file(&machine_source, "new machine\n");
        write_file(&hardware_source, "generated hardware\n");

        let target = TargetRoot::from_path(target_root).unwrap();

        install_configuration_transactionally(
            &target,
            &helper,
            &machine_source,
            Some("old machine\n"),
            Some(&hardware_source),
        )
        .unwrap();

        assert_eq!(fs::read_to_string(&machine_path).unwrap(), "new machine\n");

        assert_eq!(
            fs::read_to_string(&hardware_path).unwrap(),
            "generated hardware\n"
        );

        assert!(
            !target.transaction_root().exists()
                || fs::read_dir(target.transaction_root())
                    .unwrap()
                    .next()
                    .is_none()
        );

        fs::remove_file(helper).ok();
        fs::remove_dir_all(root).ok();
    }
}
