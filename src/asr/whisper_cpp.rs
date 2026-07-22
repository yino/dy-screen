//! 将受控 `whisper.cpp` CLI 映射到中立 [`super::AsrEngine`] 契约。
//!
//! 本模块负责参数数组、结构化输出、进度、取消、清理和错误脱敏，不承载项目业务状态。

use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use super::{
    AsrCapabilities, AsrConfidenceKind, AsrEngine, AsrEngineIdentity, AsrEnvironmentReport,
    AsrError, AsrErrorKind, AsrProgressEvent, AsrProgressSink, AsrProgressStage, AsrRequest,
    AsrResult, AsrSegment, EngineResult, ResolvedAsrResources, ResourcePreflight, TimestampPolicy,
    VadConfig,
};

/// `whisper.cpp` CLI 的 Rust 适配器。业务层只看到中立 `AsrEngine` 契约。
#[derive(Clone)]
pub struct WhisperCppEngine {
    resources: ResolvedAsrResources,
    preflight: ResourcePreflight,
    vad: VadConfig,
}

impl WhisperCppEngine {
    pub fn new(
        resources: ResolvedAsrResources,
        preflight: ResourcePreflight,
        vad: VadConfig,
    ) -> Self {
        Self {
            resources,
            preflight,
            vad,
        }
    }

    fn output_prefix(&self, request: &AsrRequest) -> EngineResult<PathBuf> {
        let parent = request.audio.path.parent().ok_or_else(|| {
            AsrError::new(
                AsrErrorKind::InvalidInput,
                "audio_parent_missing",
                "临时音频路径无效",
                false,
            )
        })?;
        let digest = hex::encode(Sha256::digest(request.request_id.as_bytes()));
        Ok(parent.join(format!("whisper-result-{}", &digest[..16])))
    }

    async fn execute(&self, request: &AsrRequest) -> EngineResult<AsrResult> {
        let output_prefix = self.output_prefix(request)?;
        let output_json = output_prefix.with_extension("json");
        let _ = tokio::fs::remove_file(&output_json).await;

        let mut command = Command::new(&self.resources.whisper_sidecar);
        command
            .arg("--model")
            .arg(&self.resources.model)
            .arg("--file")
            .arg(&request.audio.path)
            .arg("--language")
            .arg(request.language_hint.as_deref().unwrap_or("auto"))
            .arg("--threads")
            .arg(
                request
                    .max_threads
                    .min(self.resources.maximum_threads)
                    .max(1)
                    .to_string(),
            )
            .arg("--output-json-full")
            .arg("--output-file")
            .arg(&output_prefix)
            .arg("--no-prints")
            .arg("--vad")
            .arg("--vad-model")
            .arg(&self.resources.vad_model)
            .arg("--vad-threshold")
            .arg(format!(
                "{:.3}",
                f32::from(self.vad.threshold_permille) / 1_000.0
            ))
            .arg("--vad-min-speech-duration-ms")
            .arg(self.vad.minimum_speech_ms.to_string())
            .arg("--vad-min-silence-duration-ms")
            .arg(self.vad.minimum_silence_ms.to_string())
            .arg("--vad-max-speech-duration-s")
            .arg(self.vad.maximum_speech_seconds.to_string())
            .arg("--vad-speech-pad-ms")
            .arg(self.vad.speech_padding_ms.to_string())
            .arg("--vad-samples-overlap")
            .arg(format!(
                "{:.3}",
                self.vad.samples_overlap_ms as f64 / 1_000.0
            ))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if !self.resources.use_gpu {
            command.arg("--no-gpu");
        }
        if !request.hotwords.is_empty() {
            let prompt = request
                .hotwords
                .iter()
                .map(|word| word.trim())
                .filter(|word| !word.is_empty())
                .collect::<Vec<_>>()
                .join("，");
            if !prompt.is_empty() {
                command.arg("--prompt").arg(prompt);
            }
        }

        let status = command.status().await.map_err(spawn_error)?;
        if !status.success() {
            let _ = tokio::fs::remove_file(&output_json).await;
            return Err(exit_error(status));
        }
        let json = tokio::fs::read(&output_json).await.map_err(|_| {
            AsrError::new(
                AsrErrorKind::MalformedOutput,
                "whisper_output_missing",
                "本地识别组件没有生成有效结果",
                true,
            )
        })?;
        let parsed = parse_whisper_json(
            &json,
            &request.request_id,
            self.identity(),
            request.audio.duration_ms,
        );
        let _ = tokio::fs::remove_file(&output_json).await;
        parsed
    }
}

