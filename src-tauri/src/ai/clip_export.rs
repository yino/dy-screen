//! 剪辑工程的受控 FFmpeg 导出。
//!
//! 此模块只接受 repository 提供的冻结来源和内置效果枚举；不会解析前端传来的
//! 路径、滤镜字符串或 FFmpeg 参数。

use std::path::{Path, PathBuf};
use std::process::Stdio;

use dy_screen::asr::FrozenMediaSource;
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
    let mut filter_parts = Vec::with_capacity(sources.len() * 2 + 1);
    let mut concat_inputs = String::new();
    let mut total_duration_ms = 0_u64;
    for (index, source) in sources.iter().enumerate() {
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
            "[{index}:v]setpts=PTS-STARTPTS,scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,format=yuv420p{effect_filter}[v{index}]",
            width = output_dimensions.width,
            height = output_dimensions.height,
        ));
        filter_parts.push(format!(
            "[{index}:a]asetpts=PTS-STARTPTS,aformat=sample_rates=48000:channel_layouts=stereo,volume={:.2}[a{index}]",
            source.segment.volume_percent as f64 / 100.0,
        ));
        concat_inputs.push_str(&format!("[v{index}][a{index}]"));
    }
    let subtitle_input_index = sources.len();
    arguments.extend([
        "-f".to_owned(),
        "concat".to_owned(),
        "-safe".to_owned(),
        "0".to_owned(),
        "-i".to_owned(),
        subtitle_manifest.to_string_lossy().into_owned(),
    ]);
    filter_parts.push(format!(
        "{concat_inputs}concat=n={}:v=1:a=1[vjoined][aout]",
        sources.len(),
    ));
    filter_parts.push(format!(
        "[{subtitle_input_index}:v]format=rgba,setpts=PTS-STARTPTS[subtitles]"
    ));
    let bottom_margin = (output_dimensions.height as f64 * 0.04).round() as u32;
    filter_parts.push(format!(
        "[vjoined][subtitles]overlay=0:H-h-{bottom_margin}:format=auto:eof_action=pass[vout]"
    ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::{AiClipEffect, AiClipSegment};
    #[cfg(unix)]
    use crate::ai::{AiClipSubtitle, render_clip_subtitle_assets};

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

    #[cfg(unix)]
    #[tokio::test]
    async fn exports_mixed_source_dimensions_to_a_playable_mp4() {
        use std::fs::File;
        use std::io::BufReader as StdBufReader;
        use std::process::Command as ProcessCommand;

        let fixture_ffmpeg = PathBuf::from("ffmpeg");
        let runtime_ffmpeg = std::env::var_os("DY_SCREEN_CLIP_FFMPEG")
            .map(PathBuf::from)
            .unwrap_or_else(|| fixture_ffmpeg.clone());
        let runtime_ffprobe = std::env::var_os("DY_SCREEN_CLIP_FFPROBE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("ffprobe"));
        if ProcessCommand::new(&fixture_ffmpeg)
            .arg("-version")
            .output()
            .is_err()
            || ProcessCommand::new(&runtime_ffmpeg)
                .arg("-version")
                .output()
                .is_err()
            || ProcessCommand::new(&runtime_ffprobe)
                .arg("-version")
                .output()
                .is_err()
        {
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let horizontal = directory.path().join("horizontal.mp4");
        let vertical = directory.path().join("vertical.mp4");
        for (path, color, size) in [
            (&horizontal, "red", "320x240"),
            (&vertical, "blue", "240x320"),
        ] {
            let status = ProcessCommand::new(&fixture_ffmpeg)
                .args([
                    "-y",
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("color=c={color}:s={size}:d=1"),
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=1",
                    "-shortest",
                    "-c:v",
                    "libx264",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:a",
                    "aac",
                ])
                .arg(path)
                .status()
                .unwrap();
            assert!(status.success());
        }

        let mut first = source(AiClipEffect::None);
        first.source_path = horizontal.to_string_lossy().into_owned();
        first.segment.source_start_ms = 100;
        first.segment.source_end_ms = 900;
        first.subtitles = vec![AiClipSubtitle {
            stable_segment_id: "seg-first".to_owned(),
            clip_segment_id: first.segment.id,
            input_id: first.segment.input_id,
            normalized_text: "第一段中文字幕。".to_owned(),
            source_start_ms: 100,
            source_end_ms: 900,
            project_start_ms: 0,
            project_end_ms: 800,
        }];
        let mut second = source(AiClipEffect::FadeInOut);
        second.segment.id = 2;
        second.segment.position = 1;
        second.source_path = vertical.to_string_lossy().into_owned();
        second.segment.source_start_ms = 100;
        second.segment.source_end_ms = 900;
        second.subtitles = vec![AiClipSubtitle {
            stable_segment_id: "seg-second".to_owned(),
            clip_segment_id: second.segment.id,
            input_id: second.segment.input_id,
            normalized_text: "第二段中文字幕。".to_owned(),
            source_start_ms: 100,
            source_end_ms: 900,
            project_start_ms: 800,
            project_end_ms: 1_600,
        }];
        let output = directory.path().join("mixed.part.mp4");
        let output_dimensions = probe_output_dimensions(&runtime_ffprobe, &horizontal)
            .await
            .unwrap();
        let sources = [first, second];
        let subtitles = sources
            .iter()
            .flat_map(|source| source.subtitles.iter().cloned())
            .collect::<Vec<_>>();
        let subtitle_assets =
            render_clip_subtitle_assets(&subtitles, output_dimensions, 1_600).unwrap();
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

        let frame = directory.path().join("subtitle-frame.png");
        let status = ProcessCommand::new(&fixture_ffmpeg)
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
                let (red, green, blue) = (pixels[offset], pixels[offset + 1], pixels[offset + 2]);
                if red < 180 && green < 90 && blue < 90 {
                    dark_subtitle_pixels += 1;
                }
                if red > 180 && green > 180 && blue > 180 {
                    bright_subtitle_pixels += 1;
                }
            }
        }
        assert!(
            dark_subtitle_pixels > 100,
            "字幕区域没有检测到半透明黑色背景"
        );
        assert!(
            bright_subtitle_pixels > 20,
            "字幕区域没有检测到白色文字像素"
        );
    }
}
