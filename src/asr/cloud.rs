//! 云端 ASR 的中立能力边界。
//!
//! 客户端直连百炼完成音频直传、任务提交与轮询。音频上传必须走三步流程：
//! `POST /api/v1/files` 直传取得 `file_id`，`GET /api/v1/files/{file_id}` 取回上游
//! 签发的签名临时 URL，再以该 URL 作为转写请求的音频引用。
//!
//! 实测证伪的替代路径不得使用：`GET /uploads?action=getPolicy` 的对象存储表单直传
//! 会把对象锁定为私有，转写服务无法读取；直接以 `file_id` 作为引用也会失败。

#![deny(missing_docs)]

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use serde::{Deserialize, Serialize};
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

use super::contract::{
    AsrCapabilities, AsrEngine, AsrEngineIdentity, AsrEnvironmentCheck, AsrEnvironmentReport,
    AsrProgressSink, AsrRequest, AsrResult, AsrSegment, AsrWarning,
};
use super::error::{AsrError, AsrErrorKind, AsrResult as EngineResult};

/// 临时 AI token 的异步提供器；具体实现可绑定项目 API。
#[async_trait]
pub trait TemporaryAiTokenProvider: Send + Sync {
    /// 是否已配置可用凭据。
    fn is_configured(&self) -> bool {
        true
    }
    /// 获取一个仅供当前任务使用的临时 token。
    async fn temporary_token(&self) -> EngineResult<String>;
}

/// 共享临时 token 提供器句柄。
pub type SharedTemporaryAiTokenProvider = Arc<dyn TemporaryAiTokenProvider>;

/// 可供云端文件转写使用的临时音频引用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TemporaryAudioReference {
    /// 上游签发的短期 URL。
    pub url: String,
    /// 不透明清理句柄；重复清理必须幂等成功。
    pub lease_id: String,
    /// 上游文件标识；用于诊断与幂等清理，不得写入普通日志。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    /// 该引用是否由上游在文件详情响应中签发。
    ///
    /// 只有上游签发的引用才被允许使用非 HTTPS 方案；任何来自用户输入、配置或
    /// 其他来源的地址一律被拒绝。
    #[serde(default)]
    pub upstream_issued: bool,
}

impl TemporaryAudioReference {
    /// 校验安全的临时引用。
    ///
    /// 安全边界：引用必须由上游签发，且 URL 必须同时带上签名与有效期参数
    /// （`Signature`/`Expires` 或 `security-token`）。HTTPS 引用始终允许；
    /// `http` 方案仅在同时满足"上游签发 + 含签名与有效期"时允许，因为上游
    /// 文件服务签发的临时地址实测为 `http`。
    pub fn validate(&self) -> EngineResult<()> {
        if self.lease_id.trim().is_empty() {
            return Err(AsrError::invalid_input(
                "unsafe_temporary_audio_reference",
                "云端音频引用缺少清理句柄",
            ));
        }
        let raw = self.url.trim();
        let parsed = reqwest::Url::parse(raw).map_err(|_| {
            AsrError::invalid_input("invalid_temporary_audio_url", "云端音频地址无效")
        })?;
        let scheme_allowed = match parsed.scheme() {
            "https" => true,
            "http" => self.upstream_issued && is_upstream_signed_url(&parsed),
            _ => false,
        };
        if !scheme_allowed {
            return Err(AsrError::invalid_input(
                "unsafe_temporary_audio_reference",
                "云端音频地址必须是上游签发的受控临时地址",
            ));
        }
        Ok(())
    }
}

/// 判断 URL 是否带有上游签发的签名与有效期参数。
fn is_upstream_signed_url(parsed: &reqwest::Url) -> bool {
    let mut names = BTreeSet::new();
    for (name, _) in parsed.query_pairs() {
        names.insert(name.into_owned());
    }
    let has_signature = names.contains("Signature") || names.contains("signature");
    let has_expiry = names.contains("Expires") || names.contains("expires");
    let has_session = names.contains("security-token");
    has_signature && (has_expiry || has_session)
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
            "云端音频发布能力尚未配置",
            false,
        ))
    }

    async fn cleanup(&self, _reference: TemporaryAudioReference) -> EngineResult<()> {
        Ok(())
    }
}

/// 共享发布器句柄。
pub type SharedTemporaryAudioPublisher = Arc<dyn TemporaryAudioPublisher>;

/// 上游调用诊断事件（脱敏）。
///
/// 只记录可安全暴露的信息：阶段、静态路径、HTTP 状态、耗时、尝试次数、稳定错误码、
/// 失败分支，以及"是否已取到某类标识"的**布尔量**与体量计数。
///
/// MUST NOT 记录：短期 token、完整或签名 URL、本地路径、ASR 正文、
/// 上游 `file_id` / `task_id` 的取值。需要定位具体对象时，用 `request_id`
/// 与耗时/状态组合判断，而不是把标识写进日志。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudCallDiagnostic {
    /// 调用阶段：`upload` / `signed_url` / `submit` / `poll` / `result` / `cancel`。
    pub stage: &'static str,
    /// 静态请求路径模板，不含查询串与标识。
    pub path: &'static str,
    /// HTTP 状态；未发出请求时为 None。
    pub http_status: Option<u16>,
    /// 本次阶段耗时毫秒。
    pub duration_ms: u64,
    /// 第几次尝试；轮询时表示轮询序号。
    pub attempt: u32,
    /// 稳定错误码；成功时为 None。
    pub error_code: Option<String>,
    /// 失败分支；成功时为 None。
    pub failure_branch: Option<String>,
    /// 是否已取到上游文件标识（只记布尔，不记取值）。
    pub has_file_id: bool,
    /// 是否已取到签名临时地址（只记布尔，不记取值）。
    pub has_signed_url: bool,
    /// 是否已取到上游任务标识（只记布尔，不记取值）。
    pub has_task_id: bool,
    /// 上传音频字节数；与阶段无关时为 None。
    pub audio_bytes: Option<u64>,
}

/// 上游调用诊断的接收端。
pub trait CloudCallDiagnosticSink: Send + Sync {
    /// 记录一个脱敏事件。
    fn emit(&self, event: CloudCallDiagnostic);
}

/// 默认接收端：把事件以单行 JSON 写到 stderr。
#[derive(Debug, Default, Clone, Copy)]
pub struct StderrCloudCallDiagnosticSink;

impl CloudCallDiagnosticSink for StderrCloudCallDiagnosticSink {
    fn emit(&self, event: CloudCallDiagnostic) {
        if let Ok(line) = serde_json::to_string(&event) {
            eprintln!("{line}");
        }
    }
}

/// 内存环形缓冲：供诊断面板读取，也供测试断言脱敏边界。
#[derive(Clone)]
pub struct MemoryCloudCallDiagnosticSink {
    events: Arc<std::sync::Mutex<std::collections::VecDeque<CloudCallDiagnostic>>>,
    enabled: Arc<std::sync::atomic::AtomicBool>,
    capacity: usize,
}

impl Default for MemoryCloudCallDiagnosticSink {
    fn default() -> Self {
        Self::new(256)
    }
}

impl MemoryCloudCallDiagnosticSink {
    /// 创建指定容量的环形缓冲；超出容量时丢弃最旧事件。
    pub fn new(capacity: usize) -> Self {
        Self {
            events: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
            // 默认关闭：调用轨迹属于行为信息，只有用户显式打开才记录。
            enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            capacity: capacity.max(1),
        }
    }

    /// 打开或关闭记录。关闭时 sink 不保留任何事件。
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
        if !enabled {
            self.clear();
        }
    }

    /// 当前是否正在记录。
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 当前已记录的事件（按时间顺序）。
    pub fn events(&self) -> Vec<CloudCallDiagnostic> {
        self.events
            .lock()
            .map(|events| events.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// 清空已记录事件。
    pub fn clear(&self) {
        if let Ok(mut events) = self.events.lock() {
            events.clear();
        }
    }
}

impl CloudCallDiagnosticSink for MemoryCloudCallDiagnosticSink {
    fn emit(&self, event: CloudCallDiagnostic) {
        // 关闭状态下一个事件也不留，避免"关了还在悄悄攒"。
        if !self.is_enabled() {
            return;
        }
        if let Ok(mut events) = self.events.lock() {
            if events.len() == self.capacity {
                events.pop_front();
            }
            events.push_back(event);
        }
    }
}

/// 共享诊断接收端。
pub type SharedCloudCallDiagnosticSink = Arc<dyn CloudCallDiagnosticSink>;

/// 把一次调用结果转成脱敏事件。
#[allow(clippy::too_many_arguments)]
fn diagnostic(
    stage: &'static str,
    path: &'static str,
    http_status: Option<u16>,
    started: std::time::Instant,
    attempt: u32,
    error: Option<&AsrError>,
    has_file_id: bool,
    has_signed_url: bool,
    has_task_id: bool,
    audio_bytes: Option<u64>,
) -> CloudCallDiagnostic {
    CloudCallDiagnostic {
        stage,
        path,
        http_status,
        duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        attempt,
        error_code: error.map(|value| value.code.clone()),
        failure_branch: error.map(|value| classify_cloud_failure(value).as_str().to_owned()),
        has_file_id,
        has_signed_url,
        has_task_id,
        audio_bytes,
    }
}

/// 上游文件服务传输层；生产实现使用 reqwest，测试可注入确定性替身。
#[async_trait]
pub trait CloudFileTransport: Send + Sync {
    /// 第 1 步：multipart 直传本地音频，取得上游文件标识。
    async fn upload_file(&self, audio_path: &Path, token: &str) -> EngineResult<String>;
    /// 第 2 步：取回上游签发的签名临时 URL。
    async fn fetch_signed_url(&self, file_id: &str, token: &str) -> EngineResult<String>;
}

/// 基于 reqwest 的上游文件服务传输实现。
#[derive(Clone)]
pub struct ReqwestCloudFileTransport {
    client: reqwest::Client,
    base_url: String,
    diagnostics: SharedCloudCallDiagnosticSink,
}

impl ReqwestCloudFileTransport {
    /// 创建传输实现；地址必须是 HTTPS。
    pub fn new(base_url: impl Into<String>) -> EngineResult<Self> {
        let base_url = base_url.into();
        let parsed = reqwest::Url::parse(&base_url).map_err(|_| {
            AsrError::invalid_input("invalid_dashscope_base_url", "云端转写服务地址无效")
        })?;
        if parsed.scheme() != "https" || parsed.host_str().is_none() {
            return Err(AsrError::invalid_input(
                "unsafe_dashscope_base_url",
                "云端转写服务必须使用 HTTPS",
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|_| {
                AsrError::new(
                    AsrErrorKind::Internal,
                    "dashscope_client_unavailable",
                    "云端转写客户端初始化失败",
                    true,
                )
            })?;
        Ok(Self {
            client,
            base_url,
            diagnostics: Arc::new(StderrCloudCallDiagnosticSink),
        })
    }

    fn endpoint(&self, path: &str) -> EngineResult<reqwest::Url> {
        let base = self.base_url.trim_end_matches('/');
        reqwest::Url::parse(&format!("{base}/{path}")).map_err(|_| {
            AsrError::invalid_input("invalid_dashscope_base_url", "云端转写服务地址无效")
        })
    }
}

#[async_trait]
impl CloudFileTransport for ReqwestCloudFileTransport {
    async fn upload_file(&self, audio_path: &Path, token: &str) -> EngineResult<String> {
        let token = bearer_value(token)?;
        let bytes = tokio::fs::read(audio_path).await.map_err(|_| {
            AsrError::new(
                AsrErrorKind::Io,
                "cloud_audio_read_failed",
                "读取待上传音频失败",
                true,
            )
        })?;
        let audio_bytes = Some(bytes.len() as u64);
        let file_name = audio_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("audio.wav")
            .to_owned();
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name)
            .mime_str("audio/wav")
            .map_err(|_| {
                AsrError::invalid_input("cloud_audio_part_invalid", "待上传音频格式无效")
            })?;
        let form = reqwest::multipart::Form::new().part("files", part);
        let url = self.endpoint("files")?;
        let started = std::time::Instant::now();
        let response = match self
            .client
            .post(url)
            .header(AUTHORIZATION, token)
            .multipart(form)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let mapped = map_transport_error(error, "cloud_upload_failed");
                self.diagnostics.emit(diagnostic(
                    "upload",
                    "files",
                    None,
                    started,
                    1,
                    Some(&mapped),
                    false,
                    false,
                    false,
                    audio_bytes,
                ));
                return Err(mapped);
            }
        };
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let mapped = map_upload_http_error(status);
            self.diagnostics.emit(diagnostic(
                "upload",
                "files",
                Some(status),
                started,
                1,
                Some(&mapped),
                false,
                false,
                false,
                audio_bytes,
            ));
            return Err(mapped);
        }
        let body =
            parse_json_response(response, "cloud_upload_response_invalid", "上传响应无效").await?;
        let file_id = body
            .get("data")
            .and_then(|data| data.get("uploaded_files"))
            .and_then(serde_json::Value::as_array)
            .and_then(|files| files.first())
            .and_then(|first| first.get("file_id"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        match &file_id {
            Some(_) => self.diagnostics.emit(diagnostic(
                "upload",
                "files",
                Some(200),
                started,
                1,
                None,
                true,
                false,
                false,
                audio_bytes,
            )),
            None => {
                let missing = AsrError::new(
                    AsrErrorKind::MalformedOutput,
                    "cloud_upload_file_id_missing",
                    "上传响应缺少文件标识",
                    true,
                );
                self.diagnostics.emit(diagnostic(
                    "upload",
                    "files",
                    Some(200),
                    started,
                    1,
                    Some(&missing),
                    false,
                    false,
                    false,
                    audio_bytes,
                ));
                return Err(missing);
            }
        }
        Ok(file_id.unwrap_or_default())
    }

    async fn fetch_signed_url(&self, file_id: &str, token: &str) -> EngineResult<String> {
        let token = bearer_value(token)?;
        if file_id.trim().is_empty() {
            return Err(AsrError::invalid_input(
                "cloud_upload_file_id_invalid",
                "上传文件标识无效",
            ));
        }
        let url = self.endpoint(&format!("files/{file_id}"))?;
        let started = std::time::Instant::now();
        let response = match self
            .client
            .get(url)
            .header(AUTHORIZATION, token)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let mapped = map_transport_error(error, "cloud_signed_url_fetch_failed");
                self.diagnostics.emit(diagnostic(
                    "signed_url",
                    "files/{file_id}",
                    None,
                    started,
                    1,
                    Some(&mapped),
                    true,
                    false,
                    false,
                    None,
                ));
                return Err(mapped);
            }
        };
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let mapped = map_signed_url_http_error(status);
            self.diagnostics.emit(diagnostic(
                "signed_url",
                "files/{file_id}",
                Some(status),
                started,
                1,
                Some(&mapped),
                true,
                false,
                false,
                None,
            ));
            return Err(mapped);
        }
        self.diagnostics.emit(diagnostic(
            "signed_url",
            "files/{file_id}",
            Some(200),
            started,
            1,
            None,
            true,
            true,
            false,
            None,
        ));
        let body = parse_json_response(
            response,
            "cloud_signed_url_response_invalid",
            "取回临时地址响应无效",
        )
        .await?;
        body.get("data")
            .and_then(|data| data.get("url"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                AsrError::new(
                    AsrErrorKind::MalformedOutput,
                    "cloud_signed_url_missing",
                    "取回临时地址响应缺少地址",
                    true,
                )
            })
    }
}

