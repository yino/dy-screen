//! 授权运营服务端 API 的唯一客户端入口。
//!
//! 这里刻意不把 `reqwest` 暴露给业务模块：服务端使用 HTTP 200 携带业务码，
//! 如果在各 command 中自行解析，很容易出现错误码、超时和敏感信息处理不一致。
//! 当前请求体保持 JSON 明文，但 `RequestCodec` 为后续信封加密保留了替换点。

use std::env;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const DEFAULT_API_BASE_URL: &str = "http://localhost/api/";
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct ApiConfig {
    pub base_url: String,
    pub timeout: Duration,
    pub client_version: String,
    pub platform: String,
}

impl ApiConfig {
    pub fn from_env() -> Self {
        let base_url = env::var("DY_SCREEN_API_BASE_URL")
            .ok()
            .or_else(|| option_env!("DY_SCREEN_API_BASE_URL").map(str::to_owned))
            .unwrap_or_else(|| DEFAULT_API_BASE_URL.to_owned());
        Self {
            base_url: normalize_base_url(&base_url),
            timeout: DEFAULT_REQUEST_TIMEOUT,
            client_version: env!("CARGO_PKG_VERSION").to_owned(),
            platform: client_platform().to_owned(),
        }
    }
}

fn normalize_base_url(value: &str) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        DEFAULT_API_BASE_URL.trim_end_matches('/').to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn client_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "win"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "general"
    }
}

#[derive(Debug, Error, Clone)]
pub enum ApiError {
    #[error("服务端暂时不可用，请检查网络后重试")]
    Transport,
    #[error("服务端响应格式无效")]
    InvalidResponse,
    #[error("服务端请求超时")]
    Timeout,
    #[error("服务端业务错误（{code}）：{message}")]
    Business { code: i32, message: String },
    #[error("服务端配置无效")]
    Configuration,
    #[error("{source}")]
    Context {
        #[source]
        source: Box<ApiError>,
        request_id: Option<String>,
        trace_id: Option<String>,
    },
}

impl ApiError {
    pub fn code(&self) -> Option<i32> {
        match self {
            Self::Business { code, .. } => Some(*code),
            Self::Context { source, .. } => source.code(),
            _ => None,
        }
    }

