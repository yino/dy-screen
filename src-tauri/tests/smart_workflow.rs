use async_trait::async_trait;
use dy_screen::asr::{AsrError, EngineResult, FrozenMediaSource, MediaInspection, MediaInspector};
use dy_screen_app_lib::ai::{
    AiCommandError, AiEnvironmentDiagnostic, AiInputSourceKind, AiInputStatus, AiJobController,
    AiPreflight, AiProjectService, AiProjectStatus, AiRepository, AiSmartBatchStatus,
    AiSmartClipSourceInput, AiSmartDraftOwnership, AiSmartStage, AiSmartStageAttemptStatus,
    AiSmartWorkflowMode, AiSmartWorkflowStatus, CandidateAgentOutput, CandidateAgentRequest,
    ClipTextCorrectionWorkflow, CredentialStore, HighlightAgentProvider, HighlightCandidateDraft,
    HighlightCandidateScore, HighlightWorkflow, LlmError, MemoryCredentialStore,
    NewAiHighlightChunk, NewAiHighlightRun, NewAiProjectInput, NewAiSmartCandidate,
    NewAiSmartCandidateSource, NewAiSmartWorkflow, NewAiSmartWorkflowBatch, NewAsrArtifact,
    PreflightReport, ProviderDiagnostic, RankingAgentOutput, RankingAgentRequest,
    RecognitionProfile, SmartClippingWorkflow, SmartStageAttemptStart, SmartWorkflowConfiguration,
    SmartWorkflowGate, SmartWorkflowPublisher, SmartWorkflowRepository, SourceFingerprint,
    SubtitleCorrectionItem, SubtitleCorrectionOutput, SubtitleCorrectionProvider,
    SubtitleCorrectionRequest, TranscriptSegmentDraft, TransitionAgentOutput,
    TransitionAgentProvider, TransitionAgentRequest, TransitionScoreOutput, TransitionScoreRequest,
    TrustedLocalFile,
};
use dy_screen_app_lib::ai::{AiEnvironmentCheckView, AiSmartWorkflowEvent};
use dy_screen_app_lib::database::Database;
use dy_screen_app_lib::domain::{NewStreamer, NewVideo};
use dy_screen_app_lib::transition_assets::{
    MaterialAssetRegistry, TransitionMaterialAssetService, TransitionMaterialCache,
};
use dy_screen_app_lib::transition_matching::TransitionMatchingWorkflow;
use dy_screen_app_lib::transition_materials::TransitionMaterialRepository;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn profile() -> RecognitionProfile {
    RecognitionProfile {
        engine_id: "test-asr".to_owned(),
        engine_version: "1".to_owned(),
        model_id: "test-model".to_owned(),
        model_version: "1".to_owned(),
        language_hint: Some("zh".to_owned()),
        vad_model_id: "test-vad".to_owned(),
        vad_threshold_millis: 500,
        vad_padding_ms: 500,
        timestamp_policy: "segment".to_owned(),
        normalization_version: "test-normalizer".to_owned(),
        hotwords: Vec::new(),
    }
}

fn completed_highlight(repository: &AiRepository, label: &str, position: i64) -> (i64, i64, i64) {
    let project = repository.create_project(label, &profile()).unwrap();
    let fingerprint = SourceFingerprint {
        normalized_path: format!("/tmp/{label}.mp4"),
        size_bytes: 10_000 + position as u64,
        modified_at_ms: 1_700_000_000_000 + position,
        video_id: None,
    };
    let input = repository
        .add_input(
            project.id,
            NewAiProjectInput {
                position: 0,
                source_kind: AiInputSourceKind::LocalFile,
                video_id: None,
                display_name: format!("{label}.mp4"),
                source_path: fingerprint.normalized_path.clone(),
                source_fingerprint: fingerprint.clone(),
                duration_ms: Some(30_000),
                audio_present: Some(true),
            },
        )
        .unwrap();
    let artifact = repository
        .create_artifact(NewAsrArtifact {
            source_fingerprint: fingerprint,
            recognition_profile_hash: profile().fingerprint().unwrap(),
            engine_id: "test-asr".to_owned(),
            engine_version: "1".to_owned(),
            model_id: "test-model".to_owned(),
            model_version: "1".to_owned(),
        })
        .unwrap();
    repository
        .publish_artifact(
            artifact.id,
            30_000,
            Some("zh"),
            &[TranscriptSegmentDraft {
                source_start_ms: 1_000,
                source_end_ms: 20_000,
                raw_text: format!("{label} 原始字幕"),
                normalized_text: format!("{label} 原始字幕。"),
                confidence: Some(0.9),
            }],
        )
        .unwrap();
    repository.attach_artifact(input.id, artifact.id).unwrap();
    repository.freeze_project(project.id).unwrap();
    repository
        .transition_project(project.id, AiProjectStatus::Running)
        .unwrap();
    repository
        .transition_input(input.id, AiInputStatus::Validating, None)
        .unwrap();
    repository
        .transition_input(input.id, AiInputStatus::Completed, None)
        .unwrap();
    repository.recompute_project_progress(project.id).unwrap();
    let run = repository
        .create_highlight_run(NewAiHighlightRun {
            project_id: project.id,
            model_id: "deepseek-chat".to_owned(),
            prompt_version: "test-prompt".to_owned(),
            tags_snapshot: vec![label.to_owned()],
            skills_snapshot: vec!["generic-hook@1".to_owned()],
            analysis_goal: None,
            analysis_fingerprint: format!("analysis-{label}"),
            qualified_score: 70,
            excellent_score: 80,
            total_segments: 1,
            total_chars: 10,
            estimated_batches: 1,
            user_authorized: true,
        })
        .unwrap();
    let chunk = repository
        .add_highlight_chunks(
            run.id,
            &[NewAiHighlightChunk {
                ordinal: 0,
                input_id: input.id,
                segment_ids: vec![format!("segment-{label}")],
                context_segment_ids: Vec::new(),
            }],
        )
        .unwrap()
        .remove(0);
    let draft = HighlightCandidateDraft {
        candidate_key: format!("candidate-{label}"),
        title: format!("{label} 高光"),
        input_id: input.id,
        segment_ids: vec![format!("segment-{label}")],
        start_ms: 1_000,
        end_ms: 20_000,
        hook_score: 90.0,
        information_score: 88.0,
        emotion_score: 85.0,
        tag_relevance_score: 90.0,
        completeness_score: 90.0,
        shareability_score: 88.0,
        reason: "测试高光".to_owned(),
        matched_tags: vec![label.to_owned()],
    };
    let score = HighlightCandidateScore {
        candidate_key: draft.candidate_key.clone(),
        total_score: 90.0,
        hook_score: 90.0,
        information_score: 88.0,
        emotion_score: 85.0,
        tag_relevance_score: 90.0,
        completeness_score: 90.0,
        shareability_score: 88.0,
        rank: 1,
        reason: "测试高光".to_owned(),
    };
    let candidate = repository
        .replace_highlight_results(run.id, &[(chunk.id, draft)], &[score])
        .unwrap()
        .remove(0);
    repository
        .update_highlight_run_status(
            run.id,
            dy_screen_app_lib::ai::AiHighlightRunStatus::Completed,
            None,
        )
        .unwrap();
    assert!(candidate.selected);
    (project.id, run.id, candidate.id)
}

