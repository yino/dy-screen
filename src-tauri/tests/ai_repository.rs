use std::collections::HashMap;
use std::path::PathBuf;

use dy_screen_app_lib::ai::{
    AiArtifactStatus, AiClipEffect, AiClipExportStatus, AiClipSegment, AiClipSegmentUpdate,
    AiHighlightRunStatus, AiInputSourceKind, AiInputStatus, AiProjectStatus, AiRepository,
    HighlightCandidateDraft, HighlightCandidateScore, NewAiHighlightChunk, NewAiHighlightRun,
    NewAiProjectInput, NewAsrArtifact, RecognitionProfile, SourceFingerprint, TranscriptSegment,
    TranscriptSegmentDraft, project_clip_subtitles,
};
use dy_screen_app_lib::database::Database;
use dy_screen_app_lib::domain::{NewStreamer, NewVideo};
use rusqlite::{Connection, OptionalExtension};
use tempfile::tempdir;

fn profile() -> RecognitionProfile {
    RecognitionProfile {
        engine_id: "whisper.cpp".to_owned(),
        engine_version: "v1.9.1".to_owned(),
        model_id: "whisper-small-multilingual-q5_1".to_owned(),
        model_version: "small-q5_1@5359861".to_owned(),
        language_hint: Some("zh".to_owned()),
        vad_model_id: "silero-vad-v6.2.0".to_owned(),
        vad_threshold_millis: 500,
        vad_padding_ms: 500,
        timestamp_policy: "segment".to_owned(),
        normalization_version: "zh-normalize-v1".to_owned(),
        hotwords: vec!["商品名".to_owned(), "主播名".to_owned()],
    }
}