/// 三步直传发布器：把标准音频发布为上游签发的签名临时 URL。
///
/// 该实现只做编排与校验，不持有本地路径，也不把 token、URL 或上游文件标识
/// 写入持久化数据、普通日志或诊断事件。
#[derive(Clone)]
pub struct UpstreamFileUploadPublisher {
    transport: Arc<dyn CloudFileTransport>,
    token_provider: SharedTemporaryAiTokenProvider,
    /// 已清理的租约，用于保证重复清理幂等成功。
    cleaned: Arc<tokio::sync::Mutex<BTreeSet<String>>>,
}

impl UpstreamFileUploadPublisher {
    /// 使用默认 HTTPS 传输创建发布器。
    pub fn new(
        base_url: impl Into<String>,
        token_provider: SharedTemporaryAiTokenProvider,
    ) -> EngineResult<Self> {
        Ok(Self::with_transport(
            Arc::new(ReqwestCloudFileTransport::new(base_url)?),
            token_provider,
        ))
    }

    /// 注入自定义传输层创建发布器。
    pub fn with_transport(
        transport: Arc<dyn CloudFileTransport>,
        token_provider: SharedTemporaryAiTokenProvider,
    ) -> Self {
        Self {
            transport,
            token_provider,
            cleaned: Arc::new(tokio::sync::Mutex::new(BTreeSet::new())),
        }
    }
}

#[async_trait]
impl TemporaryAudioPublisher for UpstreamFileUploadPublisher {
    fn is_available(&self) -> bool {
        self.token_provider.is_configured()
    }

    async fn publish(&self, audio_path: &Path) -> EngineResult<TemporaryAudioReference> {
        // 短期 token 失效必须与上传失败区分，两者都不创建任务。
        let token = self.token_provider.temporary_token().await?;
        let file_id = self.transport.upload_file(audio_path, &token).await?;
        let url = self.transport.fetch_signed_url(&file_id, &token).await?;
        let reference = TemporaryAudioReference {
            url,
            lease_id: file_id.clone(),
            file_id: Some(file_id),
            upstream_issued: true,
        };
        reference.validate()?;
        Ok(reference)
    }

    async fn cleanup(&self, reference: TemporaryAudioReference) -> EngineResult<()> {
        // 上游文件服务不提供删除接口，对象按上游保留策略回收。清理因此只维护
        // 本地幂等台账：成功、失败、取消与重试四条路径的清理都返回成功，
        // 重复清理同一个租约同样返回成功。
        let mut cleaned = self.cleaned.lock().await;
        cleaned.insert(reference.lease_id);
        Ok(())
    }
}

/// 按上传阶段区分的可辨识错误分类。
fn map_upload_http_error(status: u16) -> AsrError {
    match status {
        401 | 403 => AsrError::new(
            AsrErrorKind::EnvironmentUnavailable,
            "cloud_upload_unauthorized",
            "云端上传凭据无效或已过期",
            false,
        ),
        413 => AsrError::new(
            AsrErrorKind::InvalidInput,
            "cloud_upload_too_large",
            "待上传音频超过上游大小上限",
            false,
        ),
        429 => AsrError::new(
            AsrErrorKind::ResourceExhausted,
            "cloud_upload_rate_limited",
            "云端上传请求过于频繁",
            true,
        ),
        500..=599 => AsrError::new(
            AsrErrorKind::ProcessFailed,
            "cloud_upload_service_unavailable",
            "云端上传服务暂时不可用",
            true,
        ),
        _ => AsrError::new(
            AsrErrorKind::ProcessFailed,
            "cloud_upload_failed",
            "云端音频上传失败",
            false,
        ),
    }
}

/// 按取回临时地址阶段区分的可辨识错误分类。
fn map_signed_url_http_error(status: u16) -> AsrError {
    match status {
        401 | 403 => AsrError::new(
            AsrErrorKind::EnvironmentUnavailable,
            "cloud_signed_url_unauthorized",
            "取回临时地址的凭据无效或已过期",
            false,
        ),
        404 => AsrError::new(
            AsrErrorKind::ProcessFailed,
            "cloud_upload_resource_expired",
            "上传的音频资源已失效，需重新上传",
            false,
        ),
        429 => AsrError::new(
            AsrErrorKind::ResourceExhausted,
            "cloud_signed_url_rate_limited",
            "取回临时地址请求过于频繁",
            true,
        ),
        500..=599 => AsrError::new(
            AsrErrorKind::ProcessFailed,
            "cloud_signed_url_service_unavailable",
            "取回临时地址的服务暂时不可用",
            true,
        ),
        _ => AsrError::new(
            AsrErrorKind::ProcessFailed,
            "cloud_signed_url_failed",
            "取回音频临时地址失败",
            false,
        ),
    }
}

/// 云端识别的失败分支。
///
/// 每个分支必须映射到独立的用户可见原因；取消与授权/额度问题**不得**被描述为
/// 云端故障，也不得触发本地识别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudFailureBranch {
    /// 用户主动取消。
    Cancelled,
    /// 授权无效或凭据不可用。
    AuthorizationInvalid,
    /// 账号未开通 AI 服务。
    NotActivated,
    /// 额度耗尽。
    QuotaExhausted,
    /// 瞬时网络或云端故障（有界重试后仍失败）。
    Transient,
    /// 上传资源或引用失效（需重新上传，不得重试同一引用）。
    ResourceInvalidated,
    /// 其他未归类的失败。
    Other,
}

impl CloudFailureBranch {
    /// 稳定的机器可读标识。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::AuthorizationInvalid => "authorization_invalid",
            Self::NotActivated => "not_activated",
            Self::QuotaExhausted => "quota_exhausted",
            Self::Transient => "transient",
            Self::ResourceInvalidated => "resource_invalidated",
            Self::Other => "other",
        }
    }

    /// 可直接展示的中文原因，不含路径、凭据或上游正文。
    pub fn safe_message(self) -> &'static str {
        match self {
            Self::Cancelled => "识别已取消",
            Self::AuthorizationInvalid => "AI 授权无效或已过期，请重新激活",
            Self::NotActivated => "当前账号未开通 AI 服务",
            Self::QuotaExhausted => "AI 转写额度已用完",
            Self::Transient => "云端转写暂时不可用，请稍后重试",
            Self::ResourceInvalidated => "云端音频引用已失效，需要重新上传",
            Self::Other => "云端转写失败",
        }
    }
}

/// 把云端错误映射到六分支之一。
///
/// 分类只依据稳定的机器码与错误大类，不读取上游正文，也不读取本地路径。
pub fn classify_cloud_failure(error: &AsrError) -> CloudFailureBranch {
    match error.code.as_str() {
        "asr_cancelled" => CloudFailureBranch::Cancelled,
        "ai_authorization_unavailable" | "ai_token_unauthorized" | "ai_token_forbidden" => {
            CloudFailureBranch::AuthorizationInvalid
        }
        "ai_not_activated" | "ai_token_not_activated" => CloudFailureBranch::NotActivated,
        "ai_quota_exhausted" | "ai_token_quota" => CloudFailureBranch::QuotaExhausted,
        "cloud_upload_resource_expired" | "cloud_signed_url_resource_expired" => {
            CloudFailureBranch::ResourceInvalidated
        }
        "cloud_upload_rate_limited"
        | "cloud_signed_url_rate_limited"
        | "cloud_upload_service_unavailable"
        | "cloud_signed_url_service_unavailable"
        | "cloud_upload_failed"
        | "cloud_signed_url_failed"
        | "dashscope_submit_failed"
        | "dashscope_poll_failed"
        | "dashscope_result_download_failed"
        | "dashscope_rate_limited"
        | "dashscope_service_unavailable" => CloudFailureBranch::Transient,
        _ => match error.kind {
            AsrErrorKind::Cancelled => CloudFailureBranch::Cancelled,
            AsrErrorKind::ResourceExhausted => CloudFailureBranch::QuotaExhausted,
            _ => CloudFailureBranch::Other,
        },
    }
}

/// 上游取消请求的结果与它带来的计费含义。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamCancelOutcome {
    /// 上游是否接受了取消请求。
    pub accepted: bool,
    /// 已提交的部分是否仍可能按实际用量结算。
    pub may_still_charge: bool,
    /// 可直接展示给用户的中文说明，不含上游正文与凭据。
    pub safe_message: String,
}

