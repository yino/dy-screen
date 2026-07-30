use std::path::Path;

use dy_screen::runtime_resources::{
    ResourceStatus, RuntimeArchive, RuntimeComponent, RuntimeFile, RuntimeInstaller,
    RuntimeManifest, RuntimeManifestSignature, RuntimePlatformManifest, sha256_file,
};
use dy_screen_app_lib::database::Database;
use dy_screen_app_lib::runtime_resource_state::RuntimeResourceState;

fn fixture_manifest(root: &Path, bundle_version: &str, contents: &[u8]) -> RuntimeManifest {
    let tool = root.join("bin/ffmpeg");
    let probe = root.join("bin/ffprobe");
    std::fs::create_dir_all(tool.parent().unwrap()).unwrap();
    std::fs::write(&tool, contents).unwrap();
    std::fs::write(&probe, contents).unwrap();
    let (tool_size_bytes, tool_sha256) = sha256_file(&tool).unwrap();
    let (probe_size_bytes, probe_sha256) = sha256_file(&probe).unwrap();
    RuntimeManifest {
        schema_version: 2,
        app_min_version: env!("CARGO_PKG_VERSION").to_owned(),
        bundle_version: bundle_version.to_owned(),
        channel: "stable".to_owned(),
        platforms: vec![RuntimePlatformManifest {
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            minimum_free_disk_bytes: 1,
            minimum_memory_bytes: 1,
        }],
        components: vec![
            RuntimeComponent {
                id: "media.ffmpeg".to_owned(),
                version: "test".to_owned(),
                required: true,
                files: vec![RuntimeFile {
                    path: "bin/ffmpeg".to_owned(),
                    size_bytes: tool_size_bytes,
                    sha256: tool_sha256,
                    executable: true,
                }],
            },
            RuntimeComponent {
                id: "media.ffprobe".to_owned(),
                version: "test".to_owned(),
                required: true,
                files: vec![RuntimeFile {
                    path: "bin/ffprobe".to_owned(),
                    size_bytes: probe_size_bytes,
                    sha256: probe_sha256,
                    executable: true,
                }],
            },
        ],
        archive: RuntimeArchive {
            path: "runtime-bundle.tar.zst".to_owned(),
            size_bytes: 1,
            sha256: "00".repeat(32),
            download_url: "runtime-bundle.tar.zst".to_owned(),
        },
        licenses: Vec::new(),
        signature: RuntimeManifestSignature {
            algorithm: "ed25519".to_owned(),
            key_id: "test".to_owned(),
            value: String::new(),
        },
    }
}

#[test]
fn packaged_upgrade_wins_over_older_installed_resources() {
    let temporary = tempfile::tempdir().unwrap();
    let app_data = temporary.path().join("app-data");
    let installed_source = temporary.path().join("installed-source");
    let bundled_root = temporary.path().join("bundled");
    std::fs::create_dir_all(&installed_source).unwrap();
    std::fs::create_dir_all(&bundled_root).unwrap();

    let installed = fixture_manifest(&installed_source, "2026.07.2", b"old");
    RuntimeInstaller::new(app_data.join("runtime-resources"))
        .unwrap()
        .install_directory(&installed_source, &installed)
        .unwrap();

    let bundled = fixture_manifest(&bundled_root, "2026.07.3", b"new");
    std::fs::write(
        bundled_root.join("runtime-manifest.json"),
        serde_json::to_vec_pretty(&bundled).unwrap(),
    )
    .unwrap();

    let database = Database::open(&app_data.join("dy-screen.sqlite3")).unwrap();
    database.migrate().unwrap();
    let state = RuntimeResourceState::new(database, app_data, bundled_root.clone());

    assert_eq!(state.view().status, ResourceStatus::Verifying);
    assert!(!state.view().ready);
    assert!(state.current_root().is_none());
    assert!(state.refresh().ready);
    assert_eq!(
        state.current_root().as_deref(),
        Some(bundled_root.as_path())
    );
}

#[test]
fn resolved_resources_are_cached_and_metadata_changes_trigger_recheck() {
    let temporary = tempfile::tempdir().unwrap();
    let app_data = temporary.path().join("app-data");
    let bundled_root = temporary.path().join("bundled");
    std::fs::create_dir_all(&bundled_root).unwrap();
    let manifest = fixture_manifest(&bundled_root, "2026.07.3", b"verified");
    std::fs::write(
        bundled_root.join("runtime-manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let database = Database::open(&app_data.join("dy-screen.sqlite3")).unwrap();
    database.migrate().unwrap();
    let state = RuntimeResourceState::new(database, app_data, bundled_root.clone());

    assert!(state.refresh().ready);
    assert_eq!(
        state.current_root().as_deref(),
        Some(bundled_root.as_path())
    );
    let (cached_ffmpeg, cached_ffprobe) = state.media_tools().unwrap();
    assert_eq!(cached_ffmpeg, bundled_root.join("bin/ffmpeg"));
    assert_eq!(cached_ffprobe, bundled_root.join("bin/ffprobe"));

    std::fs::write(bundled_root.join("bin/ffmpeg"), b"changed").unwrap();

    assert!(state.current_root().is_none());
    assert!(!state.view().ready);
}
