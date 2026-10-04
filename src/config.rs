// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 kaayzouee
// Author: https://github.com/kaayzouee

use std::{env, fs, path::Path, process::Command};

use crate::{privilege::find_in_path, target::TargetRoot};

pub const NEODOTS_REPOSITORY_URL: &str = "https://github.com/kaayzouee/neodots.git";
pub const NEODOTS_PINNED_REVISION: &str = "4720666cfa0ae1ea4cd1bad66b74ede1c912ec02";

#[derive(Debug, Clone)]
pub struct MachineConfig {
    pub system: String,
    pub username: String,
    pub hostname: String,
    pub home_directory: String,
    pub personal_enable: bool,
    pub persistence_enable: bool,
    pub persistence_path: String,
}

pub fn verify_neodots_revision(target: &TargetRoot) -> Result<(), String> {
    let git_path = find_in_path("git")
        .ok_or_else(|| "git was not found as an executable in PATH".to_string())?;

    let nixos_config_dir = target.nixos_config_dir();

    if !nixos_config_dir.is_dir() {
        return Err(format!(
            "Neodots configuration directory does not exist: {}",
            nixos_config_dir.display()
        ));
    }

    let expected_root = fs::canonicalize(&nixos_config_dir).map_err(|error| {
        format!(
            "failed to resolve the Neodots configuration directory {}: {error}",
            nixos_config_dir.display()
        )
    })?;

    let repository_root = run_git_output(
        &git_path,
        &nixos_config_dir,
        &["rev-parse", "--show-toplevel"],
    )?;

    let repository_root = Path::new(repository_root.trim());

    if repository_root != expected_root {
        return Err(format!(
            "target NixOS configuration is not the Neodots repository root: expected {}, got {}",
            expected_root.display(),
            repository_root.display()
        ));
    }

    let revision = run_git_output(
        &git_path,
        &nixos_config_dir,
        &["rev-parse", "--verify", "HEAD^{commit}"],
    )?;

    validate_pinned_neodots_revision(revision.trim())?;

    println!("[neodots]");
    println!("  ✓ repository: {}", NEODOTS_REPOSITORY_URL);
    println!("  ✓ pinned revision: {}", revision.trim());

    Ok(())
}

fn run_git_output(
    git_path: &Path,
    working_directory: &Path,
    args: &[&str],
) -> Result<String, String> {
    let output = Command::new(git_path)
        .arg("-C")
        .arg(working_directory)
        .args(args)
        .output()
        .map_err(|error| {
            format!(
                "failed to run {} in {}: {error}",
                git_path.display(),
                working_directory.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

        if stderr.is_empty() {
            return Err(format!(
                "{} in {} exited with status {}",
                git_path.display(),
                working_directory.display(),
                output.status
            ));
        }

        return Err(format!(
            "{} in {} failed with status {}: {}",
            git_path.display(),
            working_directory.display(),
            output.status,
            stderr
        ));
    }

    String::from_utf8(output.stdout).map_err(|error| format!("git returned invalid UTF-8: {error}"))
}

fn validate_pinned_neodots_revision(revision: &str) -> Result<(), String> {
    if revision.len() != 40
        || !revision
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(format!(
            "Neodots HEAD is not a valid commit revision: {revision}"
        ));
    }

    if revision != NEODOTS_PINNED_REVISION {
        return Err(format!(
            "unsupported Neodots revision: expected {}, found {}",
            NEODOTS_PINNED_REVISION, revision
        ));
    }

    Ok(())
}

pub fn detect_machine_config(target: &TargetRoot) -> Result<MachineConfig, String> {
    println!("[machine]");

    if target.machine_config().is_file() {
        let existing = read_existing_machine_config(&target.machine_config())?;
        validate_machine_config(&existing)?;
        validate_username_for_target(target, &existing.username)?;

        println!("  ✓ using existing machine.nix values");
        println!("  ✓ system: {}", existing.system);
        println!("  ✓ username: {}", existing.username);
        println!("  ✓ hostname: {}", existing.hostname);
        println!("  ✓ home directory: {}", existing.home_directory);
        println!(
            "  ✓ personal configuration: {} (preserved)",
            bool_label(existing.personal_enable)
        );
        println!(
            "  ✓ persistence: {} (path {}) (preserved)",
            bool_label(existing.persistence_enable),
            existing.persistence_path
        );

        return Ok(existing);
    }

    let detected_system = detect_system(target)?;
    println!("  ✓ detected system: {detected_system}");

    let username = detect_target_username(target)?;
    validate_username_for_target(target, &username)?;
    println!("  ✓ detected username: {username}");

    let hostname = detect_hostname(target)?;
    validate_hostname(&hostname)?;
    println!("  ✓ detected hostname: {hostname}");

    let home_directory = lookup_home_directory(target, &username)?;
    println!("  ✓ detected home directory: {home_directory}");
    println!("  ✓ personal configuration: disabled (default)");
    println!("  ✓ persistence: disabled (path /persist) (default)");

    Ok(MachineConfig {
        system: detected_system,
        username,
        hostname,
        home_directory,
        personal_enable: false,
        persistence_enable: false,
        persistence_path: "/persist".to_string(),
    })
}

fn bool_label(value: bool) -> &'static str {
    if value { "enabled" } else { "disabled" }
}

