use std::fs;
use std::path::PathBuf;

use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema_version: u32,
    fixtures: Vec<Fixture>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    path: String,
    sha256: String,
    video_codec: String,
    audio_codec: String,
    width: u32,
    height: u32,
    duration_ms: u64,
}

#[test]
fn clip_platform_fixtures_are_small_immutable_h264_hevc_aac_inputs() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/clip-platform");
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.fixtures.len(), 4);

    let mut total_size = 0;
    let mut codecs = Vec::new();
    for fixture in manifest.fixtures {
        assert!(!fixture.path.contains(['/', '\\']));
        assert_eq!(fixture.audio_codec, "aac");
        assert!(fixture.width > 0 && fixture.height > 0);
        assert!((750..=1_050).contains(&fixture.duration_ms));
        let bytes = fs::read(root.join(&fixture.path)).unwrap();
        total_size += bytes.len();
        assert_eq!(hex::encode(Sha256::digest(&bytes)), fixture.sha256);
        codecs.push(fixture.video_codec);
    }
    assert!(codecs.iter().any(|codec| codec == "h264"));
    assert!(codecs.iter().any(|codec| codec == "hevc"));
    assert!(total_size < 256 * 1024);
}
