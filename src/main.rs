// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 kaayzouee
// Author: https://github.com/kaayzouee

use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::{
        fd::AsRawFd,
        unix::{fs as unix_fs, fs::MetadataExt},
    },
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::{SystemTime, UNIX_EPOCH},
};

const HARDWARE_CONFIG_FILE: &str = "hardware-configuration.nix";
const GENERATE_CONFIG_COMMAND: &str = "nixos-generate-config";
const NIX_COMMAND: &str = "nix";

const TRANSACTION_ROOT: &str = "/etc/neodots-installer/transactions";
const TRANSACTION_LOCK: &str = "/etc/neodots-installer/installer.lock";
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

const LOCK_EX: i32 = 2;
const LOCK_UN: i32 = 8;
const LOCK_NB: i32 = 4;

unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HardwarePreparation {
    Existing,
    GenerateInStaging,
    Unavailable,
}

#[derive(Debug, Clone)]
struct TargetRoot {
    root: PathBuf,
}

impl TargetRoot {
    fn from_environment() -> Result<Self, String> {
        let root = env::var_os("NEODOTS_NIXOS_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"));

        Self::from_path(root)
    }

    fn from_path(root: PathBuf) -> Result<Self, String> {
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

    fn root(&self) -> &Path {
        &self.root
    }

    fn is_live_root(&self) -> bool {
        self.root == Path::new("/")
    }

    fn path(&self, suffix: &str) -> PathBuf {
        if suffix.is_empty() {
            self.root.clone()
        } else {
            self.root.join(suffix.trim_start_matches('/'))
        }
    }

    fn os_release_path(&self) -> PathBuf {
        self.path("/etc/os-release")
    }

    fn passwd_path(&self) -> PathBuf {
        self.path("/etc/passwd")
    }

    fn hostname_path(&self) -> PathBuf {
        self.path("/etc/hostname")
    }

    fn nixos_config_dir(&self) -> PathBuf {
        self.path("/etc/nixos")
    }

    fn transaction_root(&self) -> PathBuf {
        self.path(TRANSACTION_ROOT)
    }

    fn transaction_lock(&self) -> PathBuf {
        self.path(TRANSACTION_LOCK)
    }

    fn hardware_config(&self) -> PathBuf {
        self.nixos_config_dir().join(HARDWARE_CONFIG_FILE)
    }

    fn machine_config(&self) -> PathBuf {
        self.nixos_config_dir()
            .join("hosts")
            .join("nixos")
            .join("machine.nix")
    }
}

struct InstallerLock {
    file: File,
}

impl Drop for InstallerLock {
    fn drop(&mut self) {
        unsafe {
            let _ = flock(self.file.as_raw_fd(), LOCK_UN);
        }
    }
}

#[derive(Debug, Clone)]
struct MachineConfig {
    system: String,
    username: String,
    hostname: String,
    home_directory: String,
    personal_enable: bool,
    persistence_enable: bool,
    persistence_path: String,
}

fn main() -> ExitCode {
    println!("Neodots installer preflight scanner");
    println!();

    let target = match TargetRoot::from_environment() {
        Ok(target) => target,
        Err(error) => {
            eprintln!("  ✗ target: {error}");
            return ExitCode::FAILURE;
        }
    };

    println!("[target]");
    println!("  ✓ root: {}", target.root().display());

    if let Err(error) = check_nixos(&target) {
        eprintln!("  ✗ {error}");
        return ExitCode::FAILURE;
    }

    println!("[nixos]");
    println!("  ✓ target identifies itself as NixOS");

    let privilege_helper = match check_privilege_helper() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("  ✗ {error}");
            return ExitCode::FAILURE;
        }
    };

    let _installer_lock = match acquire_installer_lock(&target, &privilege_helper) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("  ✗ {error}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(error) = recover_pending_transactions(&target, &privilege_helper) {
        eprintln!("  ✗ {error}");
        return ExitCode::FAILURE;
    }

    let hardware = match check_hardware_configuration(&target) {
        Ok(result) => result,
        Err(error) => {
            eprintln!("  ✗ {error}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(error) = check_git() {
        eprintln!("  ✗ {error}");
        return ExitCode::FAILURE;
    }

    if let Err(error) = check_nix() {
        eprintln!("  ✗ {error}");
        return ExitCode::FAILURE;
    }

    let machine = match detect_machine_config(&target) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("  ✗ {error}");
            return ExitCode::FAILURE;
        }
    };

    print_machine_summary(&target, &machine);

    match handle_machine_config(&target, &machine, hardware, &privilege_helper) {
        Ok(()) => {
            println!();
            println!("All preflight checks passed.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!();
            eprintln!("  ✗ {error}");
            ExitCode::FAILURE
        }
    }
}