    pub fn category(&self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::InvalidResponse => "invalid_response",
            Self::Timeout => "timeout",
            Self::Configuration => "configuration",
            Self::Business { code, .. } => business_error_category(*code),
            Self::Context { source, .. } => source.category(),
        }
    }

    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Context { request_id, .. } => request_id.as_deref(),
            _ => None,
        }
    }

    pub fn trace_id(&self) -> Option<&str> {
        match self {
            Self::Context { trace_id, .. } => trace_id.as_deref(),
            _ => None,
        }
    }

    fn with_context(self, metadata: &ResponseMetadata) -> Self {
        if metadata.request_id.is_none() && metadata.trace_id.is_none() {
            self
        } else {
            Self::Context {
                source: Box::new(self),
                request_id: metadata.request_id.clone(),
                trace_id: metadata.trace_id.clone(),
            }
        }
    }

    pub fn safe_message(&self) -> String {
        self.to_string()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ApiDiagnosticEvent {
    pub component: &'static str,
    pub event: &'static str,
    pub operation: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    pub duration_ms: Option<u64>,
    pub http_status: Option<u16>,
    pub business_code: Option<i32>,
    pub error_category: Option<&'static str>,
    pub state: Option<String>,
    pub reason: Option<String>,
    pub revoked: Option<bool>,
    pub request_id: Option<String>,
    pub trace_id: Option<String>,
    pub device_id_hint: Option<String>,
    pub has_device_id: bool,
    pub has_activate_code: bool,
    pub request_fields: Vec<&'static str>,
}

pub trait ApiDiagnosticSink: Send + Sync {
    fn emit(&self, event: ApiDiagnosticEvent);
}

#[derive(Debug, Default)]
struct StderrApiDiagnosticSink;

impl ApiDiagnosticSink for StderrApiDiagnosticSink {
    fn emit(&self, event: ApiDiagnosticEvent) {
        if let Ok(line) = serde_json::to_string(&event) {
            eprintln!("{line}");
        }
    }
}

#[derive(Clone, Default)]
pub struct MemoryApiDiagnosticSink {
    events: Arc<Mutex<Vec<ApiDiagnosticEvent>>>,
}

impl MemoryApiDiagnosticSink {
    pub fn events(&self) -> Vec<ApiDiagnosticEvent> {
        self.events
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default()
    }
}

impl ApiDiagnosticSink for MemoryApiDiagnosticSink {
    fn emit(&self, event: ApiDiagnosticEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }
}

#[derive(Clone, Default)]
struct ResponseMetadata {
    http_status: Option<u16>,
    request_id: Option<String>,
    trace_id: Option<String>,
}

#[derive(Clone)]
struct RequestDiagnostic {
    operation: &'static str,
    method: &'static str,
    path: &'static str,
    trace_success: bool,
    device_id_hint: Option<String>,
    has_device_id: bool,
    has_activate_code: bool,
    request_fields: Vec<&'static str>,
}

impl RequestDiagnostic {
    fn public(operation: &'static str, method: &'static str, path: &'static str) -> Self {
        Self {
            operation,
            method,
            path,
            trace_success: false,
            device_id_hint: None,
            has_device_id: false,
            has_activate_code: false,
            request_fields: Vec::new(),
        }
    }

    fn authorized(
        operation: &'static str,
        method: &'static str,
        path: &'static str,
        device_id: &str,
        activate_code: &str,
    ) -> Self {
        Self {
            operation,
            method,
            path,
            trace_success: matches!(operation, "activate" | "heartbeat"),
            device_id_hint: Some(device_hint(device_id)),
            has_device_id: !device_id.is_empty(),
            has_activate_code: !activate_code.is_empty(),
            request_fields: Vec::new(),
        }
    }
}

/// 请求/响应编解码边界。当前 `PlainJsonCodec` 原样传递 JSON；后续可替换为
/// 信封加密实现而无需改动 endpoint 或业务调用方。
pub trait RequestCodec: Send + Sync {
    fn encode_request(&self, payload: Value) -> Result<Value, ApiError>;
    fn decode_response(&self, payload: Value) -> Result<Value, ApiError>;
}

#[derive(Debug, Default)]
pub struct PlainJsonCodec;

impl RequestCodec for PlainJsonCodec {
    fn encode_request(&self, payload: Value) -> Result<Value, ApiError> {
        Ok(payload)
    }

    fn decode_response(&self, payload: Value) -> Result<Value, ApiError> {
        Ok(payload)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClientEnvelope<T> {
    code: i32,
    data: Option<T>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TelemetryEvent {
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub props: Option<serde_json::Map<String, Value>>,
    #[serde(rename = "clientVersion", skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
}

impl TelemetryEvent {
    pub fn new(
        event: &str,
        props: serde_json::Map<String, Value>,
        client_version: &str,
    ) -> Option<Self> {
        const EVENTS: &[&str] = &["app_open", "feature_use", "export", "ai_call", "error"];
        const PROPS: &[&str] = &[
            "feature", "action", "result", "duration", "platform", "count", "source",
        ];
        EVENTS.contains(&event).then(|| Self {
            event: event.to_owned(),
            props: Some(
                props
                    .into_iter()
                    .filter(|(key, _)| PROPS.contains(&key.as_str()))
                    .collect(),
            ),
            client_version: Some(client_version.to_owned()),
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivationResponse {
    pub token: String,
    pub expire_at: i64,
    pub grace_sec: i64,
    #[serde(default)]
    pub enc_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeartbeatResponse {
    pub revoked: bool,
    pub state: String,
    pub reason: String,
    pub server_time: i64,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub expire_at: Option<i64>,
    #[serde(default)]
    pub grace_sec: Option<i64>,
    #[serde(default)]
    pub enc_key: Option<String>,
    #[serde(default)]
    pub allow_custom_api_key: Option<i64>,
    #[serde(rename = "max_screen_limit", default)]
    pub max_screen_limit: Option<i64>,
    #[serde(default)]
    pub transition_materials: Option<TransitionMaterialSignal>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TransitionMaterialSignal {
    pub catalog_version: i64,
    pub minimum_app_version: String,
}

impl TransitionMaterialSignal {
    pub fn is_valid(&self) -> bool {
        self.catalog_version > 0 && is_semantic_version(&self.minimum_app_version)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TransitionMaterialCatalogResponse {
    pub catalog_version: i64,
    pub changed: bool,
    pub cdn_base_url: String,
    #[serde(default)]
    pub materials: Option<Vec<TransitionMaterialResponse>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TransitionMaterialResponse {
    pub asset_key: String,
    pub asset_version: i64,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub category: String,
    pub render_mode: String,
    pub video_path: String,
    #[serde(default)]
    pub preview_path: Option<String>,
    #[serde(default)]
    pub cover_path: Option<String>,
    pub sha256: String,
    pub size_bytes: u64,
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub video_codec: String,
    pub has_audio: bool,
    pub sort_order: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStartResponse {
    pub model_id: String,
    pub model_asr: String,
    pub version: String,
    pub url: String,
    pub signature: Option<String>,
    pub min_client_version: String,
    pub force: bool,
    #[serde(rename = "max_screen_limit", default)]
    pub max_screen_limit: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryResponse {
    pub accepted: u32,
    pub rejected: u32,
}

#[derive(Clone)]
pub struct ApiClient {
    client: reqwest::Client,
    config: ApiConfig,
    codec: Arc<dyn RequestCodec>,
    diagnostics: Arc<dyn ApiDiagnosticSink>,
}

impl ApiClient {
    pub fn from_env() -> Result<Self, ApiError> {
        Self::new(ApiConfig::from_env(), Arc::new(PlainJsonCodec))
    }

    pub fn new(config: ApiConfig, codec: Arc<dyn RequestCodec>) -> Result<Self, ApiError> {
        Self::new_with_diagnostics(config, codec, Arc::new(StderrApiDiagnosticSink))
    }

    pub fn new_with_diagnostics(
        config: ApiConfig,
        codec: Arc<dyn RequestCodec>,
        diagnostics: Arc<dyn ApiDiagnosticSink>,
    ) -> Result<Self, ApiError> {
        let parsed = reqwest::Url::parse(&config.base_url).map_err(|_| ApiError::Configuration)?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(ApiError::Configuration);
        }
        let mut builder = reqwest::Client::builder().timeout(config.timeout);
        if endpoint_is_loopback(&parsed) {
            builder = builder.no_proxy();
        }
        let client = builder.build().map_err(|_| ApiError::Configuration)?;
        Ok(Self {
            client,
            config,
            codec,
            diagnostics,
        })
    }

    pub fn config(&self) -> &ApiConfig {
        &self.config
    }

    pub async fn activate(
        &self,
        device_id: &str,
        activate_code: &str,
    ) -> Result<ActivationResponse, ApiError> {
        let payload = serde_json::json!({
            "device_id": device_id,
            "activate_code": activate_code,
        });
        let mut diagnostic = RequestDiagnostic::authorized(
            "activate",
            "POST",
            "client/activate",
            device_id,
            activate_code,
        );
        diagnostic.request_fields = vec!["device_id", "activate_code"];
        self.post_json(
            "client/activate",
            payload,
            HeaderMap::new(),
            false,
            diagnostic,
        )
        .await
    }

    pub async fn heartbeat(
        &self,
        device_id: &str,
        activate_code: &str,
    ) -> Result<HeartbeatResponse, ApiError> {
        let headers = client_headers(device_id, activate_code)?;
        self.post_json(
            "client/heartbeat",
            serde_json::json!({}),
            headers,
            true,
            RequestDiagnostic::authorized(
                "heartbeat",
                "POST",
                "client/heartbeat",
                device_id,
                activate_code,
            ),
        )
        .await
    }

    pub async fn transition_materials(
        &self,
        device_id: &str,
        activate_code: &str,
        catalog_version: i64,
    ) -> Result<TransitionMaterialCatalogResponse, ApiError> {
        if catalog_version < 0 {
            return Err(ApiError::Configuration);
        }
        let started = Instant::now();
        let diagnostic = RequestDiagnostic::authorized(
            "transition_materials",
            "GET",
            "v1/transition-materials",
            device_id,
            activate_code,
        );
        let headers = client_headers(device_id, activate_code)?;
        let mut url = reqwest::Url::parse(&self.endpoint("v1/transition-materials"))
            .map_err(|_| ApiError::Configuration)?;
        url.query_pairs_mut()
            .append_pair("clientVersion", &catalog_version.to_string());
        let response = self
            .client
            .get(url)
            .headers(headers)
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| {
                let error = map_reqwest_error(error);
                self.emit_failure(
                    &diagnostic,
                    started,
                    &ResponseMetadata::default(),
                    &error,
                    None,
                );
                error
            })?;
        self.decode_response(response, false, false, started, diagnostic)
            .await?
            .ok_or(ApiError::InvalidResponse)
    }

    pub async fn app_start(&self) -> Result<Option<AppStartResponse>, ApiError> {
        let started = Instant::now();
        let diagnostic = RequestDiagnostic::public("app_start", "GET", "client/app-start");
        let url = self.endpoint("client/app-start");
        let response = self
            .client
            .get(url)
            .header("platform", &self.config.platform)
            .header("client_version", &self.config.client_version)
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| {
                let error = map_reqwest_error(error);
                self.emit_failure(
                    &diagnostic,
                    started,
                    &ResponseMetadata::default(),
                    &error,
                    None,
                );
                error
            })?;
        self.decode_response(response, false, true, started, diagnostic)
            .await
    }

    pub async fn telemetry(
        &self,
        device_id: Option<&str>,
        activate_code: Option<&str>,
        events: &[TelemetryEvent],
    ) -> Result<TelemetryResponse, ApiError> {
        if events.is_empty() {
            return Ok(TelemetryResponse {
                accepted: 0,
                rejected: 0,
            });
        }
        let mut headers = HeaderMap::new();
        if let (Some(device_id), Some(activate_code)) = (device_id, activate_code) {
            headers = client_headers(device_id, activate_code)?;
        }
        let payload = serde_json::to_value(events).map_err(|_| ApiError::InvalidResponse)?;
        let diagnostic = match (device_id, activate_code) {
            (Some(device_id), Some(activate_code)) => RequestDiagnostic::authorized(
                "telemetry",
                "POST",
                "client/telemetry",
                device_id,
                activate_code,
            ),
            _ => RequestDiagnostic::public("telemetry", "POST", "client/telemetry"),
        };
        self.post_json("client/telemetry", payload, headers, false, diagnostic)
            .await
    }

    async fn post_json<T: DeserializeOwned>(
        &self,
        path: &str,
        payload: Value,
        headers: HeaderMap,
        allow_force_offline: bool,
        diagnostic: RequestDiagnostic,
    ) -> Result<T, ApiError> {
        let started = Instant::now();
        if diagnostic.trace_success {
            self.diagnostics.emit(diagnostic_event(
                &diagnostic,
                "request_started",
                None,
                None,
                None,
            ));
        }
        let payload = self.codec.encode_request(payload)?;
        let response = self
            .client
            .post(self.endpoint(path))
            .headers(headers)
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|error| {
                let error = map_reqwest_error(error);
                self.emit_failure(
                    &diagnostic,
                    started,
                    &ResponseMetadata::default(),
                    &error,
                    None,
                );
                error
            })?;
        self.decode_response(response, allow_force_offline, false, started, diagnostic)
            .await?
            .ok_or(ApiError::InvalidResponse)
    }

    async fn decode_response<T: DeserializeOwned>(
        &self,
        response: reqwest::Response,
        allow_force_offline: bool,
        allow_empty_data: bool,
        started: Instant,
        diagnostic: RequestDiagnostic,
    ) -> Result<Option<T>, ApiError> {
        let metadata = response_metadata(&response);
        let payload = match response.json::<Value>().await {
            Ok(payload) => payload,
            Err(_) => {
                let error = ApiError::InvalidResponse.with_context(&metadata);
                self.emit_failure(&diagnostic, started, &metadata, &error, None);
                return Err(error);
            }
        };
        let payload = match self.codec.decode_response(payload) {
            Ok(payload) => payload,
            Err(error) => {
                let error = error.with_context(&metadata);
                self.emit_failure(&diagnostic, started, &metadata, &error, None);
                return Err(error);
            }
        };
        let envelope = match serde_json::from_value::<ClientEnvelope<Value>>(payload) {
            Ok(envelope) => envelope,
            Err(_) => {
                let error = ApiError::InvalidResponse.with_context(&metadata);
                self.emit_failure(&diagnostic, started, &metadata, &error, None);
                return Err(error);
            }
        };
        let summary = ResponseSummary::from_data(envelope.data.as_ref());
        if envelope.code != 200 && !(allow_force_offline && envelope.code == 201) {
            let error = ApiError::Business {
                code: envelope.code,
                message: business_safe_message(envelope.code),
            }
            .with_context(&metadata);
            self.emit_failure(&diagnostic, started, &metadata, &error, Some(&summary));
            return Err(error);
        }
        let decoded = match envelope.data {
            Some(data) => match serde_json::from_value::<T>(data) {
                Ok(data) => Some(data),
                Err(_) => {
                    let error = ApiError::InvalidResponse.with_context(&metadata);
                    self.emit_failure(&diagnostic, started, &metadata, &error, Some(&summary));
                    return Err(error);
                }
            },
            None if allow_empty_data => None,
            None => {
                let error = ApiError::InvalidResponse.with_context(&metadata);
                self.emit_failure(&diagnostic, started, &metadata, &error, Some(&summary));
                return Err(error);
            }
        };
        if diagnostic.trace_success {
            self.diagnostics.emit(diagnostic_event(
                &diagnostic,
                "request_completed",
                Some(started.elapsed()),
                Some((&metadata, Some(envelope.code))),
                Some(&summary),
            ));
        }
        Ok(decoded)
    }

    fn emit_failure(
        &self,
        diagnostic: &RequestDiagnostic,
        started: Instant,
        metadata: &ResponseMetadata,
        error: &ApiError,
        summary: Option<&ResponseSummary>,
    ) {
        let mut event = diagnostic_event(
            diagnostic,
            "request_failed",
            Some(started.elapsed()),
            Some((metadata, error.code())),
            summary,
        );
        event.error_category = Some(error.category());
        self.diagnostics.emit(event);
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.config.base_url, path.trim_start_matches('/'))
    }
}

fn endpoint_is_loopback(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}

#[derive(Default)]
struct ResponseSummary {
    state: Option<String>,
    reason: Option<String>,
    revoked: Option<bool>,
}

impl ResponseSummary {
    fn from_data(data: Option<&Value>) -> Self {
        let Some(data) = data.and_then(Value::as_object) else {
            return Self::default();
        };
        Self {
            state: data
                .get("state")
                .and_then(Value::as_str)
                .and_then(safe_state),
            reason: data
                .get("reason")
                .and_then(Value::as_str)
                .and_then(safe_reason),
            revoked: data.get("revoked").and_then(Value::as_bool),
        }
    }
}

fn diagnostic_event(
    diagnostic: &RequestDiagnostic,
    event: &'static str,
    duration: Option<Duration>,
    response: Option<(&ResponseMetadata, Option<i32>)>,
    summary: Option<&ResponseSummary>,
) -> ApiDiagnosticEvent {
    let (metadata, business_code) = response
        .map(|(metadata, code)| (Some(metadata), code))
        .unwrap_or((None, None));
    ApiDiagnosticEvent {
        component: "client_api",
        event,
        operation: diagnostic.operation,
        method: diagnostic.method,
        path: diagnostic.path,
        duration_ms: duration
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)),
        http_status: metadata.and_then(|metadata| metadata.http_status),
        business_code,
        error_category: None,
        state: summary.and_then(|summary| summary.state.clone()),
        reason: summary.and_then(|summary| summary.reason.clone()),
        revoked: summary.and_then(|summary| summary.revoked),
        request_id: metadata.and_then(|metadata| metadata.request_id.clone()),
        trace_id: metadata.and_then(|metadata| metadata.trace_id.clone()),
        device_id_hint: diagnostic.device_id_hint.clone(),
        has_device_id: diagnostic.has_device_id,
        has_activate_code: diagnostic.has_activate_code,
        request_fields: diagnostic.request_fields.clone(),
    }
}

fn response_metadata(response: &reqwest::Response) -> ResponseMetadata {
    ResponseMetadata {
        http_status: Some(response.status().as_u16()),
        request_id: response_header(response.headers(), "x-request-id"),
        trace_id: response_header(response.headers(), "x-trace-id"),
    }
}

fn response_header(headers: &HeaderMap, name: &'static str) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?.trim();
    (!value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

fn safe_state(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_uppercase();
    matches!(
        normalized.as_str(),
        "ACTIVE" | "DISABLED" | "EXPIRED" | "INACTIVE"
    )
    .then_some(normalized)
}

fn safe_reason(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "ok" | "disabled" | "expired" | "unbound"
    )
    .then_some(normalized)
}

fn device_hint(device_id: &str) -> String {
    if device_id.chars().count() <= 8 {
        return "…本地设备".to_owned();
    }
    let suffix = device_id
        .chars()
        .rev()
        .take(8)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("…{suffix}")
}

fn business_error_category(code: i32) -> &'static str {
    match code {
        1001 | 2001 => "header_contract",
        1002..=1006 | 2002..=2004 => "authorization_invalid",
        1007 | 9000 => "retryable_service",
        9101..=9105 => "protocol_recoverable",
        _ => "business",
    }
}

fn business_safe_message(code: i32) -> String {
    match code {
        1001 => "授权服务未收到 activate_code 请求头，请检查反向代理配置".to_owned(),
        1002 => "激活码格式无效，请重新输入".to_owned(),
        1003 => "激活码不存在，请重新输入".to_owned(),
        1004 => "激活码已过期，请续费后重新激活".to_owned(),
        1005 => "激活码已停用，请联系管理员".to_owned(),
        1006 => "激活码已绑定其他设备，请联系管理员重置绑定".to_owned(),
        1007 => "激活请求正在处理中，请稍后重试".to_owned(),
        2001 => "授权服务未收到 device_id 请求头，请检查反向代理配置".to_owned(),
        2002 => "设备标识无效，请重新激活".to_owned(),
        2003 => "设备尚未绑定，请重新激活".to_owned(),
        2004 => "设备与激活码绑定不匹配，请重新激活".to_owned(),
        9000 => "授权服务暂时异常，请稍后重试".to_owned(),
        9101 => "授权协议正文无效，请稍后重试".to_owned(),
        9102 => "授权协议签名校验失败，请重新激活".to_owned(),
        9103 => "授权请求被判定为重复请求，请稍后重试".to_owned(),
        9104 => "客户端时间与服务端不同步，正在重新校准".to_owned(),
        9105 => "授权会话状态不同步，正在重新确认".to_owned(),
        _ => "服务端拒绝了本次请求".to_owned(),
    }
}

fn client_headers(device_id: &str, activate_code: &str) -> Result<HeaderMap, ApiError> {
    let mut headers = HeaderMap::new();
    let device_id = HeaderValue::try_from(device_id).map_err(|_| ApiError::Configuration)?;
    let activate_code =
        HeaderValue::try_from(activate_code).map_err(|_| ApiError::Configuration)?;
    // Nginx 默认可能丢弃含下划线的请求头。保留既有服务端契约，同时发送
    // 连字符别名，便于代理和服务端逐步迁移到标准 HTTP 头名称。
    headers.insert(HeaderName::from_static("device_id"), device_id.clone());
    headers.insert(HeaderName::from_static("device-id"), device_id);
    headers.insert(
        HeaderName::from_static("activate_code"),
        activate_code.clone(),
    );
    headers.insert(HeaderName::from_static("activate-code"), activate_code);
    Ok(headers)
}

fn map_reqwest_error(error: reqwest::Error) -> ApiError {
    if error.is_timeout() {
        ApiError::Timeout
    } else {
        ApiError::Transport
    }
}

#[cfg(test)]
fn safe_business_message(message: String) -> String {
    let message = message.trim();
    if message.is_empty() || message.len() > 256 || message.chars().any(char::is_control) {
        "服务端拒绝了本次请求".to_owned()
    } else {
        message.to_owned()
    }
}

fn is_semantic_version(value: &str) -> bool {
    let core = value
        .trim()
        .split_once('+')
        .map_or(value.trim(), |(core, _)| core);
    let core = core.split_once('-').map_or(core, |(core, _)| core);
    let mut parts = core.split('.');
    let valid = (0..3).all(|_| {
        parts
            .next()
            .filter(|part| !part.is_empty())
            .and_then(|part| part.parse::<u64>().ok())
            .is_some()
    });
    valid && parts.next().is_none()
}

#[cfg(test)]
fn decode_envelope_payload<T: DeserializeOwned>(
    payload: Value,
    allow_force_offline: bool,
) -> Result<T, ApiError> {
    let envelope: ClientEnvelope<T> =
        serde_json::from_value(payload).map_err(|_| ApiError::InvalidResponse)?;
    if envelope.code != 200 && !(allow_force_offline && envelope.code == 201) {
        return Err(ApiError::Business {
            code: envelope.code,
            message: business_safe_message(envelope.code),
        });
    }
    envelope.data.ok_or(ApiError::InvalidResponse)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_base_url_without_exposing_query_data() {
        assert_eq!(
            normalize_base_url(" http://localhost/api/// "),
            "http://localhost/api"
        );
        assert_eq!(normalize_base_url(""), "http://localhost/api");
    }

    #[test]
    fn business_messages_are_bounded_and_control_free() {
        assert_eq!(safe_business_message("  激活失败  ".to_owned()), "激活失败");
        assert_eq!(
            safe_business_message("\u{0}".to_owned()),
            "服务端拒绝了本次请求"
        );
        assert_eq!(
            safe_business_message("x".repeat(257)),
            "服务端拒绝了本次请求"
        );
    }

    #[test]
    fn default_config_has_api_fallback_and_platform() {
        let config = ApiConfig::from_env();
        assert!(!config.base_url.is_empty());
        assert!(["win", "mac", "general"].contains(&config.platform.as_str()));
    }

    #[test]
    fn default_request_timeout_allows_slow_activation_response() {
        assert_eq!(DEFAULT_REQUEST_TIMEOUT, Duration::from_secs(30));
    }

    #[test]
    fn decodes_business_codes_independently_from_http_status() {
        let active: HeartbeatResponse = decode_envelope_payload(
            serde_json::json!({
                "code": 200,
                "data": {"revoked": false, "state": "ACTIVE", "reason": "ok", "serverTime": 42},
                "msg": "续约成功"
            }),
            true,
        )
        .unwrap();
        assert!(!active.revoked);
        assert!(active.transition_materials.is_none());

        let revoked: HeartbeatResponse = decode_envelope_payload(
            serde_json::json!({
                "code": 201,
                "data": {"revoked": true, "state": "DISABLED", "reason": "disabled", "serverTime": 43},
                "msg": "强制下线"
            }),
            true,
        )
        .unwrap();
        assert!(revoked.revoked);

        let error = decode_envelope_payload::<ActivationResponse>(
            serde_json::json!({"code": 1003, "data": null, "msg": "激活码不存在"}),
            false,
        )
        .unwrap_err();
        assert_eq!(error.code(), Some(1003));
        assert_eq!(
            error.safe_message(),
            "服务端业务错误（1003）：激活码不存在，请重新输入"
        );
    }

    #[test]
    fn validates_optional_transition_material_signal_without_affecting_heartbeat_decode() {
        let heartbeat: HeartbeatResponse = decode_envelope_payload(
            serde_json::json!({
                "code": 200,
                "data": {
                    "revoked": false,
                    "state": "ACTIVE",
                    "reason": "ok",
                    "serverTime": 42,
                    "transitionMaterials": {
                        "catalogVersion": 2,
                        "minimumAppVersion": "0.3.0"
                    }
                }
            }),
            true,
        )
        .unwrap();
        assert!(heartbeat.transition_materials.unwrap().is_valid());

        for signal in [
            TransitionMaterialSignal {
                catalog_version: 0,
                minimum_app_version: "0.3.0".to_owned(),
            },
            TransitionMaterialSignal {
                catalog_version: 2,
                minimum_app_version: "not-a-version".to_owned(),
            },
        ] {
            assert!(!signal.is_valid());
        }
    }

    #[test]
    fn telemetry_constructor_keeps_only_service_whitelist() {
        let event = TelemetryEvent::new(
            "feature_use",
            serde_json::json!({
                "feature": "monitor",
                "action": "open",
                "localPath": "/private/video.mkv",
                "transcript": "不得上传"
            })
            .as_object()
            .unwrap()
            .clone(),
            "0.2.0",
        )
        .unwrap();
        let props = event.props.unwrap();
        assert_eq!(props.len(), 2);
        assert!(!props.contains_key("localPath"));
        assert!(!props.contains_key("transcript"));
        assert!(TelemetryEvent::new("unknown", serde_json::Map::new(), "0.2.0").is_none());
    }

    #[test]
    fn rejects_base_urls_with_credentials_or_query_parameters() {
        let config = |base_url: &str| ApiConfig {
            base_url: base_url.to_owned(),
            timeout: Duration::from_secs(1),
            client_version: "test".to_owned(),
            platform: "mac".to_owned(),
        };
        assert!(
            ApiClient::new(
                config("https://user:secret@example.com/api"),
                Arc::new(PlainJsonCodec)
            )
            .is_err()
        );
        assert!(
            ApiClient::new(
                config("https://example.com/api?token=secret"),
                Arc::new(PlainJsonCodec)
            )
            .is_err()
        );
        assert!(
            ApiClient::new(config("https://example.com/api"), Arc::new(PlainJsonCodec)).is_ok()
        );
    }
}
