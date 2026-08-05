use std::fs;
use std::path::{Path, PathBuf};

const EXPECTED_CLIENT_VERSION: &str = "0.3.0";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("Tauri crate should live below the repository root")
        .to_path_buf()
}

fn json_version(path: &Path) -> String {
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path).expect("version file should be readable"))
            .expect("version file should contain valid JSON");
    value["version"]
        .as_str()
        .expect("version should be a string")
        .to_owned()
}

fn package_version_from_toml(contents: &str, package_name: &str) -> Option<String> {
    let mut in_package = false;
    let mut name_matches = false;
    for line in contents.lines() {
        let line = line.trim();
        if line.starts_with("[[package]]") || line.starts_with('[') {
            in_package = line == "[package]" || line == "[[package]]";
            name_matches = false;
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(value) = line
            .strip_prefix("name = \"")
            .and_then(|v| v.strip_suffix('"'))
        {
            name_matches = value == package_name;
        }
        if name_matches
            && let Some(value) = line
                .strip_prefix("version = \"")
                .and_then(|v| v.strip_suffix('"'))
        {
            return Some(value.to_owned());
        }
    }
    None
}

#[test]
fn desktop_client_version_is_consistent_across_release_manifests() {
    let root = repository_root();
    let package_json = json_version(&root.join("package.json"));
    let package_lock = json_version(&root.join("package-lock.json"));
    let tauri_config = json_version(&root.join("src-tauri/tauri.conf.json"));
    let cargo_toml = fs::read_to_string(root.join("src-tauri/Cargo.toml")).unwrap();
    let cargo_lock = fs::read_to_string(root.join("src-tauri/Cargo.lock")).unwrap();
    let cargo_manifest_version = package_version_from_toml(&cargo_toml, "dy-screen-app")
        .expect("Cargo manifest should declare the desktop package version");
    let cargo_lock_version = package_version_from_toml(&cargo_lock, "dy-screen-app")
        .expect("Cargo lock should contain the desktop package version");

    for (source, version) in [
        ("package.json", package_json),
        ("package-lock.json", package_lock),
        ("src-tauri/tauri.conf.json", tauri_config),
        ("src-tauri/Cargo.toml", cargo_manifest_version),
        ("src-tauri/Cargo.lock", cargo_lock_version),
    ] {
        assert_eq!(version, EXPECTED_CLIENT_VERSION, "{source} version differs");
    }
}
