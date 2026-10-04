// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 kaayzouee
// Author: https://github.com/kaayzouee

use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Component, Path, PathBuf},
    process::Command,
};

use crate::{
    config::MachineConfig,
    privilege::{create_temp_file, find_in_path},
    target::TargetRoot,
};

pub const WALLPAPER_REPOSITORY_OWNER: &str = "kaayzouee";
pub const WALLPAPER_REPOSITORY_NAME: &str = "neodots-wallpaper";
pub const WALLPAPER_PINNED_REVISION: &str = "c4be01669d109fc6f531d45f6b723f69f0ae542a";
pub const WALLPAPER_MANIFEST_PATH: &str = "manifest.tsv";

const MANIFEST_SCHEMA: u32 = 1;
const RANDOM_TOKEN_BYTES: usize = 16;
const TEMP_CREATION_ATTEMPTS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WallpaperSelection {
    None,
    Specific(String),
    Random,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WallpaperAsset {
    pub id: String,
    pub filename: String,
    pub sha256: String,
    pub size: u64,
    pub format: String,
}

#[derive(Debug, Clone)]
pub struct WallpaperManifest {
    pub schema: u32,
    pub repository: String,
    pub source_revision: String,
    pub wallpapers: Vec<WallpaperAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WallpaperInstallReceipt {
    pub destination: PathBuf,
    pub created: bool,
}

pub fn validate_wallpaper_id(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("wallpaper ID must not be empty".to_string());
    }

    if !value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || ".-_".contains(character))
    {
        return Err(format!("invalid wallpaper ID: {value}"));
    }

    Ok(())
}

fn validate_wallpaper_filename(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("wallpaper filename must not be empty".to_string());
    }

    let path = Path::new(value);

    if path.is_absolute() {
        return Err(format!("wallpaper filename must be relative: {value}"));
    }

    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::CurDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(format!(
            "wallpaper filename contains unsafe path components: {value}"
        ));
    }

    if path.file_name().and_then(|name| name.to_str()) != Some(value) {
        return Err(format!(
            "wallpaper filename must refer to a single file at the repository root: {value}"
        ));
    }

    if value.contains('\t') || value.contains('\n') || value.contains('\r') {
        return Err("wallpaper filename contains control whitespace".to_string());
    }

    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), String> {
    if value.len() != 64 || !value.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err(format!("invalid SHA-256 value: {value}"));
    }

    Ok(())
}

fn validate_format(value: &str) -> Result<(), String> {
    match value {
        "jpg" | "jpeg" | "png" | "gif" | "webp" => Ok(()),
        other => Err(format!("unsupported wallpaper format: {other}")),
    }
}

fn validate_asset(asset: &WallpaperAsset) -> Result<(), String> {
    validate_wallpaper_id(&asset.id)?;
    validate_wallpaper_filename(&asset.filename)?;
    validate_sha256(&asset.sha256)?;
    validate_format(&asset.format)?;

    let extension = Path::new(&asset.filename)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .ok_or_else(|| {
            format!(
                "wallpaper filename has no usable extension: {}",
                asset.filename
            )
        })?;

    if extension != asset.format {
        return Err(format!(
            "wallpaper format mismatch for {}: filename extension is {}, manifest says {}",
            asset.filename, extension, asset.format
        ));
    }

    if asset.size == 0 {
        return Err(format!("wallpaper {} has an invalid zero size", asset.id));
    }

    Ok(())
}

pub fn manifest_url() -> String {
    format!(
        "https://raw.githubusercontent.com/{}/{}/{}/{}",
        WALLPAPER_REPOSITORY_OWNER,
        WALLPAPER_REPOSITORY_NAME,
        WALLPAPER_PINNED_REVISION,
        WALLPAPER_MANIFEST_PATH
    )
}

fn asset_url(asset: &WallpaperAsset) -> String {
    format!(
        "https://raw.githubusercontent.com/{}/{}/{}/{}",
        WALLPAPER_REPOSITORY_OWNER,
        WALLPAPER_REPOSITORY_NAME,
        WALLPAPER_PINNED_REVISION,
        asset.filename
    )
}

