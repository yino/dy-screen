use std::fs;

use dy_screen::asr::AsrBundleManifest;

#[test]
fn third_party_notices_cover_every_manifest_resource_and_release_gate() {
    let root = env!("CARGO_MANIFEST_DIR");
    let manifest = AsrBundleManifest::from_json(
        &fs::read_to_string(format!("{root}/resources/asr/manifest.json")).unwrap(),
    )
    .unwrap();
    let notices = fs::read_to_string(format!("{root}/THIRD_PARTY_NOTICES.md")).unwrap();
    for required in [
        manifest.engine.id,
        manifest.engine.version,
        manifest.engine.source_commit,
        manifest.model.logical_id,
        manifest.model.sha256,
        manifest.model.source,
        manifest.vad.logical_id,
        manifest.vad.sha256,
        manifest.vad.source,
        manifest.normalization.sha256,
        manifest.normalization.source,
    ] {
        assert!(notices.contains(&required), "第三方清单缺少 {required}");
    }
    for release_gate in [
        "464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c",
        "9fd092511605bbebafe095ea6d38d9e40f34d12f7386e1258372df8be0576eb7",
        "--enable-nonfree",
        "--enable-gpl",
        "Developer ID",
        "Authenticode",
        "vc_redist.x64.exe",
        "WebView2",
    ] {
        assert!(
            notices.contains(release_gate),
            "第三方清单缺少发行门禁 {release_gate}"
        );
    }
}
