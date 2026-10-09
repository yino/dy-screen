//! 特效字幕增强在智能成片编排中的位置与逐条降级（步骤 3 任务组 11）。
//!
//! 三条不可退让的语义：
//!
//! 1. **不阻塞基础草稿**：增强阶段排在转场匹配之后、评审之前，但它是**非阻塞**的——
//!    基础草稿在增强完成前就可以审阅与导出，首稿时效不以增强完成为准（11.1 / 11.5）。
//! 2. **逐条降级**：某条高光失败只影响该条（保留普通原视频与基础字幕，并记录唯一最终
//!    增强方式与原因），其他高光与草稿状态不变（11.3）。
//! 3. **不覆盖人工编辑**：增强结果经 `record_enhancement` 的来源指纹判定，过期代次只保存为
//!    历史建议（11.2），也不改动候选采用决策、阈值与"必须用户显式确认导出"的语义（11.4）。

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::clip_subtitle::{
    PublishedSubtitleRange, TranscriptEffectWindowSource, derive_effect_window_from_transcript,
    transcript_effect_window_sources,
};
use super::domain::{
    AiEnhancementKind, AiEnhancementStatus, AiHighlightCandidate, AiSmartStage,
    NewAiHighlightEnhancement,
};
use super::effect_caption_render::EffectCaptionRuntimeError;
use super::repository::AiRepository;

/// 增强阶段在编排中的位置契约（任务 11.1 / 11.5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnhancementStagePlan {
    pub stage: AiSmartStage,
    pub previous_stage: AiSmartStage,
    pub following_stage: AiSmartStage,
    /// 恒为 `false`：基础草稿可先行审阅/导出，增强不参与首稿时效判定。
    pub blocking: bool,
}

/// 获取增强阶段的编排位置。
pub fn enhancement_stage_plan() -> EnhancementStagePlan {
    EnhancementStagePlan {
        stage: AiSmartStage::Enhancement,
        previous_stage: AiSmartStage::Transition,
        following_stage: AiSmartStage::Review,
        blocking: false,
    }
}

/// 缺陷防御：阶段顺序必须与 `AiSmartStage::ALL` 一致。
pub fn assert_enhancement_stage_position() -> Result<(), EffectCaptionRuntimeError> {
    let plan = enhancement_stage_plan();
    let all = AiSmartStage::ALL;
    let position = all
        .iter()
        .position(|stage| *stage == plan.stage)
        .ok_or_else(|| {
            EffectCaptionRuntimeError::new("enhancement_stage_missing", "阶段枚举缺少增强阶段")
        })?;
    if position == 0
        || position + 1 >= all.len()
        || all[position - 1] != plan.previous_stage
        || all[position + 1] != plan.following_stage
    {
        return Err(EffectCaptionRuntimeError::new(
            "enhancement_stage_order_invalid",
            "增强阶段必须紧跟在转场匹配之后、评审之前",
        ));
    }
    Ok(())
}

/// 一条待增强的高光。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnhancementHighlight {
    pub candidate_id: i64,
    /// 生成该建议时依据的工程已发布内容指纹。
    pub source_fingerprint: String,
    /// 由工程已发布句段推导出的时间窗；`None` 表示该高光没有可用句段。
    pub window: Option<(u64, u64)>,
}

/// 增强产物：一条高光渲染成功的透明帧序列与组合版本。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnhancementProduct {
    pub composition_version: String,
    pub composition_fingerprint: String,
    pub frames_dir: String,
    pub emphasis_words: Vec<String>,
}

/// 单条高光的增强执行者。真实实现走受限渲染子进程（组 4）；测试用替身。
#[async_trait]
pub trait EffectCaptionEnhancementRunner: Send + Sync {
    async fn enhance(
        &self,
        highlight: &EnhancementHighlight,
        cancellation: &CancellationToken,
    ) -> Result<EnhancementProduct, EffectCaptionRuntimeError>;
}

/// 逐条增强阶段的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnhancementStageSummary {
    pub applied: usize,
    pub failed: usize,
    pub skipped: usize,
    /// 每条高光的最终结果：`(candidate_id, 结果码)`，仅用于诊断，不含正文。
    pub outcomes: Vec<(i64, &'static str)>,
}