pub fn parse_manifest(contents: &str) -> Result<WallpaperManifest, String> {
    let mut schema = None;
    let mut repository = None;
    let mut source_revision = None;
    let mut wallpapers = Vec::new();

    let mut saw_wallpaper_header = false;
    let mut seen_ids = HashSet::new();
    let mut seen_filenames = HashSet::new();

    for raw_line in contents.lines() {
        let line = raw_line.trim_end_matches('\r');

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let fields: Vec<&str> = line.split('\t').collect();

        match fields.as_slice() {
            ["schema", value] => {
                if schema.is_some() {
                    return Err("manifest contains duplicate schema declarations".to_string());
                }

                let parsed = value
                    .parse::<u32>()
                    .map_err(|error| format!("invalid manifest schema: {error}"))?;

                schema = Some(parsed);
            }

            ["repository", value] => {
                if repository.is_some() {
                    return Err("manifest contains duplicate repository declarations".to_string());
                }

                repository = Some((*value).to_string());
            }

            ["source_revision", value] => {
                if source_revision.is_some() {
                    return Err(
                        "manifest contains duplicate source_revision declarations".to_string()
                    );
                }

                if value.len() != 40
                    || !value.chars().all(|character| character.is_ascii_hexdigit())
                {
                    return Err(format!(
                        "manifest source_revision is not a full commit SHA: {value}"
                    ));
                }

                source_revision = Some((*value).to_ascii_lowercase());
            }

            ["wallpaper", "id", "filename", "sha256", "size", "format"] => {
                if saw_wallpaper_header {
                    return Err("manifest contains duplicate wallpaper headers".to_string());
                }

                saw_wallpaper_header = true;
            }

            ["wallpaper", id, filename, sha256, size, format] => {
                if !saw_wallpaper_header {
                    return Err("wallpaper entries appeared before the manifest header".to_string());
                }

                let size = size
                    .parse::<u64>()
                    .map_err(|error| format!("invalid wallpaper size {size}: {error}"))?;

                let asset = WallpaperAsset {
                    id: (*id).to_string(),
                    filename: (*filename).to_string(),
                    sha256: sha256.to_ascii_lowercase(),
                    size,
                    format: (*format).to_string(),
                };

                validate_asset(&asset)?;

                if !seen_ids.insert(asset.id.clone()) {
                    return Err(format!("duplicate wallpaper ID: {}", asset.id));
                }

                if !seen_filenames.insert(asset.filename.clone()) {
                    return Err(format!("duplicate wallpaper filename: {}", asset.filename));
                }

                wallpapers.push(asset);
            }

            _ => {
                return Err(format!("invalid manifest line: {line}"));
            }
        }
    }

    let schema = schema.ok_or_else(|| "manifest is missing schema".to_string())?;

    if schema != MANIFEST_SCHEMA {
        return Err(format!("unsupported wallpaper manifest schema: {schema}"));
    }

    let repository = repository.ok_or_else(|| "manifest is missing repository".to_string())?;

    let expected_repository = format!(
        "{}/{}",
        WALLPAPER_REPOSITORY_OWNER, WALLPAPER_REPOSITORY_NAME
    );

    if repository != expected_repository {
        return Err(format!(
            "unexpected wallpaper repository in manifest: expected {}, found {}",
            expected_repository, repository
        ));
    }

    let source_revision =
        source_revision.ok_or_else(|| "manifest is missing source_revision".to_string())?;

    if source_revision != WALLPAPER_PINNED_REVISION {
        return Err(format!(
            "manifest source_revision does not match pinned revision: expected {}, found {}",
            WALLPAPER_PINNED_REVISION, source_revision
        ));
    }

    if !saw_wallpaper_header {
        return Err("manifest is missing wallpaper header".to_string());
    }

    if wallpapers.is_empty() {
        return Err("wallpaper manifest contains no wallpaper assets".to_string());
    }

    Ok(WallpaperManifest {
        schema,
        repository,
        source_revision,
        wallpapers,
    })
}

fn curl_path() -> Result<PathBuf, String> {
    find_in_path("curl").ok_or_else(|| "curl was not found in PATH".to_string())
}

fn sha256sum_path() -> Result<PathBuf, String> {
    find_in_path("sha256sum").ok_or_else(|| "sha256sum was not found in PATH".to_string())
}