fn spawn_error(error: std::io::Error) -> AsrError {
    if cfg!(target_os = "windows") {
        match error.raw_os_error() {
            Some(126) => return windows_runtime_missing(),
            Some(193 | 216) => return windows_binary_incompatible(),
            _ => {}
        }
    }
    AsrError::new(
        AsrErrorKind::EnvironmentUnavailable,
        "whisper_sidecar_unavailable",
        "本地识别组件不可用，请重新安装应用",
        false,
    )
}

fn exit_error(status: ExitStatus) -> AsrError {
    process_exit_error(status.code(), cfg!(target_os = "windows"))
}

fn process_exit_error(code: Option<i32>, windows: bool) -> AsrError {
    if windows {
        match code {
            // Windows loader status: STATUS_DLL_NOT_FOUND.
            Some(code) if code == 0xC000_0135_u32 as i32 => return windows_runtime_missing(),
            // Windows loader status: STATUS_INVALID_IMAGE_FORMAT.
            Some(code) if code == 0xC000_007B_u32 as i32 => {
                return windows_binary_incompatible();
            }
            _ => {}
        }
    }
    AsrError::new(
        AsrErrorKind::ProcessFailed,
        "whisper_failed",
        "本地语音识别进程异常退出",
        true,
    )
}

fn windows_runtime_missing() -> AsrError {
    AsrError::new(
        AsrErrorKind::EnvironmentUnavailable,
        "whisper_runtime_missing",
        "本地识别组件缺少 Microsoft Visual C++ x64 运行库，请修复安装",
        false,
    )
}

fn windows_binary_incompatible() -> AsrError {
    AsrError::new(
        AsrErrorKind::UnsupportedPlatform,
        "whisper_binary_incompatible",
        "本地识别组件与当前 Windows 架构不兼容，请重新安装应用",
        false,
    )
}

#[async_trait]
impl AsrEngine for WhisperCppEngine {
    fn identity(&self) -> AsrEngineIdentity {
        self.resources.identity.clone()
    }

    fn capabilities(&self) -> AsrCapabilities {
        AsrCapabilities {
            timestamp_policies: vec![TimestampPolicy::Segment],
            supports_language_hint: true,
            supports_hotwords: true,
            confidence_kind: Some(AsrConfidenceKind::Probability),
            maximum_threads: self.resources.maximum_threads,
        }
    }

    async fn diagnose(&self) -> EngineResult<AsrEnvironmentReport> {
        let preflight = self.preflight.clone();
        let resources = self.resources.clone();
        tokio::task::spawn_blocking(move || preflight.diagnose(&resources))
            .await
            .map_err(|_| {
                AsrError::new(
                    AsrErrorKind::Internal,
                    "asr_preflight_join_failed",
                    "本地 ASR 环境检查异常退出",
                    true,
                )
            })?
    }

    async fn transcribe(
        &self,
        request: AsrRequest,
        progress: Arc<dyn AsrProgressSink>,
        cancellation: CancellationToken,
    ) -> EngineResult<AsrResult> {
        request.validate()?;
        self.vad.validate()?;
        if cancellation.is_cancelled() {
            return Err(AsrError::cancelled());
        }
        if request.speech_regions.is_empty() {
            return Err(AsrError::new(
                AsrErrorKind::NoSpeech,
                "no_speech_detected",
                "视频中没有检测到有效人声",
                false,
            ));
        }
        progress.publish(progress_event(
            &request,
            AsrProgressStage::ValidatingEnvironment,
            0,
            Some(100),
            "正在检查本地识别环境",
        ));
        let report = self.diagnose().await?;
        if !report.ready {
            return Err(AsrError::new(
                AsrErrorKind::EnvironmentUnavailable,
                "asr_environment_not_ready",
                "本地 ASR 环境未就绪，请重新检测或修复安装",
                false,
            ));
        }
        progress.publish(progress_event(
            &request,
            AsrProgressStage::LoadingModel,
            10,
            Some(100),
            "正在加载本地模型",
        ));
        progress.publish(progress_event(
            &request,
            AsrProgressStage::Transcribing,
            20,
            Some(100),
            "正在识别语音",
        ));
        let result = tokio::select! {
            _ = cancellation.cancelled() => return Err(AsrError::cancelled()),
            result = self.execute(&request) => result?,
        };
        progress.publish(progress_event(
            &request,
            AsrProgressStage::Finalizing,
            100,
            Some(100),
            "正在整理识别结果",
        ));
        result.validate()?;
        Ok(result)
    }
}

fn progress_event(
    request: &AsrRequest,
    stage: AsrProgressStage,
    completed_units: u64,
    total_units: Option<u64>,
    message: &str,
) -> AsrProgressEvent {
    AsrProgressEvent {
        request_id: request.request_id.clone(),
        stage,
        completed_units,
        total_units,
        message: message.to_owned(),
    }
}