fn acquire_installer_lock(
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

fn check_nixos(target: &TargetRoot) -> Result<(), String> {
    let contents = fs::read_to_string(target.os_release_path()).map_err(|error| {
        format!(
            "cannot read {}: {error}",
            target.os_release_path().display()
        )
    })?;

    let is_nixos = contents.lines().any(|line| {
        let line = line.trim();
        line == "ID=nixos" || line == "ID=\"nixos\""
    });

    if !is_nixos {
        return Err(format!(
            "target does not identify itself as NixOS: {}",
            target.os_release_path().display()
        ));
    }

    Ok(())
}

fn check_hardware_configuration(target: &TargetRoot) -> Result<HardwarePreparation, String> {
    let nixos_dir = target.nixos_config_dir();

    if !nixos_dir.is_dir() {
        return Err(format!(
            "target NixOS configuration directory is missing: {}",
            nixos_dir.display()
        ));
    }

    let hardware_config = target.hardware_config();

    if hardware_config.is_file() {
        println!("[hardware]");
        println!("  ✓ found: {}", hardware_config.display());
        return Ok(HardwarePreparation::Existing);
    }

    println!("[hardware]");
    println!("  ! missing: {}", hardware_config.display());
    println!("  Hardware configuration can be generated in disposable validation staging.");

    if prompt_yes_no("  Generate hardware configuration during validation? [y/N] ")? {
        Ok(HardwarePreparation::GenerateInStaging)
    } else {
        Ok(HardwarePreparation::Unavailable)
    }
}

fn check_git() -> Result<(), String> {
    match find_in_path("git") {
        Some(path) => {
            println!("[git]");
            println!("  ✓ found: {}", path.display());
            Ok(())
        }
        None => Err("git was not found in PATH".to_string()),
    }
}

fn check_nix() -> Result<(), String> {
    match find_in_path(NIX_COMMAND) {
        Some(path) => {
            println!("[nix]");
            println!("  ✓ found: {}", path.display());
            Ok(())
        }
        None => Err("nix was not found in PATH".to_string()),
    }
}

fn check_privilege_helper() -> Result<PathBuf, String> {
    match find_privilege_helper() {
        Some(path) => {
            println!("[privilege]");
            println!("  ✓ found: {}", path.display());
            Ok(path)
        }
        None => Err("neither sudo nor doas was found in PATH".to_string()),
    }
}

fn detect_machine_config(target: &TargetRoot) -> Result<MachineConfig, String> {
    println!("[machine]");

    let system = detect_system()?;
    println!("  ✓ detected system: {system}");

    let username = detect_target_username(target)?;
    println!("  ✓ detected username: {username}");

    if username == "guest" {
        return Err("guest is reserved and cannot be used as the target user".to_string());
    }

    let hostname = detect_hostname(target)?;
    println!("  ✓ detected hostname: {hostname}");

    let home_directory = lookup_home_directory(target, &username)?;
    println!("  ✓ detected home directory: {home_directory}");

    let existing = if target.machine_config().is_file() {
        Some(read_existing_machine_config(&target.machine_config())?)
    } else {
        None
    };

    let personal_enable = existing
        .as_ref()
        .map(|config| config.personal_enable)
        .unwrap_or(false);

    let persistence_enable = existing
        .as_ref()
        .map(|config| config.persistence_enable)
        .unwrap_or(false);

    let persistence_path = existing
        .as_ref()
        .map(|config| config.persistence_path.clone())
        .unwrap_or_else(|| "/persist".to_string());

    println!(
        "  ✓ personal configuration: {} ({})",
        if personal_enable {
            "enabled"
        } else {
            "disabled"
        },
        if existing.is_some() {
            "preserved"
        } else {
            "default"
        }
    );

    println!(
        "  ✓ persistence: {} (path {})",
        if persistence_enable {
            "enabled"
        } else {
            "disabled"
        },
        persistence_path
    );

    Ok(MachineConfig {
        system,
        username,
        hostname,
        home_directory,
        personal_enable,
        persistence_enable,
        persistence_path,
    })
}

fn detect_system() -> Result<String, String> {
    let output = Command::new("uname")
        .arg("-m")
        .output()
        .map_err(|error| format!("failed to execute uname: {error}"))?;

    if !output.status.success() {
        return Err(format!("uname failed with status {}", output.status));
    }

    let architecture = String::from_utf8(output.stdout)
        .map_err(|error| format!("uname returned invalid UTF-8: {error}"))?
        .trim()
        .to_string();

    let system = match architecture.as_str() {
        "x86_64" => "x86_64-linux",
        "aarch64" => "aarch64-linux",
        "armv7l" => "armv7l-linux",
        other => {
            return Err(format!(
                "unsupported system architecture reported by uname: {other}"
            ));
        }
    };

    Ok(system.to_string())
}

fn detect_target_username(target: &TargetRoot) -> Result<String, String> {
    if let Some(username) = env::var_os("NEODOTS_TARGET_USER") {
        let username = username
            .into_string()
            .map_err(|_| "NEODOTS_TARGET_USER is not valid UTF-8".to_string())?;

        if username.trim().is_empty() {
            return Err("NEODOTS_TARGET_USER is empty".to_string());
        }

        return Ok(username);
    }

    if !target.is_live_root() {
        return Err("NEODOTS_TARGET_USER must be set when NEODOTS_NIXOS_ROOT is not /".to_string());
    }

    for variable in ["SUDO_USER", "USER", "LOGNAME"] {
        if let Some(username) = env::var_os(variable) {
            let username = username
                .into_string()
                .map_err(|_| format!("{variable} is not valid UTF-8"))?;

            if !username.trim().is_empty() && username != "root" {
                return Ok(username);
            }
        }
    }

    Err("could not determine the target username; set NEODOTS_TARGET_USER explicitly".to_string())
}

fn lookup_home_directory(target: &TargetRoot, username: &str) -> Result<String, String> {
    let passwd = fs::read_to_string(target.passwd_path())
        .map_err(|error| format!("cannot read {}: {error}", target.passwd_path().display()))?;

    for line in passwd.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }

        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() < 6 {
            continue;
        }

        if fields[0] == username {
            let home = fields[5].trim();

            if home.is_empty() {
                return Err(format!(
                    "target user {username} has an empty home directory in {}",
                    target.passwd_path().display()
                ));
            }

            return Ok(home.to_string());
        }
    }

    Err(format!(
        "target user {username} was not found in {}",
        target.passwd_path().display()
    ))
}