impl EnhancementStageSummary {
    pub fn total(&self) -> usize {
        self.applied + self.failed + self.skipped
    }
}

/// 由候选与**片段级转写窗口**构造待增强列表。
///
/// **片段级前置**：时间窗来自项目转写句段（`TranscriptEffectWindowSource`），
/// 而不是合辑字幕（`PublishedSubtitleRange`）。这样特效生成**不依赖合辑工程**，
/// 可以在建立合辑之前运行——这正是此前 `derive_effect_window_from_transcript`
/// 与 `transcript_effect_window_sources` 写完却**没有任何生产调用点**的原因。
///
/// 语义与合辑版一致（优先项目时间轴坐标，缺失时退回源内坐标），因此切换不会
/// 改变单个高光的窗口。
pub fn enhancement_highlights(
    candidates: &[AiHighlightCandidate],
    transcript_sources: &[TranscriptEffectWindowSource],
    source_fingerprint: &str,
) -> Vec<EnhancementHighlight> {
    candidates
        .iter()
        .map(|candidate| {
            let window = derive_effect_window_from_transcript(
                transcript_sources,
                &candidate.segment_ids,
            )
            .map(|range| (range.start_ms, range.end_ms));
            EnhancementHighlight {
                candidate_id: candidate.id,
                source_fingerprint: source_fingerprint.to_owned(),
                window,
            }
        })
        .collect()
}

/// 执行增强阶段（任务 11.2 / 11.3 / 11.4）。
///
/// 关键语义：
/// - 逐条独立执行，**任何一条失败都不会中断其余高光**，也不会改动草稿状态；
/// - 成功的记为 `applied`，失败的记为 `rejected` 并保存可区分的原因码（最终增强方式唯一）；
/// - 没有可用句段（窗口为空）的高光记为 `skipped`，保留基础字幕；
/// - 陈旧代次（来源指纹与当前不一致）由仓库层强制降级为历史建议，不覆盖人工编辑。
pub async fn run_enhancement_stage(
    repository: &AiRepository,
    runner: &dyn EffectCaptionEnhancementRunner,
    highlights: &[EnhancementHighlight],
    project_id: i64,
    current_source_fingerprint: &str,
    cancellation: &CancellationToken,
) -> Result<EnhancementStageSummary, EffectCaptionRuntimeError> {
    let mut summary = EnhancementStageSummary {
        applied: 0,
        failed: 0,
        skipped: 0,
        outcomes: Vec::new(),
    };
    for highlight in highlights {
        if cancellation.is_cancelled() {
            return Err(EffectCaptionRuntimeError::new(
                "render_cancelled",
                "增强阶段已被取消",
            ));
        }
        let Some((start_ms, end_ms)) = highlight.window else {
            summary.skipped += 1;
            summary
                .outcomes
                .push((highlight.candidate_id, "enhancement_no_published_segments"));
            // 没有可用句段也要留下记录，明确"最终方式=无增强/保留基础字幕"。
            let input = base_enhancement_input(
                project_id,
                highlight,
                start_ms_fallback(highlight),
                end_ms_fallback(highlight),
                AiEnhancementStatus::Rejected,
                Some("enhancement_no_published_segments"),
            );
            let _ = repository.record_enhancement(input, current_source_fingerprint);
            continue;
        };
        match runner.enhance(highlight, cancellation).await {
            Ok(product) => {
                let mut input = base_enhancement_input(
                    project_id,
                    highlight,
                    start_ms,
                    end_ms,
                    AiEnhancementStatus::Applied,
                    None,
                );
                input.composition_version = Some(product.composition_version);
                input.composition_fingerprint = Some(product.composition_fingerprint);
                input.render_cache_key = None;
                input.emphasis_words = product.emphasis_words;
                // 组合已通过白名单校验才会被渲染器产出；这里落库的仍是"已校验组合"派生的记录。
                repository
                    .record_enhancement(input, current_source_fingerprint)
                    .map_err(|error| {
                        EffectCaptionRuntimeError::new(
                            "render_cache_unavailable",
                            error.to_string(),
                        )
                    })?;
                summary.applied += 1;
                summary.outcomes.push((highlight.candidate_id, "applied"));
            }
            Err(error) => {
                let input = base_enhancement_input(
                    project_id,
                    highlight,
                    start_ms,
                    end_ms,
                    AiEnhancementStatus::Rejected,
                    Some(error.code),
                );
                // 失败也要留下唯一最终方式与原因；失败不抛出，避免拖垮整个阶段。
                repository
                    .record_enhancement(input, current_source_fingerprint)
                    .map_err(|repo_error| {
                        EffectCaptionRuntimeError::new(
                            "render_cache_unavailable",
                            repo_error.to_string(),
                        )
                    })?;
                summary.failed += 1;
                summary.outcomes.push((highlight.candidate_id, error.code));
            }
        }
    }
    Ok(summary)
}

