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
//!
//! 前置：工程必须已完成语音识别（`ai_project_inputs.status = 'completed'`）。
//! 敏感信息：本程序不打印 token、凭据或上游原始响应正文。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dy_screen_app_lib::activation::ActivationService;
use dy_screen_app_lib::ai::{
    ActivationLlmTokenSource, AiRepository, ClipOutputDimensions, ClipTextCorrectionWorkflow,
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

    export_clip(&repository, &app_data_dir, detail.project.id, &destination).await
}

/// 渲染字幕并导出 MP4，复用产品自身的导出链路。
async fn export_clip(
    repository: &AiRepository,
    app_data_dir: &Path,
    clip_project_id: i64,
    destination: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("[6/6] 渲染字幕并导出 MP4");
    let generated_root = app_data_dir.join("generated-animations");
    let sources = repository.clip_export_sources(clip_project_id, &generated_root)?;
    println!("      导出来源 {} 段", sources.len());
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
