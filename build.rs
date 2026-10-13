use winres::{VersionInfo, WindowsResource};

fn main() {
    embed_settings_payload();
    let version = env!("CARGO_PKG_VERSION");

    // Embed the icon and richer PE version metadata into the executable.
    let mut res = WindowsResource::new();
    let numeric_version = pack_version(version);

    res.set_icon("src/icons/redesign/quota-usage-monitor.ico")
        .set("CompanyName", "datarose.net")
        .set("ProductName", "Quota Usage Monitor")
        .set("FileDescription", "Lightweight, portable, easy-to-use, customizable, open-source usage monitor for Claude, Codex, and many other providers")
        .set("FileVersion", version)
        .set("ProductVersion", version)
        .set_version_info(VersionInfo::FILEVERSION, numeric_version)
        .set_version_info(VersionInfo::PRODUCTVERSION, numeric_version);

    res.compile().expect("Failed to compile Windows resources");
}

fn embed_settings_payload() {
    use std::{env, fs, path::PathBuf};
    println!("cargo:rerun-if-env-changed=QUOTA_SETTINGS_PAYLOAD");
    println!("cargo:rerun-if-env-changed=QUOTA_SETTINGS_MSBUILD");
    println!("cargo:rerun-if-env-changed=QUOTA_SETTINGS_SKIP_NATIVE_BUILD");
    println!("cargo:rerun-if-env-changed=CODEX_SETTINGS_PAYLOAD");
    println!("cargo:rerun-if-env-changed=CODEX_SETTINGS_MSBUILD");
    println!("cargo:rerun-if-env-changed=CODEX_SETTINGS_SKIP_NATIVE_BUILD");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icons/redesign/quota-usage-monitor.ico");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let payload = env::var_os("QUOTA_SETTINGS_PAYLOAD")
        .or_else(|| env::var_os("CODEX_SETTINGS_PAYLOAD"));
    if let Some(directory) = payload {
        assert_eq!(
            env::var("CARGO_CFG_TARGET_ARCH").unwrap(),
            "x86_64",
            "The native settings companion is built for x64 only"
        );
        let directory = PathBuf::from(directory);
        for name in ["manifest.json", "payload.bin"] {
            let source = directory.join(name);
            println!("cargo:rerun-if-changed={}", source.display());
            let bytes =
                fs::read(&source).expect("Missing settings payload; run scripts/build-release.ps1");
            assert!(!bytes.is_empty(), "Empty settings payload");
            fs::write(out.join(name), bytes).unwrap();
        }
    } else if env::var_os("QUOTA_SETTINGS_SKIP_NATIVE_BUILD")
        .or_else(|| env::var_os("CODEX_SETTINGS_SKIP_NATIVE_BUILD"))
        .as_deref()
        == Some(std::ffi::OsStr::new("1"))
    {
        assert_ne!(env::var("PROFILE").unwrap(), "release", "Release builds require the native settings payload. Run scripts/build-release.ps1 (no restore is performed by build.rs).");
        println!("cargo:warning=Native Settings build explicitly disabled; debug Settings may use the unverified settings-ui/bin/portable fallback. Remove QUOTA_SETTINGS_SKIP_NATIVE_BUILD to build current sources.");
        fs::write(out.join("manifest.json"), []).unwrap();
        fs::write(out.join("payload.bin"), []).unwrap();
    } else {
        assert_ne!(
            env::var("PROFILE").unwrap(),
            "release",
            "Release builds require the native settings payload. Run scripts/build-release.ps1."
        );
        assert_eq!(
            env::var("CARGO_CFG_TARGET_ARCH").unwrap(),
            "x86_64",
            "The native settings companion is built for x64 only"
        );
        track_native_inputs(std::path::Path::new("settings-ui"));
        for input in [
            "src/icons/256x256.png",
            "THIRD-PARTY-NOTICES.txt",
            "scripts/build-settings-dev.ps1",
            "scripts/publish-settings.ps1",
            "scripts/package-settings.ps1",
        ] {
            println!("cargo:rerun-if-changed={input}");
        }
        let status = std::process::Command::new("pwsh")
            .args(["-NoProfile", "-NonInteractive", "-File", "scripts/build-settings-dev.ps1", "-OutputDirectory"])
            .arg(&out)
            .status()
            .expect("Cannot start PowerShell 7.2+. Install pwsh and the native build prerequisites in settings-ui/BUILDING.md, or explicitly set QUOTA_SETTINGS_SKIP_NATIVE_BUILD=1 for Rust-only work.");
        assert!(status.success(), "Native Settings build failed; refusing to embed stale UI. See the MSBuild error above and settings-ui/BUILDING.md. Set QUOTA_SETTINGS_MSBUILD to the full VS 2022 MSBuild.exe path if discovery fails. QUOTA_SETTINGS_SKIP_NATIVE_BUILD=1 explicitly opts out for Rust-only work.");
        for name in ["manifest.json", "payload.bin"] {
            assert!(
                fs::metadata(out.join(name)).is_ok_and(|file| file.len() > 0),
                "Native Settings build did not produce {name}"
            );
        }
    }
}

fn track_native_inputs(directory: &std::path::Path) {
    // Do not watch the project root recursively: restore/build outputs live there.
    for entry in std::fs::read_dir(directory).expect("Cannot read native Settings sources") {
        let path = entry.expect("Cannot read native Settings input").path();
        if path.is_dir() {
            if matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some("bin" | "obj" | "packages" | ".git")
            ) {
                continue;
            }
            println!("cargo:rerun-if-changed={}", path.display());
        } else {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

fn pack_version(version: &str) -> u64 {
    let core = version.split('-').next().unwrap_or(version);
    let mut parts = core.split('.').map(|part| part.parse::<u64>().unwrap_or(0));

    let major = parts.next().unwrap_or(0).min(u16::MAX as u64);
    let minor = parts.next().unwrap_or(0).min(u16::MAX as u64);
    let patch = parts.next().unwrap_or(0).min(u16::MAX as u64);

    (major << 48) | (minor << 32) | (patch << 16)
}