fn local_workflow(
    repository: &SmartWorkflowRepository,
) -> dy_screen_app_lib::ai::AiSmartWorkflowDetail {
    repository
        .create(&NewAiSmartWorkflow {
            name: "本地智能成片".to_owned(),
            mode: AiSmartWorkflowMode::Local,
            source_session_id: None,
            source_summary: "2 个本地视频".to_owned(),
            provider: "deepseek".to_owned(),
            model_id: "deepseek-chat".to_owned(),
            text_scope: "selected_clip_subtitles".to_owned(),
            configuration_fingerprint: "config-v1".to_owned(),
        })
        .unwrap()
}

async fn wait_for_smart_workflow(
    workflow: &SmartClippingWorkflow,
    workflow_id: i64,
    predicate: impl Fn(&dy_screen_app_lib::ai::AiSmartWorkflowDetail) -> bool,
) -> dy_screen_app_lib::ai::AiSmartWorkflowDetail {
    let mut last = None;
    for _ in 0..300 {
        let detail = workflow.get(workflow_id).unwrap();
        if predicate(&detail) {
            return detail;
        }
        last = Some(detail);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("智能任务未在预期时间内收敛：{last:#?}")
}

struct SmartInspector;

#[async_trait]
impl MediaInspector for SmartInspector {
    async fn inspect(
        &self,
        _source: &FrozenMediaSource,
        cancellation: CancellationToken,
    ) -> EngineResult<MediaInspection> {
        if cancellation.is_cancelled() {
            return Err(AsrError::cancelled());
        }
        Ok(MediaInspection {
            duration_ms: 30_000,
            audio_present: true,
            audio_codec: Some("aac".to_owned()),
            audio_sample_rate_hz: Some(48_000),
            audio_channels: Some(2),
        })
    }
}

struct SmartPreflight;

#[async_trait]
impl AiPreflight for SmartPreflight {
    async fn check(&self) -> Result<PreflightReport, AsrError> {
        Ok(PreflightReport {
            ready: true,
            engine_id: "test-asr".to_owned(),
            engine_version: "1".to_owned(),
            model_id: "test-model".to_owned(),
            model_version: "1".to_owned(),
            platform_supported: true,
            sidecars_ready: true,
            models_ready: true,
            memory_ready: true,
            disk_ready: true,
            message: "测试资源就绪".to_owned(),
        })
    }
}

struct CompletingController {
    repository: AiRepository,
    enqueue_count: AtomicUsize,
    cancel_count: AtomicUsize,
}

#[async_trait]
impl AiJobController for CompletingController {
    fn default_profile(&self) -> Result<RecognitionProfile, AiCommandError> {
        Ok(profile())
    }

    async fn enqueue_project(&self, project_id: i64) -> Result<(), AiCommandError> {
        self.enqueue_count.fetch_add(1, Ordering::SeqCst);
        let detail = self.repository.get_project(project_id).unwrap();
        self.repository
            .transition_project(project_id, AiProjectStatus::Running)
            .unwrap();
        for input in detail.inputs {
            let artifact = self
                .repository
                .create_artifact(NewAsrArtifact {
                    source_fingerprint: input.source_fingerprint,
                    recognition_profile_hash: detail.project.recognition_profile_hash.clone(),
                    engine_id: "test-asr".to_owned(),
                    engine_version: "1".to_owned(),
                    model_id: "test-model".to_owned(),
                    model_version: "1".to_owned(),
                })
                .unwrap();
            self.repository
                .publish_artifact(
                    artifact.id,
                    30_000,
                    Some("zh"),
                    &[TranscriptSegmentDraft {
                        source_start_ms: 1_000,
                        source_end_ms: 21_000,
                        raw_text: "这是一段足够长的测试高光字幕".to_owned(),
                        normalized_text: "这是一段足够长的测试高光字幕。".to_owned(),
                        confidence: Some(0.95),
                    }],
                )
                .unwrap();
            self.repository
                .attach_artifact(input.id, artifact.id)
                .unwrap();
            self.repository
                .transition_input(input.id, AiInputStatus::Validating, None)
                .unwrap();
            self.repository
                .transition_input(input.id, AiInputStatus::Completed, None)
                .unwrap();
        }
        self.repository
            .recompute_project_progress(project_id)
            .unwrap();
        Ok(())
    }

    async fn cancel_project(&self, project_id: i64) -> Result<(), AiCommandError> {
        self.cancel_count.fetch_add(1, Ordering::SeqCst);
        let _ = self.repository.cancel_project(project_id);
        Ok(())
    }

    async fn retry_input(&self, _input_id: i64) -> Result<(), AiCommandError> {
        Ok(())
    }

    async fn diagnose(&self) -> Result<AiEnvironmentDiagnostic, AiCommandError> {
        Ok(AiEnvironmentDiagnostic {
            ready: true,
            platform: "test".to_owned(),
            engine_id: "test-asr".to_owned(),
            engine_version: "1".to_owned(),
            model_id: "test-model".to_owned(),
            model_version: "1".to_owned(),
            checks: vec![AiEnvironmentCheckView {
                code: "test".to_owned(),
                passed: true,
                message: "测试资源就绪".to_owned(),
            }],
            message: "测试资源就绪".to_owned(),
            runtime: None,
        })
    }
}

struct ReadyGate;

impl SmartWorkflowGate for ReadyGate {
    fn activation_ready(&self) -> bool {
        true
    }

    fn provider_ready(&self) -> bool {
        true
    }
}

#[derive(Default)]
struct EventCollector(Mutex<Vec<AiSmartWorkflowEvent>>);

impl SmartWorkflowPublisher for EventCollector {
    fn publish(&self, event: &AiSmartWorkflowEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

struct SmartHighlightProvider;

#[async_trait]
impl HighlightAgentProvider for SmartHighlightProvider {
    async fn diagnose(
        &self,
        _settings: &dy_screen_app_lib::ai::LlmProviderSettings,
        _api_key: &str,
        _cancellation: CancellationToken,
    ) -> Result<ProviderDiagnostic, LlmError> {
        Ok(ProviderDiagnostic {
            ok: true,
            category: "ok".to_owned(),
            message: "连接成功".to_owned(),
        })
    }

    async fn discover_candidates(
        &self,
        _settings: &dy_screen_app_lib::ai::LlmProviderSettings,
        _api_key: &str,
        request: CandidateAgentRequest,
        _cancellation: CancellationToken,
    ) -> Result<CandidateAgentOutput, LlmError> {
        let payload: serde_json::Value = serde_json::from_str(&request.prompt).unwrap();
        let segment = payload["segments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|segment| segment["readOnlyContext"] == false)
            .unwrap();
        Ok(CandidateAgentOutput {
            candidates: vec![HighlightCandidateDraft {
                candidate_key: "smart-highlight".to_owned(),
                title: "智能高光".to_owned(),
                input_id: segment["inputId"].as_i64().unwrap(),
                segment_ids: vec![segment["id"].as_str().unwrap().to_owned()],
                start_ms: segment["startMs"].as_u64().unwrap(),
                end_ms: segment["endMs"].as_u64().unwrap(),
                hook_score: 90.0,
                information_score: 90.0,
                emotion_score: 90.0,
                tag_relevance_score: 90.0,
                completeness_score: 90.0,
                shareability_score: 90.0,
                reason: "测试高光".to_owned(),
                matched_tags: Vec::new(),
            }],
            token_usage: 1,
        })
    }

    async fn rank_candidates(
        &self,
        _settings: &dy_screen_app_lib::ai::LlmProviderSettings,
        _api_key: &str,
        request: RankingAgentRequest,
        _cancellation: CancellationToken,
    ) -> Result<RankingAgentOutput, LlmError> {
        let payload: serde_json::Value = serde_json::from_str(&request.prompt).unwrap();
        let key = payload["candidates"][0]["candidateKey"]
            .as_str()
            .unwrap()
            .to_owned();
        Ok(RankingAgentOutput {
            scores: vec![HighlightCandidateScore {
                candidate_key: key,
                total_score: 90.0,
                hook_score: 90.0,
                information_score: 90.0,
                emotion_score: 90.0,
                tag_relevance_score: 90.0,
                completeness_score: 90.0,
                shareability_score: 90.0,
                rank: 1,
                reason: "测试高光".to_owned(),
            }],
            token_usage: 1,
        })
    }
}

struct EchoCorrectionProvider;

#[async_trait]
impl SubtitleCorrectionProvider for EchoCorrectionProvider {
    async fn correct_subtitles(
        &self,
        _settings: &dy_screen_app_lib::ai::LlmProviderSettings,
        _api_key: &str,
        request: SubtitleCorrectionRequest,
        _cancellation: CancellationToken,
    ) -> Result<SubtitleCorrectionOutput, LlmError> {
        let texts: Vec<String> =
            serde_json::from_str(request.prompt.split("输入文本数组：").nth(1).unwrap()).unwrap();
        Ok(SubtitleCorrectionOutput {
            corrections: texts
                .into_iter()
                .enumerate()
                .map(|(index, text)| SubtitleCorrectionItem {
                    index: index as u32,
                    text,
                })
                .collect(),
            token_usage: 1,
        })
    }
}

#[derive(Default)]
struct FailOnceCorrectionProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl SubtitleCorrectionProvider for FailOnceCorrectionProvider {
    async fn correct_subtitles(
        &self,
        _settings: &dy_screen_app_lib::ai::LlmProviderSettings,
        _api_key: &str,
        request: SubtitleCorrectionRequest,
        _cancellation: CancellationToken,
    ) -> Result<SubtitleCorrectionOutput, LlmError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(LlmError::Temporary);
        }
        let texts: Vec<String> =
            serde_json::from_str(request.prompt.split("输入文本数组：").nth(1).unwrap()).unwrap();
        Ok(SubtitleCorrectionOutput {
            corrections: texts
                .into_iter()
                .enumerate()
                .map(|(index, text)| SubtitleCorrectionItem {
                    index: index as u32,
                    text,
                })
                .collect(),
            token_usage: 1,
        })
    }
}

struct EmptyTransitionProvider;

#[async_trait]
impl TransitionAgentProvider for EmptyTransitionProvider {
    async fn match_transitions(
        &self,
        _settings: &dy_screen_app_lib::ai::LlmProviderSettings,
        _api_key: &str,
        _request: TransitionAgentRequest,
        _cancellation: CancellationToken,
    ) -> Result<TransitionAgentOutput, LlmError> {
        Ok(TransitionAgentOutput {
            matches: Vec::new(),
            token_usage: 0,
        })
    }

    async fn score_transitions(
        &self,
        _settings: &dy_screen_app_lib::ai::LlmProviderSettings,
        _api_key: &str,
        _request: TransitionScoreRequest,
        _cancellation: CancellationToken,
    ) -> Result<TransitionScoreOutput, LlmError> {
        Ok(TransitionScoreOutput {
            scores: Vec::new(),
            token_usage: 0,
        })
    }
}

#[test]
fn workflow_state_batch_attempt_and_recovery_are_generation_safe() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let workflows = SmartWorkflowRepository::new(database);
    let created = local_workflow(&workflows);
    assert_eq!(created.workflow.status, AiSmartWorkflowStatus::Draft);
    assert!(
        workflows
            .transition(
                created.workflow.id,
                created.workflow.generation,
                AiSmartWorkflowStatus::ReviewReady,
                AiSmartStage::Review,
                None,
            )
            .is_err()
    );
    let authorized = workflows
        .authorize(created.workflow.id, 1, "authorization-v1", "config-v1")
        .unwrap();
    assert_eq!(authorized.workflow.status, AiSmartWorkflowStatus::Queued);
    let batch = workflows
        .add_batch(
            created.workflow.id,
            &NewAiSmartWorkflowBatch {
                video_id: None,
                source_fingerprint: "source-a".to_owned(),
                finalized_at: "2026-08-14T00:00:00Z".to_owned(),
            },
        )
        .unwrap();
    let repeated = workflows
        .add_batch(
            created.workflow.id,
            &NewAiSmartWorkflowBatch {
                video_id: None,
                source_fingerprint: "source-a".to_owned(),
                finalized_at: "2026-08-14T00:00:00Z".to_owned(),
            },
        )
        .unwrap();
    assert_eq!(batch.id, repeated.id);
    let first = workflows
        .start_attempt(&SmartStageAttemptStart {
            workflow_id: created.workflow.id,
            batch_id: Some(batch.id),
            draft_generation: None,
            stage: AiSmartStage::Asr,
            input_fingerprint: "asr-v1".to_owned(),
        })
        .unwrap();
    workflows
        .finish_attempt(
            first.id,
            first.attempt_generation,
            AiSmartStageAttemptStatus::Completed,
            Some(("project", 7)),
            123,
            None,
        )
        .unwrap();
    let cached = workflows
        .start_attempt(&SmartStageAttemptStart {
            workflow_id: created.workflow.id,
            batch_id: Some(batch.id),
            draft_generation: None,
            stage: AiSmartStage::Asr,
            input_fingerprint: "asr-v1".to_owned(),
        })
        .unwrap();
    assert_eq!(cached.id, first.id);
    assert_eq!(cached.status, AiSmartStageAttemptStatus::Completed);
    let running = workflows
        .start_attempt(&SmartStageAttemptStart {
            workflow_id: created.workflow.id,
            batch_id: Some(batch.id),
            draft_generation: None,
            stage: AiSmartStage::Highlight,
            input_fingerprint: "highlight-v1".to_owned(),
        })
        .unwrap();
    assert_eq!(workflows.recover_interrupted().unwrap(), 1);
    let recovered = workflows.get(created.workflow.id).unwrap();
    assert_eq!(recovered.workflow.status, AiSmartWorkflowStatus::Paused);
    assert!(recovered.workflow.generation > authorized.workflow.generation);
    assert_eq!(
        recovered
            .attempts
            .iter()
            .find(|attempt| attempt.id == running.id)
            .unwrap()
            .status,
        AiSmartStageAttemptStatus::Interrupted
    );
    assert!(
        workflows
            .finish_attempt(
                running.id,
                running.attempt_generation,
                AiSmartStageAttemptStatus::Completed,
                None,
                1,
                None,
            )
            .is_err()
    );
}

#[test]
fn live_batches_follow_source_time_and_candidate_ledger_keeps_continuous_ranges() {
    let directory = tempdir().unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer::room(
            "时间序主播",
            "99101",
            "room-99101",
            true,
        ))
        .unwrap();
    let session = database
        .start_session(streamer.id, directory.path().to_str().unwrap())
        .unwrap();
    database.mark_session_recording(session.id).unwrap();
    let workflows = SmartWorkflowRepository::new(database.clone());
    let workflow = workflows
        .create(&NewAiSmartWorkflow {
            name: "直播时间序".to_owned(),
            mode: AiSmartWorkflowMode::Live,
            source_session_id: Some(session.id),
            source_summary: "受信直播".to_owned(),
            provider: "deepseek".to_owned(),
            model_id: "deepseek-chat".to_owned(),
            text_scope: "selected_clip_subtitles".to_owned(),
            configuration_fingerprint: "live-config".to_owned(),
        })
        .unwrap();
    workflows
        .authorize(workflow.workflow.id, 1, "live-auth", "live-config")
        .unwrap();

    let session_started_at = chrono::DateTime::parse_from_rfc3339(&session.started_at).unwrap();
    let mut videos = Vec::new();
    for minute in [1_i64, 2, 3] {
        let path = directory.path().join(format!("segment-{minute}.mp4"));
        std::fs::write(&path, b"complete segment").unwrap();
        let started_at = session_started_at + chrono::Duration::minutes(minute);
        videos.push(
            database
                .add_video(&NewVideo {
                    session_id: session.id,
                    path: path.to_string_lossy().into_owned(),
                    started_at: Some(started_at.to_rfc3339()),
                    ended_at: Some((started_at + chrono::Duration::seconds(30)).to_rfc3339()),
                    duration_seconds: Some(30),
                    size_bytes: 16,
                    audio_present: Some(true),
                    status: "complete".to_owned(),
                })
                .unwrap(),
        );
    }
    for index in [1_usize, 0, 2] {
        let video = database.get_video(videos[index]).unwrap();
        workflows
            .add_batch(
                workflow.workflow.id,
                &NewAiSmartWorkflowBatch {
                    video_id: Some(video.id),
                    source_fingerprint: format!("source-{}", video.id),
                    finalized_at: video.ended_at.unwrap(),
                },
            )
            .unwrap();
    }
    let chronological_batches = workflows.get(workflow.workflow.id).unwrap().batches;
    assert_eq!(
        chronological_batches
            .iter()
            .map(|batch| batch.video_id.unwrap())
            .collect::<Vec<_>>(),
        videos
    );
    assert_eq!(
        chronological_batches
            .iter()
            .map(|batch| batch.position)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );

    let repository = AiRepository::new(database);
    let (project_a, run_a, candidate_a) = completed_highlight(&repository, "边界前段", 10);
    let (project_b, run_b, candidate_b) = completed_highlight(&repository, "边界后段", 11);
    let (project_overlap, run_overlap, candidate_overlap) =
        completed_highlight(&repository, "边界重复", 12);
    let candidate_a_input = repository.list_highlight_candidates(run_a).unwrap()[0].input_id;
    let candidate_b_input = repository.list_highlight_candidates(run_b).unwrap()[0].input_id;
    let candidate_overlap_input =
        repository.list_highlight_candidates(run_overlap).unwrap()[0].input_id;
    for (batch, (project, run)) in chronological_batches.iter().zip([
        (project_a, run_a),
        (project_b, run_b),
        (project_overlap, run_overlap),
    ]) {
        workflows
            .attach_batch_project(batch.id, project, Some(run), AiSmartBatchStatus::Completed)
            .unwrap();
    }
    let source = |batch_id, candidate_id, input_id, start, end| NewAiSmartCandidateSource {
        batch_id,
        candidate_id,
        video_id: None,
        input_id,
        stable_segment_ids: vec![format!("segment-{candidate_id}")],
        source_start_ms: 1_000,
        source_end_ms: 20_000,
        session_start_ms: start,
        session_end_ms: end,
    };
    let candidates = [
        (candidate_a, candidate_a_input, 90, 61_000, 80_000),
        (candidate_b, candidate_b_input, 89, 81_000, 100_000),
        (
            candidate_overlap,
            candidate_overlap_input,
            88,
            62_000,
            79_000,
        ),
    ];
    for (index, (candidate_id, input_id, score, start, end)) in candidates.into_iter().enumerate() {
        workflows
            .upsert_candidate(
                workflow.workflow.id,
                &NewAiSmartCandidate {
                    dedupe_key: "continuous-highlight".to_owned(),
                    semantic_fingerprint: "same-topic".to_owned(),
                    canonical_candidate_id: candidate_id,
                    total_score: score,
                    qualified: true,
                    selected: true,
                    session_start_ms: start,
                    session_end_ms: end,
                    first_finalized_at: format!("2026-08-14T00:0{}:30Z", index + 1),
                    sources: vec![source(
                        chronological_batches[index].id,
                        candidate_id,
                        input_id,
                        start,
                        end,
                    )],
                },
            )
            .unwrap();
    }
    let (sources, candidate_ids) = workflows
        .selected_clip_inputs(workflow.workflow.id)
        .unwrap();
    assert_eq!(sources.len(), 2);
    assert_eq!(candidate_ids, vec![candidate_a, candidate_b]);
}

