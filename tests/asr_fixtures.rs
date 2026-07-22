use std::fs;
use std::path::Path;
use std::process::Command;

use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureManifest {
    schema_version: u32,
    fixtures: Vec<Fixture>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Fixture {
    id: String,
    path: String,
    sha256: String,
    duration_ms: u64,
    audio_present: bool,
    expected_speech_ranges_ms: Vec<[u64; 2]>,
    expected_keywords: Vec<String>,
    boundary_duplicate_with: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProbeOutput {
    streams: Vec<ProbeStream>,
    format: ProbeFormat,
}

#[derive(Debug, Deserialize)]
struct ProbeStream {
    codec_type: String,
}

#[derive(Debug, Deserialize)]
struct ProbeFormat {
    duration: String,
}

#[test]
fn fixed_asr_fixtures_match_hash_media_and_timestamp_expectations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/asr");
    let manifest: FixtureManifest = serde_json::from_slice(
        &fs::read(root.join("manifest.json")).expect("read fixture manifest"),
    )
    .expect("parse fixture manifest");
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.fixtures.len(), 8);

    for fixture in manifest.fixtures {
        let path = root.join(&fixture.path);
        let bytes = fs::read(&path)
            .unwrap_or_else(|error| panic!("fixture {} is missing: {error}", fixture.id));
        assert_eq!(
            hex::encode(Sha256::digest(&bytes)),
            fixture.sha256,
            "fixture {} changed without updating expectations",
            fixture.id
        );

        let output = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration:stream=codec_type",
                "-of",
                "json",
            ])
            .arg(&path)
            .output()
            .expect("run ffprobe for ASR fixture");
        assert!(output.status.success(), "ffprobe failed for {}", fixture.id);
        let probe: ProbeOutput =
            serde_json::from_slice(&output.stdout).expect("parse ffprobe output");
        let actual_duration_ms = (probe
            .format
            .duration
            .parse::<f64>()
            .expect("numeric duration")
            * 1_000.0)
            .round() as u64;
        assert_eq!(actual_duration_ms, fixture.duration_ms, "{}", fixture.id);
        assert_eq!(
            probe
                .streams
                .iter()
                .any(|stream| stream.codec_type == "audio"),
            fixture.audio_present,
            "{}",
            fixture.id
        );
        for [start_ms, end_ms] in &fixture.expected_speech_ranges_ms {
            assert!(start_ms < end_ms, "{} has empty speech range", fixture.id);
            assert!(
                *end_ms <= fixture.duration_ms,
                "{} speech range exceeds media duration",
                fixture.id
            );
        }
        if !fixture.expected_speech_ranges_ms.is_empty() {
            assert!(
                !fixture.expected_keywords.is_empty(),
                "{} must record expected transcript keywords",
                fixture.id
            );
        }
        if fixture.boundary_duplicate_with.is_some() {
            assert_eq!(fixture.id, "session_part_002");
        }
    }
}

#[test]
fn fixture_set_contains_required_edge_cases() {
    let json = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/asr/manifest.json"
    ))
    .expect("read fixture manifest");
    for required in [
        "short_zh",
        "no_speech",
        "music_only",
        "video_only",
        "mixed_zh_en",
        "session_part_001",
        "session_part_002",
        "windows_unicode_path",
    ] {
        assert!(
            json.contains(required),
            "missing required fixture {required}"
        );
    }
    assert!(json.contains("Windows 中文路径/测试 视频.mp4"));
}
