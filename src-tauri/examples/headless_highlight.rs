//! 无界面驱动：对既有 AI 工程真实执行「提取精彩 → 生成合辑 → 校正文案 → 导出 MP4」。
//!
//! 用途：在没有 GUI 的环境里验证完整链路。它**只调用产品自身的代码路径**
//! （`HighlightWorkflow`、`ClipTextCorrectionWorkflow`、`clip_export_sources`、
//! `build_export_plan`、`execute_export`），不复制业务逻辑。
//!
//! 运行：
//! ```bash
//! DY_SCREEN_API_BASE_URL='http://<api>/api/' \
//!   cargo run --offline --manifest-path src-tauri/Cargo.toml \
//!   --example headless_highlight -- <project_id> [输出.mp4]
//! ```
//!
//! 环境变量：
//! - `DY_SCREEN_DATA_DIR`：指向数据库副本，避免与应用争用（应用运行时数据目录被独占）。
//! - `HEADLESS_SKIP_HIGHLIGHT=1`：复用已有的精彩结果，只跑合辑/纠错/导出。
//! - `HEADLESS_CANCEL_AFTER_MS=N`：N 毫秒后取消，用于验证取消路径。
//! - `HEADLESS_TRANSCRIPT_CORRECTION=1`：在拼合辑前先做**转写层**文案纠正（片段级前置）。
//! - `HEADLESS_GENERATE_TRANSITION=1`：导出前为第一个片段**真实生成**一段转场
//!   （消耗付费额度或免费额度），用于验证生成产物确实进入导出计划。
//!
//! 前置：工程必须已完成语音识别（`ai_project_inputs.status = 'completed'`）。
//! 敏感信息：本程序不打印 token、凭据或上游原始响应正文。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dy_screen_app_lib::activation::ActivationService;
use dy_screen_app_lib::ai::{
    generated_animation_file_name,
    ActivationLlmTokenSource, AiGeneratedAnimationComposition,
    AiGeneratedAnimationInsertBinding,
    AiRepository, ClipOutputDimensions, ClipTextCorrectionWorkflow, FfprobeMediaProbe,
    GeneratedTransitionRequest, GenerationClient, GenerationConfig, ReqwestTransport,
    produce_generated_transition,
    HighlightWorkflow, ModelCallRuntime, RigDeepSeekProvider, build_export_plan, execute_export,
    TranscriptTextCorrectionWorkflow, probe_output_dimensions, render_clip_subtitle_assets,
    select_clip_video_encoder, shift_subtitles_for_inserts,
};
use dy_screen_app_lib::api::ApiClient;
use dy_screen_app_lib::database::Database;
use dy_screen_app_lib::transition_assets::{
    MaterialAssetRegistry, TransitionMaterialAssetService, TransitionMaterialCache,
};
use dy_screen_app_lib::transition_matching::TransitionMatchingWorkflow;
use dy_screen_app_lib::transition_materials::TransitionMaterialRepository;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let project_id: i64 = args
        .next()
        .and_then(|value| value.parse().ok())
        .ok_or("用法：headless_highlight <project_id> [输出.mp4]")?;
    let export_destination: Option<PathBuf> = args.next().map(PathBuf::from);

    let app_data_dir = dirs_app_data()?;
    let database_path = app_data_dir.join("dy-screen.sqlite3");
    if !database_path.is_file() {
        return Err(format!("找不到数据库：{}", database_path.display()).into());
    }

    println!("[1/6] 打开数据库：{}", database_path.display());
    let database = Database::open(&database_path)?;
    // 与生产启动一致：先跑迁移。副本数据库通常是旧版本，缺表/列时后续步骤会直接报错。
    database.migrate()?;
    let repository = AiRepository::new(database.clone());

    println!("[2/6] 构造激活与模型调用点");
    let api = ApiClient::from_env()?;
    let activation = ActivationService::new(database.clone(), api, &app_data_dir)?;
    let tokens = Arc::new(ActivationLlmTokenSource::new(activation));
    let model_call = Arc::new(ModelCallRuntime::bootstrap(
        repository.clone(),
        tokens,
        chrono::Utc::now().timestamp_millis(),
    )?);
    let snapshot = model_call.snapshot().map_err(|error| error.category_str())?;
    println!(
        "      模型={} 来源={} 端点={}",
        snapshot.model_id,
        snapshot.source.as_str(),
        snapshot.base_url
    );

    let cancellation = CancellationToken::new();
    // `HEADLESS_CANCEL_AFTER_MS`：在指定毫秒后取消，用于验证取消路径是否留下干净
    // 状态（可重试、无半成品）。默认不取消。
    if let Some(after_ms) = std::env::var("HEADLESS_CANCEL_AFTER_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        let cancellation = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(after_ms)).await;
            println!("      ⏹ 触发取消（{after_ms}ms）");
            cancellation.cancel();
        });
    }

    if std::env::var_os("HEADLESS_SKIP_HIGHLIGHT").is_some() {
        println!("[3/6] 跳过精彩分析（HEADLESS_SKIP_HIGHLIGHT 已设置）");
    } else {
        println!("[3/6] 启动精彩分析（真实调用，会产生上游费用）");
        let highlight = HighlightWorkflow::new(
            repository.clone(),
            Arc::new(RigDeepSeekProvider),
            model_call.clone(),
        );
        let started = std::time::Instant::now();
        match highlight
            .analyze(project_id, true, cancellation.clone())
            .await
        {
            Ok(run) => {
                println!(
                    "      状态={:?} 用时={:.1}s 模型={}",
                    run.status,
                    started.elapsed().as_secs_f32(),
                    run.model_id
                );
                if let Some(message) = run.last_error_message.as_deref() {
                    println!("      错误={message}");
                }
                let candidates = repository.list_highlight_candidates(run.id)?;
                println!(
                    "      候选={} 已选={}",
                    candidates.len(),
                    candidates.iter().filter(|c| c.selected).count()
                );
                for candidate in candidates.iter().take(5) {
                    println!(
                        "        - [{:.1}] {}  {}ms..{}ms",
                        candidate.total_score, candidate.title, candidate.start_ms, candidate.end_ms
                    );
                }
            }
            Err(error) => {
                println!("      失败：{error}");
                println!("      分类：{}", error.category_str());
                return Err("highlight 未成功".into());
            }
        }
    }

    let Some(destination) = export_destination else {
        println!("（未指定输出文件，停在提取精彩阶段）");
        return Ok(());
    };

    // 可选：片段级前置的文案纠正（作用于转写句段，发生在拼合辑之前）。
    if std::env::var_os("HEADLESS_TRANSCRIPT_CORRECTION").is_some() {
        println!("[3.5/6] 转写层文案纠正（片段级前置）");
        let transcript_correction = TranscriptTextCorrectionWorkflow::new(
            repository.clone(),
            Arc::new(RigDeepSeekProvider),
            model_call.clone(),
        );
        let summary = transcript_correction
            .correct_project(project_id, cancellation.clone())
            .await
            .map_err(|error| format!("转写纠正失败：{error}"))?;
        println!(
            "      转写纠正完成：处理={} 改正={} 未变={} 批次={} token={}",
            summary.processed,
            summary.changed,
            summary.unchanged,
            summary.total_batches,
            summary.token_usage
        );
    }

    println!("[4/6] 生成合辑工程");
    let run = repository
        .latest_highlight_run_for_project(project_id)?
        .ok_or("该工程还没有精彩运行")?;
    let detail = repository.get_or_create_clip_project(run.id)?;
    println!(
        "      工程 #{} 片段={} 字幕={}",
        detail.project.id,
        detail.segments.len(),
        detail.subtitles.len()
    );

    println!("[5/6] 校正文案（真实调用）");
    // 重新读取拿**最新**版本：`get_or_create_clip_project` 之后工程可能已被写入，
    // 乐观锁会用旧版本号直接拒绝（`ClipVersionConflict`）。
    let fresh = repository.get_clip_project(detail.project.id)?;
    println!("      当前工程版本={}", fresh.project.version);
    let correction = ClipTextCorrectionWorkflow::new(
        repository.clone(),
        Arc::new(RigDeepSeekProvider),
        model_call.clone(),
    );
    let summary = correction
        // 用**手动归属**入口：`correct_smart_project` 走的是自动化工程路径
        // （要求 `ownership='automation'` 且 `smart_workflow_id` 非空），
        // 而本驱动用 `get_or_create_clip_project` 建的是手动工程。
        .correct_project(
            fresh.project.id,
            fresh.project.version,
            CancellationToken::new(),
        )
        .await
        .map_err(|error| format!("文案纠错失败：{error}"))?;
    println!(
        "      纠错完成：run={} 处理={} 改正={} 未变={} 跳过(人工/隐藏)={}/{} token={}",
        summary.run_id,
        summary.processed,
        summary.changed,
        summary.unchanged,
        summary.skipped_manual,
        summary.skipped_hidden,
        summary.token_usage
    );

    // 转场匹配：Agent 为每个未锁定的转场边界挑选素材。素材目录已同步时才会产出结果。
    if std::env::var_os("HEADLESS_SKIP_TRANSITION").is_none() {
        println!("      — 转场匹配（真实调用）");
        let cache_root = app_data_dir.join("transition-material-cache");
        let materials = TransitionMaterialRepository::new(database.clone());
        let assets = TransitionMaterialAssetService::new(
            materials.clone(),
            TransitionMaterialCache::new(cache_root)?,
            CancellationToken::new(),
            MaterialAssetRegistry::default(),
        )?;
        let transition = TransitionMatchingWorkflow::new(
            materials,
            repository.clone(),
            Arc::new(RigDeepSeekProvider),
            model_call.clone(),
            assets,
        );
        match transition
            .match_boundaries(detail.project.id, None, CancellationToken::new())
            .await
        {
            Ok(summary) => println!(
                "      转场匹配完成：run={} 阈值={} 匹配={} 自动应用={} token={}",
                summary.run_id,
                summary.threshold,
                summary.matched,
                summary.auto_applied,
                summary.token_usage
            ),
            Err(error) => println!("      转场匹配未完成（不阻塞导出）：{error}"),
        }
    }

    if std::env::var_os("HEADLESS_GENERATE_TRANSITION").is_some() {
        println!("[5.5/6] 生成真实转场（消耗额度）");
        match generate_transition(&repository, &app_data_dir, project_id, detail.project.id, &model_call).await {
            Ok(asset_id) => println!("      生成资产 id={asset_id} 已进入工程"),
            Err(error) => println!("      ⚠️ 生成转场失败（不阻塞导出）：{error}"),
        }
    }

    export_clip(
        &repository,
        &app_data_dir,
        project_id,
        detail.project.id,
        &destination,
    )
    .await
}

