//! 授权运营服务端 API 的唯一客户端入口。
//!
//! 这里刻意不把 `reqwest` 暴露给业务模块：服务端使用 HTTP 200 携带业务码，
//! 如果在各 command 中自行解析，很容易出现错误码、超时和敏感信息处理不一致。
//! 当前请求体保持 JSON 明文，但 `RequestCodec` 为后续信封加密保留了替换点。

use std::env;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const DEFAULT_API_BASE_URL: &str = "http://localhost/api/";
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

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
}

impl ApiError {
    pub fn code(&self) -> Option<i32> {
        match self {
            Self::Business { code, .. } => Some(*code),
            _ => None,
        }
    }

    pub fn safe_message(&self) -> String {
        self.to_string()
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
    #[serde(default)]
    msg: String,
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
}

impl ApiClient {
    pub fn from_env() -> Result<Self, ApiError> {
        Self::new(ApiConfig::from_env(), Arc::new(PlainJsonCodec))
    }

    pub fn new(config: ApiConfig, codec: Arc<dyn RequestCodec>) -> Result<Self, ApiError> {
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
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|_| ApiError::Configuration)?;
        Ok(Self {
            client,
            config,
            codec,
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
        self.post_json("client/activate", payload, HeaderMap::new(), false)
            .await
    }

    pub async fn heartbeat(
        &self,
        device_id: &str,
        activate_code: &str,
    ) -> Result<HeartbeatResponse, ApiError> {
        let headers = client_headers(device_id, activate_code)?;
        self.post_json("client/heartbeat", serde_json::json!({}), headers, true)
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
            .map_err(map_reqwest_error)?;
        self.decode_envelope_with_force(response, false).await
    }

    pub async fn app_start(&self) -> Result<Option<AppStartResponse>, ApiError> {
        let url = self.endpoint("client/app-start");
        let response = self
            .client
            .get(url)
            .header("platform", &self.config.platform)
            .header("client_version", &self.config.client_version)
            .header(ACCEPT, "application/json")
            .send()
            .await
            .map_err(map_reqwest_error)?;
        let payload = response
            .json::<Value>()
            .await
            .map_err(|_| ApiError::InvalidResponse)?;
        let payload = self.codec.decode_response(payload)?;
        let envelope: ClientEnvelope<Value> =
            serde_json::from_value(payload).map_err(|_| ApiError::InvalidResponse)?;
        if envelope.code != 200 {
            return Err(ApiError::Business {
                code: envelope.code,
                message: safe_business_message(envelope.msg),
            });
        }
        envelope
            .data
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| ApiError::InvalidResponse)
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
        self.post_json("client/telemetry", payload, headers, false)
            .await
    }

    async fn post_json<T: DeserializeOwned>(
        &self,
        path: &str,
        payload: Value,
        headers: HeaderMap,
        allow_force_offline: bool,
    ) -> Result<T, ApiError> {
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
            .map_err(map_reqwest_error)?;
        self.decode_envelope_with_force(response, allow_force_offline)
            .await
    }

    async fn decode_envelope_with_force<T: DeserializeOwned>(
        &self,
        response: reqwest::Response,
        allow_force_offline: bool,
    ) -> Result<T, ApiError> {
        let payload = response
            .json::<Value>()
            .await
            .map_err(|_| ApiError::InvalidResponse)?;
        let payload = self.codec.decode_response(payload)?;
        decode_envelope_payload(payload, allow_force_offline)
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.config.base_url, path.trim_start_matches('/'))
    }
}

fn client_headers(device_id: &str, activate_code: &str) -> Result<HeaderMap, ApiError> {
    let mut headers = HeaderMap::new();
    let device_id = HeaderValue::try_from(device_id).map_err(|_| ApiError::Configuration)?;
    let activate_code =
        HeaderValue::try_from(activate_code).map_err(|_| ApiError::Configuration)?;
    headers.insert(HeaderName::from_static("device_id"), device_id);
    headers.insert(HeaderName::from_static("activate_code"), activate_code);
    Ok(headers)
}

fn map_reqwest_error(error: reqwest::Error) -> ApiError {
    if error.is_timeout() {
        ApiError::Timeout
    } else {
        ApiError::Transport
    }
}

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

fn decode_envelope_payload<T: DeserializeOwned>(
    payload: Value,
    allow_force_offline: bool,
) -> Result<T, ApiError> {
    let envelope: ClientEnvelope<T> =
        serde_json::from_value(payload).map_err(|_| ApiError::InvalidResponse)?;
    if envelope.code != 200 && !(allow_force_offline && envelope.code == 201) {
        return Err(ApiError::Business {
            code: envelope.code,
            message: safe_business_message(envelope.msg),
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
        assert_eq!(error.safe_message(), "服务端业务错误（1003）：激活码不存在");
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
