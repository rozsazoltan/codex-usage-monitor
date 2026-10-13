//! The release companion is data, not an installer. Only our private cache is written.
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};

const MANIFEST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/manifest.json"));
const PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.bin"));
const MAX_SIZE: usize = 64 * 1024 * 1024;
const REPARSE: u32 = 0x400;
const OPEN_REPARSE: u32 = 0x00200000;
const BACKUP_SEMANTICS: u32 = 0x02000000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    compression: String,
    files: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    name: String,
    size: usize,
    sha256: String,
    offset: usize,
    compressed_size: usize,
}

/// Keep verified files read-locked until the GUI and its companion have exited.
pub struct PreparedPayload {
    pub directory: PathBuf,
    _locks: Vec<File>,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn allowed_name(name: &str) -> bool {
    if matches!(
        name,
        "SettingsUI.exe" | "Microsoft.WindowsAppRuntime.Bootstrap.dll" | "SettingsUI.pri"
    ) {
        return true;
    }
    let Some(stem) = name.strip_suffix(".xbf") else {
        return false;
    };
    let reserved = matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    !reserved
        && stem.len() <= 100
        && stem.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && stem.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn parse_manifest(bytes: &[u8], payload_length: usize) -> io::Result<Manifest> {
    if bytes.len() > 128 * 1024 || payload_length > MAX_SIZE {
        return Err(invalid("Oversized settings payload"));
    }
    let manifest: Manifest = serde_json::from_slice(bytes).map_err(invalid_json)?;
    if manifest.schema != 1
        || manifest.compression != "xpress-huff"
        || !(5..=128).contains(&manifest.files.len())
    {
        return Err(invalid("Unsupported settings payload manifest"));
    }
    let mut names = HashSet::new();
    let mut end = 0usize;
    let mut total = 0usize;
    let mut previous = "";
    for entry in &manifest.files {
        if !allowed_name(&entry.name)
            || !names.insert(entry.name.to_ascii_lowercase())
            || entry.name.as_str() <= previous
        {
            return Err(invalid("Invalid, duplicate or unordered payload filename"));
        }
        previous = &entry.name;
        if entry.size == 0
            || entry.size > MAX_SIZE
            || entry.compressed_size == 0
            || entry.offset != end
            || entry.sha256.len() != 64
            || !entry
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid("Invalid settings payload entry"));
        }
        end = end
            .checked_add(entry.compressed_size)
            .ok_or_else(|| invalid("Payload offset overflow"))?;
        total = total
            .checked_add(entry.size)
            .ok_or_else(|| invalid("Payload size overflow"))?;
        if end > payload_length || total > MAX_SIZE {
            return Err(invalid("Settings payload bounds exceeded"));
        }
    }
    if end != payload_length
        || [
            "settingsui.exe",
            "microsoft.windowsappruntime.bootstrap.dll",
            "settingsui.pri",
            "app.xbf",
            "mainwindow.xbf",
        ]
        .iter()
        .any(|name| !names.contains(*name))
    {
        return Err(invalid("Incomplete settings payload"));
    }
    Ok(manifest)
}

fn invalid_json(error: serde_json::Error) -> io::Error {
    invalid(error.to_string())
}

/// This path is independent of roaming preferences and of the shared Windows runtime.
pub fn cache_root() -> io::Result<PathBuf> {
    let local =
        dirs::data_local_dir().ok_or_else(|| invalid("Local application data is unavailable"))?;
    if !local.is_absolute() {
        return Err(invalid("Local application data path must be absolute"));
    }
    Ok(local.join("QuotaUsage").join("SettingsPayloads"))
}

fn regular_metadata(metadata: &fs::Metadata, directory: bool) -> io::Result<()> {
    if metadata.file_attributes() & REPARSE != 0
        || metadata.is_dir() != directory
        || (!directory && !metadata.is_file())
    {
        return Err(invalid(
            "Refusing a reparse point or unexpected cache artifact",
        ));
    }
    Ok(())
}