fn input(path: &str, position: i64) -> NewAiProjectInput {
    NewAiProjectInput {
        position,
        source_kind: AiInputSourceKind::LocalFile,
        video_id: None,
        display_name: PathBuf::from(path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        source_path: path.to_owned(),
        source_fingerprint: SourceFingerprint {
            normalized_path: path.to_owned(),
            size_bytes: 1_024 + position as u64,
            modified_at_ms: 1_700_000_000_000 + position,
            video_id: None,
        },
        duration_ms: Some(4_000),
        audio_present: Some(true),
    }
}

#[test]
fn clip_subtitles_are_clipped_and_remapped_after_cross_source_reordering() {
    let clips = vec![
        AiClipSegment {
            id: 12,
            clip_project_id: 1,
            candidate_id: 2,
            input_id: 22,
            position: 0,
            title: "第二个来源".to_owned(),
            source_start_ms: 2_000,
            source_end_ms: 5_000,
            volume_percent: 100,
            effect: AiClipEffect::None,
        },
        AiClipSegment {
            id: 11,
            clip_project_id: 1,
            candidate_id: 1,
            input_id: 21,
            position: 1,
            title: "第一个来源".to_owned(),
            source_start_ms: 1_000,
            source_end_ms: 3_000,
            volume_percent: 100,
            effect: AiClipEffect::None,
        },
    ];
    let mut transcripts = HashMap::new();
    transcripts.insert(
        22,
        vec![TranscriptSegment {
            id: "seg_b".to_owned(),
            artifact_id: 2,
            ordinal: 0,
            source_start_ms: 1_500,
            source_end_ms: 2_600,
            raw_text: "边界字幕".to_owned(),
            normalized_text: "边界字幕。".to_owned(),
            confidence: None,
        }],
    );
    transcripts.insert(
        21,
        vec![
            TranscriptSegment {
                id: "seg_a".to_owned(),
                artifact_id: 1,
                ordinal: 0,
                source_start_ms: 1_500,
                source_end_ms: 2_500,
                raw_text: "完整字幕".to_owned(),
                normalized_text: "完整字幕。".to_owned(),
                confidence: Some(0.9),
            },
            TranscriptSegment {
                id: "seg_empty".to_owned(),
                artifact_id: 1,
                ordinal: 1,
                source_start_ms: 2_500,
                source_end_ms: 2_900,
                raw_text: "".to_owned(),
                normalized_text: "  ".to_owned(),
                confidence: None,
            },
        ],
    );

    let (subtitles, complete) = project_clip_subtitles(&clips, &transcripts);

    assert!(complete);
    assert_eq!(subtitles.len(), 2);
    assert_eq!(subtitles[0].clip_segment_id, 12);
    assert_eq!(
        (subtitles[0].source_start_ms, subtitles[0].source_end_ms),
        (2_000, 2_600)
    );
    assert_eq!(
        (subtitles[0].project_start_ms, subtitles[0].project_end_ms),
        (0, 600)
    );
    assert_eq!(subtitles[1].clip_segment_id, 11);
    assert_eq!(
        (subtitles[1].project_start_ms, subtitles[1].project_end_ms),
        (3_500, 4_500)
    );
}

#[test]
fn clip_subtitle_projection_marks_any_segment_without_text_incomplete() {
    let clips = vec![AiClipSegment {
        id: 11,
        clip_project_id: 1,
        candidate_id: 1,
        input_id: 21,
        position: 0,
        title: "无字幕片段".to_owned(),
        source_start_ms: 1_000,
        source_end_ms: 3_000,
        volume_percent: 100,
        effect: AiClipEffect::None,
    }];

    let (subtitles, complete) = project_clip_subtitles(&clips, &HashMap::new());

    assert!(subtitles.is_empty());
    assert!(!complete);
}

#[test]
fn ai_migration_is_idempotent_preserves_existing_data_and_has_foreign_keys() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("ai-migration.sqlite3");
    let database = Database::open(&path).unwrap();
    database.migrate().unwrap();
    let before_streamers: i64 = Connection::open(&path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM streamers", [], |row| row.get(0))
        .unwrap();

    database.migrate().unwrap();
    let connection = Connection::open(&path).unwrap();
    for table in [
        "ai_projects",
        "ai_project_inputs",
        "asr_artifacts",
        "transcript_segments",
        "llm_provider_settings",
        "ai_highlight_runs",
        "ai_highlight_chunks",
        "ai_highlight_candidates",
        "ai_clip_projects",
        "ai_clip_segments",
        "client_activation",
    ] {
        assert_eq!(
            connection
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [table],
                    |_| Ok(()),
                )
                .optional()
                .unwrap(),
            Some(()),
            "missing table {table}"
        );
    }
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM streamers", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        before_streamers
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = 4",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = 12",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = 13",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = 14",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    for column in ["project_tags_json", "analysis_goal", "deleting_at"] {
        assert!(
            connection
                .query_row(
                    "SELECT 1 FROM pragma_table_info('ai_projects') WHERE name = ?1",
                    [column],
                    |_| Ok(()),
                )
                .optional()
                .unwrap()
                .is_some()
        );
    }
    for column in ["scheduler_generation", "queue_priority", "queue_sequence"] {
        assert!(
            connection
                .query_row(
                    "SELECT 1 FROM pragma_table_info('ai_project_inputs') WHERE name = ?1",
                    [column],
                    |_| Ok(()),
                )
                .optional()
                .unwrap()
                .is_some()
        );
    }
    for column in ["output_width", "output_height", "version"] {
        assert!(
            connection
                .query_row(
                    "SELECT 1 FROM pragma_table_info('ai_clip_projects') WHERE name = ?1",
                    [column],
                    |_| Ok(()),
                )
                .optional()
                .unwrap()
                .is_some(),
            "missing clip project column {column}"
        );
    }
}

#[test]
fn draft_crud_ordering_and_state_transitions_are_strict() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let repository = AiRepository::new(database);
    let project = repository.create_project("直播转写", &profile()).unwrap();
    let first = repository
        .add_input(project.id, input("/tmp/a.mp4", 0))
        .unwrap();
    let second = repository
        .add_input(project.id, input("/tmp/b.mp4", 1))
        .unwrap();

    repository
        .reorder_inputs(project.id, &[second.id, first.id])
        .unwrap();
    let reordered = repository.get_project(project.id).unwrap();
    assert_eq!(reordered.inputs[0].id, second.id);
    assert_eq!(reordered.inputs[1].id, first.id);

    repository.rename_project(project.id, "整场直播").unwrap();
    repository.freeze_project(project.id).unwrap();
    assert_eq!(
        repository.get_project(project.id).unwrap().project.status,
        AiProjectStatus::Queued
    );
    let error = repository
        .add_input(project.id, input("/tmp/c.mp4", 2))
        .unwrap_err();
    assert!(error.to_string().contains("草稿"));
    let error = repository
        .transition_project(project.id, AiProjectStatus::Completed)
        .unwrap_err();
    assert!(error.to_string().contains("状态"));

    repository
        .transition_project(project.id, AiProjectStatus::Running)
        .unwrap();
    repository
        .transition_input(first.id, AiInputStatus::Validating, None)
        .unwrap();
    let in_progress = repository.recompute_project_progress(project.id).unwrap();
    assert_eq!(in_progress.progress_percent, 5);
    let restored = repository.get_project(project.id).unwrap();
    assert_eq!(
        restored
            .inputs
            .iter()
            .find(|input| input.id == first.id)
            .unwrap()
            .progress_percent,
        10
    );
    assert_eq!(
        restored
            .inputs
            .iter()
            .find(|input| input.id == second.id)
            .unwrap()
            .progress_percent,
        0
    );
    repository
        .transition_input(
            first.id,
            AiInputStatus::Failed,
            Some(("probe_failed", "无法读取音轨")),
        )
        .unwrap();
    repository
        .transition_input(second.id, AiInputStatus::Validating, None)
        .unwrap();
    repository
        .transition_input(second.id, AiInputStatus::Completed, None)
        .unwrap();
    let aggregated = repository.recompute_project_progress(project.id).unwrap();
    assert_eq!(aggregated.status, AiProjectStatus::CompletedWithErrors);
    assert_eq!(aggregated.progress_percent, 100);
}