#[derive(Debug, Deserialize)]
struct WhisperJson {
    result: WhisperLanguage,
    #[serde(default)]
    transcription: Vec<WhisperSegment>,
}

#[derive(Debug, Deserialize)]
struct WhisperLanguage {
    language: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WhisperSegment {
    offsets: WhisperOffsets,
    text: String,
    #[serde(default)]
    tokens: Vec<WhisperToken>,
}

#[derive(Debug, Deserialize)]
struct WhisperOffsets {
    from: u64,
    to: u64,
}

#[derive(Debug, Deserialize)]
struct WhisperToken {
    text: String,
    p: Option<f32>,
}

fn parse_whisper_json(
    json: &[u8],
    request_id: &str,
    identity: AsrEngineIdentity,
    audio_duration_ms: u64,
) -> EngineResult<AsrResult> {
    let output: WhisperJson = serde_json::from_slice(json).map_err(|_| {
        AsrError::new(
            AsrErrorKind::MalformedOutput,
            "invalid_whisper_json",
            "本地识别组件返回了无法解析的结果",
            false,
        )
    })?;
    let mut segments = Vec::new();
    for segment in output.transcription {
        let start_ms = segment.offsets.from.min(audio_duration_ms);
        let end_ms = segment.offsets.to.min(audio_duration_ms);
        let text = segment.text.trim().to_owned();
        if start_ms >= end_ms || text.is_empty() {
            continue;
        }
        let probabilities: Vec<f32> = segment
            .tokens
            .iter()
            .filter(|token| !token.text.starts_with("[_"))
            .filter_map(|token| token.p)
            .filter(|value| (0.0..=1.0).contains(value))
            .collect();
        let confidence = if probabilities.is_empty() {
            None
        } else {
            Some(probabilities.iter().sum::<f32>() / probabilities.len() as f32)
        };
        segments.push(AsrSegment {
            start_ms,
            end_ms,
            text,
            confidence,
        });
    }
    Ok(AsrResult {
        request_id: request_id.to_owned(),
        identity,
        detected_language: output.result.language,
        audio_duration_ms,
        segments,
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::{parse_whisper_json, process_exit_error};
    use crate::asr::AsrEngineIdentity;

    #[test]
    fn parses_full_json_clamps_rounding_and_averages_real_token_probabilities() {
        let json = r#"
        {
          "result": {"language":"zh"},
          "transcription": [{
            "offsets": {"from":0,"to":4200},
            "text":" 欢迎来到直播间。 ",
            "tokens":[
              {"text":"[_BEG_]","p":0.99},
              {"text":"欢迎","p":0.80},
              {"text":"直播间","p":0.60}
            ]
          }]
        }
        "#
        .as_bytes();
        let result = parse_whisper_json(
            json,
            "request",
            AsrEngineIdentity {
                engine_id: "whisper.cpp".to_owned(),
                engine_version: "v1.9.1".to_owned(),
                model_id: "small".to_owned(),
                model_version: "q5_1".to_owned(),
            },
            4_191,
        )
        .unwrap();
        assert_eq!(result.detected_language.as_deref(), Some("zh"));
        assert_eq!(result.segments[0].end_ms, 4_191);
        assert_eq!(result.segments[0].text, "欢迎来到直播间。");
        assert!((result.segments[0].confidence.unwrap() - 0.7).abs() < 0.001);
    }

    #[test]
    fn confidence_is_none_when_engine_has_no_explainable_probabilities() {
        let json = r#"
        {"result":{"language":"zh"},"transcription":[{
          "offsets":{"from":0,"to":1000},"text":"文本","tokens":[]
        }]}
        "#
        .as_bytes();
        let result = parse_whisper_json(
            json,
            "request",
            AsrEngineIdentity {
                engine_id: "fake".to_owned(),
                engine_version: "1".to_owned(),
                model_id: "fake".to_owned(),
                model_version: "1".to_owned(),
            },
            1_000,
        )
        .unwrap();
        assert_eq!(result.segments[0].confidence, None);
    }

    #[test]
    fn maps_windows_loader_failures_without_exposing_process_details() {
        let missing_runtime = process_exit_error(Some(0xC000_0135_u32 as i32), true);
        assert_eq!(missing_runtime.code, "whisper_runtime_missing");
        assert!(missing_runtime.safe_message.contains("Visual C++"));
        assert!(!missing_runtime.retryable);

        let incompatible = process_exit_error(Some(0xC000_007B_u32 as i32), true);
        assert_eq!(incompatible.code, "whisper_binary_incompatible");
        assert!(!incompatible.retryable);

        assert_eq!(process_exit_error(Some(9), true).code, "whisper_failed");
    }
}