// Open every ancestor without following reparse points and deny directory deletion
// while we use it. This deliberately fails closed for redirected/junction caches.
fn lock_directory_tree(path: &Path, create: bool) -> io::Result<Vec<File>> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(invalid("Cache path must be absolute and normalized"));
    }
    let mut paths: Vec<_> = path.ancestors().filter(|p| p.has_root()).collect();
    paths.reverse();
    let mut locks = Vec::new();
    for current in paths {
        if create
            && fs::symlink_metadata(current).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
        {
            match fs::create_dir(current) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        regular_metadata(&fs::symlink_metadata(current)?, true)?;
        let file = OpenOptions::new()
            .read(true)
            .share_mode(3)
            .custom_flags(OPEN_REPARSE | BACKUP_SEMANTICS)
            .open(current)?;
        regular_metadata(&file.metadata()?, true)?;
        locks.push(file);
    }
    Ok(locks)
}

fn verify_directory(directory: &Path, manifest: &Manifest) -> io::Result<Vec<File>> {
    let mut locks = lock_directory_tree(directory, false)?;
    let canonical = directory.canonicalize()?;
    let entries: Vec<_> = fs::read_dir(&canonical)?.collect::<Result<_, _>>()?;
    if entries.len() != manifest.files.len() {
        return Err(invalid(
            "Unexpected files in settings cache; remove this cache version and retry",
        ));
    }
    for entry in &manifest.files {
        let path = canonical.join(&entry.name);
        regular_metadata(&fs::symlink_metadata(&path)?, false)?;
        if path.canonicalize()?.parent() != Some(canonical.as_path()) {
            return Err(invalid("Settings cache path escaped its directory"));
        }
        let mut file = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .custom_flags(OPEN_REPARSE)
            .open(path)?;
        let metadata = file.metadata()?;
        regular_metadata(&metadata, false)?;
        if metadata.len() != entry.size as u64 {
            return Err(invalid(format!(
                "Settings cache size mismatch: {}",
                entry.name
            )));
        }
        let mut bytes = Vec::with_capacity(entry.size);
        (&mut file)
            .take(entry.size as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() != entry.size || digest(&bytes) != entry.sha256 {
            return Err(invalid(format!(
                "Settings cache hash mismatch: {}. Remove this cache version and retry.",
                entry.name
            )));
        }
        locks.push(file);
    }
    Ok(locks)
}

#[link(name = "cabinet")]
extern "system" {
    fn CreateDecompressor(
        algorithm: u32,
        allocation: *const std::ffi::c_void,
        handle: *mut isize,
    ) -> i32;
    fn Decompress(
        handle: isize,
        source: *const u8,
        size: usize,
        destination: *mut u8,
        capacity: usize,
        written: *mut usize,
    ) -> i32;
    fn CloseDecompressor(handle: isize) -> i32;
}

fn decode(source: &[u8], size: usize) -> io::Result<Vec<u8>> {
    if size == 0 || size > MAX_SIZE {
        return Err(invalid("Invalid decompression size"));
    }
    let mut handle = 0;
    // The OS owns the compressor handle; all buffers remain valid for the calls.
    if unsafe { CreateDecompressor(4, std::ptr::null(), &mut handle) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut bytes = vec![0; size];
    let mut written = 0;
    let result = unsafe {
        Decompress(
            handle,
            source.as_ptr(),
            source.len(),
            bytes.as_mut_ptr(),
            bytes.len(),
            &mut written,
        )
    };
    let error = io::Error::last_os_error();
    unsafe {
        CloseDecompressor(handle);
    }
    if result == 0 {
        return Err(error);
    }
    if written != size {
        return Err(invalid("Decompressed settings payload size mismatch"));
    }
    Ok(bytes)
}

fn extract(root: &Path, manifest_bytes: &[u8], payload: &[u8]) -> io::Result<PreparedPayload> {
    let manifest = parse_manifest(manifest_bytes, payload.len())?;
    let mut locks = lock_directory_tree(root, true)?;
    let root = root.canonicalize()?;
    let directory = root.join(format!(
        "{}-{}",
        env!("CARGO_PKG_VERSION"),
        digest(manifest_bytes)
    ));
    match fs::symlink_metadata(&directory) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| invalid(e.to_string()))?
                .as_nanos();
            let staging = root.join(format!(".stage-{}-{nonce}", std::process::id()));
            fs::create_dir(&staging)?;
            let staging_locks = lock_directory_tree(&staging, false)?;
            // No arbitrary archive paths: the manifest is flat and allowlisted above.
            for entry in &manifest.files {
                let bytes = decode(
                    &payload[entry.offset..entry.offset + entry.compressed_size],
                    entry.size,
                )?;
                if digest(&bytes) != entry.sha256 {
                    return Err(invalid("Embedded settings payload hash mismatch"));
                }
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .share_mode(0)
                    .custom_flags(OPEN_REPARSE)
                    .open(staging.join(&entry.name))?;
                file.write_all(&bytes)?;
                file.sync_all()?;
            }
            drop(staging_locks);
            match fs::rename(&staging, &directory) {
                Ok(()) => {}
                Err(error) => {
                    // A competing process may have published the same content. Never
                    // replace it or recursively delete a path supplied by the filesystem.
                    if fs::symlink_metadata(&directory).is_err() {
                        return Err(error);
                    }
                    // Leave this uniquely named staging folder inert. Uninstall can
                    // clean it with the cache; do not race another process deleting it.
                }
            }
        }
        Err(error) => return Err(error),
    }
    locks.extend(verify_directory(&directory, &manifest)?);
    Ok(PreparedPayload {
        directory: directory.canonicalize()?,
        _locks: locks,
    })
}

