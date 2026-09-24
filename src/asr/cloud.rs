//! 云端 ASR 的中立能力边界。
//!
//! 本模块不负责上传本地音频。具体后端上传或对象存储实现可以在后续部署中注入；
//! 未注入时调用方必须跳过云端并回退本地引擎。

#![deny(missing_docs)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use serde::{Deserialize, Serialize};
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

use super::contract::{
    AsrCapabilities, AsrEngine, AsrEngineIdentity, AsrEnvironmentReport, AsrProgressSink,
    AsrRequest, AsrResult, AsrSegment,
};
use super::error::{AsrError, AsrErrorKind, AsrResult as EngineResult};

/// 可供云端文件转写使用的临时音频引用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TemporaryAudioReference {
    /// 仅允许 HTTPS 的短期 URL。
    pub url: String,
    /// 不透明清理句柄。
    pub lease_id: String,
}

impl TemporaryAudioReference {
    /// 校验安全的临时引用。
    pub fn validate(&self) -> EngineResult<()> {
        let parsed = reqwest::Url::parse(self.url.trim()).map_err(|_| {
            AsrError::invalid_input("invalid_temporary_audio_url", "云端音频地址无效")
        })?;
        if parsed.scheme() != "https" || self.lease_id.trim().is_empty() {
            return Err(AsrError::invalid_input(
                "unsafe_temporary_audio_reference",
                "云端音频地址必须是受控的 HTTPS 临时地址",
            ));
        }
        Ok(())
    }
}

/// 任务生命周期内发布和清理标准音频的抽象。
#[async_trait]
pub trait TemporaryAudioPublisher: Send + Sync {
    /// 当前是否可用。
    fn is_available(&self) -> bool;
    /// 发布 WAV。
    async fn publish(&self, audio_path: &Path) -> EngineResult<TemporaryAudioReference>;
    /// 清理引用。
    async fn cleanup(&self, reference: TemporaryAudioReference) -> EngineResult<()>;
}

/// 未配置发布器时的安全实现。
#[derive(Debug, Default, Clone, Copy)]
pub struct UnavailableTemporaryAudioPublisher;

#[async_trait]
impl TemporaryAudioPublisher for UnavailableTemporaryAudioPublisher {
    fn is_available(&self) -> bool {
        false
    }

    async fn publish(&self, _audio_path: &Path) -> EngineResult<TemporaryAudioReference> {
        Err(AsrError::new(
            AsrErrorKind::EnvironmentUnavailable,
            "temporary_audio_publisher_unavailable",
            "云端音频发布能力尚未配置，已使用本地转写",
            false,
        ))
    }

    async fn cleanup(&self, _reference: TemporaryAudioReference) -> EngineResult<()> {
        Ok(())
    }
}

/// 共享发布器句柄。
pub type SharedTemporaryAudioPublisher = Arc<dyn TemporaryAudioPublisher>;

/// 百炼文件转写配置。
#[derive(Debug, Clone)]
pub struct DashScopeAsrConfig {
    /// API 根地址。
    pub base_url: String,
    /// 模型名称。
    pub model: String,
    /// 最大等待秒数。
    pub max_wait_seconds: u64,
    /// 轮询间隔毫秒。
    pub poll_interval_ms: u64,
}

impl Default for DashScopeAsrConfig {
    fn default() -> Self {
        Self {
            base_url: "https://dashscope.aliyuncs.com/api/v1".into(),
            model: "qwen3-asr-flash-filetrans".into(),
            max_wait_seconds: 300,
            poll_interval_ms: 1500,
        }
    }
}

/// 已脱敏的百炼转写句段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DashScopeSegment {
    /// 开始毫秒。
    pub begin_time: u64,
    /// 结束毫秒。
    pub end_time: u64,
    /// 文本。
    pub text: String,
}

/// 百炼异步文件转写客户端。
#[derive(Clone)]
pub struct DashScopeAsrClient {
    client: reqwest::Client,
    config: DashScopeAsrConfig,
}

