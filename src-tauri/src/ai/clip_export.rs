//! 剪辑工程的受控 FFmpeg 导出。
//!
//! 此模块只接受 repository 提供的冻结来源和内置效果枚举；不会解析前端传来的
//! 路径、滤镜字符串或 FFmpeg 参数。

use std::fs::File;
use std::io::BufReader as StdBufReader;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use dy_screen::asr::FrozenMediaSource;
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use super::{AiClipEffect, ClipExportSource, SourceFingerprint};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipExportPlan {
    pub arguments: Vec<String>,
    pub temporary_output: PathBuf,
    pub total_duration_ms: u64,
}

/// FFmpeg concat filter 要求所有视频输入具有相同的尺寸与像素格式。
/// 输出规格由工程的第一段来源决定，避免竖屏和横屏录像混剪时失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipOutputDimensions {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipExportFailure {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for ClipExportFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ClipExportFailure {}

pub(super) fn failure(code: &'static str, message: impl Into<String>) -> ClipExportFailure {
    ClipExportFailure {
        code,
        message: message.into(),
    }
}

pub fn validate_export_subtitles(sources: &[ClipExportSource]) -> Result<(), ClipExportFailure> {
    if sources.iter().any(|source| source.subtitles.is_empty()) {
        return Err(failure(
            "subtitles_incomplete",
            "剪辑片段缺少 ASR 字幕，请先完成识别或移除该片段",
        ));
    }
    Ok(())
}