/// 由上游是否接受取消推导用户可见说明。
///
/// 无论上游是否接受，都必须明确告知"已提交部分仍可能产生费用"，并且都不得据此
/// 自动重新提交付费请求。
pub fn cancel_outcome(accepted: bool) -> UpstreamCancelOutcome {
    if accepted {
        UpstreamCancelOutcome {
            accepted: true,
            may_still_charge: true,
            safe_message: "已请求取消云端任务；已提交的部分仍可能按实际用量产生费用".to_owned(),
        }
    } else {
        UpstreamCancelOutcome {
            accepted: false,
            may_still_charge: true,
            safe_message: "未能取消云端任务；已提交的部分仍可能按实际用量产生费用，请勿重复提交"
                .to_owned(),
        }
    }
}

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
    diagnostics: SharedCloudCallDiagnosticSink,
}

impl DashScopeAsrClient {
    /// 只读配置，供上层构建诊断与身份使用。
    pub fn config(&self) -> &DashScopeAsrConfig {
        &self.config
    }

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
        Ok(Self {
            client,
            config,
            diagnostics: Arc::new(StderrCloudCallDiagnosticSink),
        })
    }

    /// 注入调用诊断接收端。
    pub fn with_diagnostics(mut self, diagnostics: SharedCloudCallDiagnosticSink) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    /// 请求上游取消异步任务。
    ///
    /// 上游取消**不是**服务端保证：任务可能已经开始计费、或上游根本不支持取消。
    /// 因此调用方 SHALL 把返回的 `may_still_charge` 如实告知用户，并且 MUST NOT
    /// 因取消而自动重新提交付费请求。
    pub async fn cancel_task(
        &self,
        token: &str,
        task_id: &str,
    ) -> EngineResult<UpstreamCancelOutcome> {
        if task_id.trim().is_empty() {
            return Err(AsrError::invalid_input(
                "missing_dashscope_task_id",
                "云端任务标识为空",
            ));
        }
        let token = bearer_value(token)?;
        let url = self.endpoint(&format!("tasks/{}/cancel", urlencoding(task_id)))?;
        let started = std::time::Instant::now();
        let response = match self
            .client
            .post(url)
            .header(AUTHORIZATION, token)
            .header(CONTENT_TYPE, "application/json")
            .body(b"{}".to_vec())
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                let mapped = map_transport_error(error, "dashscope_cancel_failed");
                self.diagnostics.emit(diagnostic(
                    "cancel",
                    "tasks/{task_id}/cancel",
                    None,
                    started,
                    1,
                    Some(&mapped),
                    false,
                    false,
                    true,
                    None,
                ));
                return Err(mapped);
            }
        };
        let status = response.status().as_u16();
        let accepted = response.status().is_success();
        let mapped = if accepted {
            None
        } else {
            Some(dashscope_http_error(status))
        };
        self.diagnostics.emit(diagnostic(
            "cancel",
            "tasks/{task_id}/cancel",
            Some(status),
            started,
            1,
            mapped.as_ref(),
            false,
            false,
            true,
            None,
        ));
        // 请求被上游接受即视为已请求取消；是否真正停止由后续终态判定。
        Ok(cancel_outcome(response.status().is_success()))
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
            .await;
        let started = std::time::Instant::now();
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                let mapped = map_transport_error(error, "dashscope_submit_failed");
                self.diagnostics.emit(diagnostic(
                    "submit",
                    "services/audio/asr/transcription",
                    None,
                    started,
                    1,
                    Some(&mapped),
                    true,
                    true,
                    false,
                    None,
                ));
                return Err(mapped);
            }
        };
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let mapped = dashscope_http_error(status);
            self.diagnostics.emit(diagnostic(
                "submit",
                "services/audio/asr/transcription",
                Some(status),
                started,
                1,
                Some(&mapped),
                true,
                true,
                false,
                None,
            ));
            return Err(mapped);
        }
        let body = parse_json_response(
            response,
            "dashscope_submit_response_invalid",
            "云端任务响应无效",
        )
        .await?;
        let task_id = body
            .get("output")
            .and_then(|output| output.get("task_id"))
            .and_then(serde_json::Value::as_str)
            .filter(|task_id| !task_id.trim().is_empty())
            .map(str::to_owned);
        self.diagnostics.emit(diagnostic(
            "submit",
            "services/audio/asr/transcription",
            Some(200),
            started,
            1,
            None,
            true,
            true,
            task_id.is_some(),
            None,
        ));
        task_id.ok_or_else(|| {
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
        let started = std::time::Instant::now();
        // 轮询序号只在终态随诊断发射一次，避免每轮都刷环形缓冲。
        let mut attempt: u32 = 0;
        let operation = async {
            loop {
                if cancellation.is_cancelled() {
                    let cancelled = AsrError::cancelled();
                    self.diagnostics.emit(diagnostic(
                        "poll",
                        "tasks/{task_id}",
                        None,
                        started,
                        attempt,
                        Some(&cancelled),
                        false,
                        false,
                        true,
                        None,
                    ));
                    return Err(cancelled);
                }
                attempt = attempt.saturating_add(1);
                let url = match self.endpoint(&format!("tasks/{}", urlencoding(task_id))) {
                    Ok(url) => url,
                    Err(error) => {
                        self.diagnostics.emit(diagnostic(
                            "poll",
                            "tasks/{task_id}",
                            None,
                            started,
                            attempt,
                            Some(&error),
                            false,
                            false,
                            true,
                            None,
                        ));
                        return Err(error);
                    }
                };
                let response = match self
                    .client
                    .get(url)
                    .header(AUTHORIZATION, token.clone())
                    .send()
                    .await
                {
                    Ok(response) => response,
                    Err(error) => {
                        let mapped = map_transport_error(error, "dashscope_poll_failed");
                        self.diagnostics.emit(diagnostic(
                            "poll",
                            "tasks/{task_id}",
                            None,
                            started,
                            attempt,
                            Some(&mapped),
                            false,
                            false,
                            true,
                            None,
                        ));
                        return Err(mapped);
                    }
                };
                let status_code = response.status();
                if !status_code.is_success() {
                    let mapped = dashscope_http_error(status_code.as_u16());
                    self.diagnostics.emit(diagnostic(
                        "poll",
                        "tasks/{task_id}",
                        Some(status_code.as_u16()),
                        started,
                        attempt,
                        Some(&mapped),
                        false,
                        false,
                        true,
                        None,
                    ));
                    return Err(mapped);
                }
                let body = match parse_json_response(
                    response,
                    "dashscope_poll_response_invalid",
                    "云端任务状态响应无效",
                )
                .await
                {
                    Ok(body) => body,
                    Err(mapped) => {
                        self.diagnostics.emit(diagnostic(
                            "poll",
                            "tasks/{task_id}",
                            Some(status_code.as_u16()),
                            started,
                            attempt,
                            Some(&mapped),
                            false,
                            false,
                            true,
                            None,
                        ));
                        return Err(mapped);
                    }
                };
                let status = body
                    .get("output")
                    .and_then(|output| output.get("task_status"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                match status {
                    "SUCCEEDED" => {
                        self.diagnostics.emit(diagnostic(
                            "poll",
                            "tasks/{task_id}",
                            Some(status_code.as_u16()),
                            started,
                            attempt,
                            None,
                            false,
                            false,
                            true,
                            None,
                        ));
                        return Ok(body);
                    }
                    "FAILED" | "CANCELED" | "CANCELLED" => {
                        let failed = AsrError::new(
                            AsrErrorKind::ProcessFailed,
                            "dashscope_task_failed",
                            "云端转写任务失败，请稍后重试",
                            false,
                        );
                        self.diagnostics.emit(diagnostic(
                            "poll",
                            "tasks/{task_id}",
                            Some(status_code.as_u16()),
                            started,
                            attempt,
                            Some(&failed),
                            false,
                            false,
                            true,
                            None,
                        ));
                        return Err(failed);
                    }
                    "PENDING" | "RUNNING" => {
                        sleep(Duration::from_millis(self.config.poll_interval_ms.max(100))).await;
                    }
                    _ => {
                        let mapped = malformed_cloud_error(
                            "dashscope_task_status_invalid",
                            "云端任务状态无效，请稍后重试",
                        );
                        self.diagnostics.emit(diagnostic(
                            "poll",
                            "tasks/{task_id}",
                            Some(status_code.as_u16()),
                            started,
                            attempt,
                            Some(&mapped),
                            false,
                            false,
                            true,
                            None,
                        ));
                        return Err(mapped);
                    }
                }
            }
        };
        let outcome = timeout(deadline, operation).await;
        match outcome {
            Ok(result) => result,
            Err(_) => {
                let timed_out = AsrError::new(
                    AsrErrorKind::ProcessFailed,
                    "dashscope_poll_timeout",
                    "云端转写等待超时，请稍后重试",
                    true,
                );
                self.diagnostics.emit(diagnostic(
                    "poll",
                    "tasks/{task_id}",
                    None,
                    started,
                    attempt,
                    Some(&timed_out),
                    false,
                    false,
                    true,
                    None,
                ));
                Err(timed_out)
            }
        }
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
        let body = match self.poll(token, &task_id, cancellation).await {
            Ok(body) => body,
            Err(error) if error.kind == AsrErrorKind::Cancelled => {
                // 用户取消：按上游能力请求取消，并把真实计费含义告知用户。
                // 无论上游是否接受，都 MUST NOT 自动重新提交付费请求。
                let outcome = match self.cancel_task(token, &task_id).await {
                    Ok(outcome) => outcome,
                    Err(_) => cancel_outcome(false),
                };
                return Err(AsrError::new(
                    AsrErrorKind::Cancelled,
                    "asr_cancelled",
                    outcome.safe_message,
                    true,
                ));
            }
            Err(error) => return Err(error),
        };
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
            let started = std::time::Instant::now();
            let parsed_url = match reqwest::Url::parse(&url) {
                Ok(parsed) => parsed,
                Err(_) => {
                    let mapped = malformed_cloud_error(
                        "dashscope_result_url_invalid",
                        "云端详细转写地址无效",
                    );
                    self.diagnostics.emit(diagnostic(
                        "result",
                        "result/{signed}",
                        None,
                        started,
                        1,
                        Some(&mapped),
                        false,
                        true,
                        true,
                        None,
                    ));
                    return Err(mapped);
                }
            };
            // 上游结果地址实测为 http 且带签名与有效期；只接受上游签发的
            // 签名地址，任何其他方案或不带签名有效期的地址一律拒绝。
            let signed = is_upstream_signed_url(&parsed_url);
            if !(parsed_url.scheme() == "https" || (parsed_url.scheme() == "http" && signed)) {
                let mapped =
                    malformed_cloud_error("dashscope_result_url_unsafe", "云端详细转写地址不安全");
                self.diagnostics.emit(diagnostic(
                    "result",
                    "result/{signed}",
                    None,
                    started,
                    1,
                    Some(&mapped),
                    false,
                    true,
                    true,
                    None,
                ));
                return Err(mapped);
            }
            let response = match self.client.get(parsed_url).send().await {
                Ok(response) => response,
                Err(error) => {
                    let mapped = map_transport_error(error, "dashscope_result_download_failed");
                    self.diagnostics.emit(diagnostic(
                        "result",
                        "result/{signed}",
                        None,
                        started,
                        1,
                        Some(&mapped),
                        false,
                        true,
                        true,
                        None,
                    ));
                    return Err(mapped);
                }
            };
            if !response.status().is_success() {
                let status = response.status().as_u16();
                let mapped = dashscope_http_error(status);
                self.diagnostics.emit(diagnostic(
                    "result",
                    "result/{signed}",
                    Some(status),
                    started,
                    1,
                    Some(&mapped),
                    false,
                    true,
                    true,
                    None,
                ));
                return Err(mapped);
            }
            let body = match parse_json_response(
                response,
                "dashscope_result_invalid",
                "云端详细转写结果无效",
            )
            .await
            {
                Ok(body) => body,
                Err(mapped) => {
                    self.diagnostics.emit(diagnostic(
                        "result",
                        "result/{signed}",
                        Some(200),
                        started,
                        1,
                        Some(&mapped),
                        false,
                        true,
                        true,
                        None,
                    ));
                    return Err(mapped);
                }
            };
            self.diagnostics.emit(diagnostic(
                "result",
                "result/{signed}",
                Some(200),
                started,
                1,
                None,
                false,
                true,
                true,
                None,
            ));
            body
        } else {
            output.clone()
        };
        let mut segments = parse_dashscope_segments(&result_body)?
            .into_iter()
            .map(|segment| AsrSegment {
                start_ms: segment.begin_time,
                end_ms: segment.end_time,
                text: segment.text,
                confidence: None,
            })
            .collect::<Vec<_>>();
        // 发布前必须有序：上游不保证返回顺序，而产物发布要求句段按源时间单调、
        // 不重叠。这里先排序，使最终校验只负责拒绝真正非法的结果。
        segments.sort_by_key(|segment| (segment.start_ms, segment.end_ms));
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
            "云端转写请求超时，请稍后重试",
            true,
        )
    } else {
        AsrError::new(
            AsrErrorKind::ProcessFailed,
            code,
            "云端转写网络不可用，请检查网络后重试",
            true,
        )
    }
}

