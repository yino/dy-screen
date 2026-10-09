//! 生成式动画链路的端到端集成骨架（**固定样本**，不产生任何付费调用）。
//!
//! 这个测试把 spec 里的零件按真实顺序串起来：
//!
//! ```text
//! 时长适配 → 目标范围计算 → 复用键 → 写入固定样本资产 → 导出源 → 导出计划
//! ```
//!
//! 真实链路里"参考图/视频"来自直连万相；这里用**本地固定样本文件**代替，因此
//! 整个测试不发起任何上游请求、也不消耗额度，但可以验证零件之间的接口一致。

use std::path::PathBuf;

use dy_screen_app_lib::ai::{
    AiClipEffect, AiGeneratedAnimationComposition, AiGeneratedAnimationInsertBinding,
    AiGeneratedAnimationReplaceRange, AiGeneratedAnimationStatus, AiHighlightRunStatus,
    AiInputSourceKind, AiInputStatus, AiProjectStatus, AiRepository, ClipOutputDimensions,
    GENERATED_ANIMATION_CACHE_DIR, GeneratedAnimationReuseInput, GeneratedDurationAdaptation,
    NewAiGeneratedAnimationAsset, NewAiHighlightChunk, NewAiHighlightRun, NewAiProjectInput,
    NewAsrArtifact, RecognitionProfile, ReplaceSourceRange, ReplaceTargetRange, SourceFingerprint,
    TranscriptSegmentDraft, adapt_generated_duration, build_export_plan,
    generated_animation_file_name, generated_animation_reuse_key, resolve_replace_target_range,
};
use dy_screen_app_lib::database::Database;
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
        hotwords: Vec::new(),
        provider: None,
        chunk_strategy: None,
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
        duration_ms: Some(60_000),
        audio_present: Some(true),
    }
}