#[test]
fn freezing_persists_offsets_retry_requeues_failed_input_and_cancel_is_project_scoped() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let repository = AiRepository::new(database);
    let project = repository.create_project("状态操作", &profile()).unwrap();
    let first = repository
        .add_input(project.id, input("/tmp/first.mp4", 0))
        .unwrap();
    let mut second_input = input("/tmp/second.mp4", 1);
    second_input.duration_ms = Some(6_000);
    let second = repository.add_input(project.id, second_input).unwrap();
    repository.freeze_project(project.id).unwrap();
    let frozen = repository.get_project(project.id).unwrap();
    assert_eq!(frozen.inputs[0].project_offset_ms, Some(0));
    assert_eq!(frozen.inputs[1].project_offset_ms, Some(4_000));

    repository
        .transition_project(project.id, AiProjectStatus::Running)
        .unwrap();
    repository
        .transition_input(first.id, AiInputStatus::Validating, None)
        .unwrap();
    repository
        .transition_input(
            first.id,
            AiInputStatus::Failed,
            Some(("fake_failure", "测试失败")),
        )
        .unwrap();
    repository
        .transition_input(second.id, AiInputStatus::Validating, None)
        .unwrap();
    repository
        .transition_input(second.id, AiInputStatus::Completed, None)
        .unwrap();
    repository.recompute_project_progress(project.id).unwrap();
    assert_eq!(
        repository.get_project(project.id).unwrap().project.status,
        AiProjectStatus::CompletedWithErrors
    );

    let retried = repository.prepare_input_retry(first.id).unwrap();
    assert_eq!(retried.status, AiInputStatus::Pending);
    assert_eq!(retried.last_error_code, None);
    assert_eq!(
        repository.get_project(project.id).unwrap().project.status,
        AiProjectStatus::Queued
    );

    repository.cancel_project(project.id).unwrap();
    let cancelled = repository.get_project(project.id).unwrap();
    assert_eq!(cancelled.project.status, AiProjectStatus::Cancelled);
    assert_eq!(cancelled.project.progress_percent, 100);
    assert_eq!(cancelled.inputs[0].status, AiInputStatus::Cancelled);
    assert_eq!(cancelled.inputs[1].status, AiInputStatus::Completed);

    let retried_cancelled = repository.prepare_input_retry(first.id).unwrap();
    assert_eq!(retried_cancelled.status, AiInputStatus::Pending);
    assert_eq!(retried_cancelled.last_error_code, None);
    assert_eq!(
        repository.get_project(project.id).unwrap().project.status,
        AiProjectStatus::Queued
    );
}

