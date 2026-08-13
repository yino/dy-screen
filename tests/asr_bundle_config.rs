use std::fs;

use serde_json::Value;

fn read_config(path: &str) -> Value {
    serde_json::from_slice(&fs::read(format!("{}/{}", env!("CARGO_MANIFEST_DIR"), path)).unwrap())
        .unwrap()
}

#[test]
fn release_configs_bundle_the_staged_asr_directory_at_the_runtime_path() {
    for path in [
        "src-tauri/tauri.macos.conf.json",
        "src-tauri/tauri.windows.conf.json",
    ] {
        let config = read_config(path);
        if path.ends_with("windows.conf.json") {
            assert_eq!(
                config["bundle"]["resources"]["../resources/asr-stage/"],
                "resources/asr/"
            );
        }
        assert_eq!(
            config["bundle"]["resources"]["../THIRD_PARTY_NOTICES.md"],
            "licenses/THIRD_PARTY_NOTICES.md"
        );
    }

    let generator = fs::read_to_string(format!(
        "{}/scripts/generate-tauri-macos-resource-config.sh",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    assert!(generator.contains("jq -n --arg source"));
    assert!(generator.contains("\"resources/asr/\""));
}

#[test]
fn windows_release_is_offline_and_installs_the_declared_native_runtime() {
    let config = read_config("src-tauri/tauri.windows.conf.json");
    assert_eq!(
        config["bundle"]["windows"]["webviewInstallMode"]["type"],
        "offlineInstaller"
    );
    assert_eq!(
        config["bundle"]["windows"]["nsis"]["installerHooks"],
        "windows/asr-runtime-hooks.nsh"
    );
    let hooks = fs::read_to_string(format!(
        "{}/src-tauri/windows/asr-runtime-hooks.nsh",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    assert!(hooks.contains("vc_redist.x64.exe"));
    assert!(hooks.contains("/install /quiet /norestart"));
}