fn download_url(url: &str, destination: &Path) -> Result<(), String> {
    let curl = curl_path()?;

    let status = Command::new(&curl)
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--connect-timeout",
            "15",
            "--max-time",
            "120",
            "--retry",
            "2",
            "--output",
        ])
        .arg(destination)
        .arg(url)
        .status()
        .map_err(|error| format!("failed to execute {}: {error}", curl.display()))?;

    if !status.success() {
        return Err(format!(
            "curl failed to download {} with status {}",
            url, status
        ));
    }

    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    if !path.is_absolute() {
        return Err(format!(
            "sha256sum requires an absolute path: {}",
            path.display()
        ));
    }

    let sha256sum = sha256sum_path()?;

    let output = Command::new(&sha256sum)
        .arg0("sha256sum")
        .arg(path)
        .output()
        .map_err(|error| format!("failed to execute {}: {error}", sha256sum.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

        if stderr.is_empty() {
            return Err(format!(
                "sha256sum failed for {} with status {}",
                path.display(),
                output.status
            ));
        }

        return Err(format!(
            "sha256sum failed for {} with status {}: {}",
            path.display(),
            output.status,
            stderr
        ));
    }

    let output = String::from_utf8(output.stdout)
        .map_err(|error| format!("sha256sum returned invalid UTF-8: {error}"))?;

    let digest = output
        .split_whitespace()
        .next()
        .ok_or_else(|| format!("sha256sum returned no digest for {}", path.display()))?;

    validate_sha256(digest)?;

    Ok(digest.to_ascii_lowercase())
}

fn load_manifest() -> Result<WallpaperManifest, String> {
    let temporary = create_temp_file("neodots-installer-wallpaper-manifest", "")?;

    let result = (|| {
        download_url(&manifest_url(), &temporary)?;

        let contents = fs::read_to_string(&temporary).map_err(|error| {
            format!(
                "failed to read downloaded wallpaper manifest {}: {error}",
                temporary.display()
            )
        })?;

        let manifest = parse_manifest(&contents)?;

        if manifest.schema != MANIFEST_SCHEMA {
            return Err(format!(
                "unsupported wallpaper manifest schema: {}",
                manifest.schema
            ));
        }

        let expected_repository = format!(
            "{}/{}",
            WALLPAPER_REPOSITORY_OWNER, WALLPAPER_REPOSITORY_NAME
        );

        if manifest.repository != expected_repository {
            return Err(format!(
                "unexpected wallpaper manifest repository: {}",
                manifest.repository
            ));
        }

        if manifest.source_revision != WALLPAPER_PINNED_REVISION {
            return Err(format!(
                "unexpected wallpaper manifest source revision: expected {}, found {}",
                WALLPAPER_PINNED_REVISION, manifest.source_revision
            ));
        }

        println!(
            "    ✓ wallpaper manifest source revision: {}",
            manifest.source_revision
        );

        Ok(manifest)
    })();

    fs::remove_file(&temporary).ok();

    result
}

fn random_wallpaper_index(length: usize) -> Result<usize, String> {
    if length == 0 {
        return Err("cannot select a random wallpaper from an empty manifest".to_string());
    }

    let mut random = File::open("/dev/urandom")
        .map_err(|error| format!("failed to open /dev/urandom: {error}"))?;

    loop {
        let mut bytes = [0u8; 8];

        random
            .read_exact(&mut bytes)
            .map_err(|error| format!("failed to read secure random bytes: {error}"))?;

        let value = u64::from_ne_bytes(bytes);
        let count = length as u64;
        let limit = u64::MAX - (u64::MAX % count);

        if value < limit {
            return Ok((value % count) as usize);
        }
    }
}

pub fn resolve_wallpaper_selection(
    selection: &WallpaperSelection,
) -> Result<Option<WallpaperAsset>, String> {
    match selection {
        WallpaperSelection::None => Ok(None),

        WallpaperSelection::Specific(id) => {
            validate_wallpaper_id(id)?;

            let manifest = load_manifest()?;

            manifest
                .wallpapers
                .into_iter()
                .find(|asset| asset.id == *id)
                .map(Some)
                .ok_or_else(|| {
                    format!("wallpaper ID {id} was not found in the pinned wallpaper manifest")
                })
        }

        WallpaperSelection::Random => {
            let manifest = load_manifest()?;
            let index = random_wallpaper_index(manifest.wallpapers.len())?;

            Ok(Some(manifest.wallpapers.get(index).cloned().ok_or_else(
                || "random wallpaper selection produced an invalid manifest index".to_string(),
            )?))
        }
    }
}