/// Called lazily by the bridge for a new settings child, never at widget startup.
/// Only debug builds without an embedded payload may use a development companion.
pub fn prepare() -> Result<Option<PreparedPayload>, String> {
    if development_payload_allowed(MANIFEST, PAYLOAD, cfg!(debug_assertions))? {
        return Ok(None);
    }
    cache_root().and_then(|root| extract(&root, MANIFEST, PAYLOAD)).map(Some)
        .map_err(|e| format!("Cannot prepare native settings: {e}. The cache is under LocalAppData\\QuotaUsage\\SettingsPayloads. Settings also require Windows App Runtime 1.8 x64; install it from Microsoft's Windows App SDK downloads page. It is never installed automatically."))
}

fn development_payload_allowed(
    manifest: &[u8],
    payload: &[u8],
    debug: bool,
) -> Result<bool, String> {
    if manifest.is_empty() && payload.is_empty() {
        if debug {
            return Ok(true);
        }
        return Err("Native settings payload is missing. Reinstall the complete application to restore Settings. The usage widget can continue running.".into());
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_debug_builds_with_both_embedded_artifacts_absent_allow_development() {
        assert!(development_payload_allowed(&[], &[], true).unwrap());
        assert!(development_payload_allowed(&[], &[], false).is_err());
        for debug in [false, true] {
            for (manifest, payload) in [
                (&b"manifest"[..], &b"payload"[..]),
                (&b"manifest"[..], &b""[..]),
                (&b""[..], &b"payload"[..]),
            ] {
                assert!(!development_payload_allowed(manifest, payload, debug).unwrap());
            }
        }
    }

    #[test]
    fn names_are_flat_allowlisted_and_not_devices() {
        for name in [
            "../SettingsUI.exe",
            "C:\\SettingsUI.exe",
            "a/b.xbf",
            "a\\b.xbf",
            "evil.dll",
            "App.xbf:stream",
            "CON.xbf",
            "LPT1.xbf",
            "app.xbf.",
            "App.XBF",
            "a-b.xbf",
            "",
        ] {
            assert!(!allowed_name(name), "{name}");
        }
        for name in [
            "SettingsUI.exe",
            "App.xbf",
            "MainWindow.xbf",
            "AppearancePage.xbf",
        ] {
            assert!(allowed_name(name));
        }
    }

    fn fixture() -> serde_json::Value {
        let names = [
            "App.xbf",
            "MainWindow.xbf",
            "Microsoft.WindowsAppRuntime.Bootstrap.dll",
            "SettingsUI.exe",
            "SettingsUI.pri",
        ];
        serde_json::json!({"schema":1,"compression":"xpress-huff","files": names.iter().enumerate().map(|(i,n)| serde_json::json!({"name":n,"size":1,"sha256":"a".repeat(64),"offset":i,"compressed_size":1})).collect::<Vec<_>>()})
    }

    #[test]
    fn manifest_rejects_paths_duplicates_missing_files_and_bounds() {
        let valid = fixture();
        assert!(parse_manifest(&serde_json::to_vec(&valid).unwrap(), 5).is_ok());
        for (field, value) in [
            ("name", serde_json::json!("../App.xbf")),
            ("name", serde_json::json!("MainWindow.xbf")),
            ("offset", serde_json::json!(1)),
            ("compressed_size", serde_json::json!(usize::MAX)),
            ("size", serde_json::json!(0)),
            ("size", serde_json::json!(MAX_SIZE + 1)),
            ("sha256", serde_json::json!("invalid")),
        ] {
            let mut broken = valid.clone();
            broken["files"][0][field] = value;
            assert!(parse_manifest(&serde_json::to_vec(&broken).unwrap(), 5).is_err());
        }
        let mut broken = valid.clone();
        broken["files"].as_array_mut().unwrap().pop();
        assert!(parse_manifest(&serde_json::to_vec(&broken).unwrap(), 4).is_err());
        assert!(parse_manifest(&serde_json::to_vec(&valid).unwrap(), 6).is_err());
    }

    #[test]
    fn embedded_payload_roundtrip_reuse_and_tamper_detection() {
        if MANIFEST.is_empty() {
            assert!(PAYLOAD.is_empty());
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "quota-payload-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = extract(&root, MANIFEST, PAYLOAD).unwrap();
        let directory = first.directory.clone();
        let second = extract(&root, MANIFEST, PAYLOAD).unwrap();
        assert_eq!(first.directory, second.directory);
        assert!(fs::write(directory.join("SettingsUI.exe"), b"tamper").is_err());
        drop(first);
        drop(second);
        let extra = directory.join("unexpected.dll");
        fs::write(&extra, b"not part of the manifest").unwrap();
        assert!(extract(&root, MANIFEST, PAYLOAD).is_err());
        fs::remove_file(extra).unwrap();
        let manifest = parse_manifest(MANIFEST, PAYLOAD.len()).unwrap();
        let target = directory.join("SettingsUI.exe");
        let mut bytes = fs::read(&target).unwrap();
        bytes[0] ^= 1;
        fs::write(&target, bytes).unwrap();
        assert!(extract(&root, MANIFEST, PAYLOAD).is_err());
        for entry in manifest.files {
            fs::remove_file(directory.join(entry.name)).unwrap();
        }
        fs::remove_dir(directory).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn private_cache_is_not_preferences_or_shared_runtime() {
        let local = dirs::data_local_dir().unwrap();
        let cache = cache_root().unwrap();
        assert_eq!(cache, local.join("QuotaUsage").join("SettingsPayloads"));
        assert_ne!(cache, local);
        if let Some(roaming) = dirs::data_dir() {
            assert_ne!(cache, roaming.join("QuotaUsage"));
        }
    }

    #[test]
    fn corrupt_compressed_data_is_rejected() {
        assert!(decode(b"not compressed", 100).is_err());
        assert!(decode(b"", MAX_SIZE + 1).is_err());
    }

    #[test]
    fn junction_ancestors_are_rejected_without_touching_the_target() {
        let root = std::env::temp_dir().join(format!(
            "quota-payload-junction-test-{}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        let target = root.join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("sentinel"), b"untouched").unwrap();
        let junction = root.join("junction");
        let result = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(lock_directory_tree(&junction, false).is_err());
        assert!(lock_directory_tree(&junction.join("nested"), true).is_err());
        assert!(!target.join("nested").exists());
        assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"untouched");
        fs::remove_dir(junction).unwrap();
        fs::remove_file(target.join("sentinel")).unwrap();
        fs::remove_dir(target).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
