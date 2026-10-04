// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 kaayzouee
// Author: https://github.com/kaayzouee

use std::{
    env, fs,
    io::{self, Write},
    os::unix::fs as unix_fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::{SystemTime, UNIX_EPOCH},
};

const HARDWARE_CONFIG_FILE: &str = "hardware-configuration.nix";
const GENERATE_CONFIG_COMMAND: &str = "nixos-generate-config";
const NIX_COMMAND: &str = "nix";

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

    if let Err(error) = check_privilege_helper() {
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

    match handle_machine_config(&target, &machine, hardware) {
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

fn check_privilege_helper() -> Result<(), String> {
    match find_privilege_helper() {
        Some(path) => {
            println!("[privilege]");
            println!("  ✓ found: {}", path.display());
            Ok(())
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
) -> Result<(), String> {
    validate_machine_config(machine)?;

    let generated = render_machine_config(machine);

    if hardware == HardwarePreparation::Unavailable {
        return Err(
            "hardware-configuration.nix is missing and generation was declined".to_string(),
        );
    }

    if !prompt_yes_no("Validate staged configuration and replace machine.nix? [y/N] ")? {
        println!("  Installation skipped.");
        return Ok(());
    }

    validate_and_install_machine_config(target, &generated, hardware)
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

fn validate_staged_configuration(validation_dir: &Path) -> Result<(), String> {
    let nix_path =
        find_in_path(NIX_COMMAND).ok_or_else(|| "nix was not found in PATH".to_string())?;

    println!("    Validating staged configuration with nix flake check...");

    let status = Command::new(&nix_path)
        .arg("flake")
        .arg("check")
        .arg("--no-build")
        .arg("--no-write-lock-file")
        .arg(validation_dir)
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
) -> Result<(), String> {
    let privilege_helper = find_privilege_helper()
        .ok_or_else(|| "neither sudo nor doas was found in PATH".to_string())?;

    let validation_dir = create_temp_directory("neodots-installer-validation")?;

    let result = (|| {
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

        let mut generated_hardware_root = None;

        if hardware == HardwarePreparation::GenerateInStaging {
            let generated_root = create_temp_directory("neodots-installer-hardware")?;

            println!(
                "    Generating hardware configuration in {}...",
                generated_root.display()
            );

            generate_hardware_configuration_in_staging(&privilege_helper, &generated_root)?;

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

            generated_hardware_root = Some(generated_root);
        }

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

        validate_staged_configuration(&validation_dir)?;

        println!("    ✓ staged configuration passed validation");

        if hardware == HardwarePreparation::GenerateInStaging && !target.hardware_config().exists()
        {
            let generated_hardware = generated_hardware_root
                .as_ref()
                .expect("generated hardware root should exist")
                .join("etc")
                .join("nixos")
                .join(HARDWARE_CONFIG_FILE);

            install_file_privileged(
                &privilege_helper,
                &generated_hardware,
                &target.hardware_config(),
            )?;

            println!("    ✓ generated hardware configuration installed");
        }

        let machine_temp = create_temp_file("neodots-installer-machine", machine_contents)?;

        install_file_privileged(&privilege_helper, &machine_temp, &target.machine_config())?;

        fs::remove_file(&machine_temp).ok();

        if let Some(root) = generated_hardware_root {
            fs::remove_dir_all(root).ok();
        }

        println!(
            "    ✓ machine.nix installed at {}",
            target.machine_config().display()
        );

        Ok(())
    })();

    fs::remove_dir_all(validation_dir).ok();

    result
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