fn create_destination_temp_file(directory: &Path) -> Result<PathBuf, String> {
    if !directory.is_dir() {
        return Err(format!(
            "wallpaper destination directory does not exist: {}",
            directory.display()
        ));
    }

    for _ in 0..TEMP_CREATION_ATTEMPTS {
        let token = secure_random_token()?;
        let path = directory.join(format!(".neodots-wallpaper-{token}.tmp"));

        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(_) => return Ok(path),

            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                continue;
            }

            Err(error) => {
                return Err(format!(
                    "failed to create wallpaper staging file {}: {error}",
                    path.display()
                ));
            }
        }
    }

    Err("failed to allocate a unique wallpaper staging file after multiple attempts".to_string())
}

fn secure_random_token() -> Result<String, String> {
    let mut bytes = [0u8; RANDOM_TOKEN_BYTES];

    let mut random = File::open("/dev/urandom")
        .map_err(|error| format!("failed to open /dev/urandom: {error}"))?;

    random
        .read_exact(&mut bytes)
        .map_err(|error| format!("failed to read secure random bytes: {error}"))?;

    let mut token = String::with_capacity(RANDOM_TOKEN_BYTES * 2);

    for byte in bytes {
        use std::fmt::Write;

        write!(&mut token, "{byte:02x}")
            .map_err(|_| "failed to format secure random token".to_string())?;
    }

    Ok(token)
}

fn sync_file(path: &Path) -> Result<(), String> {
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

fn sync_directory(path: &Path) -> Result<(), String> {
    let file = File::open(path).map_err(|error| {
        format!(
            "failed to open directory {} for durability sync: {error}",
            path.display()
        )
    })?;

    file.sync_all().map_err(|error| {
        format!(
            "failed to sync directory {} to stable storage: {error}",
            path.display()
        )
    })
}

fn destination_path(
    target: &TargetRoot,
    machine: &MachineConfig,
    asset: &WallpaperAsset,
) -> PathBuf {
    target
        .path(&format!("{}/Pictures", machine.home_directory))
        .join(format!("neodots-{}", asset.filename))
}

fn validate_pictures_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "wallpaper directory is a symlink; refusing to use {}",
                    path.display()
                ));
            }

            if !metadata.is_dir() {
                return Err(format!(
                    "wallpaper destination is not a directory: {}",
                    path.display()
                ));
            }

            Ok(())
        }

        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|create_error| {
                format!(
                    "failed to create wallpaper directory {}: {create_error}",
                    path.display()
                )
            })?;

            Ok(())
        }

        Err(error) => Err(format!(
            "failed to inspect wallpaper directory {}: {error}",
            path.display()
        )),
    }
}

fn verify_download(path: &Path, asset: &WallpaperAsset) -> Result<(), String> {
    let metadata = fs::metadata(path).map_err(|error| {
        format!(
            "failed to inspect downloaded wallpaper {}: {error}",
            path.display()
        )
    })?;

    if !metadata.is_file() {
        return Err(format!(
            "downloaded wallpaper is not a regular file: {}",
            path.display()
        ));
    }

    if metadata.len() != asset.size {
        return Err(format!(
            "wallpaper size mismatch for {}: expected {}, found {}",
            asset.filename,
            asset.size,
            metadata.len()
        ));
    }

    let digest = sha256_file(path)?;

    if digest != asset.sha256 {
        return Err(format!(
            "wallpaper SHA-256 mismatch for {}: expected {}, found {}",
            asset.filename, asset.sha256, digest
        ));
    }

    Ok(())
}