fn start_ms_fallback(highlight: &EnhancementHighlight) -> u64 {
    highlight.window.map(|(start, _)| start).unwrap_or_default()
}

fn end_ms_fallback(highlight: &EnhancementHighlight) -> u64 {
    highlight
        .window
        .map(|(_, end)| end)
        // 没有已发布句段时用 1ms 占位，保证记录可落库（区间必须满足 end > start）。
        .unwrap_or(1)
}

fn base_enhancement_input(
    project_id: i64,
    highlight: &EnhancementHighlight,
    start_ms: u64,
    end_ms: u64,
    status: AiEnhancementStatus,
    rejection_code: Option<&'static str>,
) -> NewAiHighlightEnhancement {
    NewAiHighlightEnhancement {
        project_id,
        candidate_id: highlight.candidate_id,
        highlight_run_id: None,
        generation: 1,
        kind: AiEnhancementKind::EffectCaption,
        status,
        start_ms,
        end_ms: end_ms.max(start_ms + 1),
        emphasis_words: Vec::new(),
        composition_version: None,
        composition_fingerprint: None,
        render_cache_key: None,
        source_fingerprint: highlight.source_fingerprint.clone(),
        suppression_ranges: if status == AiEnhancementStatus::Applied {
            vec![super::domain::AiSuppressionRange { start_ms, end_ms }]
        } else {
            Vec::new()
        },
        rejection_code: rejection_code.map(str::to_owned),
        rejection_detail: rejection_code.map(|code| format!("增强未生效：{code}")),
        // 特效字幕分支不产生按次付费成本。
        cost_minor: 0,
    }
}

/// 一次批次级增强阶段的请求（任务 11.1 的调用点契约）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnhancementStageRequest {
    pub workflow_id: i64,
    pub batch_id: i64,
    pub draft_generation: Option<u32>,
    pub project_id: i64,
    pub highlight_run_id: Option<i64>,
    pub source_fingerprint: String,
}

/// 由工程已发布字幕构造已发布区间（隐藏句段不参与）。
pub fn published_ranges_from_subtitles(
    subtitles: &[super::domain::AiClipSubtitle],
) -> Vec<PublishedSubtitleRange> {
    let mut ranges: Vec<PublishedSubtitleRange> = subtitles
        .iter()
        .filter(|subtitle| !subtitle.hidden)
        .map(|subtitle| PublishedSubtitleRange {
            subtitle_id: subtitle.id,
            stable_segment_id: subtitle.stable_segment_id.clone(),
            start_ms: subtitle.project_start_ms,
            end_ms: subtitle.project_end_ms,
        })
        .collect();
    ranges.sort_by_key(|range| (range.start_ms, range.end_ms));
    ranges
}