#[test]
fn multi_source_draft_rejects_foreign_candidates_and_freezes_user_edits_and_export() {
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let repository = AiRepository::new(database.clone());
    let workflows = SmartWorkflowRepository::new(database);
    let workflow = local_workflow(&workflows);
    workflows
        .authorize(workflow.workflow.id, 1, "authorization-v1", "config-v1")
        .unwrap();
    let (project_a, run_a, candidate_a) = completed_highlight(&repository, "来源 A", 0);
    let (project_b, run_b, candidate_b) = completed_highlight(&repository, "来源 B", 1);
    let (_, _, foreign_candidate) = completed_highlight(&repository, "外部来源", 2);
    let batch_a = workflows
        .add_batch(
            workflow.workflow.id,
            &NewAiSmartWorkflowBatch {
                video_id: None,
                source_fingerprint: "source-a".to_owned(),
                finalized_at: "2026-08-14T00:00:00Z".to_owned(),
            },
        )
        .unwrap();
    let batch_b = workflows
        .add_batch(
            workflow.workflow.id,
            &NewAiSmartWorkflowBatch {
                video_id: None,
                source_fingerprint: "source-b".to_owned(),
                finalized_at: "2026-08-14T00:01:00Z".to_owned(),
            },
        )
        .unwrap();
    workflows
        .attach_batch_project(
            batch_a.id,
            project_a,
            Some(run_a),
            AiSmartBatchStatus::Completed,
        )
        .unwrap();
    workflows
        .attach_batch_project(
            batch_b.id,
            project_b,
            Some(run_b),
            AiSmartBatchStatus::Completed,
        )
        .unwrap();
    assert!(
        repository
            .create_smart_clip_project(
                workflow.workflow.id,
                1,
                "非法合辑",
                &[AiSmartClipSourceInput {
                    highlight_run_id: run_a,
                    workflow_batch_id: batch_a.id,
                }],
                &[candidate_a, foreign_candidate],
            )
            .is_err()
    );
    let (detail, draft) = repository
        .create_smart_clip_project(
            workflow.workflow.id,
            1,
            "直播高光合辑",
            &[
                AiSmartClipSourceInput {
                    highlight_run_id: run_a,
                    workflow_batch_id: batch_a.id,
                },
                AiSmartClipSourceInput {
                    highlight_run_id: run_b,
                    workflow_batch_id: batch_b.id,
                },
            ],
            &[candidate_a, candidate_b],
        )
        .unwrap();
    assert_eq!(detail.segments.len(), 2);
    assert_eq!(
        repository
            .list_clip_project_sources(detail.project.id)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(draft.ownership, AiSmartDraftOwnership::Automation);
    assert_eq!(detail.project.ownership, AiSmartDraftOwnership::Automation);
    assert!(
        repository
            .append_smart_clip_candidates(
                detail.project.id,
                detail.project.version,
                &[foreign_candidate]
            )
            .is_err()
    );

    let edited = repository
        .update_clip_segment(
            detail.project.id,
            detail.segments[0].id,
            &dy_screen_app_lib::ai::AiClipSegmentUpdate {
                volume_percent: 90,
                effect: dy_screen_app_lib::ai::AiClipEffect::None,
            },
        )
        .unwrap();
    assert_eq!(edited.project.ownership, AiSmartDraftOwnership::User);
    assert_eq!(
        repository.get_smart_draft(draft.id).unwrap().ownership,
        AiSmartDraftOwnership::User
    );
    assert!(
        repository
            .append_smart_clip_candidates(edited.project.id, edited.project.version, &[candidate_a])
            .is_err()
    );
    let exporting = repository
        .begin_clip_export(edited.project.id, edited.project.version)
        .unwrap();
    assert_eq!(
        exporting.export_frozen_version,
        Some(edited.project.version)
    );
    let frozen = repository.get_smart_draft(draft.id).unwrap();
    assert_eq!(frozen.frozen_project_version, Some(edited.project.version));
    assert!(
        repository
            .update_clip_segment(
                edited.project.id,
                edited.segments[0].id,
                &dy_screen_app_lib::ai::AiClipSegmentUpdate {
                    volume_percent: 100,
                    effect: dy_screen_app_lib::ai::AiClipEffect::None,
                },
            )
            .is_err()
    );
}

#[tokio::test]
async fn local_and_live_smart_workflows_run_end_to_end() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("smart-source.mp4");
    std::fs::write(&source, b"trusted fake video").unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let repository = AiRepository::new(database.clone());
    let controller = Arc::new(CompletingController {
        repository: repository.clone(),
        enqueue_count: AtomicUsize::new(0),
        cancel_count: AtomicUsize::new(0),
    });
    let project_service = AiProjectService::new(
        database.clone(),
        Arc::new(SmartInspector),
        Arc::new(SmartPreflight),
    );
    let credentials: Arc<dyn CredentialStore> = Arc::new(MemoryCredentialStore::new());
    credentials.set("sk-test").unwrap();
    let highlight = Arc::new(HighlightWorkflow::new(
        repository.clone(),
        Arc::new(SmartHighlightProvider),
        credentials.clone(),
    ));
    let correction = ClipTextCorrectionWorkflow::new(
        repository.clone(),
        Arc::new(EchoCorrectionProvider),
        credentials.clone(),
    );
    let assets = TransitionMaterialAssetService::new(
        TransitionMaterialRepository::new(database.clone()),
        TransitionMaterialCache::new(directory.path().join("transition-cache")).unwrap(),
        CancellationToken::new(),
        MaterialAssetRegistry::default(),
    )
    .unwrap();
    let transition = TransitionMatchingWorkflow::new(
        TransitionMaterialRepository::new(database.clone()),
        repository.clone(),
        Arc::new(EmptyTransitionProvider),
        credentials,
        assets,
    );
    let events = Arc::new(EventCollector::default());
    let workflow = SmartClippingWorkflow::new(
        database.clone(),
        project_service.clone(),
        controller.clone(),
        highlight,
        correction,
        transition,
        Arc::new(ReadyGate),
    )
    .with_publisher(events.clone());

    let created = workflow
        .create_local(
            SmartWorkflowConfiguration {
                name: "本地智能成片".to_owned(),
                provider: "deepseek".to_owned(),
                model_id: "deepseek-chat".to_owned(),
                text_scope: "selected_clip_subtitles".to_owned(),
                output_preference: "reviewable_compilation".to_owned(),
            },
            vec![TrustedLocalFile::new("trusted-grant", source)],
            true,
        )
        .await
        .unwrap();
    let workflow_id = created.workflow.id;
    let mut final_detail = None;
    for _ in 0..100 {
        let detail = workflow.get(workflow_id).unwrap();
        if matches!(
            detail.workflow.status,
            AiSmartWorkflowStatus::ReviewReady | AiSmartWorkflowStatus::Failed
        ) {
            final_detail = Some(detail);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let final_detail = final_detail.expect("智能任务应收敛到终态");
    assert_eq!(
        final_detail.workflow.status,
        AiSmartWorkflowStatus::ReviewReady,
        "{:?}",
        final_detail.workflow.last_error_message
    );
    assert_eq!(final_detail.workflow.stage, AiSmartStage::Review);
    assert_eq!(final_detail.workflow.candidate_count, 1);
    assert_eq!(final_detail.workflow.selected_count, 1);
    assert_eq!(final_detail.drafts.len(), 1);
    let clip = repository
        .get_clip_project(final_detail.drafts[0].clip_project_id)
        .unwrap();
    assert_eq!(clip.project.export_status.as_str(), "idle");
    assert_eq!(clip.project.ownership, AiSmartDraftOwnership::Automation);
    assert_eq!(final_detail.drafts[0].status.as_str(), "review_ready");
    assert!(final_detail.attempts.iter().any(|attempt| {
        attempt.stage == AiSmartStage::Transition
            && attempt.status == AiSmartStageAttemptStatus::Completed
            && attempt.last_error_code.as_deref() == Some("smart_transition_skipped")
    }));
    let published = events.0.lock().unwrap();
    assert!(
        published
            .windows(2)
            .all(|pair| pair[0].sequence <= pair[1].sequence)
    );
    assert!(
        published
            .iter()
            .all(|event| event.workflow_id == workflow_id)
    );
    drop(published);

    let manual = project_service
        .create_draft("普通项目", &profile())
        .unwrap();
    let manual_source = directory.path().join("manual-source.mp4");
    std::fs::write(&manual_source, b"manual fake video").unwrap();
    project_service
        .import_local_files(
            manual.id,
            vec![TrustedLocalFile::new("manual-grant", manual_source)],
            CancellationToken::new(),
        )
        .await
        .unwrap();
    project_service.start_analysis(manual.id).await.unwrap();
    controller.enqueue_project(manual.id).await.unwrap();
    assert!(
        repository
            .latest_highlight_run_for_project(manual.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(controller.enqueue_count.load(Ordering::SeqCst), 2);
    assert_eq!(controller.cancel_count.load(Ordering::SeqCst), 0);

    run_live_workflow_end_to_end().await;
}

#[tokio::test]
async fn live_batch_failure_does_not_stop_later_batches_and_retry_is_local() {
    let directory = tempdir().unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer::room(
            "失败恢复主播",
            "99002",
            "room-99002",
            true,
        ))
        .unwrap();
    let session = database
        .start_session(streamer.id, directory.path().to_str().unwrap())
        .unwrap();
    database.mark_session_recording(session.id).unwrap();
    let session_started_at = chrono::DateTime::parse_from_rfc3339(&session.started_at).unwrap();

    let repository = AiRepository::new(database.clone());
    let controller = Arc::new(CompletingController {
        repository: repository.clone(),
        enqueue_count: AtomicUsize::new(0),
        cancel_count: AtomicUsize::new(0),
    });
    let project_service = AiProjectService::new(
        database.clone(),
        Arc::new(SmartInspector),
        Arc::new(SmartPreflight),
    );
    let credentials: Arc<dyn CredentialStore> = Arc::new(MemoryCredentialStore::new());
    credentials.set("sk-test").unwrap();
    let highlight = Arc::new(HighlightWorkflow::new(
        repository.clone(),
        Arc::new(SmartHighlightProvider),
        credentials.clone(),
    ));
    let correction_provider = Arc::new(FailOnceCorrectionProvider::default());
    let correction = ClipTextCorrectionWorkflow::new(
        repository.clone(),
        correction_provider.clone(),
        credentials.clone(),
    );
    let assets = TransitionMaterialAssetService::new(
        TransitionMaterialRepository::new(database.clone()),
        TransitionMaterialCache::new(directory.path().join("failure-transition-cache")).unwrap(),
        CancellationToken::new(),
        MaterialAssetRegistry::default(),
    )
    .unwrap();
    let transition = TransitionMatchingWorkflow::new(
        TransitionMaterialRepository::new(database.clone()),
        repository.clone(),
        Arc::new(EmptyTransitionProvider),
        credentials,
        assets,
    );
    let workflow = SmartClippingWorkflow::new(
        database.clone(),
        project_service,
        controller.clone(),
        highlight,
        correction,
        transition,
        Arc::new(ReadyGate),
    );
    let created = workflow
        .create_live(
            SmartWorkflowConfiguration {
                name: "直播失败恢复".to_owned(),
                provider: "deepseek".to_owned(),
                model_id: "deepseek-chat".to_owned(),
                text_scope: "selected_clip_subtitles".to_owned(),
                output_preference: "reviewable_compilation".to_owned(),
            },
            session.id,
            true,
        )
        .await
        .unwrap();
    let workflow_id = created.workflow.id;

    let mut video_ids = Vec::new();
    for minute in [1_i64, 2] {
        let path = directory
            .path()
            .join(format!("failure-segment-{minute}.mp4"));
        std::fs::write(&path, format!("finalized segment {minute}")).unwrap();
        video_ids.push(
            database
                .add_video(&NewVideo {
                    session_id: session.id,
                    path: path.to_string_lossy().into_owned(),
                    started_at: Some(
                        (session_started_at + chrono::Duration::minutes(minute)).to_rfc3339(),
                    ),
                    ended_at: Some(
                        (session_started_at
                            + chrono::Duration::minutes(minute)
                            + chrono::Duration::seconds(30))
                        .to_rfc3339(),
                    ),
                    duration_seconds: Some(30),
                    size_bytes: 20 + minute,
                    audio_present: Some(true),
                    status: "complete".to_owned(),
                })
                .unwrap(),
        );
    }

    workflow.ingest_finalized_video(video_ids[0]).await.unwrap();
    let first_failure = wait_for_smart_workflow(&workflow, workflow_id, |detail| {
        detail.batches.len() == 1
            && detail.batches[0].status == AiSmartBatchStatus::Failed
            && detail.attempts.iter().any(|attempt| {
                attempt.batch_id == Some(detail.batches[0].id)
                    && attempt.stage == AiSmartStage::Correction
                    && attempt.status == AiSmartStageAttemptStatus::Failed
            })
    })
    .await;
    let failed_batch_id = first_failure.batches[0].id;

    workflow.ingest_finalized_video(video_ids[1]).await.unwrap();
    let continued = wait_for_smart_workflow(&workflow, workflow_id, |detail| {
        detail.batches.len() == 2
            && detail.batches.iter().any(|batch| {
                batch.id == failed_batch_id && batch.status == AiSmartBatchStatus::Failed
            })
            && detail.batches.iter().any(|batch| {
                batch.id != failed_batch_id && batch.status == AiSmartBatchStatus::Completed
            })
            && !detail.drafts.is_empty()
    })
    .await;
    assert_eq!(controller.enqueue_count.load(Ordering::SeqCst), 2);
    assert_eq!(controller.cancel_count.load(Ordering::SeqCst), 0);

    database
        .finish_session(session.id, "completed", None)
        .unwrap();
    workflow.finalize_live_session(session.id).await.unwrap();
    let ended_failed = wait_for_smart_workflow(&workflow, workflow_id, |detail| {
        detail.workflow.status == AiSmartWorkflowStatus::Failed
            && detail
                .batches
                .iter()
                .any(|batch| batch.status == AiSmartBatchStatus::Failed)
    })
    .await;
    assert_eq!(ended_failed.drafts.len(), continued.drafts.len());

    workflow
        .retry_stage(
            workflow_id,
            ended_failed.workflow.generation,
            AiSmartStage::Correction,
            Some(failed_batch_id),
        )
        .await
        .unwrap();
    let recovered = wait_for_smart_workflow(&workflow, workflow_id, |detail| {
        detail.workflow.status == AiSmartWorkflowStatus::Completed
            && detail
                .batches
                .iter()
                .all(|batch| batch.status == AiSmartBatchStatus::Completed)
    })
    .await;
    assert_eq!(recovered.batches.len(), 2);
    assert_eq!(controller.enqueue_count.load(Ordering::SeqCst), 2);
    assert_eq!(controller.cancel_count.load(Ordering::SeqCst), 0);
    assert_eq!(correction_provider.calls.load(Ordering::SeqCst), 3);
}

async fn run_live_workflow_end_to_end() {
    let directory = tempdir().unwrap();
    let database = Database::open_in_memory().unwrap();
    database.migrate().unwrap();
    let streamer = database
        .add_streamer(&NewStreamer::room(
            "直播测试主播",
            "99001",
            "room-99001",
            true,
        ))
        .unwrap();
    let session = database
        .start_session(streamer.id, directory.path().to_str().unwrap())
        .unwrap();
    database.mark_session_recording(session.id).unwrap();
    let session_started_at = chrono::DateTime::parse_from_rfc3339(&session.started_at).unwrap();
    let historical_path = directory.path().join("historical.mp4");
    std::fs::write(&historical_path, b"historical finalized segment").unwrap();
    let historical_video_id = database
        .add_video(&NewVideo {
            session_id: session.id,
            path: historical_path.to_string_lossy().into_owned(),
            started_at: Some(session_started_at.to_rfc3339()),
            ended_at: Some((session_started_at + chrono::Duration::seconds(30)).to_rfc3339()),
            duration_seconds: Some(30),
            size_bytes: 28,
            audio_present: Some(true),
            status: "complete".to_owned(),
        })
        .unwrap();

    let repository = AiRepository::new(database.clone());
    let controller = Arc::new(CompletingController {
        repository: repository.clone(),
        enqueue_count: AtomicUsize::new(0),
        cancel_count: AtomicUsize::new(0),
    });
    let project_service = AiProjectService::new(
        database.clone(),
        Arc::new(SmartInspector),
        Arc::new(SmartPreflight),
    );
    let credentials: Arc<dyn CredentialStore> = Arc::new(MemoryCredentialStore::new());
    credentials.set("sk-test").unwrap();
    let highlight = Arc::new(HighlightWorkflow::new(
        repository.clone(),
        Arc::new(SmartHighlightProvider),
        credentials.clone(),
    ));
    let correction = ClipTextCorrectionWorkflow::new(
        repository.clone(),
        Arc::new(EchoCorrectionProvider),
        credentials.clone(),
    );
    let assets = TransitionMaterialAssetService::new(
        TransitionMaterialRepository::new(database.clone()),
        TransitionMaterialCache::new(directory.path().join("live-transition-cache")).unwrap(),
        CancellationToken::new(),
        MaterialAssetRegistry::default(),
    )
    .unwrap();
    let transition = TransitionMatchingWorkflow::new(
        TransitionMaterialRepository::new(database.clone()),
        repository.clone(),
        Arc::new(EmptyTransitionProvider),
        credentials,
        assets,
    );
    let workflow = SmartClippingWorkflow::new(
        database.clone(),
        project_service,
        controller.clone(),
        highlight,
        correction,
        transition,
        Arc::new(ReadyGate),
    );
    let created = workflow
        .create_live(
            SmartWorkflowConfiguration {
                name: "直播智能成片".to_owned(),
                provider: "deepseek".to_owned(),
                model_id: "deepseek-chat".to_owned(),
                text_scope: "selected_clip_subtitles".to_owned(),
                output_preference: "reviewable_compilation".to_owned(),
            },
            session.id,
            true,
        )
        .await
        .unwrap();
    let workflow_id = created.workflow.id;
    assert_eq!(
        created.workflow.live_start_video_id,
        Some(historical_video_id)
    );
    workflow
        .ingest_finalized_video(historical_video_id)
        .await
        .unwrap();
    assert!(workflow.get(workflow_id).unwrap().batches.is_empty());

    let mut video_ids = Vec::new();
    for (index, minute) in [1_u8, 2, 3].into_iter().enumerate() {
        let path = directory.path().join(format!("segment-{minute}.mp4"));
        std::fs::write(&path, format!("finalized segment {minute}")).unwrap();
        video_ids.push(
            database
                .add_video(&NewVideo {
                    session_id: session.id,
                    path: path.to_string_lossy().into_owned(),
                    started_at: Some(
                        (session_started_at + chrono::Duration::minutes(i64::from(minute)))
                            .to_rfc3339(),
                    ),
                    ended_at: Some(
                        (session_started_at
                            + chrono::Duration::minutes(i64::from(minute))
                            + chrono::Duration::seconds(30))
                        .to_rfc3339(),
                    ),
                    duration_seconds: Some(30),
                    size_bytes: 20 + index as i64,
                    audio_present: Some(true),
                    status: "complete".to_owned(),
                })
                .unwrap(),
        );
    }

    workflow.ingest_finalized_video(video_ids[1]).await.unwrap();
    workflow.ingest_finalized_video(video_ids[0]).await.unwrap();
    workflow.ingest_finalized_video(video_ids[1]).await.unwrap();
    let first_draft = wait_for_smart_workflow(&workflow, workflow_id, |detail| {
        detail.batches.len() == 2
            && detail
                .batches
                .iter()
                .all(|batch| batch.status == AiSmartBatchStatus::Completed)
            && detail.drafts.len() == 1
            && detail.drafts[0].status.as_str() == "review_ready"
    })
    .await;
    assert_eq!(controller.enqueue_count.load(Ordering::SeqCst), 2);
    assert_eq!(
        first_draft.workflow.live_cursor_video_id,
        Some(video_ids[1])
    );
    let original_draft = first_draft.drafts[0].clone();
    let original_clip = repository
        .get_clip_project(original_draft.clip_project_id)
        .unwrap();
    assert_eq!(original_clip.segments.len(), 2);
    let user_clip = repository
        .update_clip_segment(
            original_clip.project.id,
            original_clip.segments[0].id,
            &dy_screen_app_lib::ai::AiClipSegmentUpdate {
                volume_percent: 90,
                effect: dy_screen_app_lib::ai::AiClipEffect::None,
            },
        )
        .unwrap();
    assert_eq!(user_clip.project.ownership, AiSmartDraftOwnership::User);

    workflow.ingest_finalized_video(video_ids[2]).await.unwrap();
    let next_draft = wait_for_smart_workflow(&workflow, workflow_id, |detail| {
        detail.batches.len() == 3
            && detail
                .batches
                .iter()
                .all(|batch| batch.status == AiSmartBatchStatus::Completed)
            && detail.drafts.len() == 2
            && detail.drafts[1].status.as_str() == "review_ready"
    })
    .await;
    assert_eq!(next_draft.drafts[0].ownership, AiSmartDraftOwnership::User);
    assert_eq!(next_draft.drafts[1].generation, 2);
    assert_eq!(
        repository
            .get_clip_project(next_draft.drafts[0].clip_project_id)
            .unwrap()
            .project
            .version,
        user_clip.project.version
    );
    assert_eq!(
        repository
            .get_clip_project(next_draft.drafts[1].clip_project_id)
            .unwrap()
            .segments
            .len(),
        3
    );

    database
        .finish_session(session.id, "completed", None)
        .unwrap();
    workflow.finalize_live_session(session.id).await.unwrap();
    let completed = wait_for_smart_workflow(&workflow, workflow_id, |detail| {
        detail.workflow.status == AiSmartWorkflowStatus::Completed
    })
    .await;
    assert_eq!(completed.drafts.len(), 2);
    assert_eq!(controller.cancel_count.load(Ordering::SeqCst), 0);
}
