use dy_screen_app_lib::ai::{
    AiInputSourceKind, AiInputStatus, AiProjectStatus, AiRepository, AiTranscriptProjection,
    NewAiProjectInput, NewAsrArtifact, RecognitionProfile, SourceFingerprint,
    TranscriptSegmentDraft,
};
use dy_screen_app_lib::database::Database;

fn profile() -> RecognitionProfile {
    RecognitionProfile {
        engine_id: "fake-engine".to_owned(),
        engine_version: "1".to_owned(),
        model_id: "fake-model".to_owned(),
        model_version: "1".to_owned(),
        language_hint: Some("zh".to_owned()),
        vad_model_id: "fake-vad".to_owned(),
        vad_threshold_millis: 500,
        vad_padding_ms: 500,
        timestamp_policy: "segment".to_owned(),
        normalization_version: "zh-normalize-v1".to_owned(),
        hotwords: vec!["商品名".to_owned()],
    }
}

fn input(path: &str, position: i64, duration_ms: u64) -> NewAiProjectInput {
    NewAiProjectInput {
        position,
        source_kind: AiInputSourceKind::LocalFile,
        video_id: None,
        display_name: path.rsplit(['/', '\\']).next().unwrap().to_owned(),
        source_path: path.to_owned(),
        source_fingerprint: SourceFingerprint {
            normalized_path: path.to_owned(),
            size_bytes: 100 + position as u64,
            modified_at_ms: 1_700_000_000_000 + position,
            video_id: None,
        },
        duration_ms: Some(duration_ms),
        audio_present: Some(true),
    }
}

#[test]
fn projection_preserves_stable_ids_video_boundaries_and_failed_input_gaps_without_paths() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let repository = AiRepository::new(database);
    let project = repository
        .create_project("直播转写投影", &profile())
        .unwrap();
    let first = repository
        .add_input(project.id, input("/private/主播/第一段.mp4", 0, 4_000))
        .unwrap();
    let second = repository
        .add_input(project.id, input("C:\\用户\\主播\\第二段.mp4", 1, 6_000))
        .unwrap();
    repository.freeze_project(project.id).unwrap();
    repository
        .transition_project(project.id, AiProjectStatus::Running)
        .unwrap();

    let artifact = repository
        .create_artifact(NewAsrArtifact {
            source_fingerprint: first.source_fingerprint.clone(),
            recognition_profile_hash: profile().fingerprint().unwrap(),
            engine_id: "fake-engine".to_owned(),
            engine_version: "1".to_owned(),
            model_id: "fake-model".to_owned(),
            model_version: "1".to_owned(),
        })
        .unwrap();
    let segments = repository
        .publish_artifact(
            artifact.id,
            4_000,
            Some("zh"),
            &[TranscriptSegmentDraft {
                source_start_ms: 100,
                source_end_ms: 1_500,
                raw_text: "歡迎來到直播間".to_owned(),
                normalized_text: "欢迎来到直播间。".to_owned(),
                confidence: Some(0.91),
            }],
        )
        .unwrap();
    repository.attach_artifact(first.id, artifact.id).unwrap();
    repository
        .transition_input(first.id, AiInputStatus::Validating, None)
        .unwrap();
    repository
        .transition_input(first.id, AiInputStatus::Completed, None)
        .unwrap();
    repository
        .transition_input(second.id, AiInputStatus::Validating, None)
        .unwrap();
    repository
        .transition_input(
            second.id,
            AiInputStatus::Failed,
            Some(("no_audio_track", "视频没有音轨")),
        )
        .unwrap();
    repository.recompute_project_progress(project.id).unwrap();

    let projection = AiTranscriptProjection::load(&repository, project.id).unwrap();
    assert_eq!(
        projection.project.status,
        AiProjectStatus::CompletedWithErrors
    );
    assert_eq!(projection.inputs.len(), 2);
    assert_eq!(
        projection.inputs[0].segments[0].stable_segment_id,
        segments[0].id
    );
    assert_eq!(projection.inputs[0].segments[0].project_start_ms, Some(100));
    assert_eq!(projection.inputs[1].project_offset_ms, Some(4_000));
    assert_eq!(projection.inputs[1].gap_duration_ms, Some(6_000));
    assert_eq!(
        projection.copy_input_text(first.id).unwrap(),
        "欢迎来到直播间。"
    );
    assert!(projection.copy_project_text().contains("第一段.mp4"));
    assert!(
        projection
            .to_txt()
            .contains("[00:00.100 - 00:01.500] 欢迎来到直播间。")
    );
    let json = projection.to_json().unwrap();
    assert!(json.contains(&segments[0].id));
    assert!(json.contains("projectStartMs"));
    assert!(!json.contains("/private/主播"));
    assert!(!json.contains("C:\\\\用户"));
    assert!(!projection.to_txt().contains("/private/主播"));
}
