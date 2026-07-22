//! 供应商无关的 VAD 契约及 whisper.cpp Silero sidecar Adapter。
//!
//! 输出区间始终映射回源音频毫秒时间，并通过可配置边界保护避免截断首尾音节。

use std::path::PathBuf;
use std::process::Stdio;

use async_trait::async_trait;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use super::{AsrError, AsrErrorKind, EngineResult, PreparedAudio, SpeechRegion};

/// Silero VAD 参数使用整数保存，避免配置指纹受浮点序列化差异影响。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VadConfig {
    pub threshold_permille: u16,
    pub minimum_speech_ms: u64,
    pub minimum_silence_ms: u64,
    pub maximum_speech_seconds: u64,
    pub speech_padding_ms: u64,
    pub samples_overlap_ms: u64,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            threshold_permille: 500,
            minimum_speech_ms: 250,
            minimum_silence_ms: 100,
            maximum_speech_seconds: 30,
            speech_padding_ms: 500,
            samples_overlap_ms: 100,
        }
    }
}

impl VadConfig {
    pub fn validate(&self) -> EngineResult<()> {
        if !(1..1_000).contains(&self.threshold_permille)
            || self.minimum_speech_ms == 0
            || self.maximum_speech_seconds == 0
            || self.speech_padding_ms > 5_000
        {
            return Err(AsrError::invalid_input(
                "invalid_vad_config",
                "VAD 参数无效",
            ));
        }
        Ok(())
    }
}

#[async_trait]
pub trait VadEngine: Send + Sync {
    async fn detect(
        &self,
        audio: &PreparedAudio,
        config: &VadConfig,
        cancellation: CancellationToken,
    ) -> EngineResult<Vec<SpeechRegion>>;
}

/// 调用 whisper.cpp 官方 `vad-speech-segments` 示例 sidecar 的 Silero VAD 适配器。
#[derive(Debug, Clone)]
pub struct WhisperCppVadEngine {
    executable: PathBuf,
    model: PathBuf,
    threads: usize,
    use_gpu: bool,
}

impl WhisperCppVadEngine {
    pub fn new(executable: PathBuf, model: PathBuf, threads: usize, use_gpu: bool) -> Self {
        Self {
            executable,
            model,
            threads: threads.max(1),
            use_gpu,
        }
    }
}

#[async_trait]
impl VadEngine for WhisperCppVadEngine {
    async fn detect(
        &self,
        audio: &PreparedAudio,
        config: &VadConfig,
        cancellation: CancellationToken,
    ) -> EngineResult<Vec<SpeechRegion>> {
        config.validate()?;
        if cancellation.is_cancelled() {
            return Err(AsrError::cancelled());
        }
        let mut command = Command::new(&self.executable);
        command
            .arg("--file")
            .arg(&audio.path)
            .arg("--vad-model")
            .arg(&self.model)
            .arg("--threads")
            .arg(self.threads.to_string())
            .arg("--vad-threshold")
            .arg(format!(
                "{:.3}",
                f32::from(config.threshold_permille) / 1_000.0
            ))
            .arg("--vad-min-speech-duration-ms")
            .arg(config.minimum_speech_ms.to_string())
            .arg("--vad-min-silence-duration-ms")
            .arg(config.minimum_silence_ms.to_string())
            .arg("--vad-max-speech-duration-s")
            .arg(config.maximum_speech_seconds.to_string())
            .arg("--vad-speech-pad-ms")
            .arg(config.speech_padding_ms.to_string())
            .arg("--vad-samples-overlap")
            .arg(format!("{:.3}", config.samples_overlap_ms as f64 / 1_000.0))
            .arg("--no-prints")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if self.use_gpu {
            command.arg("--use-gpu");
        }
        let output = tokio::select! {
            _ = cancellation.cancelled() => return Err(AsrError::cancelled()),
            result = command.output() => result.map_err(|_| AsrError::new(
                AsrErrorKind::EnvironmentUnavailable,
                "vad_sidecar_unavailable",
                "VAD 组件不可用，请修复应用安装",
                false,
            ))?,
        };
        if !output.status.success() {
            return Err(AsrError::new(
                AsrErrorKind::ProcessFailed,
                "vad_failed",
                "无法检测有效人声",
                true,
            ));
        }
        parse_segments(&String::from_utf8_lossy(&output.stdout), audio.duration_ms)
    }
}

fn parse_segments(output: &str, duration_ms: u64) -> EngineResult<Vec<SpeechRegion>> {
    let mut regions = Vec::<SpeechRegion>::new();
    for line in output.lines().map(str::trim) {
        if !line.starts_with("Speech segment ") {
            continue;
        }
        let (_, times) = line.split_once("start = ").ok_or_else(malformed_output)?;
        let (start, end) = times.split_once(", end = ").ok_or_else(malformed_output)?;
        // v1.9.1 的官方 vad-speech-segments 示例直接打印底层 centisecond 值，
        // README 中却把示例标成秒。适配器按锁定版本的真实输出转换为毫秒。
        let raw_start_ms = centiseconds_to_ms(start).ok_or_else(malformed_output)?;
        let raw_end_ms = centiseconds_to_ms(end).ok_or_else(malformed_output)?;
        if raw_start_ms >= raw_end_ms {
            return Err(malformed_output());
        }
        // FFprobe 的容器时长与 FFmpeg 解码后的音频尾部可能存在少量差异。
        // 完全落在冻结媒体时长之外的 VAD 尾段没有可映射的源时间，应安全忽略；
        // 不能因此让一个长视频的全部有效人声结果失败。
        if raw_start_ms >= duration_ms {
            continue;
        }
        let start_ms = raw_start_ms;
        let end_ms = raw_end_ms.min(duration_ms);
        if let Some(previous) = regions.last_mut()
            && start_ms <= previous.end_ms
        {
            previous.end_ms = previous.end_ms.max(end_ms);
            continue;
        }
        regions.push(SpeechRegion { start_ms, end_ms });
    }
    Ok(regions)
}

fn centiseconds_to_ms(value: &str) -> Option<u64> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|centiseconds| centiseconds.is_finite() && *centiseconds >= 0.0)
        .map(|centiseconds| (centiseconds * 10.0).round() as u64)
}

fn malformed_output() -> AsrError {
    AsrError::new(
        AsrErrorKind::MalformedOutput,
        "invalid_vad_output",
        "VAD 返回了无法解析的时间范围",
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::parse_segments;

    #[test]
    fn parses_and_merges_padded_vad_ranges() {
        let output = r#"
            Detected 3 speech segments:
            Speech segment 0: start = 10.00, end = 100.00
            Speech segment 1: start = 95.00, end = 200.00
            Speech segment 2: start = 325.00, end = 500.00
        "#;
        let regions = parse_segments(output, 4_000).unwrap();
        assert_eq!(regions.len(), 2);
        assert_eq!((regions[0].start_ms, regions[0].end_ms), (100, 2_000));
        assert_eq!((regions[1].start_ms, regions[1].end_ms), (3_250, 4_000));
    }

    #[test]
    fn empty_output_means_no_detected_speech() {
        assert!(
            parse_segments("Detected 0 speech segments:", 1_000)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn ignores_vad_tail_that_exceeds_the_frozen_media_duration() {
        let output = r#"
            Detected 2 speech segments:
            Speech segment 0: start = 0.00, end = 100.00
            Speech segment 1: start = 110.00, end = 120.00
        "#;
        let regions = parse_segments(output, 1_050).unwrap();
        assert_eq!(regions.len(), 1);
        assert_eq!((regions[0].start_ms, regions[0].end_ms), (0, 1_000));
    }
}