fn detect_hostname(target: &TargetRoot) -> Result<String, String> {
    let hostname_path = target.hostname_path();

    if let Ok(contents) = fs::read_to_string(&hostname_path) {
        let hostname = contents.trim();

        if !hostname.is_empty() {
            return Ok(hostname.to_string());
        }
    }

    let output = Command::new("hostname")
        .output()
        .map_err(|error| format!("failed to execute hostname: {error}"))?;

    if !output.status.success() {
        return Err(format!("hostname failed with status {}", output.status));
    }

    let hostname = String::from_utf8(output.stdout)
        .map_err(|error| format!("hostname returned invalid UTF-8: {error}"))?
        .trim()
        .to_string();

    if hostname.is_empty() {
        return Err("detected hostname is empty".to_string());
    }

    Ok(hostname)
}

fn read_existing_machine_config(path: &Path) -> Result<MachineConfig, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;

    let system = parse_string_assignment(&contents, "system")
        .ok_or_else(|| format!("missing system in {}", path.display()))?;

    let username = parse_block_string_assignment(&contents, "neodots", "username")
        .ok_or_else(|| format!("missing neodots.username in {}", path.display()))?;

    let hostname = parse_block_string_assignment(&contents, "neodots", "hostname")
        .ok_or_else(|| format!("missing neodots.hostname in {}", path.display()))?;

    let home_directory = parse_block_string_assignment(&contents, "neodots", "homeDirectory")
        .ok_or_else(|| format!("missing neodots.homeDirectory in {}", path.display()))?;

    let personal_enable =
        parse_nested_bool_assignment(&contents, "neodots", "personal", "enable").unwrap_or(false);

    let persistence_enable =
        parse_nested_bool_assignment(&contents, "neodots", "persistence", "enable")
            .unwrap_or(false);

    let persistence_path =
        parse_nested_string_assignment(&contents, "neodots", "persistence", "path")
            .unwrap_or_else(|| "/persist".to_string());

    Ok(MachineConfig {
        system,
        username,
        hostname,
        home_directory,
        personal_enable,
        persistence_enable,
        persistence_path,
    })
}