#[test]
fn fixed_sample_generated_animation_builds_a_complete_export_plan() {
    // ---------- 零件层：时长适配（决策四） ----------
    // 生成 5.0s、目标 4.9s：差异 2% ≤ 5% 且生成不短于目标 → 裁剪。
    assert_eq!(
        adapt_generated_duration(5_000, 4_900),
        GeneratedDurationAdaptation::Trim
    );
    // 生成 4.9s、目标 5.0s：差异 2%，放慢 1.020x 落在受限范围内 → 受限变速。
    assert_eq!(
        adapt_generated_duration(4_900, 5_000),
        GeneratedDurationAdaptation::SlowDown {
            slowdown_permille: 1_020
        }
    );
    // 差异 10%：判定失败，MUST NOT 补帧凑时长。
    assert!(matches!(
        adapt_generated_duration(4_500, 5_000),
        GeneratedDurationAdaptation::Rejected("over_tolerance")
    ));

    // ---------- 零件层：目标范围（决策一） ----------
    let ranges = [ReplaceSourceRange {
        start_ms: 1_000,
        end_ms: 20_000,
    }];
    let ReplaceTargetRange::Matched(range) = resolve_replace_target_range(&ranges, 5_000) else {
        panic!("区间存在时必须命中");
    };
    // 区间比生成画面长：取等于生成时长的一段，绝不越界。
    assert_eq!(range.start_ms, 1_000);
    assert_eq!(range.end_ms, 6_000);
    // 两个短区间相加够长，但不得跨片段拼接。
    let cross = [
        ReplaceSourceRange {
            start_ms: 0,
            end_ms: 3_000,
        },
        ReplaceSourceRange {
            start_ms: 10_000,
            end_ms: 13_000,
        },
    ];
    assert!(matches!(
        resolve_replace_target_range(&cross, 5_000),
        ReplaceTargetRange::Unmatched("no_source_range_within_tolerance")
    ));

    // ---------- 零件层：复用键 ----------
    let reuse = GeneratedAnimationReuseInput {
        storyboard: "一个人拆快递",
        image_prompt: "特写：双手拆快递箱",
        video_prompt: "镜头缓慢推近，纸箱被打开",
        target_duration_ms: 5_000,
        spec_fingerprint: "wanx-768x512@30",
    };
    let key = generated_animation_reuse_key(&reuse);
    assert_eq!(key, generated_animation_reuse_key(&reuse));
    let changed = GeneratedAnimationReuseInput {
        target_duration_ms: 4_900,
        ..reuse
    };
    assert_ne!(key, generated_animation_reuse_key(&changed));

    // ---------- 整机层：固定样本 → 导出计划 ----------
    let directory = tempdir().unwrap();
    let database = Database::open(&directory.path().join("pipeline.sqlite3")).unwrap();
    database.migrate().unwrap();
    let repository = AiRepository::new(database);
    let project = repository
        .create_project("生成动画骨架", &profile())
        .unwrap();
    let project_input = repository
        .add_input(project.id, input("/tmp/pipeline.mp4", 0))
        .unwrap();
    let artifact = repository
        .create_artifact(NewAsrArtifact {
            source_fingerprint: project_input.source_fingerprint.clone(),
            recognition_profile_hash: profile().fingerprint().unwrap(),
            engine_id: "whisper.cpp".to_owned(),
            engine_version: "v1.9.1".to_owned(),
            model_id: "whisper-small-multilingual-q5_1".to_owned(),
            model_version: "small-q5_1@5359861".to_owned(),
            provider: "local".to_owned(),
            fallback_reason: None,
        })
        .unwrap();
    repository
        .publish_artifact(
            artifact.id,
            60_000,
            Some("zh"),
            &[
                TranscriptSegmentDraft {
                    source_start_ms: 1_000,
                    source_end_ms: 29_000,
                    raw_text: "第一段".to_owned(),
                    normalized_text: "第一段。".to_owned(),
                    confidence: Some(0.9),
                },
                TranscriptSegmentDraft {
                    source_start_ms: 30_000,
                    source_end_ms: 59_000,
                    raw_text: "第二段".to_owned(),
                    normalized_text: "第二段。".to_owned(),
                    confidence: Some(0.9),
                },
            ],
        )
        .unwrap();
    repository
        .attach_artifact(project_input.id, artifact.id)
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
            model_id: "qwen-plus".to_owned(),
            prompt_version: "highlight-v1".to_owned(),
            tags_snapshot: Vec::new(),
            skills_snapshot: vec!["generic-hook@1.0.0".to_owned()],
            analysis_goal: None,
            analysis_fingerprint: "pipeline".to_owned(),
            qualified_score: 70,
            excellent_score: 80,
            total_segments: 2,
            total_chars: 8,
            estimated_batches: 2,
            user_authorized: true,
        })
        .unwrap();
    let chunks = repository
        .add_highlight_chunks(
            run.id,
            &[
                NewAiHighlightChunk {
                    ordinal: 0,
                    input_id: project_input.id,
                    segment_ids: vec!["seg_1".to_owned()],
                    context_segment_ids: Vec::new(),
                },
                NewAiHighlightChunk {
                    ordinal: 1,
                    input_id: project_input.id,
                    segment_ids: vec!["seg_2".to_owned()],
                    context_segment_ids: Vec::new(),
                },
            ],
        )
        .unwrap();
    let draft = |key: &str, segment_id: &str, start_ms: u64| dy_screen_app_lib::ai::HighlightCandidateDraft {
        candidate_key: key.to_owned(),
        title: key.to_owned(),
        input_id: project_input.id,
        segment_ids: vec![segment_id.to_owned()],
        start_ms,
        end_ms: start_ms + 28_000,
        hook_score: 82.0,
        information_score: 78.0,
        emotion_score: 75.0,
        tag_relevance_score: 92.0,
        completeness_score: 80.0,
        shareability_score: 84.0,
        reason: "骨架".to_owned(),
        matched_tags: Vec::new(),
    };
    let score = |key: &str| dy_screen_app_lib::ai::HighlightCandidateScore {
        candidate_key: key.to_owned(),
        total_score: 82.0,
        hook_score: 82.0,
        information_score: 78.0,
        emotion_score: 75.0,
        tag_relevance_score: 92.0,
        completeness_score: 80.0,
        shareability_score: 84.0,
        rank: 1,
        reason: "骨架".to_owned(),
    };
    let mut published = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        let key = format!("candidate-{index}");
        let segment_id = format!("seg_{}", index + 1);
        let start_ms = (index as u64) * 30_000;
        repository
            .mark_highlight_chunk_running(run.id, chunk.id)
            .unwrap();
        repository
            .complete_highlight_chunk(run.id, chunk.id, &[draft(&key, &segment_id, start_ms)], 7)
            .unwrap();
        published = repository
            .publish_highlight_results(
                run.id,
                chunk.id,
                &[draft(&key, &segment_id, start_ms)],
                &[score(&key)],
                12,
            )
            .unwrap();
    }
    repository
        .select_highlight_candidates(run.id, &[published[0].id, published[1].id])
        .unwrap();
    repository
        .update_highlight_run_status(run.id, AiHighlightRunStatus::Completed, None)
        .unwrap();
    let clip = repository.get_or_create_clip_project(run.id).unwrap();
    assert_eq!(clip.segments.len(), 2);

    // 固定样本落盘：真实链路里这些文件来自受控下载，这里直接写本地样本。
    let cache_root = directory.path().join("app-data");
    let cache_dir = cache_root.join(GENERATED_ANIMATION_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    // `replace`：绑定第一个片段内的 5s 源区间。
    let replace_asset = repository
        .record_generated_animation_asset(NewAiGeneratedAnimationAsset {
            project_id: project.id,
            candidate_id: published[0].id,
            highlight_run_id: Some(run.id),
            generation: 1,
            reuse_key: key.clone(),
            composition: AiGeneratedAnimationComposition::Replace,
            status: AiGeneratedAnimationStatus::Validated,
            replace_range: Some(AiGeneratedAnimationReplaceRange {
                start_ms: 5_000,
                end_ms: 10_000,
            }),
            insert_binding: None,
            upstream_task_id: Some("task-replace".to_owned()),
            sha256: Some("c".repeat(64)),
            width: Some(768),
            height: Some(512),
            fps: Some(30),
            duration_ms: Some(5_000),
            rejection_code: None,
        })
        .unwrap();
    std::fs::write(
        cache_dir.join(generated_animation_file_name(replace_asset.id)),
        b"fixed replace sample",
    )
    .unwrap();

    // `insert`：绑定第一段与第二段之间的边界。
    let insert_asset = repository
        .record_generated_animation_asset(NewAiGeneratedAnimationAsset {
            project_id: project.id,
            candidate_id: published[1].id,
            highlight_run_id: Some(run.id),
            generation: 1,
            reuse_key: format!("{key}-insert"),
            composition: AiGeneratedAnimationComposition::Insert,
            status: AiGeneratedAnimationStatus::Validated,
            replace_range: None,
            insert_binding: Some(AiGeneratedAnimationInsertBinding {
                left_segment_id: clip.segments[0].id,
                right_segment_id: clip.segments[1].id,
                boundary_id: 1,
            }),
            upstream_task_id: Some("task-insert".to_owned()),
            sha256: Some("d".repeat(64)),
            width: Some(768),
            height: Some(512),
            fps: Some(30),
            duration_ms: Some(1_500),
            rejection_code: None,
        })
        .unwrap();
    std::fs::write(
        cache_dir.join(generated_animation_file_name(insert_asset.id)),
        b"fixed insert sample",
    )
    .unwrap();

    // ---------- 导出源：两种合成方式都被接线 ----------
    let sources = repository
        .clip_export_sources(clip.project.id, &cache_root)
        .unwrap();
    let replace = sources[0].replace.as_ref().expect("第一个片段必须带 replace");
    // 片段内的相对时间：源内 5_000–10_000 减去片段起点 0。
    assert_eq!(replace.start_ms, 5_000);
    assert_eq!(replace.end_ms, 10_000);
    let insert = sources[0]
        .insert_after
        .as_ref()
        .expect("第一个片段后必须带 insert");
    assert_eq!(insert.duration_ms, 1_500);

    // ---------- 导出计划：FFmpeg 参数真的带上了两种合成 ----------
    let plan = build_export_plan(
        &sources,
        directory.path().join("out.part.mp4"),
        ClipOutputDimensions {
            width: 1080,
            height: 1920,
        },
        &directory.path().join("subtitles.ffconcat"),
        "libx264",
    )
    .expect("固定样本资产必须能构建导出计划");
    let joined = plan.arguments.join(" ");
    // `replace`：按时间范围叠加生成画面。
    assert!(joined.contains("enable='between(t,5.000, 10.000)'"));
    assert!(joined.contains(&generated_animation_file_name(replace_asset.id)));
    // `insert`：独立单元 + 48 kHz 双声道静音补齐。
    assert!(joined.contains(&generated_animation_file_name(insert_asset.id)));
    assert!(joined.contains("anullsrc=r=48000:cl=stereo"));
    // 工程总时长 = 28s + 28s + 1.5s 插入单元（replace 不改变时长）。
    assert_eq!(plan.total_duration_ms, 28_000 + 28_000 + 1_500);
    // 片段效果枚举仍然只来自内置白名单。
    assert!(sources.iter().all(|source| source.segment.effect == AiClipEffect::None));
}