pub fn install_selected_wallpaper(
    target: &TargetRoot,
    machine: &MachineConfig,
    asset: &WallpaperAsset,
) -> Result<WallpaperInstallReceipt, String> {
    validate_asset(asset)?;

    let pictures_dir = target.path(&format!("{}/Pictures", machine.home_directory));
    validate_pictures_directory(&pictures_dir)?;

    let destination = destination_path(target, machine, asset);

    if let Ok(metadata) = fs::symlink_metadata(&destination) {
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "wallpaper destination is a symlink; refusing to overwrite {}",
                destination.display()
            ));
        }

        if !metadata.is_file() {
            return Err(format!(
                "wallpaper destination exists but is not a regular file: {}",
                destination.display()
            ));
        }

        verify_download(&destination, asset)?;

        println!(
            "    ✓ wallpaper already installed and hash-verified: {}",
            destination.display()
        );

        return Ok(WallpaperInstallReceipt {
            destination,
            created: false,
        });
    }

    let download_temp = create_temp_file("neodots-installer-wallpaper", "")?;

    let result = (|| {
        let url = asset_url(asset);

        println!("    Downloading wallpaper {}...", asset.id);
        download_url(&url, &download_temp)?;

        verify_download(&download_temp, asset)?;

        let staged_destination = create_destination_temp_file(&pictures_dir)?;

        let result = (|| {
            fs::copy(&download_temp, &staged_destination).map_err(|error| {
                format!("failed to copy verified wallpaper into destination staging: {error}")
            })?;

            let mut permissions = fs::metadata(&staged_destination)
                .map_err(|error| {
                    format!(
                        "failed to inspect wallpaper staging file {}: {error}",
                        staged_destination.display()
                    )
                })?
                .permissions();

            permissions.set_mode(0o644);

            fs::set_permissions(&staged_destination, permissions).map_err(|error| {
                format!(
                    "failed to set wallpaper permissions on {}: {error}",
                    staged_destination.display()
                )
            })?;

            sync_file(&staged_destination)?;

            if destination.exists() {
                return Err(format!(
                    "wallpaper destination appeared during installation: {}",
                    destination.display()
                ));
            }

            fs::rename(&staged_destination, &destination).map_err(|error| {
                format!(
                    "failed to atomically install wallpaper {}: {error}",
                    destination.display()
                )
            })?;

            sync_file(&destination)?;
            sync_directory(&pictures_dir)?;

            Ok(())
        })();

        if result.is_err() {
            fs::remove_file(&staged_destination).ok();
        }

        result
    })();

    fs::remove_file(&download_temp).ok();

    result?;

    println!(
        "    ✓ installed hash-verified wallpaper: {}",
        destination.display()
    );

    Ok(WallpaperInstallReceipt {
        destination,
        created: true,
    })
}

pub fn rollback_wallpaper_install(receipt: &WallpaperInstallReceipt) -> Result<(), String> {
    if !receipt.created {
        return Ok(());
    }

    match fs::symlink_metadata(&receipt.destination) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "refusing to remove installed wallpaper because {} is now a symlink",
            receipt.destination.display()
        )),

        Ok(metadata) if !metadata.is_file() => Err(format!(
            "refusing to remove installed wallpaper because {} is no longer a regular file",
            receipt.destination.display()
        )),

        Ok(_) => {
            fs::remove_file(&receipt.destination).map_err(|error| {
                format!(
                    "failed to roll back wallpaper {}: {error}",
                    receipt.destination.display()
                )
            })?;

            if let Some(parent) = receipt.destination.parent() {
                sync_directory(parent)?;
            }

            Ok(())
        }

        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),

        Err(error) => Err(format!(
            "failed to inspect wallpaper during rollback {}: {error}",
            receipt.destination.display()
        )),
    }
}