pub fn validate_export_sources(sources: &[ClipExportSource]) -> Result<(), ClipExportFailure> {
    for source in sources {
        let path = Path::new(&source.source_path);
        let frozen_media = FrozenMediaSource::from_path(path)
            .map_err(|_| failure("source_unavailable", "剪辑来源视频不存在或无法读取"))?;
        let frozen = SourceFingerprint {
            normalized_path: frozen_media.path.to_string_lossy().into_owned(),
            size_bytes: frozen_media.size_bytes,
            modified_at_ms: i64::try_from(frozen_media.modified_at_ms)
                .map_err(|_| failure("source_unavailable", "无法读取剪辑来源视频的修改时间"))?,
            video_id: source.source_fingerprint.video_id,
        };
        if frozen != source.source_fingerprint {
            return Err(failure(
                "source_changed",
                "来源视频在加入 AI 项目后发生变化，已拒绝导出以避免错剪",
            ));
        }
        if let Some(bridge) = &source.bridge_after {
            let bridge_path = Path::new(&bridge.source_path);
            if !bridge_path.is_file() || bridge.duration_ms == 0 {
                return Err(failure(
                    "transition_material_unavailable",
                    format!(
                        "转场素材 {} v{} 尚未准备完成，请重试素材下载",
                        bridge.asset_key, bridge.asset_version
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// 从受控 FFprobe 读取第一条视频轨道的输出规格。
pub async fn probe_output_dimensions(
    ffprobe_path: &Path,
    source_path: &Path,
) -> Result<ClipOutputDimensions, ClipExportFailure> {
    let output = Command::new(ffprobe_path)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0:s=x",
        ])
        .arg(source_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|_| failure("ffprobe_unavailable", "无法启动随应用提供的 FFprobe"))?;
    if !output.status.success() {
        return Err(failure("source_invalid", "无法读取剪辑来源视频的画面尺寸"));
    }
    let dimensions = String::from_utf8_lossy(&output.stdout);
    let (width, height) = dimensions
        .trim()
        .split_once('x')
        .and_then(|(width, height)| Some((width.parse::<u32>().ok()?, height.parse::<u32>().ok()?)))
        .filter(|(width, height)| *width > 0 && *height > 0)
        .ok_or_else(|| failure("source_invalid", "来源视频不包含有效画面尺寸"))?;
    Ok(ClipOutputDimensions {
        width: normalize_dimension(width)?,
        height: normalize_dimension(height)?,
    })
}

fn normalize_dimension(value: u32) -> Result<u32, ClipExportFailure> {
    if value.is_multiple_of(2) {
        return Ok(value);
    }
    value
        .checked_add(1)
        .ok_or_else(|| failure("source_invalid", "来源视频的画面尺寸无效"))
}

pub fn build_export_plan(
    sources: &[ClipExportSource],
    temporary_output: PathBuf,
    output_dimensions: ClipOutputDimensions,
    subtitle_manifest: &Path,
    video_encoder: &str,
) -> Result<ClipExportPlan, ClipExportFailure> {
    build_export_plan_with_overlays(
        sources,
        temporary_output,
        output_dimensions,
        subtitle_manifest,
        video_encoder,
        &[],
        &[],
    )
}

/// 构造导出计划，并把特效字幕透明层一起接进**同一个合成模板**（任务 7.2）。
///
/// `subtitle_ranges` 是基础字幕覆盖的时间区间，用于保证同一时间范围只有一层文字。
#[allow(clippy::too_many_arguments)]
pub fn build_export_plan_with_overlays(
    sources: &[ClipExportSource],
    temporary_output: PathBuf,
    output_dimensions: ClipOutputDimensions,
    subtitle_manifest: &Path,
    video_encoder: &str,
    overlays: &[EffectCaptionOverlay],
    subtitle_ranges: &[TextLayerRange],
) -> Result<ClipExportPlan, ClipExportFailure> {
    validate_single_text_layer(subtitle_ranges, overlays)?;
    if sources.is_empty() {
        return Err(failure("empty_project", "剪辑工程没有可导出的片段"));
    }
    if !matches!(video_encoder, "h264_videotoolbox" | "h264_mf" | "libx264") {
        return Err(failure("encoder_unavailable", "受控 H.264 编码器不可用"));
    }
    let mut arguments = vec![
        "-y".to_owned(),
        "-hide_banner".to_owned(),
        "-nostdin".to_owned(),
    ];
    let mut filter_parts = Vec::with_capacity(sources.len() * 4 + 2);
    let mut concat_inputs = String::new();
    let mut total_duration_ms = 0_u64;
    let mut input_index = 0_usize;
    let mut unit_index = 0_usize;
    for source in sources {
        let duration_ms = source
            .segment
            .source_end_ms
            .checked_sub(source.segment.source_start_ms)
            .ok_or_else(|| failure("invalid_segment", "剪辑片段的时间范围无效"))?;
        total_duration_ms = total_duration_ms.saturating_add(duration_ms);
        arguments.extend([
            "-ss".to_owned(),
            format_seconds(source.segment.source_start_ms),
            "-t".to_owned(),
            format_seconds(duration_ms),
            "-i".to_owned(),
            source.source_path.clone(),
        ]);
        let duration_seconds = duration_ms as f64 / 1_000.0;
        let effect_filter = visual_filter(source.segment.effect, duration_seconds);
        filter_parts.push(format!(
            "[{input_index}:v]setpts=PTS-STARTPTS,scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,format=yuv420p{effect_filter}[v{unit_index}]",
            width = output_dimensions.width,
            height = output_dimensions.height,
        ));
        filter_parts.push(format!(
            "[{input_index}:a]asetpts=PTS-STARTPTS,aformat=sample_rates=48000:channel_layouts=stereo,volume={:.2}[a{unit_index}]",
            source.segment.volume_percent as f64 / 100.0,
        ));
        concat_inputs.push_str(&format!("[v{unit_index}][a{unit_index}]"));
        input_index += 1;
        unit_index += 1;

        if let Some(bridge) = &source.bridge_after {
            total_duration_ms = total_duration_ms.saturating_add(bridge.duration_ms);
            let bridge_input_index = input_index;
            arguments.extend(["-i".to_owned(), bridge.source_path.clone()]);
            input_index += 1;
            filter_parts.push(format!(
                "[{bridge_input_index}:v]setpts=PTS-STARTPTS,scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,format=yuv420p[v{unit_index}]",
                width = output_dimensions.width,
                height = output_dimensions.height,
            ));
            if bridge.has_audio {
                filter_parts.push(format!(
                    "[{bridge_input_index}:a]asetpts=PTS-STARTPTS,aformat=sample_rates=48000:channel_layouts=stereo,volume=1.00[a{unit_index}]"
                ));
            } else {
                let silent_input_index = input_index;
                arguments.extend([
                    "-f".to_owned(),
                    "lavfi".to_owned(),
                    "-t".to_owned(),
                    format_seconds(bridge.duration_ms),
                    "-i".to_owned(),
                    "anullsrc=r=48000:cl=stereo".to_owned(),
                ]);
                input_index += 1;
                filter_parts.push(format!(
                    "[{silent_input_index}:a]asetpts=PTS-STARTPTS,aformat=sample_rates=48000:channel_layouts=stereo[a{unit_index}]"
                ));
            }
            concat_inputs.push_str(&format!("[v{unit_index}][a{unit_index}]"));
            unit_index += 1;
        }
    }
    let subtitle_input_index = input_index;
    arguments.extend([
        "-f".to_owned(),
        "concat".to_owned(),
        "-safe".to_owned(),
        "0".to_owned(),
        "-i".to_owned(),
        subtitle_manifest.to_string_lossy().into_owned(),
    ]);
    // 字幕清单已占用一个输入位：后续叠加层必须继续递增，否则会引用到字幕流。
    input_index += 1;
    filter_parts.push(format!(
        "{concat_inputs}concat=n={}:v=1:a=1[vjoined][aout]",
        unit_index,
    ));
    filter_parts.push(format!(
        "[{subtitle_input_index}:v]format=rgba,setpts=PTS-STARTPTS[subtitles]"
    ));
    let bottom_margin = (output_dimensions.height as f64 * 0.04).round() as u32;
    let total_ms = total_duration_ms;
    for overlay in overlays {
        if overlay.end_ms > total_ms {
            return Err(failure(
                "invalid_overlay_range",
                "特效字幕的时间范围超出工程总时长",
            ));
        }
    }
    if overlays.is_empty() {
        filter_parts.push(format!(
            "[vjoined][subtitles]overlay=0:H-h-{bottom_margin}:format=auto:eof_action=pass[vout]"
        ));
    } else {
        // 基础字幕与特效字幕走同一条 overlay 模板：[底图][透明层]overlay[下一层]。
        let mut current = "vsub".to_owned();
        filter_parts.push(format!(
            "[vjoined][subtitles]overlay=0:H-h-{bottom_margin}:format=auto:eof_action=pass[{current}]"
        ));
        for (index, overlay) in overlays.iter().enumerate() {
            let overlay_input_index = input_index;
            arguments.extend([
                "-f".to_owned(),
                "concat".to_owned(),
                "-safe".to_owned(),
                "0".to_owned(),
                "-i".to_owned(),
                overlay.manifest_path.to_string_lossy().into_owned(),
            ]);
            input_index += 1;
            filter_parts.push(format!(
                "[{overlay_input_index}:v]format=rgba,setpts=PTS-STARTPTS[effect{index}]"
            ));
            let next = if index + 1 == overlays.len() {
                "vout".to_owned()
            } else {
                format!("veffect{index}")
            };
            // 整帧叠加 + enable 限定时间范围：范围之外完全不参与合成。
            filter_parts.push(format!(
                "[{current}][effect{index}]overlay=0:0:format=auto:eof_action=pass:enable='between(t,{}, {})'[{next}]",
                format_seconds(overlay.start_ms),
                format_seconds(overlay.end_ms),
            ));
            current = next;
        }
    }
    arguments.extend([
        "-filter_complex".to_owned(),
        filter_parts.join(";"),
        "-map".to_owned(),
        "[vout]".to_owned(),
        "-map".to_owned(),
        "[aout]".to_owned(),
        "-c:v".to_owned(),
        video_encoder.to_owned(),
    ]);
    if video_encoder == "h264_videotoolbox" {
        arguments.extend(["-allow_sw".to_owned(), "1".to_owned()]);
    }
    arguments.extend([
        "-pix_fmt".to_owned(),
        "yuv420p".to_owned(),
        "-movflags".to_owned(),
        "+faststart".to_owned(),
        "-c:a".to_owned(),
        "aac".to_owned(),
        "-b:a".to_owned(),
        "192k".to_owned(),
        "-progress".to_owned(),
        "pipe:1".to_owned(),
        "-nostats".to_owned(),
        "-f".to_owned(),
        "mp4".to_owned(),
        temporary_output.to_string_lossy().into_owned(),
    ]);
    Ok(ClipExportPlan {
        arguments,
        temporary_output,
        total_duration_ms,
    })
}

pub async fn select_clip_video_encoder(
    ffmpeg_path: &Path,
) -> Result<&'static str, ClipExportFailure> {
    let output = Command::new(ffmpeg_path)
        .args(["-hide_banner", "-encoders"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|_| failure("ffmpeg_unavailable", "无法启动随应用提供的 FFmpeg"))?;
    if !output.status.success() {
        return Err(failure("encoder_unavailable", "无法读取 FFmpeg 编码器能力"));
    }
    let encoders = String::from_utf8_lossy(&output.stdout);
    #[cfg(target_os = "macos")]
    let candidates = ["h264_videotoolbox", "libx264"];
    #[cfg(target_os = "windows")]
    let candidates = ["h264_mf", "libx264"];
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let candidates = ["libx264", "h264_videotoolbox"];
    candidates
        .into_iter()
        .find(|candidate| {
            encoders
                .lines()
                .any(|line| line.split_whitespace().nth(1) == Some(candidate))
        })
        .ok_or_else(|| {
            failure(
                "encoder_unavailable",
                "随应用提供的 FFmpeg 缺少受控 H.264 编码器",
            )
        })
}

fn visual_filter(effect: AiClipEffect, duration_seconds: f64) -> String {
    let fade_length = duration_seconds.clamp(0.01, 0.35);
    let fade_out_start = (duration_seconds - fade_length).max(0.0);
    match effect {
        AiClipEffect::None => String::new(),
        AiClipEffect::FadeIn => format!(",fade=t=in:st=0:d={fade_length:.3}"),
        AiClipEffect::FadeOut => format!(",fade=t=out:st={fade_out_start:.3}:d={fade_length:.3}"),
        AiClipEffect::FadeInOut => format!(
            ",fade=t=in:st=0:d={fade_length:.3},fade=t=out:st={fade_out_start:.3}:d={fade_length:.3}"
        ),
        // 闪白和淡黑是固定内置滤镜，不接受任何用户提供的滤镜片段。
        AiClipEffect::Flash => format!(",fade=t=in:st=0:d={fade_length:.3}:color=white"),
        AiClipEffect::Black => format!(",fade=t=in:st=0:d={fade_length:.3}:color=black"),
    }
}

fn format_seconds(milliseconds: u64) -> String {
    format!("{:.3}", milliseconds as f64 / 1_000.0)
}

pub async fn execute_export<F>(
    ffmpeg_path: &Path,
    plan: ClipExportPlan,
    cancellation: CancellationToken,
    mut progress: F,
) -> Result<PathBuf, ClipExportFailure>
where
    F: FnMut(u8) + Send,
{
    let mut child = Command::new(ffmpeg_path)
        .args(&plan.arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| failure("ffmpeg_unavailable", "无法启动随应用提供的 FFmpeg"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| failure("export_failed", "无法读取 FFmpeg 导出进度"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| failure("export_failed", "无法读取 FFmpeg 错误输出"))?;
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        let _ = BufReader::new(stderr).read_to_end(&mut bytes).await;
        bytes
    });
    let mut lines = BufReader::new(stdout).lines();
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => {
                let _ = child.kill().await;
                let _ = stderr_task.await;
                let _ = tokio::fs::remove_file(&plan.temporary_output).await;
                return Err(failure("cancelled", "视频导出已取消"));
            }
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    if let Some(value) = line.strip_prefix("out_time_ms=")
                        && let Ok(microseconds) = value.parse::<u64>() {
                        let milliseconds = microseconds / 1_000;
                        let ratio = milliseconds as f64 / plan.total_duration_ms.max(1) as f64;
                        progress((ratio * 95.0).round().clamp(1.0, 95.0) as u8);
                    }
                }
                Ok(None) => break,
                Err(_) => return Err(failure("export_failed", "读取 FFmpeg 导出进度失败")),
            }
        }
    }
    let status = child
        .wait()
        .await
        .map_err(|_| failure("export_failed", "等待 FFmpeg 导出结果失败"))?;
    let stderr = stderr_task.await.unwrap_or_default();
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr);
        let message = if detail.contains("No such file") {
            "来源视频不存在，无法导出"
        } else {
            "FFmpeg 无法导出该剪辑工程"
        };
        let _ = tokio::fs::remove_file(&plan.temporary_output).await;
        return Err(failure("export_failed", message));
    }
    if !plan.temporary_output.is_file() {
        return Err(failure("export_missing", "FFmpeg 未生成预期的视频文件"));
    }
    progress(98);
    Ok(plan.temporary_output)
}