fn parse_string_assignment(contents: &str, key: &str) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim();

        if !line.starts_with(key) {
            continue;
        }

        let (lhs, rhs) = line.split_once('=')?;

        if lhs.trim() != key {
            continue;
        }

        return Some(unquote_nix_string(rhs.trim().trim_end_matches(';').trim()));
    }

    None
}

fn parse_block_string_assignment(contents: &str, outer_block: &str, key: &str) -> Option<String> {
    let mut in_outer = false;
    let mut depth = 0usize;

    for line in contents.lines() {
        let trimmed = line.trim();

        if !in_outer && trimmed.starts_with(outer_block) && trimmed.contains('{') {
            in_outer = true;
            depth = 1;
            continue;
        }

        if !in_outer {
            continue;
        }

        if let Some(value) = parse_string_assignment(trimmed, key) {
            return Some(value);
        }

        depth = update_brace_depth(depth, trimmed);

        if depth == 0 {
            break;
        }
    }

    None
}

fn parse_nested_bool_assignment(
    contents: &str,
    outer_block: &str,
    nested_block: &str,
    key: &str,
) -> Option<bool> {
    parse_nested_assignment(contents, outer_block, nested_block, key)
        .and_then(|value| value.parse::<bool>().ok())
}

fn parse_nested_string_assignment(
    contents: &str,
    outer_block: &str,
    nested_block: &str,
    key: &str,
) -> Option<String> {
    parse_nested_assignment(contents, outer_block, nested_block, key)
        .map(|value| unquote_nix_string(&value))
}

fn parse_nested_assignment(
    contents: &str,
    outer_block: &str,
    nested_block: &str,
    key: &str,
) -> Option<String> {
    let mut in_outer = false;
    let mut in_nested = false;
    let mut outer_depth = 0usize;
    let mut nested_depth = 0usize;

    for line in contents.lines() {
        let trimmed = line.trim();

        if !in_outer && trimmed.starts_with(outer_block) && trimmed.contains('{') {
            in_outer = true;
            outer_depth = 1;
            continue;
        }

        if !in_outer {
            continue;
        }

        if !in_nested && trimmed.starts_with(nested_block) && trimmed.contains('{') {
            in_nested = true;
            nested_depth = 1;
            continue;
        }

        if in_nested {
            if let Some((lhs, rhs)) = trimmed.split_once('=')
                && lhs.trim() == key
            {
                return Some(rhs.trim().trim_end_matches(';').trim().to_string());
            }

            nested_depth = update_brace_depth(nested_depth, trimmed);

            if nested_depth == 0 {
                in_nested = false;
            }
        }

        outer_depth = update_brace_depth(outer_depth, trimmed);

        if outer_depth == 0 {
            break;
        }
    }

    None
}

fn update_brace_depth(current: usize, line: &str) -> usize {
    let opens = line.matches('{').count();
    let closes = line.matches('}').count();

    current.saturating_add(opens).saturating_sub(closes)
}

fn unquote_nix_string(value: &str) -> String {
    let value = value.trim();

    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        value.to_string()
    }
}

fn print_machine_summary(target: &TargetRoot, machine: &MachineConfig) {
    let path = target.machine_config();

    if path.is_file() {
        println!("  Existing machine configuration: {}", path.display());
    } else {
        println!("  No existing machine configuration: {}", path.display());
    }

    println!();
    println!("Generated machine.nix:");
    println!("----------------------------------------");
    print!("{}", render_machine_config(machine));
    println!("----------------------------------------");
}

fn handle_machine_config(
    target: &TargetRoot,
    machine: &MachineConfig,
    hardware: HardwarePreparation,
    privilege_helper: &Path,
) -> Result<(), String> {
    validate_machine_config(machine)?;

    let generated = render_machine_config(machine);

    if hardware == HardwarePreparation::Unavailable {
        return Err(
            "hardware-configuration.nix is missing and generation was declined".to_string(),
        );
    }

    if !prompt_yes_no(
        "Validate staged configuration and transactionally install machine.nix? [y/N] ",
    )? {
        println!("  Installation skipped.");
        return Ok(());
    }

    validate_and_install_machine_config(target, &generated, hardware, privilege_helper)
}