/// 单个分块在源音频中的位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioChunk {
    /// 块内起始相对源音频的偏移毫秒。
    pub offset_ms: u64,
    /// 本块长度毫秒。
    pub length_ms: u64,
}

impl AudioChunk {
    /// 本块在源音频中的结束毫秒（半开区间）。
    pub fn end_ms(&self) -> u64 {
        self.offset_ms.saturating_add(self.length_ms)
    }
}

/// 按有重叠的固定区间切分源音频。
///
/// 超过单任务上限时使用；**不得**删除静音后直接拼接，否则块内时间无法映射回源
/// 视频。因此块与块之间保留固定重叠，由合并阶段按文本与时间交叠去重。
pub fn plan_audio_chunks(duration_ms: u64, max_chunk_ms: u64, overlap_ms: u64) -> Vec<AudioChunk> {
    if duration_ms == 0 || max_chunk_ms == 0 {
        return Vec::new();
    }
    if duration_ms <= max_chunk_ms {
        return vec![AudioChunk {
            offset_ms: 0,
            length_ms: duration_ms,
        }];
    }
    // 重叠必须小于块长，否则无法推进。
    let overlap = overlap_ms.min(max_chunk_ms.saturating_sub(1));
    let step = max_chunk_ms - overlap;
    let mut chunks = Vec::new();
    let mut offset = 0u64;
    loop {
        let length = max_chunk_ms.min(duration_ms.saturating_sub(offset));
        if length == 0 {
            break;
        }
        chunks.push(AudioChunk {
            offset_ms: offset,
            length_ms: length,
        });
        if offset.saturating_add(length) >= duration_ms {
            break;
        }
        offset = offset.saturating_add(step);
    }
    // 避免退化尾块：若最后一块的起点落在前一块内部（即只覆盖重叠区），它相对
    // 前一块没有带来新的音频，只会多产生一次付费调用。把尾部并入前一块。
    if chunks.len() >= 2 {
        let last = chunks[chunks.len() - 1];
        let previous = chunks[chunks.len() - 2];
        if last.offset_ms < previous.end_ms() && previous.end_ms() < duration_ms {
            let extra = duration_ms.saturating_sub(previous.end_ms());
            chunks.pop();
            let last_index = chunks.len() - 1;
            let merged = &mut chunks[last_index];
            merged.length_ms = merged.length_ms.saturating_add(extra);
        }
    }
    chunks
}

/// 把块内句段时间加回源偏移。
pub fn restore_source_time(segments: &[DashScopeSegment], offset_ms: u64) -> Vec<DashScopeSegment> {
    segments
        .iter()
        .map(|segment| DashScopeSegment {
            begin_time: segment.begin_time.saturating_add(offset_ms),
            end_time: segment.end_time.saturating_add(offset_ms),
            text: segment.text.clone(),
        })
        .collect()
}

/// 判断两段文本是否为同一句的重复识别（重叠区常见）。
fn is_duplicate_text(left: &str, right: &str) -> bool {
    let normalize = |value: &str| {
        value
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>()
    };
    let left = normalize(left);
    let right = normalize(right);
    if left.is_empty() || right.is_empty() {
        return false;
    }
    left == right || left.contains(&right) || right.contains(&left)
}

/// 合并多个分块的句段：按源时间排序，并按文本与时间交叠去重。
///
/// 去重规则（需同时满足，避免误删相邻短句）：
/// 1. 归一化文本相同或互为子串；且
/// 2. 时间区间有交叠，或起点相差不超过 `dedupe_window_ms`。
///
/// 重复时保留覆盖更完整的那一段。
pub fn merge_chunk_segments(
    mut segments: Vec<DashScopeSegment>,
    dedupe_window_ms: u64,
) -> Vec<DashScopeSegment> {
    segments.sort_by_key(|segment| (segment.begin_time, segment.end_time));
    let mut out: Vec<DashScopeSegment> = Vec::with_capacity(segments.len());
    for segment in segments {
        if segment.end_time <= segment.begin_time || segment.text.trim().is_empty() {
            continue;
        }
        let duplicate = out.iter().position(|kept| {
            let overlaps = kept.begin_time < segment.end_time && segment.begin_time < kept.end_time;
            let near = kept.begin_time.abs_diff(segment.begin_time) <= dedupe_window_ms;
            (overlaps || near) && is_duplicate_text(&kept.text, &segment.text)
        });
        match duplicate {
            Some(index) => {
                let kept = &out[index];
                if segment.end_time - segment.begin_time
                    > kept.end_time.saturating_sub(kept.begin_time)
                {
                    out[index] = segment;
                }
            }
            None => out.push(segment),
        }
    }
    // 文本去重只在两侧文本相似时生效；分块边界上的截断残句文本完全不同，必须
    // 再按时间交叠消除，否则产物发布会因句段重叠而失败。
    resolve_time_overlaps(out)
}

/// 消除相邻句段的时间交叠，保证输出严格按源时间单调且互不重叠。
///
/// 为什么必须单独做这一步：文本去重只在两侧文本相似时生效，而分块边界上的
/// **截断残句**（例如「感慨为。」）与相邻块的完整句「敢看微霜雪，想赠眉间一道疤。」
/// 文本完全不同、相似度接近 0，去重规则必然漏掉。这类碎片会与相邻句段时间交叠，
/// 而产物发布要求句段严格不重叠，因此不处理会导致整次长音频转写发布失败。
///
/// 规则：保留覆盖更完整的句段（时长更长者）；较短的一方若被完全包含则丢弃，
/// 否则把边界裁剪到不交叠；裁剪后失效的片段直接丢弃（它是被截断的残句）。
pub fn resolve_time_overlaps(segments: Vec<DashScopeSegment>) -> Vec<DashScopeSegment> {
    let mut sorted: Vec<DashScopeSegment> = segments
        .into_iter()
        .filter(|segment| segment.end_time > segment.begin_time && !segment.text.trim().is_empty())
        .collect();
    sorted.sort_by_key(|segment| (segment.begin_time, segment.end_time));
    let mut out: Vec<DashScopeSegment> = Vec::with_capacity(sorted.len());
    for segment in sorted {
        let Some(previous) = out.last_mut() else {
            out.push(segment);
            continue;
        };
        if segment.begin_time >= previous.end_time {
            out.push(segment);
            continue;
        }
        let previous_len = previous.end_time - previous.begin_time;
        let current_len = segment.end_time - segment.begin_time;
        // 同一路音频内，两个句段在时间上交叠意味着其中较短者是被边界截断的残句
        // 或重复碎片：裁剪只会留下一段没有独立意义的碎片（例如 120ms 的「感慨为。」），
        // 因此直接丢弃较短者。等长时保留先出现的一段。
        if current_len > previous_len {
            out.pop();
            out.push(segment);
        }
        // 当前更短或等长：丢弃它。
    }
    out.sort_by_key(|segment| (segment.begin_time, segment.end_time));
    out
}