impl DashScopeAsrClient {
    /// 使用默认 HTTP 客户端创建客户端。
    pub fn new(config: DashScopeAsrConfig) -> EngineResult<Self> {
        let parsed = reqwest::Url::parse(&config.base_url).map_err(|_| {
            AsrError::invalid_input("invalid_dashscope_base_url", "云端转写服务地址无效")
        })?;
        if parsed.scheme() != "https" || parsed.host_str().is_none() {
            return Err(AsrError::invalid_input(
                "unsafe_dashscope_base_url",
                "云端转写服务必须使用 HTTPS",
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| {
                AsrError::new(
                    AsrErrorKind::Internal,
                    "dashscope_client_unavailable",
                    "云端转写客户端初始化失败",
                    true,
                )
            })?;
        Ok(Self { client, config })
    }

    /// 提交异步文件转写任务并返回不透明 task id。
    pub async fn submit(
        &self,
        token: &str,
        reference: &TemporaryAudioReference,
        request: &AsrRequest,
    ) -> EngineResult<String> {
        request.validate()?;
        reference.validate()?;
        let token = bearer_value(token)?;
        let url = self.endpoint("services/audio/asr/transcription")?;
        let mut parameters = serde_json::json!({
            "channel_id": [0],
            "enable_itn": true,
            "enable_words": true,
        });
        if let Some(language) = request.language_hint.as_deref() {
            parameters["language"] = serde_json::Value::String(language.to_owned());
        }
        let response = self
            .client
            .post(url)
            .header(AUTHORIZATION, token)
            .header(CONTENT_TYPE, "application/json")
            .header("X-DashScope-Async", "enable")
            .body(
                serde_json::to_vec(&serde_json::json!({
                    "model": self.config.model,
                    "input": {"file_url": reference.url},
                    "parameters": parameters,
                }))
                .map_err(|_| {
                    malformed_cloud_error("dashscope_request_invalid", "云端转写请求生成失败")
                })?,
            )
            .send()
            .await
            .map_err(|error| map_transport_error(error, "dashscope_submit_failed"))?;
        if !response.status().is_success() {
            return Err(dashscope_http_error(response.status().as_u16()));
        }
        let body = parse_json_response(
            response,
            "dashscope_submit_response_invalid",
            "云端任务响应无效",
        )
        .await?;
        body.get("output")
            .and_then(|output| output.get("task_id"))
            .and_then(serde_json::Value::as_str)
            .filter(|task_id| !task_id.trim().is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                malformed_cloud_error("dashscope_task_id_missing", "云端任务响应缺少任务标识")
            })
    }

    /// 在最大等待时间内轮询任务，返回成功时的详细结果 JSON。
    pub async fn poll(
        &self,
        token: &str,
        task_id: &str,
        cancellation: CancellationToken,
    ) -> EngineResult<serde_json::Value> {
        let token = bearer_value(token)?;
        if task_id.trim().is_empty() {
            return Err(AsrError::invalid_input(
                "missing_dashscope_task_id",
                "云端任务标识为空",
            ));
        }
        let deadline = Duration::from_secs(self.config.max_wait_seconds.max(1));
        let operation = async {
            loop {
                if cancellation.is_cancelled() {
                    return Err(AsrError::cancelled());
                }
                let url = self.endpoint(&format!("tasks/{}", urlencoding(task_id)))?;
                let response = self
                    .client
                    .get(url)
                    .header(AUTHORIZATION, token.clone())
                    .send()
                    .await
                    .map_err(|error| map_transport_error(error, "dashscope_poll_failed"))?;
                if !response.status().is_success() {
                    return Err(dashscope_http_error(response.status().as_u16()));
                }
                let body = parse_json_response(
                    response,
                    "dashscope_poll_response_invalid",
                    "云端任务状态响应无效",
                )
                .await?;
                let status = body
                    .get("output")
                    .and_then(|output| output.get("task_status"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                match status {
                    "SUCCEEDED" => return Ok(body),
                    "FAILED" | "CANCELED" | "CANCELLED" => {
                        return Err(AsrError::new(
                            AsrErrorKind::ProcessFailed,
                            "dashscope_task_failed",
                            "云端转写任务失败，已切换到本地转写",
                            false,
                        ));
                    }
                    "PENDING" | "RUNNING" => {
                        sleep(Duration::from_millis(self.config.poll_interval_ms.max(100))).await;
                    }
                    _ => {
                        return Err(malformed_cloud_error(
                            "dashscope_task_status_invalid",
                            "云端任务状态无效，已切换到本地转写",
                        ));
                    }
                }
            }
        };
        timeout(deadline, operation).await.map_err(|_| {
            AsrError::new(
                AsrErrorKind::ProcessFailed,
                "dashscope_poll_timeout",
                "云端转写等待超时，已切换到本地转写",
                true,
            )
        })?
    }

    /// 提交并轮询一次云端转写，转换为统一 ASR 结果。
    pub async fn transcribe_file(
        &self,
        token: &str,
        reference: &TemporaryAudioReference,
        request: &AsrRequest,
        identity: AsrEngineIdentity,
        cancellation: CancellationToken,
    ) -> EngineResult<AsrResult> {
        let task_id = self.submit(token, reference, request).await?;
        let body = self.poll(token, &task_id, cancellation).await?;
        let output = body.get("output").unwrap_or(&body);
        let detailed =
            if let Some(results) = output.get("results").and_then(|value| value.as_array()) {
                results
                    .first()
                    .and_then(|result| result.get("transcription_url"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            } else {
                None
            };
        let result_body = if let Some(url) = detailed {
            let parsed_url = reqwest::Url::parse(&url).map_err(|_| {
                malformed_cloud_error("dashscope_result_url_invalid", "云端详细转写地址无效")
            })?;
            if parsed_url.scheme() != "https" {
                return Err(malformed_cloud_error(
                    "dashscope_result_url_unsafe",
                    "云端详细转写地址不安全",
                ));
            }
            let response =
                self.client.get(parsed_url).send().await.map_err(|error| {
                    map_transport_error(error, "dashscope_result_download_failed")
                })?;
            if !response.status().is_success() {
                return Err(dashscope_http_error(response.status().as_u16()));
            }
            parse_json_response(response, "dashscope_result_invalid", "云端详细转写结果无效")
                .await?
        } else {
            output.clone()
        };
        let segments = parse_dashscope_segments(&result_body)?
            .into_iter()
            .map(|segment| AsrSegment {
                start_ms: segment.begin_time,
                end_ms: segment.end_time,
                text: segment.text,
                confidence: None,
            })
            .collect::<Vec<_>>();
        let result = AsrResult {
            request_id: request.request_id.clone(),
            identity,
            detected_language: request.language_hint.clone(),
            audio_duration_ms: request.audio.duration_ms,
            provider: "aliyun".to_owned(),
            fallback_reason: None,
            segments,
            warnings: Vec::new(),
        };
        result.validate()?;
        Ok(result)
    }

    fn endpoint(&self, path: &str) -> EngineResult<reqwest::Url> {
        let mut base = reqwest::Url::parse(&self.config.base_url).map_err(|_| {
            AsrError::new(
                AsrErrorKind::Internal,
                "dashscope_base_url_invalid",
                "云端转写服务地址无效",
                false,
            )
        })?;
        let joined = format!(
            "{}/{}",
            base.path().trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        base.set_path(&joined);
        Ok(base)
    }
}

fn bearer_value(token: &str) -> EngineResult<HeaderValue> {
    if token.trim().is_empty() || token.chars().any(char::is_control) {
        return Err(AsrError::invalid_input(
            "dashscope_token_invalid",
            "云端转写授权无效",
        ));
    }
    HeaderValue::from_str(&format!("Bearer {}", token.trim()))
        .map_err(|_| AsrError::invalid_input("dashscope_token_invalid", "云端转写授权无效"))
}

fn urlencoding(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

async fn parse_json_response(
    response: reqwest::Response,
    code: &str,
    message: &str,
) -> EngineResult<serde_json::Value> {
    let bytes = response
        .bytes()
        .await
        .map_err(|_| malformed_cloud_error(code, message))?;
    serde_json::from_slice(&bytes).map_err(|_| malformed_cloud_error(code, message))
}

fn malformed_cloud_error(code: &str, message: &str) -> AsrError {
    AsrError::new(AsrErrorKind::MalformedOutput, code, message, false)
}

fn map_transport_error(error: reqwest::Error, code: &str) -> AsrError {
    if error.is_timeout() {
        AsrError::new(
            AsrErrorKind::ProcessFailed,
            "dashscope_timeout",
            "云端转写请求超时，已切换到本地转写",
            true,
        )
    } else {
        AsrError::new(
            AsrErrorKind::ProcessFailed,
            code,
            "云端转写网络不可用，已切换到本地转写",
            true,
        )
    }
}

/// 将百炼详细转写结果解析为句段。
pub fn parse_dashscope_segments(value: &serde_json::Value) -> EngineResult<Vec<DashScopeSegment>> {
    let transcripts = value
        .get("transcripts")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            malformed_cloud_error("dashscope_missing_transcripts", "云端转写结果缺少文本")
        })?;
    let mut out = Vec::new();
    for transcript in transcripts {
        if let Some(sentences) = transcript
            .get("sentences")
            .and_then(serde_json::Value::as_array)
        {
            for sentence in sentences {
                let start = sentence
                    .get("begin_time")
                    .and_then(serde_json::Value::as_u64);
                let end = sentence.get("end_time").and_then(serde_json::Value::as_u64);
                let text = sentence
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim);
                if let (Some(begin_time), Some(end_time), Some(text)) = (start, end, text)
                    && begin_time < end_time
                    && !text.is_empty()
                {
                    out.push(DashScopeSegment {
                        begin_time,
                        end_time,
                        text: text.to_owned(),
                    });
                }
            }
        }
    }
    if out.is_empty() {
        return Err(malformed_cloud_error(
            "dashscope_empty_transcript",
            "云端转写结果没有有效句段",
        ));
    }
    Ok(out)
}

/// 临时 AI token 的异步提供器；具体实现可绑定项目 API。
#[async_trait]
pub trait TemporaryAiTokenProvider: Send + Sync {
    /// 获取一个仅供当前任务使用的临时 token。
    async fn token(&self) -> EngineResult<String>;
}

/// 云端优先、本地回退的 ASR engine。
pub struct CloudFirstAsrEngine {
    local: Arc<dyn AsrEngine>,
    publisher: SharedTemporaryAudioPublisher,
    token_provider: Option<Arc<dyn TemporaryAiTokenProvider>>,
    dashscope: DashScopeAsrClient,
    identity: AsrEngineIdentity,
}

impl CloudFirstAsrEngine {
    /// 创建路由器。token provider 或发布器不可用时，云端路径自动跳过。
    pub fn new(
        local: Arc<dyn AsrEngine>,
        publisher: SharedTemporaryAudioPublisher,
        token_provider: Option<Arc<dyn TemporaryAiTokenProvider>>,
        dashscope: DashScopeAsrClient,
    ) -> Self {
        let identity = local.identity();
        Self {
            local,
            publisher,
            token_provider,
            dashscope,
            identity,
        }
    }

    async fn cloud_attempt(
        &self,
        request: &AsrRequest,
        cancellation: CancellationToken,
    ) -> EngineResult<AsrResult> {
        let provider = self.token_provider.as_ref().ok_or_else(|| {
            AsrError::new(
                AsrErrorKind::EnvironmentUnavailable,
                "temporary_ai_token_provider_unavailable",
                "AI 临时密钥能力尚未配置",
                false,
            )
        })?;
        if !self.publisher.is_available() {
            return Err(AsrError::new(
                AsrErrorKind::EnvironmentUnavailable,
                "temporary_audio_publisher_unavailable",
                "云端音频发布能力尚未配置",
                false,
            ));
        }
        let reference = self.publisher.publish(&request.audio.path).await?;
        let result = match provider.token().await {
            Ok(token) => {
                self.dashscope
                    .transcribe_file(
                        &token,
                        &reference,
                        request,
                        self.identity.clone(),
                        cancellation,
                    )
                    .await
            }
            Err(error) => Err(error),
        };
        let cleanup_result = self.publisher.cleanup(reference).await;
        if let Err(error) = cleanup_result
            && result.is_ok()
        {
            return Err(error);
        }
        result
    }
}

#[async_trait]
impl AsrEngine for CloudFirstAsrEngine {
    fn identity(&self) -> AsrEngineIdentity {
        self.identity.clone()
    }

    fn capabilities(&self) -> AsrCapabilities {
        self.local.capabilities()
    }

    async fn diagnose(&self) -> EngineResult<AsrEnvironmentReport> {
        self.local.diagnose().await
    }

    async fn transcribe(
        &self,
        request: AsrRequest,
        progress: Arc<dyn AsrProgressSink>,
        cancellation: CancellationToken,
    ) -> EngineResult<AsrResult> {
        request.validate()?;
        match self
            .cloud_attempt(&request, cancellation.child_token())
            .await
        {
            Ok(result) => Ok(result),
            Err(cloud_error) => {
                let mut result = self
                    .local
                    .transcribe(request, progress, cancellation)
                    .await?;
                result.fallback_reason = Some(cloud_error.code);
                Ok(result)
            }
        }
    }
}

/// 将百炼 HTTP 状态映射为安全错误。
pub fn dashscope_http_error(status: u16) -> AsrError {
    let (code, message, retryable) = match status {
        401 => ("dashscope_unauthorized", "云端转写授权已失效", false),
        403 => ("dashscope_forbidden", "当前临时密钥无权使用云端转写", false),
        429 => (
            "dashscope_rate_limited",
            "云端转写请求过于频繁，已切换到本地转写",
            true,
        ),
        500..=599 => (
            "dashscope_service_unavailable",
            "云端转写服务暂时不可用，已切换到本地转写",
            true,
        ),
        _ => (
            "dashscope_request_failed",
            "云端转写请求失败，已切换到本地转写",
            false,
        ),
    };
    AsrError::new(AsrErrorKind::ProcessFailed, code, message, retryable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_https_references() {
        let reference = TemporaryAudioReference {
            url: "https://example.invalid/a.wav".into(),
            lease_id: "lease".into(),
        };
        assert!(reference.validate().is_ok());
        for url in ["file:///tmp/a.wav", "http://example.invalid/a.wav", ""] {
            assert!(
                TemporaryAudioReference {
                    url: url.into(),
                    lease_id: "lease".into()
                }
                .validate()
                .is_err()
            );
        }
    }

    #[tokio::test]
    async fn unavailable_publisher_never_publishes_local_paths() {
        let error = UnavailableTemporaryAudioPublisher
            .publish(Path::new("/private/video.wav"))
            .await
            .unwrap_err();
        assert_eq!(error.code, "temporary_audio_publisher_unavailable");
    }

    #[test]
    fn maps_upstream_statuses_without_returning_raw_details() {
        for (status, code) in [
            (401, "dashscope_unauthorized"),
            (403, "dashscope_forbidden"),
            (429, "dashscope_rate_limited"),
            (503, "dashscope_service_unavailable"),
        ] {
            let error = dashscope_http_error(status);
            assert_eq!(error.code, code);
            assert!(!error.safe_message.contains("Authorization"));
            assert!(!error.safe_message.contains("Bearer"));
        }
    }

    #[test]
    fn parses_and_maps_dashscope_segments_without_raw_payload() {
        let value = serde_json::json!({"transcripts": [{"sentences": [
            {"begin_time": 0, "end_time": 1200, "text": "你好"},
            {"begin_time": 1200, "end_time": 2200, "text": " 世界 "}
        ]}]});
        let segments = parse_dashscope_segments(&value).unwrap();
        assert_eq!(segments[1].text, "世界");
        assert_eq!(dashscope_http_error(429).code, "dashscope_rate_limited");
    }
}