fn render_machine_config(machine: &MachineConfig) -> String {
    format!(
        r#"# SPDX-License-Identifier: GPL-3.0-only
# Copyright (C) 2026 kaayzouee
# Author: https://github.com/kaayzouee

# Generated by neodots-installer.
# Manual edits are valid, but the installer may replace this file.

{{
  system = "{}";

  neodots = {{
    username = "{}";
    hostname = "{}";
    homeDirectory = "{}";

    personal = {{
      enable = {};
    }};

    persistence = {{
      enable = {};
      path = "{}";
    }};
  }};
}}
"#,
        nix_string(&machine.system),
        nix_string(&machine.username),
        nix_string(&machine.hostname),
        nix_string(&machine.home_directory),
        machine.personal_enable,
        machine.persistence_enable,
        nix_string(&machine.persistence_path),
    )
}

fn nix_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn validate_machine_config(machine: &MachineConfig) -> Result<(), String> {
    validate_username(&machine.username)?;
    validate_hostname(&machine.hostname)?;
    validate_absolute_path(&machine.home_directory, "homeDirectory", false)?;
    validate_absolute_path(&machine.persistence_path, "persistence.path", true)?;

    if machine.system.trim().is_empty() {
        return Err("system must not be empty".to_string());
    }

    if !is_nix_safe_string(&machine.system) {
        return Err("system contains characters unsafe for a Nix string".to_string());
    }

    Ok(())
}

fn validate_username(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("username must not be empty".to_string());
    }

    if value == "guest" {
        return Err("username guest is reserved".to_string());
    }

    let bytes = value.as_bytes();

    let first_valid = bytes[0].is_ascii_lowercase() || bytes[0] == b'_';

    if !first_valid
        || !bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_' || *byte == b'-'
        })
    {
        return Err(format!("invalid username: {value}"));
    }

    Ok(())
}

fn validate_hostname(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("hostname must not be empty".to_string());
    }

    let bytes = value.as_bytes();

    if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
        return Err(format!("invalid hostname: {value}"));
    }

    if !bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'.' || *byte == b'-')
    {
        return Err(format!("invalid hostname: {value}"));
    }

    Ok(())
}

fn validate_absolute_path(value: &str, field: &str, reject_root: bool) -> Result<(), String> {
    let path = Path::new(value);

    if !path.is_absolute() {
        return Err(format!("{field} must be an absolute path: {value}"));
    }

    if reject_root && path == Path::new("/") {
        return Err(format!("{field} must not be /"));
    }

    if !is_nix_safe_string(value) {
        return Err(format!(
            "{field} contains characters unsafe for a Nix string: {value}"
        ));
    }

    Ok(())
}

fn is_nix_safe_string(value: &str) -> bool {
    !value.chars().any(|character| {
        character == '\0'
            || character == '"'
            || character == '\\'
            || character == '\n'
            || character == '\r'
    })
}

fn validate_staged_configuration(staged_nixos_dir: &Path) -> Result<(), String> {
    let nix_path =
        find_in_path(NIX_COMMAND).ok_or_else(|| "nix was not found in PATH".to_string())?;

    println!(
        "    Validating staged configuration with nix flake check at {}...",
        staged_nixos_dir.display()
    );

    let status = Command::new(&nix_path)
        .arg("flake")
        .arg("check")
        .arg("--no-build")
        .arg("--no-write-lock-file")
        .arg(staged_nixos_dir)
        .status()
        .map_err(|error| format!("failed to run nix flake check: {error}"))?;

    if !status.success() {
        return Err(format!(
            "staged nix flake check failed with status {}",
            status
        ));
    }

    Ok(())
}