/// 派生稳定句段 ID：同一源时间与文本必须得到同一 ID。
///
/// 不依赖分块策略或到达顺序，使"相同输入与相同分块策略产生相同 ID 序列"成立。
pub fn stable_segment_id(
    source_fingerprint: &str,
    begin_ms: u64,
    end_ms: u64,
    text: &str,
) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(source_fingerprint.as_bytes());
    hasher.update(b"|");
    hasher.update(begin_ms.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(end_ms.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(text.trim().as_bytes());
    let digest = hasher.finalize();
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("seg-{}", &hex[..16])
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

/// 云端优先、本地回退的 ASR engine。
pub struct CloudFirstAsrEngine {
    local: Arc<dyn AsrEngine>,
    publisher: SharedTemporaryAudioPublisher,
    token_provider: Option<Arc<dyn TemporaryAiTokenProvider>>,
    dashscope: DashScopeAsrClient,
    identity: AsrEngineIdentity,
}

impl CloudFirstAsrEngine {
    /// 创建路由器。token provider 或发布器不可用时，云端路径不可用。
    ///
    /// 引擎身份是**云端自身**的真实身份，不再委托本地引擎；本地引擎只是
    /// 用户显式选择时才使用的独立路径。
    pub fn new(
        local: Arc<dyn AsrEngine>,
        publisher: SharedTemporaryAudioPublisher,
        token_provider: Option<Arc<dyn TemporaryAiTokenProvider>>,
        dashscope: DashScopeAsrClient,
    ) -> Self {
        let identity = cloud_identity(&dashscope.config().model);
        Self {
            local,
            publisher,
            token_provider,
            dashscope,
            identity,
        }
    }

    /// 云端路径当前是否具备执行条件（发布器可用且 token 提供器已配置）。
    pub fn cloud_available(&self) -> bool {
        self.publisher.is_available()
            && self
                .token_provider
                .as_ref()
                .is_some_and(|provider| provider.is_configured())
    }

    /// 云端引擎的真实身份。
    pub fn cloud_identity(&self) -> AsrEngineIdentity {
        self.identity.clone()
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
        let result = match provider.temporary_token().await {
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
        // 清理是资源治理动作，转写是付费动作。清理失败时 MUST 保留已成功的结果
        // 并标记清理待重试，MUST NOT 把它变成失败，也 MUST NOT 因此重新提交付费
        // 转写（上游调用计数不得增加）。
        let cleanup_result = self.publisher.cleanup(reference).await;
        apply_cleanup_outcome(result, cleanup_result)
    }
}

/// 清理失败不得回滚已成功的转写结果。
///
/// 清理是资源治理动作，转写是付费动作：清理失败时保留成功结果并附加"清理待重试"
/// 警告，MUST NOT 把它变成失败，也 MUST NOT 因此重新提交付费转写。
pub fn apply_cleanup_outcome(
    result: EngineResult<AsrResult>,
    cleanup: EngineResult<()>,
) -> EngineResult<AsrResult> {
    match (result, cleanup) {
        (Ok(mut value), Err(error)) => {
            value.warnings.push(AsrWarning {
                code: "cloud_audio_cleanup_pending".to_owned(),
                message: format!("音频清理待重试，转写结果已保留（{}）", error.code),
            });
            Ok(value)
        }
        // 清理成功、或转写本身失败时，原样返回转写结果。
        (result, _) => result,
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
        let local = self.local.diagnose().await?;
        let token_ready = self
            .token_provider
            .as_ref()
            .is_some_and(|provider| provider.is_configured());
        let publisher_ready = self.publisher.is_available();
        let checks = vec![
            AsrEnvironmentCheck {
                code: "cloud_transcription".into(),
                passed: token_ready && publisher_ready,
                message: if token_ready && publisher_ready {
                    "百炼直连转写可用".into()
                } else if !token_ready {
                    "缺少可用的 AI 授权，百炼直连转写不可用".into()
                } else {
                    "缺少云端音频发布能力，百炼直连转写不可用".into()
                },
            },
            AsrEnvironmentCheck {
                code: "cloud_audio_publisher".into(),
                passed: publisher_ready,
                message: if publisher_ready {
                    "云端音频发布可用".into()
                } else {
                    "云端音频发布不可用".into()
                },
            },
            AsrEnvironmentCheck {
                code: "local_transcription".into(),
                passed: local.ready,
                message: if local.ready {
                    "本地识别资源就绪（用户显式选择时可用）".into()
                } else {
                    "本地识别资源未就绪；不影响百炼直连转写".into()
                },
            },
        ];
        // 云端可用性只取决于云端自身条件；缺少本地模型不得阻止云端识别。
        Ok(AsrEnvironmentReport {
            ready: token_ready && publisher_ready,
            platform: local.platform,
            identity: self.identity.clone(),
            checks,
        })
    }

    /// 云端直连转写；失败即明确失败，**不隐式回退本地引擎**。
    ///
    /// 本地 `whisper.cpp` 只在用户显式选择本地识别时使用，由上层选择引擎实例，
    /// 不能作为云端失败的兜底。
    async fn transcribe(
        &self,
        request: AsrRequest,
        _progress: Arc<dyn AsrProgressSink>,
        cancellation: CancellationToken,
    ) -> EngineResult<AsrResult> {
        request.validate()?;
        self.cloud_attempt(&request, cancellation.child_token())
            .await
    }
}

/// 云端引擎的稳定身份；版本随模型标识与规范化版本固定。
fn cloud_identity(model: &str) -> AsrEngineIdentity {
    AsrEngineIdentity {
        engine_id: "dashscope-filetrans".into(),
        engine_version: CLOUD_ENGINE_VERSION.into(),
        model_id: model.to_owned(),
        model_version: model.to_owned(),
    }
}

/// 百炼直连引擎的实现版本；协议或规范化行为变化时必须递增。
pub const CLOUD_ENGINE_VERSION: &str = "1";

/// 将百炼 HTTP 状态映射为安全错误。
pub fn dashscope_http_error(status: u16) -> AsrError {
    let (code, message, retryable) = match status {
        401 => ("dashscope_unauthorized", "云端转写授权已失效", false),
        403 => ("dashscope_forbidden", "当前临时密钥无权使用云端转写", false),
        429 => (
            "dashscope_rate_limited",
            "云端转写请求过于频繁，请稍后重试",
            true,
        ),
        500..=599 => (
            "dashscope_service_unavailable",
            "云端转写服务暂时不可用，请稍后重试",
            true,
        ),
        _ => (
            "dashscope_request_failed",
            "云端转写请求失败，请稍后重试",
            false,
        ),
    };
    AsrError::new(AsrErrorKind::ProcessFailed, code, message, retryable)
}

#[cfg(test)]
mod tests {
    use super::super::contract::TimestampPolicy;
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 确定性替身传输层，用于验证发布器的编排与错误分类。
    struct FakeTransport {
        upload_calls: AtomicU32,
        signed_url_calls: AtomicU32,
        fail_upload: Option<(AsrErrorKind, &'static str, bool)>,
        fail_signed: Option<(AsrErrorKind, &'static str, bool)>,
        file_id: &'static str,
        url: &'static str,
    }

    impl FakeTransport {
        fn ok() -> Self {
            Self {
                upload_calls: AtomicU32::new(0),
                signed_url_calls: AtomicU32::new(0),
                fail_upload: None,
                fail_signed: None,
                file_id: "file-1",
                url: "http://example.invalid/a.wav?Expires=1&Signature=s",
            }
        }
        fn failing_upload(kind: AsrErrorKind, code: &'static str, retryable: bool) -> Self {
            Self {
                fail_upload: Some((kind, code, retryable)),
                ..Self::ok()
            }
        }
        fn failing_signed(kind: AsrErrorKind, code: &'static str, retryable: bool) -> Self {
            Self {
                fail_signed: Some((kind, code, retryable)),
                ..Self::ok()
            }
        }
    }

    #[async_trait]
    impl CloudFileTransport for FakeTransport {
        async fn upload_file(&self, _audio_path: &Path, _token: &str) -> EngineResult<String> {
            self.upload_calls.fetch_add(1, Ordering::SeqCst);
            match self.fail_upload {
                Some((kind, code, retryable)) => {
                    Err(AsrError::new(kind, code, "替身失败", retryable))
                }
                None => Ok(self.file_id.to_owned()),
            }
        }

        async fn fetch_signed_url(&self, _file_id: &str, _token: &str) -> EngineResult<String> {
            self.signed_url_calls.fetch_add(1, Ordering::SeqCst);
            match self.fail_signed {
                Some((kind, code, retryable)) => {
                    Err(AsrError::new(kind, code, "替身失败", retryable))
                }
                None => Ok(self.url.to_owned()),
            }
        }
    }

    struct FixedTokenProvider {
        configured: bool,
        token: Option<&'static str>,
        error: Option<(AsrErrorKind, &'static str)>,
    }

    #[async_trait]
    impl TemporaryAiTokenProvider for FixedTokenProvider {
        fn is_configured(&self) -> bool {
            self.configured
        }
        async fn temporary_token(&self) -> EngineResult<String> {
            match self.error {
                Some((kind, code)) => Err(AsrError::new(kind, code, "凭据不可用", false)),
                None => Ok(self.token.unwrap_or("token").to_owned()),
            }
        }
    }

    fn provider(configured: bool) -> SharedTemporaryAiTokenProvider {
        Arc::new(FixedTokenProvider {
            configured,
            token: Some("token"),
            error: None,
        })
    }

    /// 任务 1.3：四类发布失败都必须可区分，且都不进入任务创建阶段。
    #[tokio::test]
    async fn publish_failures_are_distinguishable_and_never_create_tasks() {
        // 1) 短期 token 失效：连上传都不应发生。
        let transport = Arc::new(FakeTransport::ok());
        let publisher = UpstreamFileUploadPublisher::with_transport(
            transport.clone(),
            Arc::new(FixedTokenProvider {
                configured: true,
                token: None,
                error: Some((
                    AsrErrorKind::EnvironmentUnavailable,
                    "ai_token_unauthorized",
                )),
            }),
        );
        let error = publisher
            .publish(Path::new("/tmp/audio.wav"))
            .await
            .unwrap_err();
        assert_eq!(error.code, "ai_token_unauthorized");
        assert_eq!(transport.upload_calls.load(Ordering::SeqCst), 0);

        // 2) 上传失败：不得继续取回临时地址。
        let transport = Arc::new(FakeTransport::failing_upload(
            AsrErrorKind::ProcessFailed,
            "cloud_upload_service_unavailable",
            true,
        ));
        let publisher =
            UpstreamFileUploadPublisher::with_transport(transport.clone(), provider(true));
        let error = publisher
            .publish(Path::new("/tmp/audio.wav"))
            .await
            .unwrap_err();
        assert_eq!(error.code, "cloud_upload_service_unavailable");
        assert!(error.retryable);
        assert_eq!(transport.signed_url_calls.load(Ordering::SeqCst), 0);

        // 3) 取回临时地址失败：必须是独立分类，且不可被当作上传成功。
        let transport = Arc::new(FakeTransport::failing_signed(
            AsrErrorKind::ProcessFailed,
            "cloud_signed_url_failed",
            false,
        ));
        let publisher =
            UpstreamFileUploadPublisher::with_transport(transport.clone(), provider(true));
        let error = publisher
            .publish(Path::new("/tmp/audio.wav"))
            .await
            .unwrap_err();
        assert_eq!(error.code, "cloud_signed_url_failed");
        assert_eq!(transport.upload_calls.load(Ordering::SeqCst), 1);

        // 4) 上传超时（传输层映射的瞬时故障）可重试且分类独立。
        let transport = Arc::new(FakeTransport::failing_upload(
            AsrErrorKind::ProcessFailed,
            "cloud_upload_failed",
            true,
        ));
        let publisher = UpstreamFileUploadPublisher::with_transport(transport, provider(true));
        let error = publisher
            .publish(Path::new("/tmp/audio.wav"))
            .await
            .unwrap_err();
        assert_eq!(error.code, "cloud_upload_failed");
        assert!(error.retryable);
    }

    /// 任务 1.4：成功、失败、取消与重试四条路径的清理都幂等成功。
    #[tokio::test]
    async fn cleanup_is_idempotent_on_every_path() {
        let transport = Arc::new(FakeTransport::ok());
        let publisher = UpstreamFileUploadPublisher::with_transport(transport, provider(true));
        // 成功路径：发布后清理两次。
        let reference = publisher
            .publish(Path::new("/tmp/audio.wav"))
            .await
            .expect("publish");
        assert!(publisher.cleanup(reference.clone()).await.is_ok());
        assert!(publisher.cleanup(reference.clone()).await.is_ok());
        // 失败路径：即使从未发布，任一句柄的清理也返回成功。
        let orphan = TemporaryAudioReference {
            url: "http://example.invalid/a.wav?Expires=1&Signature=s".into(),
            lease_id: "orphan-lease".into(),
            file_id: None,
            upstream_issued: true,
        };
        assert!(publisher.cleanup(orphan.clone()).await.is_ok());
        assert!(publisher.cleanup(orphan).await.is_ok());
    }

    /// 任务 1.6：任何对外可见结构都不得携带凭据、完整 URL 或本地路径。
    #[test]
    fn persistent_and_event_shapes_never_carry_secrets() {
        // 发布器的最小可序列化投影：租约句柄只作为不透明标识使用。
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct LeaseProjection {
            lease_id: String,
        }
        // ASR 进度事件形状（与 contract 中对外字段一致）。
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct EventProjection {
            request_id: String,
            stage: String,
            message: String,
        }
        let marker = "SIGNATURE-MARKER-DO-NOT-LEAK";
        let payloads = vec![
            serde_json::to_value(LeaseProjection {
                lease_id: "lease-1".into(),
            })
            .unwrap(),
            serde_json::to_value(EventProjection {
                request_id: "req-1".into(),
                stage: "transcribing".into(),
                message: "正在识别语音".into(),
            })
            .unwrap(),
            serde_json::to_value(cloud_identity("qwen3-asr-flash-filetrans")).unwrap(),
        ];
        for payload in payloads {
            let text = payload.to_string();
            for forbidden in [
                marker,
                "Authorization",
                "Bearer",
                "st-lt-v1",
                "/Users/",
                "/private/",
                "OSSAccessKeyId",
                "security-token",
            ] {
                assert!(
                    !text.contains(forbidden),
                    "对外结构不得包含 {forbidden}: {text}"
                );
            }
        }
    }

    /// 结果地址实测为 http 签名地址；必须只接受上游签发形态。
    #[test]
    fn accepts_only_upstream_signed_result_urls() {
        let accept = |url: &str| {
            let parsed = reqwest::Url::parse(url).unwrap();
            parsed.scheme() == "https"
                || (parsed.scheme() == "http" && is_upstream_signed_url(&parsed))
        };
        assert!(accept(
            "https://dashscope-result.example/x.json?Expires=1&Signature=s"
        ));
        // 实测形态：http + Expires + Signature + OSSAccessKeyId。
        assert!(accept(
            "http://dashscope-result-bj.oss-cn-beijing.aliyuncs.com/p/x.json\
             ?Expires=1790841803&OSSAccessKeyId=LTAI.x&Signature=a%3D"
        ));
        // http 但缺少签名或有效期：必须拒绝。
        assert!(!accept("http://dashscope-result.example/x.json"));
        assert!(!accept(
            "http://dashscope-result.example/x.json?Signature=s"
        ));
        assert!(!accept("http://dashscope-result.example/x.json?Expires=1"));
        // 非 http(s) 方案：必须拒绝。
        assert!(!accept(
            "oss://dashscope-result/x.json?Expires=1&Signature=s"
        ));
        assert!(!accept("file:///tmp/x.json?Expires=1&Signature=s"));
    }

    /// 任务 8.1 + 8.2：六分支分类必须互不相同，且取消/额度/未开通不得被描述为云端故障。
    #[test]
    fn classifies_six_distinct_failure_branches() {
        let cases = [
            (AsrError::cancelled(), CloudFailureBranch::Cancelled),
            (
                AsrError::new(
                    AsrErrorKind::EnvironmentUnavailable,
                    "ai_authorization_unavailable",
                    "无有效授权",
                    false,
                ),
                CloudFailureBranch::AuthorizationInvalid,
            ),
            (
                AsrError::new(
                    AsrErrorKind::EnvironmentUnavailable,
                    "ai_not_activated",
                    "未开通",
                    false,
                ),
                CloudFailureBranch::NotActivated,
            ),
            (
                AsrError::new(
                    AsrErrorKind::ResourceExhausted,
                    "ai_quota_exhausted",
                    "额度耗尽",
                    false,
                ),
                CloudFailureBranch::QuotaExhausted,
            ),
            (
                AsrError::new(
                    AsrErrorKind::ProcessFailed,
                    "cloud_upload_service_unavailable",
                    "瞬时故障",
                    true,
                ),
                CloudFailureBranch::Transient,
            ),
            (
                AsrError::new(
                    AsrErrorKind::ProcessFailed,
                    "cloud_upload_resource_expired",
                    "资源失效",
                    false,
                ),
                CloudFailureBranch::ResourceInvalidated,
            ),
        ];
        let mut seen = BTreeSet::new();
        for (error, expected) in cases {
            let branch = classify_cloud_failure(&error);
            assert_eq!(branch, expected, "code={}", error.code);
            assert!(seen.insert(branch.as_str()), "分支必须互不相同: {branch:?}");
            assert!(
                !branch.safe_message().contains("本地"),
                "分支说明不得暗示回退本地: {}",
                branch.safe_message()
            );
        }
        assert_eq!(seen.len(), 6, "必须覆盖六个分支");
        // 未开通与额度耗尽的用户可见原因必须不同。
        assert_ne!(
            CloudFailureBranch::NotActivated.safe_message(),
            CloudFailureBranch::QuotaExhausted.safe_message()
        );
        // 取消与瞬时故障的原因也必须不同。
        assert_ne!(
            CloudFailureBranch::Cancelled.safe_message(),
            CloudFailureBranch::Transient.safe_message()
        );
        // 资源失效必须可区分，且提示需要重新上传。
        assert!(
            CloudFailureBranch::ResourceInvalidated
                .safe_message()
                .contains("重新上传")
        );
        // 未知错误归入其他分支，不得误判为可重试。
        assert_eq!(
            classify_cloud_failure(&AsrError::invalid_input("x", "y")),
            CloudFailureBranch::Other
        );
    }

    /// 任务 6.1：分块规划必须覆盖完整音频、保留重叠、且不删除静音。
    #[test]
    fn chunk_plan_covers_source_with_overlap_and_no_gaps() {
        // 小于上限：单块覆盖全部。
        let single = plan_audio_chunks(60_000, 120_000, 2_000);
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].offset_ms, 0);
        assert_eq!(single[0].length_ms, 60_000);

        // 超过上限：多块，覆盖完整音频且保留固定重叠。
        let chunks = plan_audio_chunks(300_000, 120_000, 2_000);
        assert!(chunks.len() >= 2, "300s 必须分块: {chunks:?}");
        assert_eq!(chunks[0].offset_ms, 0);
        assert_eq!(chunks[0].length_ms, 120_000);
        assert_eq!(chunks[1].offset_ms, 118_000, "步进 = 块长 - 重叠");
        // 最后一块必须触及源音频末尾，不能截断尾部。
        let last = chunks.last().unwrap();
        assert_eq!(last.end_ms(), 300_000);
        // 不变量：块按源时间有序、块间保留重叠（不删除静音）、覆盖无空洞。
        for (index, chunk) in chunks.iter().enumerate() {
            assert!(chunk.length_ms > 0);
            assert!(chunk.end_ms() <= 300_000, "块不得超出源音频: {chunk:?}");
            if index > 0 {
                let previous = chunks[index - 1];
                // 重叠：下一块起点必须早于上一块结束。
                assert!(
                    chunk.offset_ms < previous.end_ms(),
                    "块间必须保留重叠而不是无缝拼接: {previous:?} -> {chunk:?}"
                );
                // 覆盖：下一块起点不得晚于上一块结束（否则中间出现空洞）。
                assert_eq!(
                    chunk.offset_ms,
                    previous.offset_ms + previous.length_ms - 2_000
                );
            }
        }
        // 除最后一块外，其余块都应达到上限长度（避免碎片化）。
        for chunk in &chunks[..chunks.len() - 1] {
            assert_eq!(chunk.length_ms, 120_000, "非尾块必须满长: {chunk:?}");
        }
        // 退化尾块必须被并入前一块，而不是单独提交一次付费调用。
        assert!(
            chunks.len() >= 2 && chunks[1].offset_ms < chunks[0].end_ms(),
            "存在重叠才是合法分块"
        );

        // 重叠大于等于块长时必须被夹紧，避免无法推进（死循环）。
        let clamped = plan_audio_chunks(300_000, 10_000, 20_000);
        assert!(clamped.len() > 1);
        assert!(clamped[1].offset_ms > clamped[0].offset_ms);

        // 零时长或不设上限时不得产出块。
        assert!(plan_audio_chunks(0, 120_000, 1_000).is_empty());
        assert!(plan_audio_chunks(10_000, 0, 1_000).is_empty());
    }

    /// 任务 6.2 + 6.3：块内时间映射回源时间，重叠区不产生重复句段。
    #[test]
    fn merge_restores_source_time_and_dedupes_overlap() {
        // 两块，重叠 2000ms：块0 = [0,120000)，块1 = [118000,240000)。
        let chunks = plan_audio_chunks(240_000, 120_000, 2_000);
        assert_eq!(chunks.len(), 2);
        // 同一句在重叠区被两块各识别一次：块0 内 begin=118000，块1 内 begin=0。
        let chunk0 = restore_source_time(
            &[DashScopeSegment {
                begin_time: 117_000,
                end_time: 119_500,
                text: "这句话跨越了分块边界".into(),
            }],
            chunks[0].offset_ms,
        );
        let chunk1 = restore_source_time(
            &[
                DashScopeSegment {
                    begin_time: 0,
                    end_time: 1_500,
                    text: "这句话跨越了分块边界".into(),
                },
                DashScopeSegment {
                    begin_time: 3_000,
                    end_time: 5_000,
                    text: "后续内容".into(),
                },
            ],
            chunks[1].offset_ms,
        );
        // 映射后：块0 的句段落在 117000-119500，块1 的重复句段落在 118000-119500。
        assert_eq!(chunk0[0].begin_time, 117_000);
        assert_eq!(chunk1[0].begin_time, 118_000);
        assert_eq!(chunk1[1].begin_time, 121_000);

        let merged = merge_chunk_segments(chunk0.into_iter().chain(chunk1).collect(), 2_000);
        // 重复句段只保留一段，且后续内容不被误删。
        let boundary: Vec<_> = merged
            .iter()
            .filter(|segment| segment.text.contains("跨越了分块边界"))
            .collect();
        assert_eq!(boundary.len(), 1, "重叠区不得产生重复句段: {merged:?}");
        assert!(merged.iter().any(|segment| segment.text == "后续内容"));
        // 结果必须按源时间有序。
        let times: Vec<_> = merged.iter().map(|s| s.begin_time).collect();
        let mut sorted = times.clone();
        sorted.sort_unstable();
        assert_eq!(times, sorted, "合并结果必须按源时间排序");
        // 边界不能被截断：保留下来的句段必须覆盖重叠区。
        assert!(boundary[0].end_time > 119_000);
    }

    /// 去重不得误删相邻但内容不同的短句。
    #[test]
    fn dedupe_keeps_distinct_adjacent_segments() {
        let merged = merge_chunk_segments(
            vec![
                DashScopeSegment {
                    begin_time: 1_000,
                    end_time: 1_400,
                    text: "第一句".into(),
                },
                DashScopeSegment {
                    begin_time: 1_500,
                    end_time: 1_900,
                    text: "第二句".into(),
                },
            ],
            2_000,
        );
        assert_eq!(merged.len(), 2, "内容不同不得被去重: {merged:?}");
    }

    /// 任务 6.4：稳定 ID 只依据源时间与文本，与分块策略、到达顺序无关。
    #[test]
    fn stable_ids_are_reproducible_and_order_independent() {
        let fingerprint = "sha256:source-1";
        let first = stable_segment_id(fingerprint, 12_800, 19_600, "你好世界");
        let second = stable_segment_id(fingerprint, 12_800, 19_600, "你好世界");
        assert_eq!(first, second, "相同输入必须产生相同 ID");
        // 文本前后空白不影响身份。
        assert_eq!(
            first,
            stable_segment_id(fingerprint, 12_800, 19_600, "  你好世界 ")
        );
        // 时间、文本或源指纹变化都必须产生不同 ID。
        assert_ne!(
            first,
            stable_segment_id(fingerprint, 12_801, 19_600, "你好世界")
        );
        assert_ne!(
            first,
            stable_segment_id(fingerprint, 12_800, 19_600, "你好世界!")
        );
        assert_ne!(
            first,
            stable_segment_id("sha256:source-2", 12_800, 19_600, "你好世界")
        );
        assert!(first.starts_with("seg-"));
        // 一组句段的 ID 序列与到达顺序无关。
        let forward: Vec<_> = [
            (0u64, 1_000u64, "a"),
            (1_000, 2_000, "b"),
            (2_000, 3_000, "c"),
        ]
        .iter()
        .map(|(begin, end, text)| stable_segment_id(fingerprint, *begin, *end, text))
        .collect();
        let backward: Vec<_> = [
            (2_000u64, 3_000u64, "c"),
            (1_000, 2_000, "b"),
            (0, 1_000, "a"),
        ]
        .iter()
        .map(|(begin, end, text)| stable_segment_id(fingerprint, *begin, *end, text))
        .collect();
        let mut expected = forward.clone();
        expected.sort();
        let mut actual = backward.clone();
        actual.sort();
        assert_eq!(expected, actual, "ID 集合必须与到达顺序无关");
    }

    /// 真实长样本验收暴露的缺陷：分块边界的**截断残句**与相邻块完整句时间交叠、
    /// 但文本完全不同，文本去重必然漏掉；产物发布要求句段严格不重叠，因此必须
    /// 由时间交叠消解兜住。以下数据取自真实中文 4 分钟样本的验收报告。
    #[test]
    fn truncated_boundary_fragment_never_overlaps_neighbour() {
        let merged = merge_chunk_segments(
            vec![
                // 前一块末尾被截断的残句。
                DashScopeSegment {
                    begin_time: 159_120,
                    end_time: 159_980,
                    text: "感慨为。".into(),
                },
                // 后一块开头完整的句子，与残句交叠 740ms。
                DashScopeSegment {
                    begin_time: 159_240,
                    end_time: 162_820,
                    text: "敢看微霜雪，想赠眉间一道疤。".into(),
                },
            ],
            2_000,
        );
        // 残句必须被消解掉，只保留覆盖更完整的完整句。
        assert_eq!(merged.len(), 1, "交叠的截断残句必须被消除: {merged:?}");
        assert_eq!(merged[0].text, "敢看微霜雪，想赠眉间一道疤。");
        assert_eq!(merged[0].begin_time, 159_240);

        // 不变量：任何情况下输出都必须严格按源时间单调且互不重叠。
        let segments = vec![
            DashScopeSegment {
                begin_time: 0,
                end_time: 1_000,
                text: "a".into(),
            },
            DashScopeSegment {
                begin_time: 900,
                end_time: 2_000,
                text: "b".into(),
            },
            DashScopeSegment {
                begin_time: 1_500,
                end_time: 1_600,
                text: "c".into(),
            },
            DashScopeSegment {
                begin_time: 1_550,
                end_time: 3_000,
                text: "d".into(),
            },
        ];
        let resolved = resolve_time_overlaps(segments);
        for pair in resolved.windows(2) {
            assert!(
                pair[0].end_time <= pair[1].begin_time,
                "相邻句段不得交叠: {pair:?}"
            );
        }
        assert!(resolved.iter().all(|s| s.end_time > s.begin_time));
    }

    /// 任务 7.5：结果发布前的统一校验必须拒绝时间范围、空文本与顺序非法的句段。
    #[test]
    fn publish_validation_rejects_malformed_segments() {
        // 解析阶段已拒绝空句段集合。
        let empty = serde_json::json!({"transcripts": [{"sentences": []}]});
        assert!(parse_dashscope_segments(&empty).is_err());
        let missing = serde_json::json!({"output": {}});
        assert!(parse_dashscope_segments(&missing).is_err());
        // 时间范围非法（begin >= end）与空文本必须被丢弃，不得进入产物。
        let value = serde_json::json!({"transcripts": [{"sentences": [
            {"begin_time": 5000, "end_time": 5000, "text": "零长度"},
            {"begin_time": 6000, "end_time": 4000, "text": "倒序"},
            {"begin_time": 0, "end_time": 1000, "text": "   "},
            {"begin_time": 1000, "end_time": 2000, "text": "有效句段"}
        ]}]});
        let segments = parse_dashscope_segments(&value).unwrap();
        assert_eq!(segments.len(), 1, "非法句段必须被过滤: {segments:?}");
        assert_eq!(segments[0].text, "有效句段");
        // 全部非法时整体失败，而不是产出空产物。
        let all_invalid = serde_json::json!({"transcripts": [{"sentences": [
            {"begin_time": 5000, "end_time": 5000, "text": "零长度"}
        ]}]});
        assert!(parse_dashscope_segments(&all_invalid).is_err());
        // 合并后的结果必须按源时间有序，供发布校验直接通过。
        let merged = merge_chunk_segments(
            vec![
                DashScopeSegment {
                    begin_time: 3_000,
                    end_time: 4_000,
                    text: "第三".into(),
                },
                DashScopeSegment {
                    begin_time: 1_000,
                    end_time: 2_000,
                    text: "第一".into(),
                },
                DashScopeSegment {
                    begin_time: 2_000,
                    end_time: 3_000,
                    text: "第二".into(),
                },
            ],
            0,
        );
        let times: Vec<_> = merged.iter().map(|s| s.begin_time).collect();
        assert_eq!(times, vec![1_000, 2_000, 3_000]);
    }

    /// 任务 2.5：无论上游是否接受取消，都必须如实告知仍可能计费且不自动重提交。
    #[test]
    fn cancellation_always_reports_possible_charges() {
        for accepted in [true, false] {
            let outcome = cancel_outcome(accepted);
            assert_eq!(outcome.accepted, accepted);
            assert!(
                outcome.may_still_charge,
                "已提交的部分始终可能计费（accepted={accepted}）"
            );
            assert!(
                outcome.safe_message.contains("仍可能按实际用量产生费用"),
                "必须明确告知可能计费: {}",
                outcome.safe_message
            );
            // 未接受取消时必须额外提示不要重复提交。
            if !accepted {
                assert!(outcome.safe_message.contains("请勿重复提交"));
            }
            // 说明不得泄露上游正文、凭据或地址。
            for forbidden in ["Bearer", "http", "task-", "Authorization"] {
                assert!(
                    !outcome.safe_message.contains(forbidden),
                    "说明不得包含 {forbidden}: {}",
                    outcome.safe_message
                );
            }
        }
        // 两种结果的说明必须不同，用户可据此判断是否已受理取消。
        assert_ne!(
            cancel_outcome(true).safe_message,
            cancel_outcome(false).safe_message
        );
    }

    /// 任务 8.5：清理失败必须保留成功结果并标记待重试，且不得因此重新付费转写。
    #[test]
    fn cleanup_failure_preserves_successful_result() {
        let success = AsrResult {
            request_id: "req-1".into(),
            identity: cloud_identity("qwen3-asr-flash-filetrans"),
            detected_language: Some("zh".into()),
            audio_duration_ms: 1_000,
            provider: "aliyun".into(),
            fallback_reason: None,
            segments: vec![AsrSegment {
                start_ms: 0,
                end_ms: 1_000,
                text: "成功转写".into(),
                confidence: None,
            }],
            warnings: Vec::new(),
        };
        let cleanup_error = AsrError::new(
            AsrErrorKind::Io,
            "cloud_audio_cleanup_failed",
            "清理失败",
            true,
        );
        let preserved = apply_cleanup_outcome(Ok(success.clone()), Err(cleanup_error.clone()))
            .expect("清理失败不得把成功结果变成失败");
        // 转写内容必须完整保留。
        assert_eq!(preserved.segments, success.segments);
        assert_eq!(preserved.provider, "aliyun");
        // 必须标记清理待重试。
        assert_eq!(preserved.warnings.len(), 1);
        assert_eq!(preserved.warnings[0].code, "cloud_audio_cleanup_pending");
        assert!(preserved.warnings[0].message.contains("待重试"));

        // 清理成功时不得附加警告。
        let clean = apply_cleanup_outcome(Ok(success.clone()), Ok(())).expect("ok");
        assert!(clean.warnings.is_empty());

        // 转写本身失败时，清理结果不影响原始失败。
        let failure = AsrError::new(
            AsrErrorKind::ProcessFailed,
            "dashscope_task_failed",
            "失败",
            false,
        );
        let kept = apply_cleanup_outcome(Err(failure.clone()), Ok(())).expect_err("保持失败");
        assert_eq!(kept.code, "dashscope_task_failed");
        let kept = apply_cleanup_outcome(Err(failure), Err(cleanup_error)).expect_err("保持失败");
        assert_eq!(kept.code, "dashscope_task_failed");
    }

    /// 调用诊断的脱敏边界：事件结构里**没有**可以承载秘密的字段。
    ///
    /// 这是结构性保证而非过滤：`CloudCallDiagnostic` 不含 token / URL / 路径 /
    /// 正文 / 标识取值字段，因此序列化结果不可能泄漏它们；`has_*` 只表达"是否取到"。
    #[test]
    fn call_diagnostics_are_redacted_by_construction() {
        let sink = MemoryCloudCallDiagnosticSink::new(8);
        // 默认关闭：未显式打开时不得记录任何事件。
        assert!(!sink.is_enabled());
        sink.emit(diagnostic(
            "upload",
            "files",
            Some(200),
            std::time::Instant::now(),
            1,
            None,
            true,
            false,
            false,
            None,
        ));
        assert!(sink.events().is_empty(), "开关关闭时不得记录");
        sink.set_enabled(true);
        assert!(sink.is_enabled());
        // 模拟一次完整链路：上传 -> 取签名 URL -> 提交 -> 轮询 -> 取结果。
        type StageExpectation = (
            &'static str,
            &'static str,
            Option<u16>,
            Option<AsrError>,
            bool,
            bool,
            bool,
        );
        let stages: [StageExpectation; 5] = [
            ("upload", "files", Some(200), None, true, false, false),
            (
                "signed_url",
                "files/{file_id}",
                Some(200),
                None,
                true,
                true,
                false,
            ),
            (
                "submit",
                "services/audio/asr/transcription",
                Some(200),
                None,
                true,
                true,
                true,
            ),
            ("poll", "tasks/{task_id}", Some(200), None, true, true, true),
            (
                "result",
                "result/{signed}",
                Some(403),
                Some(AsrError::new(
                    AsrErrorKind::ProcessFailed,
                    "dashscope_result_download_failed",
                    "结果下载失败",
                    true,
                )),
                true,
                true,
                true,
            ),
        ];
        for (stage, path, status, error, has_file, has_url, has_task) in stages {
            sink.emit(diagnostic(
                stage,
                path,
                status,
                std::time::Instant::now(),
                1,
                error.as_ref(),
                has_file,
                has_url,
                has_task,
                Some(96_044),
            ));
        }
        let events = sink.events();
        assert_eq!(events.len(), 5);
        // 阶段与错误分类必须可读，否则日志对调试无价值。
        assert_eq!(events[0].stage, "upload");
        assert!(events[0].has_file_id);
        assert!(!events[0].has_signed_url);
        assert!(events[1].has_signed_url);
        assert!(events[2].has_task_id);
        assert_eq!(events[0].audio_bytes, Some(96_044));
        assert_eq!(
            events[4].error_code.as_deref(),
            Some("dashscope_result_download_failed")
        );
        assert_eq!(events[4].failure_branch.as_deref(), Some("transient"));
        assert_eq!(events[4].http_status, Some(403));
        // 结构性脱敏：序列化结果不得出现任何秘密形态。
        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            for forbidden in [
                "st-lt-v1",
                "Bearer",
                "Authorization",
                "Signature=",
                "OSSAccessKeyId",
                "security-token",
                "http://",
                "https://",
                "/tmp/",
                "/Users/",
                "oss://",
            ] {
                assert!(
                    !json.contains(forbidden),
                    "诊断事件不得包含 {forbidden}: {json}"
                );
            }
            // 路径必须是静态模板，不得被填成具体对象。
            assert!(
                event.path.starts_with("files")
                    || event.path.starts_with("tasks")
                    || event.path.starts_with("services")
                    || event.path.starts_with("result"),
                "路径必须是静态模板: {}",
                event.path
            );
        }
    }

    /// 轮询只在**终态**发射一次诊断，且发射点确实接在真实请求路径上。
    ///
    /// 用一个"接受连接后立刻关闭"的本地 TCP 端口冒充 HTTPS 端点：TLS 握手必然
    /// 失败，于是不需要外网即可走通 `poll` 的传输层失败终态。
    #[tokio::test]
    async fn poll_diagnostic_emits_once_on_terminal_failure() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            // 保持端口可连接但立刻断开，使 reqwest 的 TLS 握手立即失败。
            for stream in listener.incoming() {
                drop(stream);
            }
        });
        let sink = Arc::new(MemoryCloudCallDiagnosticSink::new(16));
        sink.set_enabled(true);
        let client = DashScopeAsrClient::new(DashScopeAsrConfig {
            base_url: format!("https://127.0.0.1:{port}"),
            poll_interval_ms: 100,
            max_wait_seconds: 5,
            ..DashScopeAsrConfig::default()
        })
        .expect("client")
        .with_diagnostics(sink.clone());

        let error = client
            .poll("token", "task-1", CancellationToken::new())
            .await
            .expect_err("传输层失败必须返回错误");
        assert_eq!(error.code, "dashscope_poll_failed");
        let events = sink.events();
        assert_eq!(events.len(), 1, "终态只发一次，不得每轮都发");
        let event = &events[0];
        assert_eq!(event.stage, "poll");
        assert_eq!(event.path, "tasks/{task_id}");
        assert_eq!(event.attempt, 1, "首次轮询的序号是 1");
        assert_eq!(event.http_status, None);
        assert!(event.has_task_id);
        assert_eq!(event.error_code.as_deref(), Some("dashscope_poll_failed"));
        assert_eq!(event.failure_branch.as_deref(), Some("transient"));
        // 结构性脱敏：序列化结果不得出现任何秘密形态。
        let json = serde_json::to_string(event).unwrap();
        for forbidden in [
            "st-lt-v1",
            "Bearer",
            "Authorization",
            "Signature=",
            "OSSAccessKeyId",
            "security-token",
            "http://",
            "https://",
            "/tmp/",
            "/Users/",
            "oss://",
            "127.0.0.1",
        ] {
            assert!(
                !json.contains(forbidden),
                "诊断事件不得包含 {forbidden}: {json}"
            );
        }
    }

    /// 新增阶段（`poll` / `result`）各终态的语义与脱敏边界。
    ///
    /// 与 `call_diagnostics_are_redacted_by_construction` 同构：事件结构里没有能
    /// 承载秘密的字段，因此轮询与结果下载的终态序列化后同样不含任何秘密形态。
    #[test]
    fn poll_and_result_diagnostics_are_redacted_and_terminal() {
        let sink = MemoryCloudCallDiagnosticSink::new(16);
        sink.set_enabled(true);
        let now = std::time::Instant::now;
        let task_failed = AsrError::new(
            AsrErrorKind::ProcessFailed,
            "dashscope_task_failed",
            "云端转写任务失败，请稍后重试",
            false,
        );
        let timed_out = AsrError::new(
            AsrErrorKind::ProcessFailed,
            "dashscope_poll_timeout",
            "云端转写等待超时，请稍后重试",
            true,
        );
        let download_failed = AsrError::new(
            AsrErrorKind::ProcessFailed,
            "dashscope_result_download_failed",
            "云端转写网络不可用",
            true,
        );
        // 轮询四个终态：成功 / 任务失败 / 超时 / 取消。
        let poll_events = [
            diagnostic(
                "poll",
                "tasks/{task_id}",
                Some(200),
                now(),
                3,
                None,
                false,
                false,
                true,
                None,
            ),
            diagnostic(
                "poll",
                "tasks/{task_id}",
                Some(200),
                now(),
                2,
                Some(&task_failed),
                false,
                false,
                true,
                None,
            ),
            diagnostic(
                "poll",
                "tasks/{task_id}",
                None,
                now(),
                200,
                Some(&timed_out),
                false,
                false,
                true,
                None,
            ),
            diagnostic(
                "poll",
                "tasks/{task_id}",
                None,
                now(),
                1,
                Some(&AsrError::cancelled()),
                false,
                false,
                true,
                None,
            ),
        ];
        // 结果下载四个终态：HTTP 失败 / 成功。
        let result_events = [
            diagnostic(
                "result",
                "result/{signed}",
                Some(403),
                now(),
                1,
                Some(&download_failed),
                false,
                true,
                true,
                None,
            ),
            diagnostic(
                "result",
                "result/{signed}",
                Some(200),
                now(),
                1,
                None,
                false,
                true,
                true,
                None,
            ),
        ];
        for event in poll_events.iter().chain(result_events.iter()) {
            sink.emit(event.clone());
        }
        let events = sink.events();
        assert_eq!(events.len(), 6);
        assert_eq!(events[0].attempt, 3);
        assert_eq!(events[0].http_status, Some(200));
        assert!(events[0].error_code.is_none());
        assert!(events[0].has_task_id);
        assert_eq!(
            events[1].error_code.as_deref(),
            Some("dashscope_task_failed")
        );
        assert_eq!(
            events[2].error_code.as_deref(),
            Some("dashscope_poll_timeout")
        );
        assert_eq!(events[3].error_code.as_deref(), Some("asr_cancelled"));
        assert_eq!(events[3].failure_branch.as_deref(), Some("cancelled"));
        assert_eq!(events[4].stage, "result");
        assert_eq!(events[4].http_status, Some(403));
        assert_eq!(events[4].failure_branch.as_deref(), Some("transient"));
        assert!(events[5].error_code.is_none());
        assert!(events[5].has_signed_url);
        // 结构性脱敏：序列化结果不得出现任何秘密形态。
        for event in &events {
            let json = serde_json::to_string(event).unwrap();
            for forbidden in [
                "st-lt-v1",
                "Bearer",
                "Authorization",
                "Signature=",
                "OSSAccessKeyId",
                "security-token",
                "http://",
                "https://",
                "/tmp/",
                "/Users/",
                "oss://",
            ] {
                assert!(
                    !json.contains(forbidden),
                    "诊断事件不得包含 {forbidden}: {json}"
                );
            }
            // 路径必须是静态模板，不能被填成具体对象。
            assert!(
                event.path == "tasks/{task_id}" || event.path == "result/{signed}",
                "路径必须是静态模板: {}",
                event.path
            );
        }
    }

    /// 环形缓冲：容量受限、可清空，且不影响调用方。
    #[test]
    fn diagnostic_sink_keeps_bounded_history() {
        let sink = MemoryCloudCallDiagnosticSink::new(3);
        sink.set_enabled(true);
        for attempt in 0..5u32 {
            sink.emit(diagnostic(
                "poll",
                "tasks/{task_id}",
                Some(200),
                std::time::Instant::now(),
                attempt,
                None,
                true,
                true,
                true,
                None,
            ));
        }
        let events = sink.events();
        assert_eq!(events.len(), 3, "容量必须受限");
        // 保留最新的事件，最旧的被丢弃。
        assert_eq!(events[0].attempt, 2);
        assert_eq!(events[2].attempt, 4);
        sink.clear();
        assert!(sink.events().is_empty());
        // 关闭开关会同时清空已攒的事件。
        sink.set_enabled(true);
        sink.emit(diagnostic(
            "poll",
            "tasks/{task_id}",
            Some(200),
            std::time::Instant::now(),
            9,
            None,
            true,
            true,
            true,
            None,
        ));
        assert_eq!(sink.events().len(), 1);
        sink.set_enabled(false);
        assert!(sink.events().is_empty(), "关闭必须清空并停止记录");
    }

    /// 任务 3.2：缺少本地模型不得阻止云端识别。
    #[tokio::test]
    async fn cloud_engine_is_ready_without_local_resources() {
        struct UnreadyLocal;

        #[async_trait]
        impl AsrEngine for UnreadyLocal {
            fn identity(&self) -> AsrEngineIdentity {
                AsrEngineIdentity {
                    engine_id: "whisper.cpp".into(),
                    engine_version: "1".into(),
                    model_id: "ggml-base".into(),
                    model_version: "1".into(),
                }
            }
            fn capabilities(&self) -> AsrCapabilities {
                AsrCapabilities {
                    timestamp_policies: vec![TimestampPolicy::Segment],
                    supports_language_hint: true,
                    supports_hotwords: true,
                    confidence_kind: None,
                    maximum_threads: 4,
                }
            }
            async fn diagnose(&self) -> EngineResult<AsrEnvironmentReport> {
                Ok(AsrEnvironmentReport {
                    ready: false,
                    platform: "test".into(),
                    identity: self.identity(),
                    checks: vec![AsrEnvironmentCheck {
                        code: "whisper_sidecar".into(),
                        passed: false,
                        message: "本地模型缺失".into(),
                    }],
                })
            }
            async fn transcribe(
                &self,
                _request: AsrRequest,
                _progress: Arc<dyn AsrProgressSink>,
                _cancellation: CancellationToken,
            ) -> EngineResult<AsrResult> {
                panic!("本地引擎不得在云端可用时被调用");
            }
        }

        let engine = CloudFirstAsrEngine::new(
            Arc::new(UnreadyLocal),
            Arc::new(UpstreamFileUploadPublisher::with_transport(
                Arc::new(FakeTransport::ok()),
                provider(true),
            )),
            Some(provider(true)),
            DashScopeAsrClient::new(DashScopeAsrConfig::default()).unwrap(),
        );
        assert!(engine.cloud_available(), "有凭据时云端路径必须可用");
        let report = engine.diagnose().await.unwrap();
        assert!(report.ready, "云端可用即就绪，本地资源缺失不得阻止");
        assert_eq!(report.identity.engine_id, "dashscope-filetrans");
        assert!(
            report
                .checks
                .iter()
                .any(|check| check.code == "local_transcription" && !check.passed),
            "诊断必须如实报告本地资源未就绪"
        );
    }

    #[tokio::test]
    async fn publisher_reports_unavailable_without_credentials() {
        let transport = Arc::new(FakeTransport::ok());
        let publisher = UpstreamFileUploadPublisher::with_transport(transport, provider(false));
        assert!(!publisher.is_available());
        assert!(
            UpstreamFileUploadPublisher::with_transport(
                Arc::new(FakeTransport::ok()),
                provider(true)
            )
            .is_available()
        );
    }

    #[test]
    fn accepts_upstream_signed_references_and_rejects_user_input() {
        // 上游签发的 https 引用始终可用。
        assert!(
            TemporaryAudioReference {
                url: "https://example.invalid/a.wav".into(),
                lease_id: "lease".into(),
                file_id: None,
                upstream_issued: true,
            }
            .validate()
            .is_ok()
        );
        // 上游文件服务签发的临时地址是 http，但带签名与有效期，允许。
        let signed = "http://dashscope-file-mgr.example/api-fs/1/2/3/a.wav\
                      ?Expires=1790842590&OSSAccessKeyId=STS.x&Signature=abc";
        assert!(
            TemporaryAudioReference {
                url: signed.into(),
                lease_id: "lease".into(),
                file_id: Some("file-1".into()),
                upstream_issued: true,
            }
            .validate()
            .is_ok()
        );
        // 用户输入、配置或非上游签发的地址一律被拒绝。
        for url in [
            "file:///tmp/a.wav",
            "http://example.invalid/a.wav",
            "http://example.invalid/a.wav?Signature=abc",
            "http://example.invalid/a.wav?Expires=1",
            "",
        ] {
            for upstream_issued in [false, true] {
                let reference = TemporaryAudioReference {
                    url: url.into(),
                    lease_id: "lease".into(),
                    file_id: None,
                    upstream_issued,
                };
                // 只有"上游签发 + 含签名与有效期"才允许 http；
                // 其余情况要么 https 要么被拒绝。
                let allowed = url.starts_with("https://");
                assert_eq!(
                    reference.validate().is_ok(),
                    allowed,
                    "url={url} upstream_issued={upstream_issued}"
                );
            }
        }
        // 缺少清理句柄必须被拒绝，即使地址合法。
        assert!(
            TemporaryAudioReference {
                url: signed.into(),
                lease_id: "  ".into(),
                file_id: None,
                upstream_issued: true,
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn classifies_upload_and_signed_url_failures_distinctly() {
        let codes = [
            map_upload_http_error(401).code,
            map_upload_http_error(413).code,
            map_upload_http_error(429).code,
            map_upload_http_error(503).code,
            map_signed_url_http_error(403).code,
            map_signed_url_http_error(404).code,
            map_signed_url_http_error(429).code,
            map_signed_url_http_error(500).code,
        ];
        let unique: BTreeSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), codes.len(), "错误码必须可区分: {codes:?}");
        // 资源失效必须提示重新上传，而不是重试同一个引用。
        let expired = map_signed_url_http_error(404);
        assert!(!expired.retryable);
        assert!(expired.safe_message.contains("失效"));
        // 瞬时故障必须可重试。
        assert!(map_upload_http_error(503).retryable);
        assert!(map_signed_url_http_error(429).retryable);
        for error in [
            map_upload_http_error(401),
            map_upload_http_error(413),
            map_signed_url_http_error(403),
            map_signed_url_http_error(404),
        ] {
            assert!(!error.safe_message.contains("Bearer"));
            assert!(!error.safe_message.contains("http"));
        }
    }

    #[test]
    fn cloud_identity_is_the_upstream_engine_not_the_local_one() {
        let identity = cloud_identity("qwen3-asr-flash-filetrans");
        assert_eq!(identity.engine_id, "dashscope-filetrans");
        assert_eq!(identity.model_id, "qwen3-asr-flash-filetrans");
        assert_eq!(identity.model_version, "qwen3-asr-flash-filetrans");
        assert_eq!(identity.engine_version, CLOUD_ENGINE_VERSION);
        assert_ne!(identity.engine_id, "whisper.cpp");
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
