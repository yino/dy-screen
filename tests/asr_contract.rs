use std::path::PathBuf;

use dy_screen::asr::{
    AsrBundleManifest, AsrRequest, AsrResult, AsrSegment, PreparedAudio, PreparedAudioFormat,
    SpeechRegion, TimestampPolicy,
};

fn request() -> AsrRequest {
    AsrRequest {
        request_id: "request-001".to_owned(),
        audio: PreparedAudio {
            path: PathBuf::from("temporary.wav"),
            format: PreparedAudioFormat::PcmS16LeWav,
            sample_rate_hz: 16_000,
            channels: 1,
            duration_ms: 4_000,
        },
        language_hint: Some("zh".to_owned()),
        hotwords: vec!["主播名".to_owned(), "商品名".to_owned()],
        speech_regions: vec![SpeechRegion {
            start_ms: 100,
            end_ms: 3_900,
        }],
        timestamp_policy: TimestampPolicy::Segment,
        max_threads: 4,
    }
}

#[test]
fn neutral_request_rejects_invalid_audio_and_overlapping_regions() {
    let mut invalid_audio = request();
    invalid_audio.audio.sample_rate_hz = 44_100;
    assert_eq!(
        invalid_audio.validate().unwrap_err().code,
        "unsupported_audio_format"
    );

    let mut overlap = request();
    overlap.speech_regions = vec![
        SpeechRegion {
            start_ms: 100,
            end_ms: 2_000,
        },
        SpeechRegion {
            start_ms: 1_900,
            end_ms: 3_000,
        },
    ];
    assert_eq!(
        overlap.validate().unwrap_err().code,
        "overlapping_speech_regions"
    );
}

#[test]
fn neutral_contract_does_not_serialize_supplier_specific_fields() {
    let json = serde_json::to_string(&request()).expect("serialize ASR request");
    for forbidden in [
        "whisper",
        "sidecar",
        "executable",
        "commandLine",
        "modelPath",
    ] {
        assert!(!json.contains(forbidden), "unexpected field: {forbidden}");
    }
}

#[test]
fn result_allows_missing_confidence_but_rejects_fabricated_ranges() {
    let mut result = AsrResult {
        request_id: "request-001".to_owned(),
        identity: dy_screen::asr::AsrEngineIdentity {
            engine_id: "fake-engine".to_owned(),
            engine_version: "1".to_owned(),
            model_id: "fake-model".to_owned(),
            model_version: "1".to_owned(),
        },
        detected_language: Some("zh".to_owned()),
        audio_duration_ms: 4_000,
        segments: vec![AsrSegment {
            start_ms: 100,
            end_ms: 3_900,
            text: "测试文本".to_owned(),
            confidence: None,
        }],
        warnings: Vec::new(),
    };
    result.validate().expect("missing confidence is supported");

    result.segments[0].end_ms = 4_001;
    assert_eq!(result.validate().unwrap_err().code, "invalid_segment_range");
}

#[test]
fn bundled_manifest_is_strict_and_selects_declared_platforms_only() {
    let json = include_str!("../resources/asr/manifest.json");
    let manifest = AsrBundleManifest::from_json(json).expect("valid bundled manifest");
    assert_eq!(manifest.engine.version, "v1.9.1");
    assert_eq!(manifest.model.size_bytes, 190_085_487);
    assert_eq!(
        manifest
            .platform("macos", "aarch64")
            .expect("macOS arm64 platform")
            .accelerator,
        "metal"
    );
    assert_eq!(
        manifest.platform("windows", "x86_64").unwrap().runtime,
        Some("Microsoft Visual C++ 2015-2022 Redistributable x64".to_owned())
    );
    assert_eq!(
        manifest.platform("macos", "x86_64").unwrap_err().code,
        "unsupported_asr_platform"
    );

    let mut escaped: serde_json::Value = serde_json::from_str(json).unwrap();
    escaped["platforms"][0]["libraries"] = serde_json::json!(["../outside.dylib"]);
    assert_eq!(
        AsrBundleManifest::from_json(&escaped.to_string())
            .unwrap_err()
            .code,
        "invalid_resource_manifest"
    );

    let mut partially_sealed: serde_json::Value = serde_json::from_str(json).unwrap();
    partially_sealed["platforms"][0]["resourceIntegrity"] = serde_json::json!([{
        "file": "bin/macos-aarch64/whisper-cli",
        "sizeBytes": 1,
        "sha256": "00".repeat(32)
    }]);
    assert_eq!(
        AsrBundleManifest::from_json(&partially_sealed.to_string())
            .unwrap_err()
            .code,
        "invalid_resource_manifest"
    );

    let mut non_canonical: serde_json::Value = serde_json::from_str(json).unwrap();
    non_canonical["platforms"][1]["ffmpeg"] = serde_json::json!(r"bin\windows-x86_64\ffmpeg.exe");
    assert_eq!(
        AsrBundleManifest::from_json(&non_canonical.to_string())
            .unwrap_err()
            .code,
        "invalid_resource_manifest"
    );
}