/// 特效字幕叠加层：一段透明 RGBA 帧序列（`ffconcat` 清单 + 源目录）。
///
/// 与基础字幕层**共用同一合成模板**：都是 rgba 输入 + `overlay` 滤镜；
/// 区别只是特效字幕按整帧叠加并用 `enable` 限定时间范围（任务 7.2 / 7.4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectCaptionOverlay {
    pub manifest_path: PathBuf,
    pub frames_dir: PathBuf,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// 基础字幕覆盖的时间区间，用于"同一时间范围只有一层文字"校验（任务 7.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextLayerRange {
    pub start_ms: u64,
    pub end_ms: u64,
}

fn ranges_overlap(left: TextLayerRange, right: TextLayerRange) -> bool {
    left.start_ms < right.end_ms && right.start_ms < left.end_ms
}

/// 校验基础字幕与特效字幕在同一时间范围不重叠。
pub fn validate_single_text_layer(
    subtitle_ranges: &[TextLayerRange],
    overlays: &[EffectCaptionOverlay],
) -> Result<(), ClipExportFailure> {
    for overlay in overlays {
        if overlay.end_ms <= overlay.start_ms {
            return Err(failure("invalid_overlay_range", "特效字幕的时间范围无效"));
        }
        if overlay.manifest_path.as_os_str().is_empty() {
            return Err(failure("invalid_overlay_range", "特效字幕缺少帧清单"));
        }
        let overlay_range = TextLayerRange {
            start_ms: overlay.start_ms,
            end_ms: overlay.end_ms,
        };
        if subtitle_ranges
            .iter()
            .any(|range| ranges_overlap(*range, overlay_range))
        {
            return Err(failure(
                "dual_text_layers",
                "同一时间范围同时存在基础字幕与特效字幕",
            ));
        }
    }
    Ok(())
}

/// 全分辨率帧序列的资源基线（任务 7.6）：峰值磁盘占用与"渲染耗时 / 素材时长"比值。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExportResourceBaseline {
    pub max_peak_disk_bytes: u64,
    pub max_realtime_ratio: f64,
}

impl Default for ExportResourceBaseline {
    fn default() -> Self {
        Self {
            // 8 GB 目标设备的磁盘基线：透明帧序列峰值不超过 2 GiB。
            max_peak_disk_bytes: 2 * 1024 * 1024 * 1024,
            // 任务 0.3 的进入条件：渲染墙钟时间必须快于高光时长。
            max_realtime_ratio: 1.0,
        }
    }
}

/// 一次渲染/合成的资源实测值。
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResourceSummary {
    pub frames: u64,
    pub duration_ms: u64,
    pub render_ms: u64,
    pub peak_disk_bytes: u64,
}

impl ExportResourceSummary {
    pub fn realtime_ratio(&self) -> f64 {
        if self.duration_ms == 0 {
            return 0.0;
        }
        self.render_ms as f64 / self.duration_ms as f64
    }
}

/// 校验资源实测值是否在 8 GB 目标设备基线内。
pub fn check_export_resource_baseline(
    summary: &ExportResourceSummary,
    baseline: &ExportResourceBaseline,
) -> Result<(), ClipExportFailure> {
    if summary.peak_disk_bytes > baseline.max_peak_disk_bytes {
        return Err(failure(
            "export_disk_baseline_exceeded",
            "透明帧序列峰值磁盘占用超过目标设备基线",
        ));
    }
    if summary.realtime_ratio() > baseline.max_realtime_ratio {
        return Err(failure(
            "export_time_baseline_exceeded",
            "渲染耗时超过高光时长（未达到实时基线）",
        ));
    }
    Ok(())
}

/// 透明帧序列的抽样校验结果（任务 7.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransparentFrameSummary {
    pub frame_count: u64,
    pub checked_frames: u64,
    pub transparent_frames: u64,
}