pub fn print_wallpaper_summary(asset: Option<&WallpaperAsset>) {
    match asset {
        Some(asset) => {
            println!();
            println!("[wallpaper]");
            println!("  ✓ id: {}", asset.id);
            println!("  ✓ filename: {}", asset.filename);
            println!("  ✓ size: {} bytes", asset.size);
            println!("  ✓ format: {}", asset.format);
            println!("  ✓ sha256: {}", asset.sha256);
            println!("  ✓ repository revision: {}", WALLPAPER_PINNED_REVISION);
        }

        None => {
            println!();
            println!("[wallpaper]");
            println!("  ✓ selection: none");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_file(contents: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "neodots-wallpaper-test-{}",
            crate::privilege::timestamp_nanos()
        ));

        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn manifest_header_is_parsed() {
        let contents = "schema\t1\n\
repository\tkaayzouee/neodots-wallpaper\n\
source_revision\tc4be01669d109fc6f531d45f6b723f69f0ae542a\n\
wallpaper\tid\tfilename\tsha256\tsize\tformat\n\
wallpaper\tastronaut.png\tastronaut.png\t\
0000000000000000000000000000000000000000000000000000000000000000\
\t4081051\tpng\n";

        let manifest = parse_manifest(contents).unwrap();

        assert_eq!(manifest.schema, 1);
        assert_eq!(manifest.repository, "kaayzouee/neodots-wallpaper");
        assert_eq!(manifest.source_revision, WALLPAPER_PINNED_REVISION);
        assert_eq!(manifest.wallpapers.len(), 1);
        assert_eq!(manifest.wallpapers[0].id, "astronaut.png");
    }

    #[test]
    fn manifest_rejects_unpinned_source_revision() {
        let contents = "schema\t1\n\
repository\tkaayzouee/neodots-wallpaper\n\
source_revision\t7bfdf10d16ad3a689f9f0cf3a0930da3d1a245a8\n\
wallpaper\tid\tfilename\tsha256\tsize\tformat\n\
wallpaper\tastronaut.png\tastronaut.png\t\
0000000000000000000000000000000000000000000000000000000000000000\
\t4081051\tpng\n";

        let error = parse_manifest(contents).unwrap_err();

        assert!(error.contains("does not match pinned revision"));
        assert!(error.contains(WALLPAPER_PINNED_REVISION));
    }

    #[test]
    fn manifest_rejects_path_traversal() {
        let contents = "schema\t1\n\
repository\tkaayzouee/neodots-wallpaper\n\
source_revision\tc4be01669d109fc6f531d45f6b723f69f0ae542a\n\
wallpaper\tid\tfilename\tsha256\tsize\tformat\n\
wallpaper\tbad\t../bad.png\t\
0000000000000000000000000000000000000000000000000000000000000000\
\t1\tpng\n";

        assert!(parse_manifest(contents).is_err());
    }

    #[test]
    fn manifest_rejects_duplicate_ids() {
        let digest = "0000000000000000000000000000000000000000000000000000000000000000";

        let contents = format!(
            "schema\t1\n\
repository\tkaayzouee/neodots-wallpaper\n\
source_revision\t{WALLPAPER_PINNED_REVISION}\n\
wallpaper\tid\tfilename\tsha256\tsize\tformat\n\
wallpaper\tone\tone.png\t{digest}\t1\tpng\n\
wallpaper\tone\ttwo.png\t{digest}\t1\tpng\n"
        );

        assert!(parse_manifest(&contents).is_err());
    }

    #[test]
    fn specific_selection_returns_matching_asset() {
        let contents = format!(
            "schema\t1\n\
repository\tkaayzouee/neodots-wallpaper\n\
source_revision\t{WALLPAPER_PINNED_REVISION}\n\
wallpaper\tid\tfilename\tsha256\tsize\tformat\n\
wallpaper\tone\tone.png\t\
0000000000000000000000000000000000000000000000000000000000000000\
\t1\tpng\n"
        );

        let manifest = parse_manifest(&contents).unwrap();

        let selected = manifest
            .wallpapers
            .iter()
            .find(|asset| asset.id == "one")
            .cloned();

        assert_eq!(selected, Some(manifest.wallpapers[0].clone()));
    }

    #[test]
    fn random_selection_returns_an_entry() {
        let contents = format!(
            "schema\t1\n\
repository\tkaayzouee/neodots-wallpaper\n\
source_revision\t{WALLPAPER_PINNED_REVISION}\n\
wallpaper\tid\tfilename\tsha256\tsize\tformat\n\
wallpaper\tone\tone.png\t\
0000000000000000000000000000000000000000000000000000000000000000\
\t1\tpng\n\
wallpaper\ttwo\ttwo.png\t\
0000000000000000000000000000000000000000000000000000000000000000\
\t1\tpng\n"
        );

        let manifest = parse_manifest(&contents).unwrap();
        let index = random_wallpaper_index(manifest.wallpapers.len()).unwrap();

        assert!(index < manifest.wallpapers.len());
    }

    #[test]
    fn wallpaper_id_validation_is_strict() {
        assert!(validate_wallpaper_id("astronaut.png").is_ok());
        assert!(validate_wallpaper_id("night-forest").is_ok());
        assert!(validate_wallpaper_id("../bad").is_err());
        assert!(validate_wallpaper_id("bad/name").is_err());
    }

    #[test]
    fn wallpaper_filename_validation_rejects_absolute_paths() {
        assert!(validate_wallpaper_filename("astronaut.png").is_ok());
        assert!(validate_wallpaper_filename("/etc/passwd").is_err());
        assert!(validate_wallpaper_filename("../astronaut.png").is_err());
    }

    #[test]
    fn sha256sum_detects_known_contents() {
        let path = temp_file("hello");

        let digest = sha256_file(&path).unwrap();

        assert_eq!(
            digest,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );

        fs::remove_file(path).ok();
    }

    #[test]
    fn manifest_url_is_pinned_to_exact_revision() {
        let url = manifest_url();

        assert!(url.contains(WALLPAPER_PINNED_REVISION));
        assert!(url.contains("/manifest.tsv"));
        assert!(!url.contains("/master/"));
        assert!(!url.contains("/main/"));
    }
}
