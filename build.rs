use winres::{VersionInfo, WindowsResource};

fn main() {
    embed_settings_payload();
    let version = env!("CARGO_PKG_VERSION");

    // Embed the icon and richer PE version metadata into the executable.
    let mut res = WindowsResource::new();
    let numeric_version = pack_version(version);

    res.set_icon("src/icons/icon.ico")
        .set("FileVersion", version)
        .set("ProductVersion", version)
        .set_version_info(VersionInfo::FILEVERSION, numeric_version)
        .set_version_info(VersionInfo::PRODUCTVERSION, numeric_version);

    res.compile().expect("Failed to compile Windows resources");
}

fn embed_settings_payload() {
    use std::{env, fs, path::PathBuf};
    println!("cargo:rerun-if-env-changed=CODEX_SETTINGS_PAYLOAD");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icons/icon.ico");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    if let Some(directory) = env::var_os("CODEX_SETTINGS_PAYLOAD") {
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
    } else {
        assert_ne!(env::var("PROFILE").unwrap(), "release", "Release builds require the native settings payload. Run scripts/build-release.ps1 (no restore is performed by build.rs).");
        fs::write(out.join("manifest.json"), []).unwrap();
        fs::write(out.join("payload.bin"), []).unwrap();
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