fn detect_system(target: &TargetRoot) -> Result<String, String> {
    if let Some(system) = env::var_os("NEODOTS_TARGET_SYSTEM") {
        let system = system
            .into_string()
            .map_err(|_| "NEODOTS_TARGET_SYSTEM is not valid UTF-8".to_string())?;

        validate_system(&system)?;
        return Ok(system);
    }

    if !target.is_live_root() {
        return Err(
            "cannot safely detect the architecture of a mounted target; set NEODOTS_TARGET_SYSTEM explicitly"
                .to_string(),
        );
    }

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

    match architecture.as_str() {
        "x86_64" => Ok("x86_64-linux".to_string()),
        "aarch64" => Ok("aarch64-linux".to_string()),
        "armv7l" => Ok("armv7l-linux".to_string()),
        other => Err(format!(
            "unsupported system architecture reported by uname: {other}"
        )),
    }
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

pub fn home_directory_for_username(username: &str) -> String {
    format!("/home/{username}")
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

    Ok(home_directory_for_username(username))
}

fn detect_hostname(target: &TargetRoot) -> Result<String, String> {
    if let Some(hostname) = env::var_os("NEODOTS_TARGET_HOSTNAME") {
        let hostname = hostname
            .into_string()
            .map_err(|_| "NEODOTS_TARGET_HOSTNAME is not valid UTF-8".to_string())?;

        validate_hostname(&hostname)?;
        return Ok(hostname);
    }

    let hostname_path = target.hostname_path();

    if let Ok(contents) = fs::read_to_string(&hostname_path) {
        let hostname = contents.trim();

        if !hostname.is_empty() {
            return Ok(hostname.to_string());
        }
    }

    if !target.is_live_root() {
        return Err(
            "target /etc/hostname is missing or empty; set NEODOTS_TARGET_HOSTNAME explicitly"
                .to_string(),
        );
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
    current
        .saturating_add(line.matches('{').count())
        .saturating_sub(line.matches('}').count())
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

pub fn render_machine_config(machine: &MachineConfig) -> String {
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

fn validate_system(value: &str) -> Result<(), String> {
    match value {
        "x86_64-linux" | "aarch64-linux" | "armv7l-linux" => Ok(()),
        other => Err(format!(
            "unsupported target system: {other}; expected x86_64-linux, aarch64-linux, or armv7l-linux"
        )),
    }
}

pub fn validate_machine_config(machine: &MachineConfig) -> Result<(), String> {
    validate_username(&machine.username)?;
    validate_hostname(&machine.hostname)?;
    validate_absolute_path(&machine.home_directory, "homeDirectory", false)?;
    validate_absolute_path(&machine.persistence_path, "persistence.path", true)?;

    validate_system(&machine.system)?;

    if !is_nix_safe_string(&machine.system) {
        return Err("system contains characters unsafe for a Nix string".to_string());
    }

    Ok(())
}

pub fn validate_username(value: &str) -> Result<(), String> {
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

pub fn validate_username_for_target(target: &TargetRoot, value: &str) -> Result<(), String> {
    validate_username(value)?;

    if value == "root" {
        return Err("username root is reserved".to_string());
    }

    let passwd = fs::read_to_string(target.passwd_path()).map_err(|error| {
        format!(
            "cannot inspect target system accounts in {}: {error}",
            target.passwd_path().display()
        )
    })?;

    for line in passwd.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }

        let fields: Vec<&str> = line.split(':').collect();

        if fields.len() < 4 || fields[0] != value {
            continue;
        }

        let uid = fields[2].parse::<u32>().map_err(|error| {
            format!(
                "target passwd entry for {value} has an invalid UID {:?}: {error}",
                fields[2]
            )
        })?;

        if uid < 1000 {
            return Err(format!(
                "username {value} is already used by a target system account (UID {uid})"
            ));
        }

        break;
    }

    Ok(())
}

pub fn validate_hostname(value: &str) -> Result<(), String> {
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

pub fn validate_absolute_path(value: &str, field: &str, reject_root: bool) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::*;

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

        let path = env::temp_dir().join(format!(
            "neodots-installer-test-{}",
            crate::privilege::timestamp_nanos()
        ));

        fs::write(&path, contents).unwrap();

        let config = read_existing_machine_config(&path).unwrap();

        assert!(config.personal_enable);
        assert!(config.persistence_enable);
        assert_eq!(config.persistence_path, "/persist");

        fs::remove_file(path).ok();
    }

    #[test]
    fn home_directory_defaults_from_username() {
        assert_eq!(home_directory_for_username("alice"), "/home/alice");
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
    fn mounted_target_does_not_fall_back_to_installer_architecture() {
        let root = std::env::temp_dir().join(format!(
            "neodots-installer-target-system-{}",
            crate::privilege::timestamp_nanos()
        ));

        fs::create_dir_all(root.join("etc/nixos/hosts/nixos")).unwrap();

        let target = TargetRoot::from_path(root.clone()).unwrap();
        let error = detect_system(&target).unwrap_err();

        assert!(error.contains("NEODOTS_TARGET_SYSTEM"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn mounted_target_does_not_fall_back_to_installer_hostname() {
        let root = std::env::temp_dir().join(format!(
            "neodots-installer-target-hostname-{}",
            crate::privilege::timestamp_nanos()
        ));

        fs::create_dir_all(root.join("etc/nixos")).unwrap();

        let target = TargetRoot::from_path(root.clone()).unwrap();
        let error = detect_hostname(&target).unwrap_err();

        assert!(error.contains("NEODOTS_TARGET_HOSTNAME"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn system_account_usernames_are_rejected_for_a_target() {
        let root = std::env::temp_dir().join(format!(
            "neodots-installer-target-passwd-{}",
            crate::privilege::timestamp_nanos()
        ));

        let etc = root.join("etc");
        fs::create_dir_all(&etc).unwrap();

        fs::write(
            etc.join("passwd"),
            "root:x:0:0:root:/root:/bin/bash\ndaemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin\nalice:x:1000:100:Alice:/home/alice:/bin/bash\n",
        )
        .unwrap();

        let target = TargetRoot::from_path(root.clone()).unwrap();

        assert!(validate_username_for_target(&target, "root").is_err());
        assert!(validate_username_for_target(&target, "daemon").is_err());
        assert!(validate_username_for_target(&target, "alice").is_ok());

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn pinned_neodots_revision_is_a_full_commit_sha() {
        assert_eq!(NEODOTS_PINNED_REVISION.len(), 40);
        assert!(
            NEODOTS_PINNED_REVISION
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
    }

    #[test]
    fn unsupported_neodots_revision_is_rejected() {
        assert!(validate_pinned_neodots_revision(&"0".repeat(40)).is_err());
        assert!(validate_pinned_neodots_revision(NEODOTS_PINNED_REVISION).is_ok());
    }

    #[test]
    fn nix_string_escaping_works() {
        let value = "path\\with\"quotes\n";

        assert_eq!(nix_string(value), "path\\\\with\\\"quotes\\n");
    }
}