/// 批次级增强阶段：登记阶段尝试，装载该批次的高光与已发布句段，逐条执行增强。
///
/// 这是 `SmartClippingWorkflow` 在评审就绪后调用的入口。它**不修改**草稿状态：
/// 阶段结果是 `ai_highlight_enhancements` 记录与阶段尝试行（任务 11.1 / 11.2）。
pub async fn run_enhancement_for_batch(
    repository: &AiRepository,
    workflows: &super::smart_workflow::SmartWorkflowRepository,
    runner: &dyn EffectCaptionEnhancementRunner,
    request: &EnhancementStageRequest,
    cancellation: &CancellationToken,
) -> Result<EnhancementStageSummary, EffectCaptionRuntimeError> {
    let attempt = workflows
        .start_attempt(&super::smart_workflow::SmartStageAttemptStart {
            workflow_id: request.workflow_id,
            batch_id: Some(request.batch_id),
            draft_generation: request.draft_generation,
            stage: AiSmartStage::Enhancement,
            input_fingerprint: request.source_fingerprint.clone(),
        })
        .map_err(|error| {
            EffectCaptionRuntimeError::new("enhancement_stage_unavailable", error.to_string())
        })?;
    let started = std::time::Instant::now();
    let prepared = (|| -> Result<Vec<EnhancementHighlight>, EffectCaptionRuntimeError> {
        let candidates = match request.highlight_run_id {
            Some(run_id) => {
                let selected = repository
                    .list_highlight_candidates(run_id)
                    .map_err(|error| {
                        EffectCaptionRuntimeError::new(
                            "enhancement_stage_unavailable",
                            error.to_string(),
                        )
                    })?
                    .into_iter()
                    .filter(|candidate| candidate.selected)
                    .collect::<Vec<_>>();
                // 任务 7.1 / 决策五：已分配 `video_animation` 的高光不得再生成特效字幕，
                // 两种增强方式互斥（普通 ASR 字幕仍按既有规则显示）。
                let applied = repository
                    .applied_enhancements_for_project(request.project_id)
                    .map_err(|error| {
                        EffectCaptionRuntimeError::new(
                            "enhancement_stage_unavailable",
                            error.to_string(),
                        )
                    })?;
                let eligible = super::domain::candidates_eligible_for_effect_caption(
                    &selected.iter().map(|candidate| candidate.id).collect::<Vec<_>>(),
                    &applied,
                );
                selected
                    .into_iter()
                    .filter(|candidate| eligible.contains(&candidate.id))
                    .collect::<Vec<_>>()
            }
            None => Vec::new(),
        };
        // 片段级前置：窗口来源取项目转写，**不读取合辑工程**，因此增强可以在
        // 建立合辑之前运行（也不因合辑尚未创建而失败）。
        let projection = crate::ai::AiTranscriptProjection::load(repository, request.project_id)
            .map_err(|error| {
                EffectCaptionRuntimeError::new("enhancement_stage_unavailable", error.to_string())
            })?;
        let transcript_sources = transcript_effect_window_sources(&projection);
        Ok(enhancement_highlights(
            &candidates,
            &transcript_sources,
            &request.source_fingerprint,
        ))
    })();
    let highlights = match prepared {
        Ok(highlights) => highlights,
        Err(error) => {
            let _ = workflows.finish_attempt(
                attempt.id,
                attempt.attempt_generation,
                super::domain::AiSmartStageAttemptStatus::Failed,
                None,
                started.elapsed().as_millis() as u64,
                Some((error.code, error.detail.as_str())),
            );
            return Err(error);
        }
    };
    let summary = match run_enhancement_stage(
        repository,
        runner,
        &highlights,
        request.project_id,
        &request.source_fingerprint,
        cancellation,
    )
    .await
    {
        Ok(summary) => summary,
        Err(error) => {
            let _ = workflows.finish_attempt(
                attempt.id,
                attempt.attempt_generation,
                super::domain::AiSmartStageAttemptStatus::Failed,
                None,
                started.elapsed().as_millis() as u64,
                Some((error.code, error.detail.as_str())),
            );
            return Err(error);
        }
    };
    // 单条失败不影响阶段结果：阶段本身完成，失败明细在增强记录里。
    workflows
        .finish_attempt(
            attempt.id,
            attempt.attempt_generation,
            super::domain::AiSmartStageAttemptStatus::Completed,
            Some(("effect_caption_enhancement", request.batch_id)),
            started.elapsed().as_millis() as u64,
            None,
        )
        .map_err(|error| {
            EffectCaptionRuntimeError::new("enhancement_stage_unavailable", error.to_string())
        })?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::ai::domain::AiEnhancementStatus as Status;
    use crate::database::Database;

    fn repository() -> AiRepository {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate");
        AiRepository::new(database)
    }

    /// 片段级窗口来源（**不依赖合辑工程**）。字段与合辑字幕范围同构，因此
    /// 切换来源不改变窗口取值。
    fn transcript_sources() -> Vec<TranscriptEffectWindowSource> {
        vec![
            TranscriptEffectWindowSource {
                stable_segment_id: "seg-1".to_owned(),
                start_ms: 0,
                end_ms: 1_000,
            },
            TranscriptEffectWindowSource {
                stable_segment_id: "seg-2".to_owned(),
                start_ms: 1_000,
                end_ms: 2_000,
            },
        ]
    }

    fn candidates() -> Vec<AiHighlightCandidate> {
        let mut candidate = AiHighlightCandidate {
            id: 11,
            run_id: 1,
            chunk_id: None,
            candidate_key: "candidate-11".to_owned(),
            title: "高光".to_owned(),
            input_id: 1,
            segment_ids: vec!["seg-1".to_owned()],
            start_ms: 0,
            end_ms: 1_000,
            total_score: 90.0,
            hook_score: 90.0,
            information_score: 90.0,
            emotion_score: 90.0,
            tag_relevance_score: 90.0,
            completeness_score: 90.0,
            shareability_score: 90.0,
            reason: "测试".to_owned(),
            matched_tags: Vec::new(),
            rank: Some(1),
            selected: true,
        };
        let mut second = candidate.clone();
        second.id = 12;
        second.candidate_key = "candidate-12".to_owned();
        second.segment_ids = vec!["seg-2".to_owned()];
        candidate.segment_ids = vec!["seg-1".to_owned()];
        vec![candidate, second]
    }

    struct SelectiveRunner {
        failing: Vec<i64>,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl EffectCaptionEnhancementRunner for SelectiveRunner {
        async fn enhance(
            &self,
            highlight: &EnhancementHighlight,
            _cancellation: &CancellationToken,
        ) -> Result<EnhancementProduct, EffectCaptionRuntimeError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.failing.contains(&highlight.candidate_id) {
                return Err(EffectCaptionRuntimeError::new(
                    "render_subprocess_failed",
                    "测试用渲染失败",
                ));
            }
            Ok(EnhancementProduct {
                composition_version: format!("composition-{}", highlight.candidate_id),
                composition_fingerprint: format!("fingerprint-{}", highlight.candidate_id),
                frames_dir: format!("/tmp/frames-{}", highlight.candidate_id),
                emphasis_words: Vec::new(),
            })
        }
    }

    #[test]
    fn enhancement_stage_sits_between_transition_and_review_and_is_not_blocking() {
        assert_enhancement_stage_position().expect("阶段顺序必须成立");
        let plan = enhancement_stage_plan();
        assert_eq!(plan.previous_stage, AiSmartStage::Transition);
        assert_eq!(plan.following_stage, AiSmartStage::Review);
        assert!(!plan.blocking, "增强阶段不得阻塞基础草稿（任务 11.1/11.5）");
        assert_eq!(AiSmartStage::ALL.len(), 8);
        assert_eq!(
            AiSmartStage::parse("enhancement"),
            Some(AiSmartStage::Enhancement)
        );
    }

    #[test]
    fn highlights_take_their_window_from_published_segments_only() {
        let highlights = enhancement_highlights(&candidates(), &transcript_sources(), "fp-1");
        assert_eq!(highlights.len(), 2);
        assert_eq!(highlights[0].window, Some((0, 1_000)));
        assert_eq!(highlights[1].window, Some((1_000, 2_000)));

        // 句段不存在 → 没有时间窗（模型时间不构成输入）。
        let mut orphan = candidates();
        orphan[0].segment_ids = vec!["invented".to_owned()];
        let highlights = enhancement_highlights(&orphan, &transcript_sources(), "fp-1");
        assert_eq!(highlights[0].window, None);
    }

    #[tokio::test]
    async fn single_highlight_failure_does_not_affect_others() {
        let repository = repository();
        let project = repository
            .create_project("增强工程", &profile())
            .expect("create project");
        let highlights = enhancement_highlights(&candidates(), &transcript_sources(), "fp-1");
        let runner = SelectiveRunner {
            failing: vec![11],
            calls: AtomicUsize::new(0),
        };
        let summary = run_enhancement_stage(
            &repository,
            &runner,
            &highlights,
            project.id,
            "fp-1",
            &CancellationToken::new(),
        )
        .await
        .expect("阶段本身必须成功返回");

        assert_eq!(summary.applied, 1, "失败的高光不影响其余高光");
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.total(), 2);
        assert_eq!(runner.calls.load(Ordering::SeqCst), 2, "两条都要尝试");

        // 失败的那条：最终方式唯一，且记录了可区分原因；保留基础字幕（无抑制区间）。
        let failed = repository
            .active_enhancement_for_candidate(11)
            .unwrap()
            .expect("失败也必须留记录");
        assert_eq!(failed.status, Status::Rejected);
        assert_eq!(
            failed.rejection_code.as_deref(),
            Some("render_subprocess_failed")
        );
        assert!(
            failed.suppression_ranges.is_empty(),
            "失败时不得抑制基础字幕"
        );

        // 成功的那条：applied + 抑制区间来自已发布句段。
        let applied = repository
            .active_enhancement_for_candidate(12)
            .unwrap()
            .expect("成功记录");
        assert_eq!(applied.status, Status::Applied);
        assert_eq!(applied.suppression_ranges.len(), 1);
        assert_eq!(
            applied.composition_version.as_deref(),
            Some("composition-12")
        );
    }

    #[tokio::test]
    async fn stale_generation_is_history_only_and_never_overwrites_manual_edits() {
        let repository = repository();
        let project = repository
            .create_project("增强工程", &profile())
            .expect("create project");
        let highlights = enhancement_highlights(&candidates(), &transcript_sources(), "fp-old");
        let runner = SelectiveRunner {
            failing: Vec::new(),
            calls: AtomicUsize::new(0),
        };
        // 先落一条"人工编辑前"的生效结果。
        let mut first = highlights.clone();
        first.truncate(1);
        run_enhancement_stage(
            &repository,
            &runner,
            &first,
            project.id,
            "fp-old",
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let before = repository
            .active_enhancement_for_candidate(11)
            .unwrap()
            .unwrap();
        assert_eq!(before.status, Status::Applied);

        // 用户改过工程后再收到旧代次结果：只能作为历史建议。
        let mut stale = highlights.clone();
        stale.truncate(1);
        stale[0].source_fingerprint = "fp-old".to_owned();
        run_enhancement_stage(
            &repository,
            &runner,
            &stale,
            project.id,
            "fp-new",
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let after = repository
            .active_enhancement_for_candidate(11)
            .unwrap()
            .unwrap();
        assert_eq!(
            after.status,
            Status::Applied,
            "人工编辑后的生效结果不得被覆盖"
        );
        assert_eq!(after.composition_version, before.composition_version);
    }

    #[tokio::test]
    async fn highlight_without_published_segments_is_recorded_as_skipped() {
        let repository = repository();
        let project = repository
            .create_project("增强工程", &profile())
            .expect("create project");
        let mut orphan = candidates();
        orphan.truncate(1);
        orphan[0].segment_ids = vec!["invented".to_owned()];
        let highlights = enhancement_highlights(&orphan, &transcript_sources(), "fp-1");
        let runner = SelectiveRunner {
            failing: Vec::new(),
            calls: AtomicUsize::new(0),
        };
        let summary = run_enhancement_stage(
            &repository,
            &runner,
            &highlights,
            project.id,
            "fp-1",
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(summary.skipped, 1);
        assert_eq!(
            runner.calls.load(Ordering::SeqCst),
            0,
            "没有句段就不该调用渲染"
        );
        let record = repository
            .active_enhancement_for_candidate(11)
            .unwrap()
            .unwrap();
        assert_eq!(record.status, Status::Rejected);
        assert_eq!(
            record.rejection_code.as_deref(),
            Some("enhancement_no_published_segments")
        );
    }

    #[tokio::test]
    async fn enhancement_stage_leaves_candidate_selection_and_project_state_untouched() {
        let repository = repository();
        let project = repository
            .create_project("增强工程", &profile())
            .expect("create project");
        let highlights = enhancement_highlights(&candidates(), &transcript_sources(), "fp-1");
        let runner = SelectiveRunner {
            failing: vec![11],
            calls: AtomicUsize::new(0),
        };
        run_enhancement_stage(
            &repository,
            &runner,
            &highlights,
            project.id,
            "fp-1",
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        // 任务 11.4：增强阶段不得改动工程状态（候选采用决策与导出确认语义都不在这里）。
        let unchanged = repository.get_project(project.id).unwrap();
        assert_eq!(unchanged.project.status, crate::ai::AiProjectStatus::Draft);
        assert_eq!(unchanged.project.name, "增强工程");
    }

    /// 任务 3.9：白名单校验失败的组合不得写入工程（不产生生效的增强记录），
    /// 也不得创建任何渲染缓存条目。
    #[tokio::test]
    async fn rejected_composition_never_becomes_a_project_effect_or_a_cache_entry() {
        use crate::ai::effect_caption::{EffectCaptionLimits, validate_effect_caption};

        struct ValidatingRunner;
        #[async_trait]
        impl EffectCaptionEnhancementRunner for ValidatingRunner {
            async fn enhance(
                &self,
                _highlight: &EnhancementHighlight,
                _cancellation: &CancellationToken,
            ) -> Result<EnhancementProduct, EffectCaptionRuntimeError> {
                // 真实实现同样先做白名单校验，再考虑启动渲染进程。
                let invalid = "<div class=\"caption-root\"><script>alert(1)</script></div>";
                match validate_effect_caption(invalid, &[], &EffectCaptionLimits::default()) {
                    Ok(_) => Ok(EnhancementProduct {
                        composition_version: "should-not-happen".to_owned(),
                        composition_fingerprint: "should-not-happen".to_owned(),
                        frames_dir: "/tmp/should-not-happen".to_owned(),
                        emphasis_words: Vec::new(),
                    }),
                    Err(rejection) => Err(EffectCaptionRuntimeError::new(
                        "effect_caption_rejected",
                        format!("{:?}", rejection.code),
                    )),
                }
            }
        }

        let repository = repository();
        let project = repository
            .create_project("校验失败工程", &profile())
            .expect("create project");
        let mut candidate = candidates();
        candidate.truncate(1);
        let highlights = enhancement_highlights(&candidate, &transcript_sources(), "fp-1");
        let summary = run_enhancement_stage(
            &repository,
            &ValidatingRunner,
            &highlights,
            project.id,
            "fp-1",
            &CancellationToken::new(),
        )
        .await
        .expect("阶段本身必须成功返回");
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.applied, 0);

        // 不得成为工程上的生效增强：要么没有记录，要么只能是 rejected。
        if let Some(record) = repository
            .active_enhancement_for_candidate(candidate[0].id)
            .unwrap()
        {
            assert_eq!(record.status, Status::Rejected);
            assert!(record.composition_version.is_none());
            assert!(record.suppression_ranges.is_empty());
        }
        // 渲染缓存必须完全干净：没有尝试行，也没有 ready 帧。
        assert_eq!(
            repository.render_attempt_count().unwrap(),
            0,
            "校验失败不得创建渲染缓存条目"
        );
        // 组合版本也不可能被写进工程（草稿版本不变）。
        let detail = repository.get_project(project.id).unwrap();
        assert_eq!(detail.project.status, crate::ai::AiProjectStatus::Draft);
    }

    use crate::ai::domain::RecognitionProfile;

    fn profile() -> RecognitionProfile {
        serde_json::from_str(
            r#"{
                "engineId": "dashscope-filetrans",
                "engineVersion": "1",
                "modelId": "qwen3-asr-flash-filetrans",
                "modelVersion": "qwen3-asr-flash-filetrans",
                "languageHint": "zh",
                "vadModelId": "ggml-vad@1",
                "vadThresholdMillis": 120,
                "vadPaddingMs": 300,
                "timestampPolicy": "segment",
                "normalizationVersion": "OpenCC ver.1.4.1",
                "hotwords": []
            }"#,
        )
        .expect("profile")
    }
}