/// 校验 `png-sequence` 输出：命名连续（1 基）、带真实 Alpha、且确实存在透明区域。
///
/// "存在透明区域"是透明中间产物的必要条件：整帧不透明会把底下的画面全部盖住，
/// 这种产物必须被拒绝，而不是靠后期色键补救（任务 7.3）。
pub fn verify_transparent_frame_sequence(
    frames_dir: &Path,
    sample_limit: usize,
) -> Result<TransparentFrameSummary, ClipExportFailure> {
    let mut frames: Vec<(u64, PathBuf)> = Vec::new();
    let entries = std::fs::read_dir(frames_dir)
        .map_err(|_| failure("overlay_frames_missing", "找不到透明帧序列目录"))?;
    for entry in entries {
        let entry = entry.map_err(|_| failure("overlay_frames_missing", "透明帧序列不可读"))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let Some(stem) = name.strip_suffix(".png") else {
            continue;
        };
        let Some(digits) = stem.strip_prefix("frame_") else {
            continue;
        };
        if digits.is_empty() || !digits.chars().all(|character| character.is_ascii_digit()) {
            continue;
        }
        let index = digits
            .parse::<u64>()
            .map_err(|_| failure("overlay_frames_missing", "帧序号无法解析"))?;
        frames.push((index, path));
    }
    if frames.is_empty() {
        return Err(failure("overlay_frames_missing", "透明帧序列为空"));
    }
    frames.sort_by_key(|(index, _)| *index);
    for (expected, (index, _)) in (1_u64..).zip(frames.iter()) {
        if expected != *index {
            return Err(failure(
                "overlay_frame_gap",
                "透明帧序号不连续（png-sequence 必须从 1 开始）",
            ));
        }
    }
    let checked = if sample_limit == 0 {
        frames.len()
    } else {
        frames.len().min(sample_limit)
    };
    let mut transparent = 0_u64;
    for (_, path) in frames.iter().take(checked) {
        let file =
            File::open(path).map_err(|_| failure("overlay_frames_unreadable", "透明帧无法读取"))?;
        let decoder = png::Decoder::new(StdBufReader::new(file));
        let mut reader = decoder
            .read_info()
            .map_err(|_| failure("overlay_frames_unreadable", "透明帧不是有效 PNG"))?;
        let mut pixels = vec![0; reader.output_buffer_size().unwrap_or(0)];
        let info = reader
            .next_frame(&mut pixels)
            .map_err(|_| failure("overlay_frames_unreadable", "透明帧无法解码"))?;
        let channels = match info.color_type {
            png::ColorType::Rgba => 4,
            png::ColorType::GrayscaleAlpha => 2,
            _ => {
                return Err(failure(
                    "overlay_frame_not_rgba",
                    "透明帧不含 Alpha 通道，无法作为透明中间产物",
                ));
            }
        };
        let alpha_offset = channels - 1;
        let mut minimum_alpha = u8::MAX;
        let pixels_len = (info.width as usize) * (info.height as usize) * channels;
        let mut offset = alpha_offset;
        while offset < pixels_len && offset < pixels.len() {
            minimum_alpha = minimum_alpha.min(pixels[offset]);
            if minimum_alpha == 0 {
                break;
            }
            offset += channels;
        }
        if minimum_alpha == u8::MAX {
            return Err(failure(
                "overlay_frame_opaque",
                "透明帧整帧不透明，会遮盖底层画面",
            ));
        }
        transparent += 1;
    }
    Ok(TransparentFrameSummary {
        frame_count: frames.len() as u64,
        checked_frames: checked as u64,
        transparent_frames: transparent,
    })
}