/// 为第一个片段生成一段真实转场，并按 `replace` 合成落库。
///
/// 走的是产品自身的 `produce_generated_transition`：生成→下载→校验→落库→改名到
/// `generated_animation_file_name(id)`。之后 `clip_export_sources` 会自动消费它，
/// 因此本函数**不需要**改导出链路。
///
/// **易错点**：`AiClipProjectDetail.project` 是**合辑工程**，不是 AI 工程。
/// 生成资产的 `project_id` 必须是 **AI 工程 id**——`clip_export_sources` 会从
/// 合辑的 `highlight_run_id` 反查 AI 工程再按它取资产。传错就会"资产落库了、
/// 却永远匹配不上导出计划"，而且**导出照样成功**，只是没有转场。
async fn generate_transition(
    repository: &AiRepository,
    app_data_dir: &Path,
    ai_project_id: i64,
    clip_project_id: i64,
    model_call: &Arc<ModelCallRuntime>,
) -> Result<i64, Box<dyn std::error::Error>> {
    let detail = repository.get_clip_project(clip_project_id)?;
    // 与产品路径一致：转场是**边界插入**（两段之间的桥接），不是片段内容替换。
    let boundaries = repository.clip_transition_boundaries(clip_project_id)?;
    let boundary = boundaries
        .iter()
        .find(|boundary| boundary.active)
        .ok_or("工程没有相邻片段边界，无法生成转场（需要至少 2 个片段）")?;
    let (left, right) = (
        boundary
            .left_clip_segment_id
            .ok_or("边界缺少左侧片段")?,
        boundary
            .right_clip_segment_id
            .ok_or("边界缺少右侧片段")?,
    );
    let segment = detail
        .segments
        .iter()
        .find(|segment| segment.id == left)
        .ok_or("边界左侧片段不在工程内")?;
    let run = repository
        .latest_highlight_run_for_project(ai_project_id)?
        .ok_or("找不到精彩分析运行")?;

    let lease = model_call
        .acquire_token(chrono::Utc::now().timestamp())
        .await?;
    let probe = FfprobeMediaProbe {
        ffprobe_path: PathBuf::from("/opt/homebrew/bin/ffprobe"),
    };
    let client = GenerationClient::new(ReqwestTransport::default(), GenerationConfig::default());

    let (asset, model) = produce_generated_transition(
        &client,
        &probe,
        repository,
        app_data_dir,
        &lease.token,
        dy_screen_app_lib::ai::GenerationPoll::default(),
        GeneratedTransitionRequest {
            // 必须是 AI 工程 id，不是合辑工程 id（见上方易错点）。
            project_id: ai_project_id,
            candidate_id: segment.candidate_id,
            highlight_run_id: Some(run.id),
            generation: 1,
            composition: AiGeneratedAnimationComposition::Insert,
            replace_range: None,
            insert_binding: Some(AiGeneratedAnimationInsertBinding {
                left_segment_id: left,
                right_segment_id: right,
                boundary_id: boundary.id,
            }),
            // 稳定复用键：同一边界重复运行只应产生一代资产（不重复计费）。
            reuse_key: format!("headless-boundary-{}-{}", ai_project_id, boundary.id),
            image_prompt: format!("{}，写实摄影风格，自然光，电影质感", segment.title),
            video_prompt: "镜头缓慢推近，温暖的自然光，画面稳定".to_owned(),
        },
    )
    .await
    .map_err(|error| format!("{}: {error:?}", error.code()))?;

    println!(
        "      模型产物已校验并落库（模型={model}）：{}×{} {}fps {}ms sha256={}…",
        asset.width.unwrap_or(0),
        asset.height.unwrap_or(0),
        asset.fps.unwrap_or(0),
        asset.duration_ms.unwrap_or(0),
        asset
            .sha256
            .as_deref()
            .map(|digest| &digest[..12])
            .unwrap_or("无")
    );
    Ok(asset.id)
}