#[test]
fn artifact_publish_is_atomic_reusable_and_uses_stable_segment_ids() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let repository = AiRepository::new(database);
    let project = repository.create_project("缓存测试", &profile()).unwrap();
    let project_input = repository
        .add_input(project.id, input("/tmp/cache.mp4", 0))
        .unwrap();
    let profile_hash = profile().fingerprint().unwrap();
    let artifact = repository
        .create_artifact(NewAsrArtifact {
            source_fingerprint: project_input.source_fingerprint.clone(),
            recognition_profile_hash: profile_hash.clone(),
            engine_id: "whisper.cpp".to_owned(),
            engine_version: "v1.9.1".to_owned(),
            model_id: "whisper-small-multilingual-q5_1".to_owned(),
            model_version: "small-q5_1@5359861".to_owned(),
        })
        .unwrap();
    assert_eq!(artifact.status, AiArtifactStatus::Pending);
    assert!(
        repository
            .find_published_artifact(&project_input.source_fingerprint, &profile_hash)
            .unwrap()
            .is_none()
    );

    let segments = repository
        .publish_artifact(
            artifact.id,
            4_000,
            Some("zh"),
            &[
                TranscriptSegmentDraft {
                    source_start_ms: 100,
                    source_end_ms: 1_500,
                    raw_text: " 欢迎来到直播间 ".to_owned(),
                    normalized_text: "欢迎来到直播间。".to_owned(),
                    confidence: None,
                },
                TranscriptSegmentDraft {
                    source_start_ms: 1_500,
                    source_end_ms: 3_900,
                    raw_text: "九十九元".to_owned(),
                    normalized_text: "九十九元。".to_owned(),
                    confidence: Some(0.82),
                },
            ],
        )
        .unwrap();
    assert_eq!(segments.len(), 2);
    assert!(segments[0].id.starts_with("seg_"));
    repository
        .attach_artifact(project_input.id, artifact.id)
        .unwrap();
    let hit = repository
        .find_published_artifact(&project_input.source_fingerprint, &profile_hash)
        .unwrap()
        .expect("published artifact is reusable");
    assert_eq!(hit.id, artifact.id);
    assert_eq!(
        repository
            .list_segments_for_input(project_input.id)
            .unwrap(),
        segments
    );
}

#[test]
fn recovery_delete_and_artifact_cleanup_do_not_touch_source_media() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("原始 视频.mp4");
    std::fs::write(&source, b"immutable media").unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let repository = AiRepository::new(database);
    let project = repository.create_project("恢复测试", &profile()).unwrap();
    let project_input = repository
        .add_input(project.id, input(source.to_str().unwrap(), 0))
        .unwrap();
    repository.freeze_project(project.id).unwrap();
    repository
        .transition_project(project.id, AiProjectStatus::Running)
        .unwrap();
    repository
        .transition_input(project_input.id, AiInputStatus::Validating, None)
        .unwrap();
    repository
        .transition_input(project_input.id, AiInputStatus::PreparingAudio, None)
        .unwrap();
    repository
        .transition_input(project_input.id, AiInputStatus::DetectingSpeech, None)
        .unwrap();
    repository
        .transition_input(project_input.id, AiInputStatus::Transcribing, None)
        .unwrap();

    let recovered = repository.recover_interrupted().unwrap();
    assert_eq!(recovered.projects, 1);
    assert_eq!(recovered.inputs, 1);
    assert_eq!(
        repository.get_project(project.id).unwrap().project.status,
        AiProjectStatus::Queued
    );

    repository.delete_project(project.id).unwrap();
    assert!(source.is_file());
    assert_eq!(std::fs::read(&source).unwrap(), b"immutable media");
    assert!(repository.list_projects().unwrap().is_empty());
}

#[test]
fn video_deletion_invalidates_then_cleans_artifacts_without_deleting_media_or_models() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("library-source.mkv");
    std::fs::write(&source, b"original video bytes").unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer::room("测试主播", "900", "room-900", false))
        .unwrap();
    let session = database
        .start_session(streamer.id, directory.path().to_str().unwrap())
        .unwrap();
    database
        .add_video(&NewVideo {
            session_id: session.id,
            path: source.to_string_lossy().into_owned(),
            started_at: None,
            ended_at: None,
            duration_seconds: Some(4),
            size_bytes: 20,
            audio_present: Some(true),
            status: "complete".to_owned(),
        })
        .unwrap();
    let video = database
        .get_video(
            database
                .list_session_videos(session.id)
                .unwrap()
                .first()
                .unwrap()
                .id,
        )
        .unwrap();
    let repository = AiRepository::new(database.clone());
    let project = repository.create_project("视频库缓存", &profile()).unwrap();
    let source_fingerprint = SourceFingerprint {
        normalized_path: source.to_string_lossy().into_owned(),
        size_bytes: 20,
        modified_at_ms: 1_700_000_000_000,
        video_id: Some(video.id),
    };
    let project_input = repository
        .add_input(
            project.id,
            NewAiProjectInput {
                position: 0,
                source_kind: AiInputSourceKind::VideoLibrary,
                video_id: Some(video.id),
                display_name: "library-source.mkv".to_owned(),
                source_path: source.to_string_lossy().into_owned(),
                source_fingerprint: source_fingerprint.clone(),
                duration_ms: Some(4_000),
                audio_present: Some(true),
            },
        )
        .unwrap();
    let profile_hash = profile().fingerprint().unwrap();
    let artifact = repository
        .create_artifact(NewAsrArtifact {
            source_fingerprint: source_fingerprint.clone(),
            recognition_profile_hash: profile_hash.clone(),
            engine_id: "whisper.cpp".to_owned(),
            engine_version: "v1.9.1".to_owned(),
            model_id: "whisper-small-multilingual-q5_1".to_owned(),
            model_version: "small-q5_1@5359861".to_owned(),
        })
        .unwrap();
    repository
        .publish_artifact(
            artifact.id,
            4_000,
            Some("zh"),
            &[TranscriptSegmentDraft {
                source_start_ms: 100,
                source_end_ms: 3_900,
                raw_text: "原始文本".to_owned(),
                normalized_text: "原始文本。".to_owned(),
                confidence: None,
            }],
        )
        .unwrap();
    repository
        .attach_artifact(project_input.id, artifact.id)
        .unwrap();

    assert_eq!(
        repository.invalidate_artifacts_for_video(video.id).unwrap(),
        1
    );
    assert!(
        repository
            .find_published_artifact(&source_fingerprint, &profile_hash)
            .unwrap()
            .is_none()
    );
    database.delete_video_record(video.id).unwrap();
    repository.delete_project(project.id).unwrap();
    assert_eq!(
        repository
            .cleanup_unreferenced_artifacts("9999-01-01T00:00:00Z")
            .unwrap(),
        1
    );
    assert_eq!(std::fs::read(&source).unwrap(), b"original video bytes");
    assert!(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../resources/asr/manifest.json")
            .is_file()
    );
}