fn validate_and_install_machine_config(
    target: &TargetRoot,
    machine_contents: &str,
    hardware: HardwarePreparation,
    privilege_helper: &Path,
) -> Result<(), String> {
    let validation_dir = create_temp_directory("neodots-installer-validation")?;

    let result = (|| {
        if let Some(legacy_transaction) = find_legacy_transaction(target)? {
            return Err(format!(
                "legacy installer transaction found under /etc/nixos: {}; recover or remove it before installing",
                legacy_transaction.display()
            ));
        }

        println!(
            "    Staging target configuration from {}...",
            target.nixos_config_dir().display()
        );

        let staged_nixos_dir = validation_dir.join("etc").join("nixos");

        fs::create_dir_all(&staged_nixos_dir).map_err(|error| {
            format!(
                "failed to create staged NixOS directory {}: {error}",
                staged_nixos_dir.display()
            )
        })?;

        copy_recursively(&target.nixos_config_dir(), &staged_nixos_dir)?;

        let generated_hardware_root = if hardware == HardwarePreparation::GenerateInStaging {
            let generated_root = create_temp_directory("neodots-installer-hardware")?;

            println!(
                "    Generating hardware configuration in {}...",
                generated_root.display()
            );

            generate_hardware_configuration_in_staging(privilege_helper, &generated_root)?;

            let generated_hardware = generated_root
                .join("etc")
                .join("nixos")
                .join(HARDWARE_CONFIG_FILE);

            if !generated_hardware.is_file() {
                return Err(format!(
                    "nixos-generate-config completed without producing {}",
                    generated_hardware.display()
                ));
            }

            fs::copy(
                &generated_hardware,
                staged_nixos_dir.join(HARDWARE_CONFIG_FILE),
            )
            .map_err(|error| {
                format!("failed to copy generated hardware configuration into staging: {error}")
            })?;

            Some(generated_root)
        } else {
            None
        };

        let staged_machine_path = staged_nixos_dir
            .join("hosts")
            .join("nixos")
            .join("machine.nix");

        if let Some(parent) = staged_machine_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create staged machine configuration directory {}: {error}",
                    parent.display()
                )
            })?;
        }

        fs::write(&staged_machine_path, machine_contents).map_err(|error| {
            format!(
                "failed to write staged machine.nix {}: {error}",
                staged_machine_path.display()
            )
        })?;

        validate_staged_configuration(&staged_nixos_dir)?;

        println!("    ✓ staged configuration passed validation");

        let generated_hardware = generated_hardware_root
            .as_ref()
            .map(|root| root.join("etc").join("nixos").join(HARDWARE_CONFIG_FILE));

        let machine_temp = create_temp_file("neodots-installer-machine", machine_contents)?;

        let install_result = install_configuration_transactionally(
            target,
            privilege_helper,
            &machine_temp,
            generated_hardware.as_deref(),
        );

        fs::remove_file(&machine_temp).ok();

        if let Some(root) = generated_hardware_root {
            fs::remove_dir_all(root).ok();
        }

        install_result
    })();

    if let Err(error) = &result {
        eprintln!("    ✗ {error}");
    }

    fs::remove_dir_all(validation_dir).ok();

    result
}