/// 渲染字幕并导出 MP4，复用产品自身的导出链路。
async fn export_clip(
    repository: &AiRepository,
    app_data_dir: &Path,
    ai_project_id: i64,
    clip_project_id: i64,
    destination: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("[6/6] 渲染字幕并导出 MP4");
    // 注意：`clip_export_sources` **自己**会再拼一层 `GENERATED_ANIMATION_CACHE_DIR`，
    // 所以这里必须传数据目录本身，传 `.../generated-animations` 会变成双层嵌套、
    // 导致"资产已落盘但导出找不到文件"。
    let generated_root = app_data_dir.to_path_buf();
    let sources = repository.clip_export_sources(clip_project_id, &generated_root)?;
    println!("      导出来源 {} 段", sources.len());
    // 诊断：生成资产必须**真的出现在导出源里**，否则"导出成功"只是普通成片。
    for (index, source) in sources.iter().enumerate() {
        if let Some(replace) = source.replace.as_ref() {
            println!(
                "      片段[{index}] replace {}-{}ms（生成画面覆盖该区间）",
                replace.start_ms, replace.end_ms
            );
        }
        if let Some(insert) = source.insert_after.as_ref() {
            println!("      片段[{index}] insert_after {}ms", insert.duration_ms);
        }
    }
    let detail = repository.get_clip_project(clip_project_id)?;

    let ffmpeg = PathBuf::from("/opt/homebrew/bin/ffmpeg");
    let ffprobe = PathBuf::from("/opt/homebrew/bin/ffprobe");
    let encoder = select_clip_video_encoder(&ffmpeg).await?;
    let dimensions = match (detail.project.output_width, detail.project.output_height) {
        (Some(width), Some(height)) => ClipOutputDimensions { width, height },
        _ => probe_output_dimensions(&ffprobe, Path::new(&sources[0].source_path)).await?,
    };
    let total_duration_ms = sources.iter().fold(0_u64, |total, source| {
        total
            .saturating_add(
                source
                    .segment
                    .source_end_ms
                    .saturating_sub(source.segment.source_start_ms),
            )
            .saturating_add(source.bridge_after.as_ref().map_or(0, |b| b.duration_ms))
            .saturating_add(source.insert_after.as_ref().map_or(0, |i| i.duration_ms))
    });
    let subtitles = shift_subtitles_for_inserts(
        &sources
            .iter()
            .flat_map(|source| source.subtitles.iter().cloned())
            .collect::<Vec<_>>(),
        &sources,
    );
    let temporary = std::env::temp_dir().join("headless-clip-export");
    std::fs::create_dir_all(&temporary)?;
    // 第二个参数是**输出文件路径**，不是目录。
    let temporary_output = temporary.join("output.mp4");
    let _ = std::fs::remove_file(&temporary_output);
    let assets = render_clip_subtitle_assets(&subtitles, dimensions, total_duration_ms)?;
    let plan = build_export_plan(
        &sources,
        temporary_output,
        dimensions,
        assets.manifest_path(),
        encoder,
    )?;
    let joined = plan.arguments.join(" ");
    let animations = repository
        .list_generated_animation_assets(ai_project_id)
        .unwrap_or_default();
    println!("      工程内生成资产 {} 条", animations.len());
    for animation in &animations {
        let name = generated_animation_file_name(animation.id);
        println!(
            "      资产 id={} {}×{} {}ms 是否出现在 FFmpeg 参数：{}",
            animation.id,
            animation.width.unwrap_or(0),
            animation.height.unwrap_or(0),
            animation.duration_ms.unwrap_or(0),
            joined.contains(&name)
        );
    }
    if std::env::var_os("HEADLESS_DUMP_PLAN").is_some() {
        println!("      --- FFmpeg 参数 ---");
        for (index, argument) in plan.arguments.iter().enumerate() {
            println!("      [{index}] {argument}");
        }
        println!("      --- 结束 ---");
    }
    let produced = execute_export(&ffmpeg, plan, CancellationToken::new(), |_| {}).await?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&produced, destination)?;
    let size = std::fs::metadata(destination)?.len();
    println!(
        "      ✅ 导出完成：{}（{:.1} MB，encoder={encoder}）",
        destination.display(),
        size as f64 / (1024.0 * 1024.0)
    );
    Ok(())
}

/// 与生产一致的应用数据目录；`DY_SCREEN_DATA_DIR` 可指向数据库副本。
fn dirs_app_data() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(dir) = std::env::var("DY_SCREEN_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var("HOME")?;
    Ok(PathBuf::from(home).join("Library/Application Support/com.yino.dyscreen"))
}