/// 清理特效字幕的透明帧序列与清单（成功、失败、取消后都必须调用）。
///
/// 只删除叠加层自己的目录与清单文件：工程与 ASR 产物不在这些路径内。
pub fn cleanup_overlay_layers(overlays: &[EffectCaptionOverlay]) {
    for overlay in overlays {
        let _ = std::fs::remove_file(&overlay.manifest_path);
        let _ = std::fs::remove_dir_all(&overlay.frames_dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{
        AiClipEffect, AiClipSegment, AiClipSubtitle, ClipExportBridge, build_clip_subtitle_frames,
        render_clip_subtitle_assets,
    };
    use std::io::BufWriter as StdBufWriter;

    fn source(effect: AiClipEffect) -> ClipExportSource {
        ClipExportSource {
            segment: AiClipSegment {
                id: 1,
                clip_project_id: 1,
                candidate_id: 1,
                input_id: 1,
                position: 0,
                title: "片段".to_owned(),
                source_start_ms: 1_000,
                source_end_ms: 3_000,
                volume_percent: 120,
                effect,
            },
            source_path: "/safe/input.mp4".to_owned(),
            source_fingerprint: SourceFingerprint {
                normalized_path: "/safe/input.mp4".to_owned(),
                size_bytes: 1,
                modified_at_ms: 1,
                video_id: None,
            },
            subtitles: Vec::new(),
            bridge_after: None,
        }
    }

    #[test]
    fn export_plan_only_uses_internal_effect_filters_and_h264_aac() {
        let plan = build_export_plan(
            &[source(AiClipEffect::Flash)],
            PathBuf::from("/safe/out.part.mp4"),
            ClipOutputDimensions {
                width: 1080,
                height: 1920,
            },
            Path::new("/safe/subtitles.ffconcat"),
            "h264_videotoolbox",
        )
        .unwrap();
        assert!(
            plan.arguments
                .windows(2)
                .any(|pair| pair == ["-c:v", "h264_videotoolbox"])
        );
        assert!(
            plan.arguments
                .windows(2)
                .any(|pair| pair == ["-c:a", "aac"])
        );
        assert!(
            plan.arguments
                .iter()
                .any(|argument| argument.contains("color=white"))
        );
        assert!(plan.arguments.iter().any(|argument| {
            argument.contains("scale=1080:1920:force_original_aspect_ratio=decrease,pad=1080:1920")
        }));
        assert!(
            plan.arguments
                .iter()
                .any(|argument| argument.contains("overlay="))
        );
        assert!(
            plan.arguments
                .windows(2)
                .any(|pair| { pair == ["-i", "/safe/subtitles.ffconcat"] })
        );
        assert!(
            plan.arguments
                .windows(2)
                .any(|pair| { pair == ["-f", "mp4"] })
        );
        assert!(
            !plan
                .arguments
                .iter()
                .any(|argument| argument.contains("/unsafe/filter"))
        );
    }

    #[test]
    fn bridge_is_a_complete_timeline_unit_with_original_audio() {
        let mut first = source(AiClipEffect::None);
        first.bridge_after = Some(ClipExportBridge {
            boundary_id: 7,
            asset_key: "reaction_laugh".to_owned(),
            asset_version: 2,
            source_path: "/safe/bridge.mp4".to_owned(),
            duration_ms: 1_500,
            has_audio: true,
        });
        let plan = build_export_plan(
            &[first, source(AiClipEffect::None)],
            PathBuf::from("/safe/out.part.mp4"),
            ClipOutputDimensions {
                width: 1920,
                height: 1080,
            },
            Path::new("/safe/subtitles.ffconcat"),
            "libx264",
        )
        .unwrap();
        assert_eq!(plan.total_duration_ms, 5_500);
        assert!(
            plan.arguments
                .iter()
                .any(|value| value == "/safe/bridge.mp4")
        );
        assert!(
            plan.arguments
                .iter()
                .any(|value| value.contains("concat=n=3:v=1:a=1"))
        );
        assert!(
            plan.arguments
                .iter()
                .any(|value| value.contains("[1:a]") && value.contains("volume=1.00"))
        );
    }

    #[test]
    fn normalizes_odd_output_dimensions_for_yuv420() {
        assert_eq!(normalize_dimension(1919).unwrap(), 1920);
        assert_eq!(normalize_dimension(1080).unwrap(), 1080);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_export_kills_the_process_and_removes_temporary_output() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("wait-for-cancellation");
        fs::write(&executable, "#!/bin/sh\nwhile :; do :; done\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let temporary_output = directory.path().join("clip.part.mp4");
        fs::write(&temporary_output, "unfinished").unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = execute_export(
            &executable,
            ClipExportPlan {
                arguments: Vec::new(),
                temporary_output: temporary_output.clone(),
                total_duration_ms: 1_000,
            },
            cancellation,
            |_| {},
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, "cancelled");
        assert!(!temporary_output.exists());
    }

    #[tokio::test]
    async fn exports_mixed_source_dimensions_to_a_playable_mp4() {
        use std::fs::File;
        use std::io::BufReader as StdBufReader;
        use std::process::Command as ProcessCommand;

        let fixture_ffmpeg = std::env::var_os("DY_SCREEN_FIXTURE_FFMPEG").map(PathBuf::from);
        let runtime_ffmpeg = std::env::var_os("DY_SCREEN_CLIP_FFMPEG")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("ffmpeg"));
        let runtime_ffprobe = std::env::var_os("DY_SCREEN_CLIP_FFPROBE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("ffprobe"));
        let runtime_available = ProcessCommand::new(&runtime_ffmpeg)
            .arg("-version")
            .output()
            .is_ok()
            && ProcessCommand::new(&runtime_ffprobe)
                .arg("-version")
                .output()
                .is_ok();
        if !runtime_available {
            assert_ne!(
                std::env::var("DY_SCREEN_REQUIRE_CLIP_PLATFORM_VALIDATION").as_deref(),
                Ok("1"),
                "发行验收要求包内 FFmpeg/FFprobe 可执行，不能跳过真实导出测试"
            );
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let fixture_root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/clip-platform");
        let horizontal = fixture_root.join("horizontal-h264-aac.mp4");
        let vertical = fixture_root.join("vertical-h264-aac.mp4");
        let hevc_bridge = fixture_root.join("bridge-hevc-aac.mp4");
        assert!(horizontal.is_file());
        assert!(vertical.is_file());
        assert!(hevc_bridge.is_file());

        let mut first = source(AiClipEffect::None);
        first.source_path = horizontal.to_string_lossy().into_owned();
        first.segment.source_start_ms = 100;
        first.segment.source_end_ms = 900;
        first.subtitles = vec![
            AiClipSubtitle {
                id: 1,
                clip_project_id: 1,
                stable_segment_id: "seg-first".to_owned(),
                clip_segment_id: first.segment.id,
                input_id: first.segment.input_id,
                original_text: "第一段中文字幕。".to_owned(),
                text: "第一段中文字幕。".to_owned(),
                hidden: false,
                source_start_ms: 100,
                source_end_ms: 900,
                project_start_ms: 0,
                project_end_ms: 800,
            },
            AiClipSubtitle {
                id: 3,
                clip_project_id: 1,
                stable_segment_id: "seg-overlap".to_owned(),
                clip_segment_id: first.segment.id,
                input_id: first.segment.input_id,
                original_text: "后开始的重叠字幕。".to_owned(),
                text: "后开始的重叠字幕。".to_owned(),
                hidden: false,
                source_start_ms: 300,
                source_end_ms: 700,
                project_start_ms: 200,
                project_end_ms: 600,
            },
        ];
        first.bridge_after = Some(ClipExportBridge {
            boundary_id: 1,
            asset_key: "controlled_hevc_bridge".to_owned(),
            asset_version: 1,
            source_path: hevc_bridge.to_string_lossy().into_owned(),
            duration_ms: 800,
            has_audio: true,
        });
        let mut second = source(AiClipEffect::FadeInOut);
        second.segment.id = 2;
        second.segment.position = 1;
        second.source_path = vertical.to_string_lossy().into_owned();
        second.segment.source_start_ms = 100;
        second.segment.source_end_ms = 900;
        second.subtitles = vec![
            AiClipSubtitle {
                id: 2,
                clip_project_id: 1,
                stable_segment_id: "seg-second-hidden".to_owned(),
                clip_segment_id: second.segment.id,
                input_id: second.segment.input_id,
                original_text: "隐藏字幕。".to_owned(),
                text: "隐藏字幕。".to_owned(),
                hidden: true,
                source_start_ms: 100,
                source_end_ms: 400,
                project_start_ms: 1_600,
                project_end_ms: 1_900,
            },
            AiClipSubtitle {
                id: 4,
                clip_project_id: 1,
                stable_segment_id: "seg-second-visible".to_owned(),
                clip_segment_id: second.segment.id,
                input_id: second.segment.input_id,
                original_text: "桥接结束后的字幕。".to_owned(),
                text: "桥接结束后的字幕。".to_owned(),
                hidden: false,
                source_start_ms: 400,
                source_end_ms: 900,
                project_start_ms: 1_900,
                project_end_ms: 2_400,
            },
        ];
        let output = directory.path().join("mixed.part.mp4");
        let output_dimensions = probe_output_dimensions(&runtime_ffprobe, &horizontal)
            .await
            .unwrap();
        let sources = [first, second];
        let subtitles = sources
            .iter()
            .flat_map(|source| source.subtitles.iter().cloned())
            .collect::<Vec<_>>();
        validate_export_subtitles(&sources).unwrap();
        let display_frames = build_clip_subtitle_frames(&subtitles, output_dimensions, 2_400);
        assert!(display_frames.iter().any(|frame| {
            frame.project_start_ms <= 300 && frame.project_end_ms > 300 && frame.subtitle_id == 3
        }));
        assert!(!display_frames.iter().any(|frame| frame.subtitle_id == 2));
        assert!(
            !display_frames
                .iter()
                .any(|frame| { frame.project_start_ms < 1_600 && frame.project_end_ms > 800 })
        );
        assert!(display_frames.iter().any(|frame| {
            frame.subtitle_id == 4
                && frame.project_start_ms >= 1_900
                && frame.project_end_ms <= 2_400
        }));
        let mut missing_subtitles = sources.clone();
        missing_subtitles[1].subtitles.clear();
        assert_eq!(
            validate_export_subtitles(&missing_subtitles)
                .unwrap_err()
                .code,
            "subtitles_incomplete"
        );
        let subtitle_assets =
            render_clip_subtitle_assets(&subtitles, output_dimensions, 2_400).unwrap();
        let available_encoders = ProcessCommand::new(&runtime_ffmpeg)
            .args(["-hide_banner", "-encoders"])
            .output()
            .unwrap();
        let video_encoder = if String::from_utf8_lossy(&available_encoders.stdout)
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some("libx264"))
        {
            "libx264"
        } else {
            select_clip_video_encoder(&runtime_ffmpeg).await.unwrap()
        };
        let plan = build_export_plan(
            &sources,
            output.clone(),
            output_dimensions,
            subtitle_assets.manifest_path(),
            video_encoder,
        )
        .unwrap();
        let exported = execute_export(&runtime_ffmpeg, plan, CancellationToken::new(), |_| {})
            .await
            .unwrap();
        assert_eq!(exported, output);
        assert!(exported.is_file());

        let output = ProcessCommand::new(&runtime_ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name,width,height",
                "-of",
                "csv=p=0",
            ])
            .arg(&exported)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "h264,320,240"
        );
        let output = ProcessCommand::new(&runtime_ffprobe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "a:0",
                "-show_entries",
                "stream=codec_name",
                "-of",
                "csv=p=0",
            ])
            .arg(&exported)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "aac");
        let output = ProcessCommand::new(&runtime_ffprobe)
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "default=nw=1:nk=1",
            ])
            .arg(&exported)
            .output()
            .unwrap();
        assert!(output.status.success());
        let duration = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<f64>()
            .unwrap();
        assert!((2.30..=2.55).contains(&duration));

        if let Some(fixture_ffmpeg) = fixture_ffmpeg.as_ref() {
            let frame = directory.path().join("subtitle-frame.png");
            let status = ProcessCommand::new(fixture_ffmpeg)
                .args(["-y", "-v", "error", "-ss", "0.4", "-i"])
                .arg(&exported)
                .args(["-frames:v", "1"])
                .arg(&frame)
                .status()
                .unwrap();
            assert!(status.success());
            let decoder = png::Decoder::new(StdBufReader::new(File::open(frame).unwrap()));
            let mut reader = decoder.read_info().unwrap();
            let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
            let info = reader.next_frame(&mut pixels).unwrap();
            let channels = match info.color_type {
                png::ColorType::Rgb => 3,
                png::ColorType::Rgba => 4,
                color_type => panic!("字幕帧颜色类型不受支持：{color_type:?}"),
            };
            let mut dark_subtitle_pixels = 0;
            let mut bright_subtitle_pixels = 0;
            for y in 160..info.height as usize {
                for x in 0..info.width as usize {
                    let offset = (y * info.width as usize + x) * channels;
                    let (red, green, blue) =
                        (pixels[offset], pixels[offset + 1], pixels[offset + 2]);
                    if red < 180 && green < 90 && blue < 90 {
                        dark_subtitle_pixels += 1;
                    }
                    if red > 180 && green > 180 && blue > 180 {
                        bright_subtitle_pixels += 1;
                    }
                }
            }
            assert!(dark_subtitle_pixels > 100, "字幕区域没有检测到黑色文字描边");
            assert!(
                bright_subtitle_pixels > 20,
                "字幕区域没有检测到白色文字像素"
            );
        }

        let mut all_hidden_sources = sources.to_vec();
        for source in &mut all_hidden_sources {
            for subtitle in &mut source.subtitles {
                subtitle.hidden = true;
            }
        }
        validate_export_subtitles(&all_hidden_sources).unwrap();
        let all_hidden_subtitles = all_hidden_sources
            .iter()
            .flat_map(|source| source.subtitles.iter().cloned())
            .collect::<Vec<_>>();
        let hidden_assets =
            render_clip_subtitle_assets(&all_hidden_subtitles, output_dimensions, 2_400).unwrap();
        let hidden_output = directory.path().join("all-hidden.part.mp4");
        let hidden_plan = build_export_plan(
            &all_hidden_sources,
            hidden_output.clone(),
            output_dimensions,
            hidden_assets.manifest_path(),
            video_encoder,
        )
        .unwrap();
        execute_export(
            &runtime_ffmpeg,
            hidden_plan,
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
        assert!(hidden_output.is_file());
        if let Some(fixture_ffmpeg) = fixture_ffmpeg.as_ref() {
            let hidden_frame = directory.path().join("all-hidden-frame.png");
            let status = ProcessCommand::new(fixture_ffmpeg)
                .args(["-y", "-v", "error", "-ss", "0.4", "-i"])
                .arg(&hidden_output)
                .args(["-frames:v", "1"])
                .arg(&hidden_frame)
                .status()
                .unwrap();
            assert!(status.success());
            let decoder = png::Decoder::new(StdBufReader::new(File::open(hidden_frame).unwrap()));
            let mut reader = decoder.read_info().unwrap();
            let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
            let info = reader.next_frame(&mut pixels).unwrap();
            let channels = match info.color_type {
                png::ColorType::Rgb => 3,
                png::ColorType::Rgba => 4,
                color_type => panic!("隐藏字幕帧颜色类型不受支持：{color_type:?}"),
            };
            let mut hidden_dark_pixels = 0;
            let mut hidden_bright_pixels = 0;
            for y in 160..info.height as usize {
                for x in 0..info.width as usize {
                    let offset = (y * info.width as usize + x) * channels;
                    let (red, green, blue) =
                        (pixels[offset], pixels[offset + 1], pixels[offset + 2]);
                    if red < 180 && green < 90 && blue < 90 {
                        hidden_dark_pixels += 1;
                    }
                    if red > 180 && green > 180 && blue > 180 {
                        hidden_bright_pixels += 1;
                    }
                }
            }
            assert!(hidden_dark_pixels < 20, "全部隐藏后仍检测到黑色字幕描边");
            assert!(hidden_bright_pixels < 20, "全部隐藏后仍检测到白色字幕字形");
        }
    }

    // ---- 步骤 3 任务组 7：特效字幕透明层合成 ----

    fn overlay(directory: &Path, start_ms: u64, end_ms: u64) -> EffectCaptionOverlay {
        EffectCaptionOverlay {
            manifest_path: directory.join("effect.ffconcat"),
            frames_dir: directory.join("frames"),
            start_ms,
            end_ms,
        }
    }

    fn dimensions() -> ClipOutputDimensions {
        ClipOutputDimensions {
            width: 1080,
            height: 1920,
        }
    }

    #[test]
    fn effect_overlay_uses_the_shared_template_and_final_yuv420p_only() {
        let directory = tempfile::tempdir().unwrap();
        let overlay = overlay(directory.path(), 500, 1_500);
        std::fs::write(&overlay.manifest_path, "ffconcat version 1.0\n").unwrap();
        let plan = build_export_plan_with_overlays(
            &[source(AiClipEffect::None)],
            PathBuf::from("/safe/out.part.mp4"),
            dimensions(),
            Path::new("/safe/subtitles.ffconcat"),
            "libx264",
            std::slice::from_ref(&overlay),
            &[],
        )
        .unwrap();
        let filter = plan
            .arguments
            .iter()
            .position(|argument| argument == "-filter_complex")
            .map(|index| plan.arguments[index + 1].clone())
            .expect("必须存在 filter_complex");
        // 两类字幕共用同一 overlay 模板，只是特效层按整帧叠加并限定时间范围。
        assert!(filter.contains("overlay=0:H-h-"), "基础字幕仍是底部叠加");
        assert!(filter.contains("overlay=0:0"), "特效字幕按整帧叠加");
        assert!(filter.contains("enable='between(t,0.500, 1.500)'"));
        assert!(filter.contains("[vout]"));
        assert!(filter.contains("format=rgba"));
        // 两层透明中间产物都必须是 rgba，且都不得被转成 yuv420p；也不允许色键抠图。
        let chains: Vec<&str> = filter.split(';').collect();
        for layer in ["[subtitles]", "[effect0]"] {
            let chain = chains
                .iter()
                .find(|chain| chain.ends_with(layer))
                .unwrap_or_else(|| panic!("找不到产出 {layer} 的滤镜链"));
            assert!(
                chain.contains("format=rgba"),
                "透明层 {layer} 必须保持 rgba：{chain}"
            );
            assert!(
                !chain.contains("yuv420p"),
                "透明层 {layer} 不得转成 yuv420p：{chain}"
            );
        }
        assert!(!filter.contains("colorkey") && !filter.contains("chromakey"));
        // 最终编码阶段才转 yuv420p，且只出现一次。
        let pix_fmt_count = plan
            .arguments
            .windows(2)
            .filter(|pair| pair[0] == "-pix_fmt" && pair[1] == "yuv420p")
            .count();
        assert_eq!(pix_fmt_count, 1);
        // 叠加层作为独立输入接入（ffconcat 清单）。
        let concat_inputs = plan
            .arguments
            .iter()
            .filter(|argument| {
                *argument == "/safe/subtitles.ffconcat" || argument.ends_with("effect.ffconcat")
            })
            .count();
        assert_eq!(concat_inputs, 2, "基础字幕与特效字幕各一个输入");
        assert_eq!(plan.total_duration_ms, 2_000);
    }

    #[test]
    fn overlapping_text_layers_are_rejected_before_planning() {
        let directory = tempfile::tempdir().unwrap();
        let overlay = overlay(directory.path(), 500, 1_500);
        let rejection = build_export_plan_with_overlays(
            &[source(AiClipEffect::None)],
            PathBuf::from("/safe/out.part.mp4"),
            dimensions(),
            Path::new("/safe/subtitles.ffconcat"),
            "libx264",
            std::slice::from_ref(&overlay),
            &[TextLayerRange {
                start_ms: 1_000,
                end_ms: 1_800,
            }],
        )
        .unwrap_err();
        assert_eq!(rejection.code, "dual_text_layers");

        // 不相邻但连续的两层（基础字幕 0–500，特效 500–1500）是允许的。
        assert!(
            validate_single_text_layer(
                &[TextLayerRange {
                    start_ms: 0,
                    end_ms: 500,
                }],
                std::slice::from_ref(&overlay),
            )
            .is_ok()
        );
    }

    #[test]
    fn overlay_beyond_project_duration_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let overlay = overlay(directory.path(), 1_000, 9_000);
        let rejection = build_export_plan_with_overlays(
            &[source(AiClipEffect::None)],
            PathBuf::from("/safe/out.part.mp4"),
            dimensions(),
            Path::new("/safe/subtitles.ffconcat"),
            "libx264",
            std::slice::from_ref(&overlay),
            &[],
        )
        .unwrap_err();
        assert_eq!(rejection.code, "invalid_overlay_range");
    }

    fn write_rgba_frame(path: &Path, alpha: u8) {
        let file = File::create(path).unwrap();
        let mut encoder = png::Encoder::new(StdBufWriter::new(file), 8, 8);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        let mut pixels = vec![255_u8; 8 * 8 * 4];
        for chunk in pixels.chunks_exact_mut(4) {
            chunk[3] = alpha;
        }
        writer.write_image_data(&pixels).unwrap();
    }

    fn write_rgb_frame(path: &Path) {
        let file = File::create(path).unwrap();
        let mut encoder = png::Encoder::new(StdBufWriter::new(file), 8, 8);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[255_u8; 8 * 8 * 3]).unwrap();
    }

    #[test]
    fn transparent_frame_sequence_must_be_continuous_rgba_and_actually_transparent() {
        let directory = tempfile::tempdir().unwrap();
        let frames = directory.path().join("frames");
        std::fs::create_dir_all(&frames).unwrap();
        for index in 1..=3 {
            write_rgba_frame(&frames.join(format!("frame_{index:06}.png")), 0);
        }
        let summary = verify_transparent_frame_sequence(&frames, 2).unwrap();
        assert_eq!(summary.frame_count, 3);
        assert_eq!(summary.checked_frames, 2);
        assert_eq!(summary.transparent_frames, 2);

        // 整帧不透明 → 拒绝（不能靠后期色键补救）
        write_rgba_frame(&frames.join("frame_000002.png"), 255);
        assert_eq!(
            verify_transparent_frame_sequence(&frames, 3)
                .unwrap_err()
                .code,
            "overlay_frame_opaque"
        );
        write_rgba_frame(&frames.join("frame_000002.png"), 0);

        // 序号不连续 → 拒绝
        std::fs::remove_file(frames.join("frame_000002.png")).unwrap();
        assert_eq!(
            verify_transparent_frame_sequence(&frames, 3)
                .unwrap_err()
                .code,
            "overlay_frame_gap"
        );

        // 无 Alpha 通道 → 拒绝
        std::fs::write(frames.join("frame_000002.png"), b"").unwrap();
        write_rgb_frame(&frames.join("frame_000002.png"));
        assert_eq!(
            verify_transparent_frame_sequence(&frames, 3)
                .unwrap_err()
                .code,
            "overlay_frame_not_rgba"
        );

        // 空目录 → 拒绝
        let empty = directory.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(
            verify_transparent_frame_sequence(&empty, 3)
                .unwrap_err()
                .code,
            "overlay_frames_missing"
        );
    }

    #[test]
    fn cleanup_removes_overlay_layers_but_keeps_project_and_asr_artifacts() {
        let directory = tempfile::tempdir().unwrap();
        let overlay = overlay(directory.path(), 0, 1_000);
        std::fs::create_dir_all(&overlay.frames_dir).unwrap();
        write_rgba_frame(&overlay.frames_dir.join("frame_000001.png"), 0);
        std::fs::write(&overlay.manifest_path, "ffconcat version 1.0\n").unwrap();
        let project = directory.path().join("project.json");
        let asr = directory.path().join("asr-artifact.json");
        std::fs::write(&project, "project").unwrap();
        std::fs::write(&asr, "asr").unwrap();

        cleanup_overlay_layers(std::slice::from_ref(&overlay));
        assert!(!overlay.frames_dir.exists());
        assert!(!overlay.manifest_path.exists());
        assert!(project.exists(), "工程产物必须保留");
        assert!(asr.exists(), "ASR 产物必须保留");
        // 幂等
        cleanup_overlay_layers(std::slice::from_ref(&overlay));
    }

    #[test]
    fn export_resource_baseline_covers_disk_and_realtime() {
        let baseline = ExportResourceBaseline::default();
        let within = ExportResourceSummary {
            frames: 450,
            duration_ms: 15_000,
            render_ms: 12_000,
            peak_disk_bytes: 400 * 1024 * 1024,
        };
        assert!(within.realtime_ratio() < 1.0);
        assert!(check_export_resource_baseline(&within, &baseline).is_ok());

        // 沙箱实测：原生 png-sequence 路径是实时的 2.95–5.22 倍 → 必须判失败
        let native = ExportResourceSummary {
            render_ms: 45_000,
            ..within
        };
        assert_eq!(
            check_export_resource_baseline(&native, &baseline)
                .unwrap_err()
                .code,
            "export_time_baseline_exceeded"
        );

        let heavy = ExportResourceSummary {
            peak_disk_bytes: 3 * 1024 * 1024 * 1024,
            ..within
        };
        assert_eq!(
            check_export_resource_baseline(&heavy, &baseline)
                .unwrap_err()
                .code,
            "export_disk_baseline_exceeded"
        );
    }

    /// 任务 7.2 / 7.3：特效字幕透明层与原画面合成后，必须保留原音频与工程总时长，
    /// 只在 `enable` 时间窗内出现，且不依赖色键抠图。
    #[tokio::test]
    async fn exports_effect_caption_overlay_keeping_audio_and_duration() {
        use std::process::Command as ProcessCommand;

        let runtime_ffmpeg = std::env::var_os("DY_SCREEN_CLIP_FFMPEG")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("ffmpeg"));
        let runtime_ffprobe = std::env::var_os("DY_SCREEN_CLIP_FFPROBE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("ffprobe"));
        let runtime_available = ProcessCommand::new(&runtime_ffmpeg)
            .arg("-version")
            .output()
            .is_ok()
            && ProcessCommand::new(&runtime_ffprobe)
                .arg("-version")
                .output()
                .is_ok();
        if !runtime_available {
            assert_ne!(
                std::env::var("DY_SCREEN_REQUIRE_CLIP_PLATFORM_VALIDATION").as_deref(),
                Ok("1"),
                "发行验收要求包内 FFmpeg/FFprobe 可执行，不能跳过特效字幕合成测试"
            );
            return;
        }
        let fixture_root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/clip-platform");
        let horizontal = fixture_root.join("horizontal-h264-aac.mp4");
        assert!(horizontal.is_file(), "缺少受控样本 {horizontal:?}");
        let dimensions = probe_output_dimensions(&runtime_ffprobe, &horizontal)
            .await
            .unwrap();

        let mut first = source(AiClipEffect::None);
        first.source_path = horizontal.to_string_lossy().into_owned();
        first.segment.source_start_ms = 100;
        first.segment.source_end_ms = 900;
        // 基础字幕全部隐藏：本用例验证的是特效层，基础层留空以便"同一时间只有一层文字"。
        first.subtitles = vec![AiClipSubtitle {
            id: 9,
            clip_project_id: 1,
            stable_segment_id: "seg-hidden".to_owned(),
            clip_segment_id: first.segment.id,
            input_id: first.segment.input_id,
            original_text: "隐藏字幕。".to_owned(),
            text: "隐藏字幕。".to_owned(),
            hidden: true,
            source_start_ms: 100,
            source_end_ms: 900,
            project_start_ms: 0,
            project_end_ms: 800,
        }];
        validate_export_subtitles(std::slice::from_ref(&first)).unwrap();
        let sources = [first];

        // 透明帧序列：0–400ms 出现红色方块，其余时间整帧透明。
        let directory = tempfile::tempdir().unwrap();
        let frames_dir = directory.path().join("effect-frames");
        std::fs::create_dir_all(&frames_dir).unwrap();
        let mut manifest = String::from("ffconcat version 1.0\n");
        for index in 0..10_u32 {
            let name = format!("frame_{:06}.png", index + 1);
            write_overlay_frame(&frames_dir.join(&name), dimensions, true);
            manifest.push_str(&format!("file '{name}'\nduration 0.040000\n"));
        }
        manifest.push_str("file 'frame_000010.png'\n");
        std::fs::write(frames_dir.join("effect.ffconcat"), manifest).unwrap();
        let overlay = EffectCaptionOverlay {
            manifest_path: frames_dir.join("effect.ffconcat"),
            frames_dir: frames_dir.clone(),
            start_ms: 0,
            end_ms: 400,
        };

        // 复用既有基础字幕层（隐藏字幕不产生文字，但清单仍是合法输入）。
        let subtitles = sources
            .iter()
            .flat_map(|source| source.subtitles.iter().cloned())
            .collect::<Vec<_>>();
        let subtitle_assets = render_clip_subtitle_assets(&subtitles, dimensions, 800).unwrap();
        let available_encoders = ProcessCommand::new(&runtime_ffmpeg)
            .args(["-hide_banner", "-encoders"])
            .output()
            .unwrap();
        let video_encoder = if String::from_utf8_lossy(&available_encoders.stdout)
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some("libx264"))
        {
            "libx264"
        } else {
            select_clip_video_encoder(&runtime_ffmpeg).await.unwrap()
        };
        let output = directory.path().join("effect.part.mp4");
        let plan = build_export_plan_with_overlays(
            &sources,
            output.clone(),
            dimensions,
            subtitle_assets.manifest_path(),
            video_encoder,
            std::slice::from_ref(&overlay),
            &[],
        )
        .unwrap();
        assert_eq!(plan.total_duration_ms, 800);
        execute_export(
            &runtime_ffmpeg,
            plan.clone(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
        assert!(output.is_file(), "导出必须产出成片");

        // 音频与总时长必须保留。
        let probe = ProcessCommand::new(&runtime_ffprobe)
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type",
                "-of",
                "csv=p=0",
            ])
            .arg(&output)
            .output()
            .unwrap();
        let streams = String::from_utf8_lossy(&probe.stdout);
        assert!(streams.contains("audio"), "合成不得丢失原音频：{streams}");
        let duration_probe = ProcessCommand::new(&runtime_ffprobe)
            .args([
                "-v",
                "error",
                "-show_entries",
                "format=duration",
                "-of",
                "csv=p=0",
            ])
            .arg(&output)
            .output()
            .unwrap();
        let duration: f64 = String::from_utf8_lossy(&duration_probe.stdout)
            .trim()
            .parse()
            .unwrap();
        assert!(
            (duration - 0.8).abs() < 0.2,
            "工程总时长必须保持约 0.8s，实际 {duration}"
        );

        // 时间窗内出现特效层，窗外不可见（enable 生效）。
        assert_overlay_visible(
            &runtime_ffmpeg,
            &output,
            &directory.path().join("in.png"),
            0.2,
            true,
        );
        assert_overlay_visible(
            &runtime_ffmpeg,
            &output,
            &directory.path().join("out.png"),
            0.6,
            false,
        );

        // 成功导出后清理透明层，工程与 ASR 产物不受影响。
        cleanup_overlay_layers(std::slice::from_ref(&overlay));
        assert!(!frames_dir.exists());
        assert!(output.is_file(), "成片不得被清理");
    }

    fn write_overlay_frame(path: &Path, dimensions: ClipOutputDimensions, with_square: bool) {
        let file = File::create(path).unwrap();
        let mut encoder =
            png::Encoder::new(StdBufWriter::new(file), dimensions.width, dimensions.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        let mut pixels = vec![0_u8; (dimensions.width * dimensions.height * 4) as usize];
        if with_square {
            let (x0, x1) = (dimensions.width / 4, dimensions.width * 3 / 4);
            let (y0, y1) = (dimensions.height / 4, dimensions.height * 3 / 4);
            for y in y0..y1 {
                for x in x0..x1 {
                    let offset = ((y * dimensions.width + x) * 4) as usize;
                    // 唯一色：受控样本底片是纯红，特效层用 (7,251,3) 才能被明确识别。
                    pixels[offset] = 7;
                    pixels[offset + 1] = 251;
                    pixels[offset + 2] = 3;
                    pixels[offset + 3] = 255;
                }
            }
        }
        writer.write_image_data(&pixels).unwrap();
    }

    fn assert_overlay_visible(
        ffmpeg: &Path,
        output: &Path,
        frame_path: &Path,
        seconds: f64,
        expected: bool,
    ) {
        use std::process::Command as ProcessCommand;
        let status = ProcessCommand::new(ffmpeg)
            .args(["-y", "-v", "error", "-ss", &format!("{seconds:.3}"), "-i"])
            .arg(output)
            .args(["-frames:v", "1"])
            .arg(frame_path)
            .status()
            .unwrap();
        assert!(status.success());
        let decoder = png::Decoder::new(StdBufReader::new(File::open(frame_path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        let channels = match info.color_type {
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
            color_type => panic!("颜色类型不受支持：{color_type:?}"),
        };
        let mut layer_pixels = 0_usize;
        for y in 0..info.height as usize {
            for x in 0..info.width as usize {
                let offset = (y * info.width as usize + x) * channels;
                let (red, green, blue) = (pixels[offset], pixels[offset + 1], pixels[offset + 2]);
                // 容忍编码色度抽样带来的轻微偏移，但仍能排除底片的纯红。
                if (red as i32 - 7).abs() <= 12 && (green as i32 - 251).abs() <= 12 && blue <= 24 {
                    layer_pixels += 1;
                }
            }
        }
        let will_fail = if expected {
            layer_pixels <= 50
        } else {
            layer_pixels != 0
        };
        if will_fail {
            // 诊断用：失败时打印出现最多的颜色，便于区分"未合成"与"色度偏移"。
            let mut counts: std::collections::HashMap<(u8, u8, u8), usize> =
                std::collections::HashMap::new();
            for y in 0..info.height as usize {
                for x in (0..info.width as usize).step_by(7) {
                    let offset = (y * info.width as usize + x) * channels;
                    *counts
                        .entry((pixels[offset], pixels[offset + 1], pixels[offset + 2]))
                        .or_default() += 1;
                }
            }
            let mut ranked: Vec<((u8, u8, u8), usize)> = counts.into_iter().collect();
            ranked.sort_by_key(|entry| std::cmp::Reverse(entry.1));
            eprintln!(
                "[{seconds}s] top colors: {:?}",
                &ranked[..ranked.len().min(4)]
            );
        }
        if expected {
            assert!(
                layer_pixels > 50,
                "{seconds}s 处应当出现特效字幕层，实际命中像素 {layer_pixels}"
            );
        } else {
            assert_eq!(
                layer_pixels, 0,
                "{seconds}s 处超出 enable 时间窗，不应出现特效字幕层"
            );
        }
    }
}