#[test]
fn highlight_run_is_authorized_snapshot_and_candidate_selection_is_atomic() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let repository = AiRepository::new(database);
    let project = repository.create_project("高光候选", &profile()).unwrap();
    let project_input = repository
        .add_input(project.id, input("/tmp/highlight.mp4", 0))
        .unwrap();
    repository
        .set_project_context(
            project.id,
            &["带货".to_owned(), "搞笑".to_owned()],
            Some("重点找出反转和卖点"),
        )
        .unwrap();
    repository.freeze_project(project.id).unwrap();
    repository
        .transition_project(project.id, AiProjectStatus::Running)
        .unwrap();
    repository
        .transition_input(project_input.id, AiInputStatus::Validating, None)
        .unwrap();
    repository
        .transition_input(project_input.id, AiInputStatus::Completed, None)
        .unwrap();
    repository.recompute_project_progress(project.id).unwrap();
    let run = repository
        .create_highlight_run(NewAiHighlightRun {
            project_id: project.id,
            model_id: "deepseek-chat".to_owned(),
            prompt_version: "highlight-v1".to_owned(),
            tags_snapshot: vec!["带货".to_owned()],
            skills_snapshot: vec![
                "generic-hook@1.0.0".to_owned(),
                "ecommerce-conversion@1.0.0".to_owned(),
            ],
            analysis_goal: Some("重点找出反转和卖点".to_owned()),
            analysis_fingerprint: "fingerprint-1".to_owned(),
            qualified_score: 70,
            excellent_score: 80,
            total_segments: 2,
            total_chars: 20,
            estimated_batches: 1,
            user_authorized: true,
        })
        .unwrap();
    assert_eq!(run.tags_snapshot, vec!["带货"]);
    assert_eq!(
        repository
            .latest_highlight_run_for_project(project.id)
            .unwrap()
            .unwrap()
            .id,
        run.id
    );
    let unauthorized = repository.create_highlight_run(NewAiHighlightRun {
        analysis_fingerprint: "fingerprint-2".to_owned(),
        user_authorized: false,
        ..NewAiHighlightRun {
            project_id: project.id,
            model_id: "deepseek-chat".to_owned(),
            prompt_version: "highlight-v1".to_owned(),
            tags_snapshot: Vec::new(),
            skills_snapshot: vec!["generic-hook@1.0.0".to_owned()],
            analysis_goal: None,
            analysis_fingerprint: "fingerprint-2".to_owned(),
            qualified_score: 70,
            excellent_score: 80,
            total_segments: 0,
            total_chars: 0,
            estimated_batches: 0,
            user_authorized: false,
        }
    });
    assert!(unauthorized.is_err());
    let chunks = repository
        .add_highlight_chunks(
            run.id,
            &[NewAiHighlightChunk {
                ordinal: 0,
                input_id: project_input.id,
                segment_ids: vec!["seg_1".to_owned()],
                context_segment_ids: Vec::new(),
            }],
        )
        .unwrap();
    assert_eq!(chunks.len(), 1);
    assert!(
        repository
            .list_highlight_candidates(run.id)
            .unwrap()
            .is_empty()
    );
    let persisted_draft = HighlightCandidateDraft {
        candidate_key: "candidate-1".to_owned(),
        title: "价格反转".to_owned(),
        input_id: project_input.id,
        segment_ids: vec!["seg_1".to_owned()],
        start_ms: 0,
        end_ms: 15_000,
        hook_score: 82.0,
        information_score: 78.0,
        emotion_score: 75.0,
        tag_relevance_score: 92.0,
        completeness_score: 80.0,
        shareability_score: 84.0,
        reason: "包含明确卖点和反转".to_owned(),
        matched_tags: vec!["带货".to_owned()],
    };
    repository
        .mark_highlight_chunk_running(run.id, chunks[0].id)
        .unwrap();
    repository
        .complete_highlight_chunk(
            run.id,
            chunks[0].id,
            std::slice::from_ref(&persisted_draft),
            7,
        )
        .unwrap();
    let progress = repository.highlight_progress(run.id).unwrap();
    assert_eq!(progress.total_batches, 1);
    assert_eq!(progress.completed_batches, 1);
    assert_eq!(progress.failed_batches, 0);
    assert_eq!(progress.candidate_count, 1);
    let restored = repository.list_completed_highlight_drafts(run.id).unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].1, vec![persisted_draft.clone()]);
    let persisted_score = HighlightCandidateScore {
        candidate_key: "candidate-1".to_owned(),
        total_score: 82.0,
        hook_score: 82.0,
        information_score: 78.0,
        emotion_score: 75.0,
        tag_relevance_score: 92.0,
        completeness_score: 80.0,
        shareability_score: 84.0,
        rank: 1,
        reason: "带货标签相关".to_owned(),
    };

    let candidates = repository
        .publish_highlight_results(
            run.id,
            chunks[0].id,
            std::slice::from_ref(&persisted_draft),
            std::slice::from_ref(&persisted_score),
            12,
        )
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert!(!candidates[0].selected);
    let selected = repository
        .select_highlight_candidates(run.id, &[candidates[0].id])
        .unwrap();
    assert!(selected[0].selected);
    assert!(repository.get_or_create_clip_project(run.id).is_err());
    let reranked = repository
        .replace_highlight_results(
            run.id,
            &[(chunks[0].id, persisted_draft.clone())],
            std::slice::from_ref(&persisted_score),
        )
        .unwrap();
    assert!(reranked[0].selected);
    let invalid = repository.select_highlight_candidates(run.id, &[999_999]);
    assert!(invalid.is_err());
    assert!(
        repository
            .list_highlight_candidates(run.id)
            .unwrap()
            .iter()
            .any(|candidate| candidate.selected)
    );
    let newer = repository
        .create_highlight_run(NewAiHighlightRun {
            project_id: project.id,
            model_id: "deepseek-chat".to_owned(),
            prompt_version: "highlight-v1".to_owned(),
            tags_snapshot: Vec::new(),
            skills_snapshot: vec!["generic-hook@1.0.0".to_owned()],
            analysis_goal: None,
            analysis_fingerprint: "fingerprint-latest".to_owned(),
            qualified_score: 70,
            excellent_score: 80,
            total_segments: 2,
            total_chars: 20,
            estimated_batches: 1,
            user_authorized: true,
        })
        .unwrap();
    assert_eq!(
        repository
            .latest_highlight_run_for_project(project.id)
            .unwrap()
            .unwrap()
            .id,
        newer.id
    );

    let newer_chunks = repository
        .add_highlight_chunks(
            newer.id,
            &[NewAiHighlightChunk {
                ordinal: 0,
                input_id: project_input.id,
                segment_ids: vec!["seg_1".to_owned()],
                context_segment_ids: Vec::new(),
            }],
        )
        .unwrap();
    let excellent_draft = HighlightCandidateDraft {
        candidate_key: "candidate-excellent".to_owned(),
        title: "优秀片段".to_owned(),
        ..persisted_draft.clone()
    };
    let qualified_draft = HighlightCandidateDraft {
        candidate_key: "candidate-qualified".to_owned(),
        title: "合格片段".to_owned(),
        ..persisted_draft.clone()
    };
    let excellent_score = HighlightCandidateScore {
        candidate_key: excellent_draft.candidate_key.clone(),
        total_score: 89.0,
        ..persisted_score.clone()
    };
    let qualified_score = HighlightCandidateScore {
        candidate_key: qualified_draft.candidate_key.clone(),
        total_score: 72.0,
        rank: 2,
        ..persisted_score.clone()
    };
    let mut ranked_drafts = vec![excellent_draft.clone(), qualified_draft.clone()];
    let mut ranked_scores = vec![excellent_score.clone(), qualified_score.clone()];
    for index in 0_u64..10 {
        let candidate_key = format!("candidate-extra-{index}");
        ranked_drafts.push(HighlightCandidateDraft {
            candidate_key: candidate_key.clone(),
            title: format!("额外合格片段 {index}"),
            start_ms: 20_000 + index * 20_000,
            end_ms: 35_000 + index * 20_000,
            ..persisted_draft.clone()
        });
        ranked_scores.push(HighlightCandidateScore {
            candidate_key,
            total_score: if index == 9 { 69.0 } else { 71.0 },
            rank: (index + 3) as u32,
            ..persisted_score.clone()
        });
    }
    let ranked_sources = ranked_drafts
        .iter()
        .cloned()
        .map(|draft| (newer_chunks[0].id, draft))
        .collect::<Vec<_>>();
    let first_publish = repository
        .replace_highlight_results(newer.id, &ranked_sources, &ranked_scores)
        .unwrap();
    assert_eq!(first_publish.len(), 12);
    assert!(
        repository
            .list_highlight_candidates(newer.id)
            .unwrap()
            .iter()
            .any(|candidate| candidate.total_score == 69.0)
    );
    assert!(
        first_publish
            .iter()
            .any(|candidate| candidate.total_score == 89.0 && candidate.selected)
    );
    assert!(
        first_publish
            .iter()
            .any(|candidate| candidate.total_score == 72.0 && !candidate.selected)
    );
    let page = repository
        .list_qualified_highlight_candidates(newer.id, 0, 1)
        .unwrap();
    assert_eq!(page.total_candidates, 12);
    assert_eq!(page.qualified_candidates, 11);
    assert_eq!(page.selected_candidates, 1);
    assert_eq!(page.items.len(), 1);
    assert_eq!(
        repository
            .list_qualified_highlight_candidates(newer.id, 1, 5)
            .unwrap()
            .items
            .len(),
        5
    );

    let upgraded_scores = ranked_scores
        .iter()
        .cloned()
        .map(|mut score| {
            if score.candidate_key == qualified_draft.candidate_key {
                score.total_score = 85.0;
            }
            score
        })
        .collect::<Vec<_>>();
    let upgraded = repository
        .replace_highlight_results(newer.id, &ranked_sources, &upgraded_scores)
        .unwrap();
    let upgraded_candidate = upgraded
        .iter()
        .find(|candidate| candidate.candidate_key == qualified_draft.candidate_key)
        .unwrap();
    assert!(upgraded_candidate.selected);

    repository
        .set_highlight_candidate_selected(newer.id, upgraded_candidate.id, false)
        .unwrap();
    let republished = repository
        .replace_highlight_results(newer.id, &ranked_sources, &upgraded_scores)
        .unwrap();
    assert!(
        !republished
            .iter()
            .find(|candidate| candidate.candidate_key == qualified_draft.candidate_key)
            .unwrap()
            .selected
    );

    let qualified_candidate = republished
        .iter()
        .find(|candidate| candidate.candidate_key == qualified_draft.candidate_key)
        .unwrap();
    repository
        .set_highlight_candidate_selected(newer.id, qualified_candidate.id, true)
        .unwrap();
    let selected_page = repository
        .list_selected_highlight_candidates(newer.id, 0, 50)
        .unwrap();
    assert_eq!(selected_page.selected_candidates, 2);
    assert_eq!(selected_page.items.len(), 2);

    repository
        .update_highlight_run_status(newer.id, AiHighlightRunStatus::Completed, None)
        .unwrap();

    let clip = repository.get_or_create_clip_project(newer.id).unwrap();
    assert_eq!(clip.segments.len(), 2);
    assert_eq!(clip.segments[0].position, 0);
    assert_eq!(clip.segments[1].position, 1);
    let reopened = repository.get_or_create_clip_project(newer.id).unwrap();
    assert_eq!(reopened.project.id, clip.project.id);
    assert_eq!(reopened.segments.len(), 2);
    assert_eq!(reopened.project.version, 1);
    assert!(
        repository
            .insert_clip_candidate(
                clip.project.id,
                clip.segments[0].candidate_id,
                clip.segments.len() as u32,
            )
            .is_err()
    );
    let unselected_candidate = republished
        .iter()
        .find(|candidate| !candidate.selected)
        .unwrap();
    assert!(
        repository
            .insert_clip_candidate(clip.project.id, unselected_candidate.id, 0)
            .is_err()
    );
    let output_dimensions = repository
        .set_clip_output_dimensions(clip.project.id, 1920, 1080)
        .unwrap();
    assert_eq!(output_dimensions.output_width, Some(1920));
    assert_eq!(output_dimensions.output_height, Some(1080));

    let updated = repository
        .update_clip_segment(
            clip.project.id,
            clip.segments[0].id,
            &AiClipSegmentUpdate {
                volume_percent: 135,
                effect: AiClipEffect::FadeInOut,
            },
        )
        .unwrap();
    assert_eq!(updated.segments[0].volume_percent, 135);
    assert_eq!(updated.segments[0].effect, AiClipEffect::FadeInOut);
    assert_eq!(updated.project.version, 2);
    assert!(
        repository
            .update_clip_segment(
                clip.project.id,
                clip.segments[0].id,
                &AiClipSegmentUpdate {
                    volume_percent: 201,
                    effect: AiClipEffect::None,
                },
            )
            .is_err()
    );

    let reversed_ids = vec![updated.segments[1].id, updated.segments[0].id];
    let reordered = repository
        .reorder_clip_segments(clip.project.id, &reversed_ids)
        .unwrap();
    assert_eq!(
        reordered
            .segments
            .iter()
            .map(|segment| segment.id)
            .collect::<Vec<_>>(),
        reversed_ids
    );
    let after_remove = repository
        .remove_clip_segment(clip.project.id, reordered.segments[0].id)
        .unwrap();
    assert_eq!(after_remove.segments.len(), 1);
    let restored = repository
        .insert_clip_candidate(clip.project.id, reordered.segments[0].candidate_id, 1)
        .unwrap();
    assert_eq!(restored.segments.len(), 2);
    assert_eq!(
        restored.segments[1].candidate_id,
        reordered.segments[0].candidate_id
    );
    assert_eq!(restored.segments[1].position, 1);
    assert!(
        repository
            .insert_clip_candidate(clip.project.id, 999_999, 0)
            .is_err()
    );
    assert!(
        repository
            .insert_clip_candidate(clip.project.id, restored.segments[0].candidate_id, 99)
            .is_err()
    );
    let after_restored_remove = repository
        .remove_clip_segment(clip.project.id, restored.segments[1].id)
        .unwrap();
    assert!(
        repository
            .remove_clip_segment(clip.project.id, after_restored_remove.segments[0].id)
            .is_err()
    );
    assert_eq!(
        repository
            .get_clip_project(clip.project.id)
            .unwrap()
            .segments
            .len(),
        1
    );
    repository
        .set_clip_export_state(
            clip.project.id,
            AiClipExportStatus::Exporting,
            42,
            None,
            None,
        )
        .unwrap();
    assert_eq!(repository.recover_interrupted_clip_exports().unwrap(), 1);
    let restored_export = repository
        .get_clip_project(clip.project.id)
        .unwrap()
        .project;
    assert_eq!(restored_export.export_status, AiClipExportStatus::Cancelled);
    assert_eq!(
        restored_export.last_error_code.as_deref(),
        Some("app_restarted")
    );
}

#[test]
fn stale_scheduler_events_are_ignored_after_generation_changes() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let repository = AiRepository::new(database);
    let project = repository.create_project("代次隔离", &profile()).unwrap();
    let project_input = repository
        .add_input(project.id, input("/tmp/generation.mp4", 0))
        .unwrap();
    assert!(
        !repository
            .accept_scheduler_event(project_input.id, 2, AiInputStatus::Validating, None,)
            .unwrap()
    );
    assert_eq!(
        repository.get_input(project_input.id).unwrap().status,
        AiInputStatus::Pending
    );
    repository
        .update_scheduler_state(project_input.id, 2, 0, Some(1))
        .unwrap();
    assert!(
        !repository
            .accept_scheduler_event(project_input.id, 1, AiInputStatus::Validating, None,)
            .unwrap()
    );
    assert!(
        repository
            .accept_scheduler_event(project_input.id, 2, AiInputStatus::Validating, None,)
            .unwrap()
    );
    assert_eq!(
        repository.get_input(project_input.id).unwrap().status,
        AiInputStatus::Validating
    );
}