fn install_configuration_transactionally(
    target: &TargetRoot,
    privilege_helper: &Path,
    machine_source: &Path,
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

        // STATE_PREPARED is written before any mutation of /etc/nixos.
        // Therefore a missing state file means the transaction never reached
        // the live-configuration mutation phase.
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

        if generated_hardware_source.is_some() {
            write_transaction_state(
                privilege_helper,
                &transaction_dir,
                STATE_INSTALLING_HARDWARE,
            )?;

            move_path_without_overwrite(privilege_helper, &hardware_stage, &hardware_destination)?;

            write_transaction_state(privilege_helper, &transaction_dir, STATE_HARDWARE_INSTALLED)?;
        }

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

fn recover_pending_transactions(
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

fn recover_transaction(
    target: &TargetRoot,
    privilege_helper: &Path,
    transaction_dir: &Path,
) -> Result<(), String> {
    let state = read_transaction_state(transaction_dir)?;

    let Some(state) = state else {
        // STATE_PREPARED is committed before any mutation of /etc/nixos.
        // Therefore a transaction with no state file is an incomplete setup
        // transaction and can be discarded without touching the live config.
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

    if !hardware_expected.is_file() {
        return Ok(());
    }

    if !hardware_destination.exists() {
        return Ok(());
    }

    if !files_equal(&hardware_destination, &hardware_expected)? {
        return Err(format!(
            "cannot safely roll back hardware configuration because {} differs from the transaction's generated configuration",
            hardware_destination.display()
        ));
    }

    remove_path_privileged(privilege_helper, &hardware_destination)
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
        replace_path_atomically(privilege_helper, &state_stage, &state_path)?;

        Ok(())
    })();

    fs::remove_file(&state_temp).ok();

    result
}

fn read_transaction_state(transaction_dir: &Path) -> Result<Option<String>, String> {
    let state_path = transaction_dir.join(TRANSACTION_STATE_FILE);

    let state = match fs::read_to_string(&state_path) {
        Ok(state) => state,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
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

    remove_path_privileged(privilege_helper, transaction_dir)
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
    )
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

fn find_legacy_transaction(target: &TargetRoot) -> Result<Option<PathBuf>, String> {
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

fn generate_hardware_configuration_in_staging(
    privilege_helper: &Path,
    generated_root: &Path,
) -> Result<(), String> {
    let nixos_dir = generated_root.join("etc").join("nixos");

    fs::create_dir_all(&nixos_dir).map_err(|error| {
        format!(
            "failed to create temporary NixOS directory {}: {error}",
            nixos_dir.display()
        )
    })?;

    let generated_root_string = path_to_string(generated_root)?;

    run_privileged_command(
        privilege_helper,
        GENERATE_CONFIG_COMMAND,
        &["--root", generated_root_string.as_str(), "--no-filesystems"],
    )
    .map_err(|error| format!("nixos-generate-config failed in disposable staging: {error}"))?;

    Ok(())
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

fn run_privileged_command(
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

fn prompt_yes_no(prompt: &str) -> Result<bool, String> {
    print!("{prompt}");

    io::stdout()
        .flush()
        .map_err(|error| format!("failed to flush stdout: {error}"))?;

    let mut input = String::new();

    io::stdin()
        .read_line(&mut input)
        .map_err(|error| format!("failed to read input: {error}"))?;

    let response = input.trim().to_ascii_lowercase();

    Ok(matches!(response.as_str(), "y" | "yes"))
}

fn find_privilege_helper() -> Option<PathBuf> {
    find_in_path("sudo").or_else(|| find_in_path("doas"))
}

fn find_in_path(command: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;

    for directory in env::split_paths(&path) {
        let candidate = directory.join(command);

        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

fn create_temp_directory(prefix: &str) -> Result<PathBuf, String> {
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

fn create_temp_file(prefix: &str, contents: &str) -> Result<PathBuf, String> {
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

fn timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn path_to_string(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn copy_recursively(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("failed to inspect {}: {error}", source.display()))?;

    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source)
            .map_err(|error| format!("failed to read symlink {}: {error}", source.display()))?;

        if destination.exists() {
            remove_existing_path(destination)?;
        }

        unix_fs::symlink(&target, destination).map_err(|error| {
            format!(
                "failed to create symlink {} -> {}: {error}",
                destination.display(),
                target.display()
            )
        })?;

        return Ok(());
    }

    if metadata.is_dir() {
        fs::create_dir_all(destination).map_err(|error| {
            format!(
                "failed to create directory {}: {error}",
                destination.display()
            )
        })?;

        for entry in fs::read_dir(source)
            .map_err(|error| format!("failed to read directory {}: {error}", source.display()))?
        {
            let entry = entry.map_err(|error| {
                format!(
                    "failed to read directory entry in {}: {error}",
                    source.display()
                )
            })?;

            let child_source = entry.path();
            let child_destination = destination.join(entry.file_name());

            copy_recursively(&child_source, &child_destination)?;
        }

        return Ok(());
    }

    if metadata.is_file() {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create parent directory {}: {error}",
                    parent.display()
                )
            })?;
        }

        fs::copy(source, destination).map_err(|error| {
            format!(
                "failed to copy {} to {}: {error}",
                source.display(),
                destination.display()
            )
        })?;

        return Ok(());
    }

    Err(format!(
        "unsupported filesystem object in target configuration: {}",
        source.display()
    ))
}

fn remove_existing_path(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;

    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(path)
            .map_err(|error| format!("failed to remove {}: {error}", path.display()))?;
    } else if metadata.is_dir() {
        fs::remove_dir_all(path)
            .map_err(|error| format!("failed to remove {}: {error}", path.display()))?;
    }

    Ok(())
}

fn remove_path_privileged(privilege_helper: &Path, path: &Path) -> Result<(), String> {
    let path_string = path_to_string(path)?;

    run_privileged_command(privilege_helper, "rm", &["-rf", "--", path_string.as_str()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    fn test_temp_dir(prefix: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
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
        let path = env::temp_dir().join(format!(
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
    fn target_root_maps_paths_correctly() {
        let live = TargetRoot::from_path(PathBuf::from("/")).unwrap();

        assert_eq!(live.os_release_path(), PathBuf::from("/etc/os-release"));
        assert_eq!(live.passwd_path(), PathBuf::from("/etc/passwd"));
        assert_eq!(live.nixos_config_dir(), PathBuf::from("/etc/nixos"));
        assert_eq!(
            live.machine_config(),
            PathBuf::from("/etc/nixos/hosts/nixos/machine.nix")
        );

        let mounted = TargetRoot::from_path(PathBuf::from("/mnt")).unwrap_or_else(|_| TargetRoot {
            root: PathBuf::from("/mnt"),
        });

        assert_eq!(
            mounted.os_release_path(),
            PathBuf::from("/mnt/etc/os-release")
        );
        assert_eq!(mounted.passwd_path(), PathBuf::from("/mnt/etc/passwd"));
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

    #[test]
    fn transaction_root_is_outside_nixos_configuration() {
        let root = test_temp_dir("neodots-installer-transaction-root");
        let target = TargetRoot::from_path(root.clone()).unwrap();

        let transaction_root = target.transaction_root();
        let nixos_root = target.nixos_config_dir();

        assert_eq!(
            transaction_root,
            root.join("etc/neodots-installer/transactions")
        );
        assert!(!transaction_root.starts_with(&nixos_root));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn render_machine_contract_is_complete() {
        let machine = MachineConfig {
            system: "x86_64-linux".to_string(),
            username: "kay".to_string(),
            hostname: "nixos".to_string(),
            home_directory: "/home/kay".to_string(),
            personal_enable: true,
            persistence_enable: false,
            persistence_path: "/persist".to_string(),
        };

        let rendered = render_machine_config(&machine);

        assert!(rendered.contains(r#"system = "x86_64-linux";"#));
        assert!(rendered.contains(r#"username = "kay";"#));
        assert!(rendered.contains(r#"hostname = "nixos";"#));
        assert!(rendered.contains(r#"homeDirectory = "/home/kay";"#));
        assert!(rendered.contains("personal = {"));
        assert!(rendered.contains("enable = true;"));
        assert!(rendered.contains("persistence = {"));
        assert!(rendered.contains(r#"path = "/persist";"#));
    }

    #[test]
    fn existing_machine_config_preserves_persistence() {
        let contents = r#"
{
  system = "x86_64-linux";

  neodots = {
    username = "kay";
    hostname = "nixos";
    homeDirectory = "/home/kay";

    personal = {
      enable = true;
    };

    persistence = {
      enable = true;
      path = "/persist";
    };
  };
}
"#;

        let path = env::temp_dir().join(format!("neodots-installer-test-{}", timestamp_nanos()));

        fs::write(&path, contents).unwrap();

        let config = read_existing_machine_config(&path).unwrap();

        assert!(config.personal_enable);
        assert!(config.persistence_enable);
        assert_eq!(config.persistence_path, "/persist");

        fs::remove_file(path).ok();
    }

    #[test]
    fn invalid_username_is_rejected() {
        assert!(validate_username("guest").is_err());
        assert!(validate_username("Root").is_err());
        assert!(validate_username("kay_user").is_ok());
        assert!(validate_username("kay-123").is_ok());
    }

    #[test]
    fn invalid_hostname_is_rejected() {
        assert!(validate_hostname("-nixos").is_err());
        assert!(validate_hostname("nixos-").is_err());
        assert!(validate_hostname("nixos").is_ok());
        assert!(validate_hostname("nixos.home").is_ok());
    }

    #[test]
    fn persistence_path_must_be_absolute_and_non_root() {
        assert!(validate_absolute_path("/persist", "persistence.path", true).is_ok());
        assert!(validate_absolute_path("/data/persist", "persistence.path", true).is_ok());
        assert!(validate_absolute_path("persist", "persistence.path", true).is_err());
        assert!(validate_absolute_path("/", "persistence.path", true).is_err());
    }

    #[test]
    fn nix_string_escaping_works() {
        let value = "path\\with\"quotes\n";
        let escaped = nix_string(value);

        assert_eq!(escaped, "path\\\\with\\\"quotes\\n");
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
    fn installer_lock_is_exclusive() {
        let root = test_temp_dir("neodots-installer-lock");
        let helper = fake_privilege_helper();
        let target_root = root.join("target");

        fs::create_dir_all(&target_root).unwrap();

        let target = TargetRoot::from_path(target_root).unwrap();

        let first = acquire_installer_lock(&target, &helper).unwrap();

        assert!(acquire_installer_lock(&target, &helper).is_err());

        drop(first);

        let second = acquire_installer_lock(&target, &helper).unwrap();

        drop(second);
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
        let machine_path = nixos_dir.join("hosts/nixos/machine.nix");
        let hardware_path = nixos_dir.join(HARDWARE_CONFIG_FILE);
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

        assert_eq!(
            fs::read_to_string(&machine_path).unwrap(),
            "existing machine\n"
        );
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

        assert_eq!(fs::read_to_string(&machine_path).unwrap(), "new machine\n");
        assert!(!transaction_dir.exists());

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
