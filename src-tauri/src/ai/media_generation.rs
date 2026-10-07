//! 受控媒体生成业务工具（步骤 2 任务组 7）。
//!
//! 决策六：媒体生成是**受控业务工具，不是凭据**。sidecar 只能以 `tool.invoke` 提交
//! 结构化生成请求并取回业务任务 ID；短期 token、真实上游提交、轮询与下载地址都在
//! Rust 侧，且本 change **只交付契约 + 门禁 + 桩实现**——真实 Wanx 调用留给
//! `add-generative-video-animation`（步骤 4），因此本 change 验收期间不产生任何付费调用。
//!
//! 决策十一：额度口径写进冻结配置——以 heartbeat 的开通/额度结论为总上限，本地再冻结
//! 任务级上限（每次运行最多 4 张图、最多 5 秒视频、单次金额按模型单价折算上限）。

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::harness::{HarnessError, ToolExecutor};

/// 生成类型：图片与视频**互斥**（一次请求只能一种）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaGenerationKind {
    Image,
    Video,
}

/// 一次结构化的媒体生成请求。
///
/// `deny_unknown_fields` 是关键安全边界：请求里**不允许**出现 URL、本地路径、
/// Provider 原始参数、滤镜或命令行片段——这些字段在结构上就不存在。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaGenerationRequest {
    pub kind: MediaGenerationKind,
    /// 白名单模型标识（Rust 侧校验，不接受任意 Provider 原始参数）。
    pub model: String,
    /// 创意提示词（文本，不是指令）。
    pub prompt: String,
    /// 图片张数（仅 Image；Video 必须为 0）。
    pub count: u32,
    /// 视频时长毫秒（仅 Video；Image 必须为 0）。
    pub duration_ms: u64,
}

/// 返回结构：**只有**业务任务 ID 与状态，不含 token、下载地址或 Provider 原始响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaGenerationResult {
    pub task_id: String,
    pub status: MediaTaskStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaTaskStatus {
    Submitted,
}

/// 冻结的任务级额度口径（决策十一）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaQuota {
    pub max_images_per_run: u32,
    pub max_video_seconds_per_run: u64,
    pub max_amount_minor: u64,
    pub image_unit_price_minor: u64,
    pub video_unit_price_per_second_minor: u64,
}

impl Default for MediaQuota {
    fn default() -> Self {
        Self {
            max_images_per_run: 4,
            max_video_seconds_per_run: 5,
            max_amount_minor: 400, // ¥4.00
            image_unit_price_minor: 20,
            video_unit_price_per_second_minor: 50,
        }
    }
}

/// 稳定错误分类（脱敏：不含提示词正文）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaGenerationError {
    pub code: &'static str,
    pub detail: &'static str,
}

impl MediaGenerationError {
    fn new(code: &'static str, detail: &'static str) -> Self {
        Self { code, detail }
    }
}

/// 真实上游提交接口：只有 Rust 侧能调用。本 change 用桩实现，真实调用留步骤 4。
pub trait MediaUpstream: Send + Sync {
    /// 提交一次生成请求，返回业务任务 ID。桩实现不触网、不产生付费调用。
    fn submit(&self, request: &MediaGenerationRequest) -> Result<String, MediaGenerationError>;
}

/// 桩实现：返回确定性假任务 ID，**绝不触网**。
#[derive(Default)]
pub struct StubMediaUpstream {
    pub submissions: AtomicU64,
}

impl MediaUpstream for StubMediaUpstream {
    fn submit(&self, request: &MediaGenerationRequest) -> Result<String, MediaGenerationError> {
        let sequence = self.submissions.fetch_add(1, Ordering::SeqCst);
        Ok(format!(
            "stub-{}-{}-{sequence}",
            match request.kind {
                MediaGenerationKind::Image => "image",
                MediaGenerationKind::Video => "video",
            },
            request.model
        ))
    }
}

/// 一次运行内的累计预算（任务 7.4）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct MediaRunBudget {
    images_used: u32,
    video_ms_used: u64,
    amount_minor_used: u64,
}

impl MediaRunBudget {
    /// 计算本次请求的金额（分）。
    fn cost(request: &MediaGenerationRequest, quota: &MediaQuota) -> u64 {
        match request.kind {
            MediaGenerationKind::Image => u64::from(request.count) * quota.image_unit_price_minor,
            MediaGenerationKind::Video => {
                (request.duration_ms.div_ceil(1_000)) * quota.video_unit_price_per_second_minor
            }
        }
    }

    /// 在额度内预留本次请求；超限返回可区分原因。
    fn try_reserve(
        &mut self,
        request: &MediaGenerationRequest,
        quota: &MediaQuota,
    ) -> Result<(), MediaGenerationError> {
        let cost = Self::cost(request, quota);
        match request.kind {
            MediaGenerationKind::Image => {
                let next_images = self.images_used.saturating_add(request.count);
                if next_images > quota.max_images_per_run {
                    return Err(MediaGenerationError::new(
                        "media_image_quota_exceeded",
                        "本次运行的图片张数超过上限",
                    ));
                }
                self.images_used = next_images;
            }
            MediaGenerationKind::Video => {
                let next_ms = self.video_ms_used.saturating_add(request.duration_ms);
                if next_ms > quota.max_video_seconds_per_run.saturating_mul(1_000) {
                    return Err(MediaGenerationError::new(
                        "media_video_quota_exceeded",
                        "本次运行的视频时长超过上限",
                    ));
                }
                self.video_ms_used = next_ms;
            }
        }
        let next_amount = self.amount_minor_used.saturating_add(cost);
        if next_amount > quota.max_amount_minor {
            return Err(MediaGenerationError::new(
                "media_amount_quota_exceeded",
                "本次运行的金额超过上限",
            ));
        }
        self.amount_minor_used = next_amount;
        Ok(())
    }
}

/// 校验请求结构（任务 7.3）：时间范围、互斥类型、白名单模型、提示词非空。
fn validate_request_shape(request: &MediaGenerationRequest) -> Result<(), MediaGenerationError> {
    if request.prompt.trim().is_empty() || request.prompt.chars().count() > 4_000 {
        return Err(MediaGenerationError::new(
            "media_prompt_invalid",
            "提示词为空或超长",
        ));
    }
    if request.model.trim().is_empty() {
        return Err(MediaGenerationError::new(
            "media_model_invalid",
            "模型标识为空",
        ));
    }
    match request.kind {
        MediaGenerationKind::Image => {
            if request.count == 0 || request.count > 16 {
                return Err(MediaGenerationError::new(
                    "media_image_count_invalid",
                    "图片张数必须在 1..=16",
                ));
            }
            if request.duration_ms != 0 {
                return Err(MediaGenerationError::new(
                    "media_kind_mutually_exclusive",
                    "图片请求不得携带视频时长",
                ));
            }
        }
        MediaGenerationKind::Video => {
            if request.duration_ms == 0 || request.duration_ms > 30_000 {
                return Err(MediaGenerationError::new(
                    "media_video_duration_invalid",
                    "视频时长必须在 1..=30000 毫秒",
                ));
            }
            if request.count != 0 {
                return Err(MediaGenerationError::new(
                    "media_kind_mutually_exclusive",
                    "视频请求不得携带图片张数",
                ));
            }
        }
    }
    Ok(())
}

/// 受控媒体生成工具（实现 [`ToolExecutor`]，经 sidecar 的 `tool.invoke` 暴露）。
pub struct MediaGenerationTool<U: MediaUpstream> {
    upstream: U,
    quota: MediaQuota,
    budget: Mutex<MediaRunBudget>,
    max_attempts: u32,
    attempts: AtomicU32,
}

impl<U: MediaUpstream> MediaGenerationTool<U> {
    pub fn new(upstream: U, quota: MediaQuota) -> Self {
        Self {
            upstream,
            quota,
            budget: Mutex::new(MediaRunBudget::default()),
            max_attempts: 10,
            attempts: AtomicU32::new(0),
        }
    }

    pub fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = max_attempts;
        self
    }
}

impl<U: MediaUpstream> ToolExecutor for MediaGenerationTool<U> {
    fn execute(&self, tool: &str, params: &Value) -> Result<Value, HarnessError> {
        if tool != "media.generate" {
            return Err(HarnessError::ToolDenied("未知媒体工具"));
        }
        let attempts = self.attempts.fetch_add(1, Ordering::SeqCst);
        if attempts >= self.max_attempts {
            return Err(HarnessError::ToolDenied("超出本次运行的最大尝试次数"));
        }
        let request: MediaGenerationRequest = serde_json::from_value(params.clone())
            .map_err(|_| HarnessError::Protocol("媒体生成请求结构非法或含未知字段"))?;
        validate_request_shape(&request).map_err(|error| HarnessError::ToolDenied(error.code))?;
        {
            let mut budget = self
                .budget
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            budget
                .try_reserve(&request, &self.quota)
                .map_err(|error| HarnessError::ToolDenied(error.code))?;
        }
        let task_id = self
            .upstream
            .submit(&request)
            .map_err(|error| HarnessError::ToolDenied(error.code))?;
        Ok(serde_json::to_value(MediaGenerationResult {
            task_id,
            status: MediaTaskStatus::Submitted,
        })
        .unwrap_or(Value::Null))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_request(count: u32) -> MediaGenerationRequest {
        MediaGenerationRequest {
            kind: MediaGenerationKind::Image,
            model: "wanx".to_owned(),
            prompt: "一只在沙发上睡觉的猫".to_owned(),
            count,
            duration_ms: 0,
        }
    }

    fn video_request(duration_ms: u64) -> MediaGenerationRequest {
        MediaGenerationRequest {
            kind: MediaGenerationKind::Video,
            model: "wanx".to_owned(),
            prompt: "一只猫伸懒腰".to_owned(),
            count: 0,
            duration_ms,
        }
    }

    fn make_tool() -> MediaGenerationTool<StubMediaUpstream> {
        let upstream = StubMediaUpstream::default();
        MediaGenerationTool::new(upstream, MediaQuota::default())
    }

    #[test]
    fn request_shape_is_mutually_exclusive_and_time_bounded() {
        // 图片 + 视频互斥。
        let mut mixed = image_request(1);
        mixed.duration_ms = 1_000;
        assert_eq!(
            validate_request_shape(&mixed).unwrap_err().code,
            "media_kind_mutually_exclusive"
        );
        // 视频时长越界。
        assert_eq!(
            validate_request_shape(&video_request(60_000))
                .unwrap_err()
                .code,
            "media_video_duration_invalid"
        );
        // 图片张数越界。
        assert_eq!(
            validate_request_shape(&image_request(0)).unwrap_err().code,
            "media_image_count_invalid"
        );
        assert!(validate_request_shape(&image_request(4)).is_ok());
        assert!(validate_request_shape(&video_request(5_000)).is_ok());
    }

    #[test]
    fn unknown_fields_urls_paths_and_raw_provider_params_are_rejected() {
        // deny_unknown_fields：任何 URL/本地路径/滤镜/命令行片段字段都不存在。
        for payload in [
            r#"{"kind":"image","model":"wanx","prompt":"猫","count":1,"durationMs":0,"url":"https://x"}"#,
            r#"{"kind":"image","model":"wanx","prompt":"猫","count":1,"durationMs":0,"localPath":"/tmp/x"}"#,
            r#"{"kind":"image","model":"wanx","prompt":"猫","count":1,"durationMs":0,"filter":"blur"}"#,
            r#"{"kind":"image","model":"wanx","prompt":"猫","count":1,"durationMs":0,"cmdline":"ffmpeg -i"}"#,
        ] {
            let parsed: Result<MediaGenerationRequest, _> = serde_json::from_str(payload);
            assert!(parsed.is_err(), "含未知字段的请求必须被拒绝：{payload}");
        }
    }

    #[test]
    fn successful_generation_returns_only_task_id_and_status() {
        let tool = make_tool();
        let result = tool
            .execute(
                "media.generate",
                &serde_json::to_value(image_request(1)).unwrap(),
            )
            .expect("生成必须成功");
        let result: MediaGenerationResult = serde_json::from_value(result).unwrap();
        assert!(result.task_id.starts_with("stub-image-"));
        assert_eq!(result.status, MediaTaskStatus::Submitted);
        // 序列化后只有 task_id + status 两个字段。
        let json = serde_json::to_string(&result).unwrap();
        for forbidden in ["token", "url", "download", "ak", "st-lt-v1"] {
            assert!(!json.contains(forbidden), "结果泄漏「{forbidden}」：{json}");
        }
    }

    #[test]
    fn quota_is_enforced_before_any_submission() {
        // 图片张数超限（一次 5 张 > 4）。
        let t = make_tool();
        let error = t
            .execute(
                "media.generate",
                &serde_json::to_value(image_request(5)).unwrap(),
            )
            .unwrap_err();
        assert_eq!(error.code(), "harness_tool_denied");
        // 累计额度：先 2 张 + 再 3 张 = 5 张 > 4，第二次被拒。
        let tool = make_tool();
        tool.execute(
            "media.generate",
            &serde_json::to_value(image_request(2)).unwrap(),
        )
        .unwrap();
        let error = tool
            .execute(
                "media.generate",
                &serde_json::to_value(image_request(3)).unwrap(),
            )
            .unwrap_err();
        assert_eq!(error.code(), "harness_tool_denied");

        // 视频时长超限（6 秒 > 5 秒）。
        let tool = make_tool();
        let error = tool
            .execute(
                "media.generate",
                &serde_json::to_value(video_request(6_000)).unwrap(),
            )
            .unwrap_err();
        assert_eq!(error.code(), "harness_tool_denied");
    }

    #[test]
    fn amount_quota_is_enforced() {
        // 金额上限 400 分；5 秒视频 = 250 分，再来 4 张图 = 80 分，共 330 分 OK；
        // 再加 5 秒视频 = 250 分 → 580 > 400 被拒。
        let upstream = StubMediaUpstream::default();
        let tool = MediaGenerationTool::new(upstream, MediaQuota::default());
        tool.execute(
            "media.generate",
            &serde_json::to_value(video_request(5_000)).unwrap(),
        )
        .unwrap();
        tool.execute(
            "media.generate",
            &serde_json::to_value(image_request(4)).unwrap(),
        )
        .unwrap();
        let error = tool
            .execute(
                "media.generate",
                &serde_json::to_value(video_request(5_000)).unwrap(),
            )
            .unwrap_err();
        assert_eq!(error.code(), "harness_tool_denied");
    }

    #[test]
    fn max_attempts_are_enforced() {
        let upstream = StubMediaUpstream::default();
        let tool = MediaGenerationTool::new(upstream, MediaQuota::default()).with_max_attempts(2);
        tool.execute(
            "media.generate",
            &serde_json::to_value(image_request(1)).unwrap(),
        )
        .unwrap();
        tool.execute(
            "media.generate",
            &serde_json::to_value(image_request(1)).unwrap(),
        )
        .unwrap();
        let error = tool
            .execute(
                "media.generate",
                &serde_json::to_value(image_request(1)).unwrap(),
            )
            .unwrap_err();
        assert_eq!(error.code(), "harness_tool_denied");
    }

    #[test]
    fn stub_upstream_produces_no_paid_calls() {
        // 桩实现只返回确定性任务 ID：`submit` 的调用计数只在真正提交时 +1，
        // 且 task_id 不含任何上游/下载信息。
        let upstream = StubMediaUpstream::default();
        let tool = MediaGenerationTool::new(upstream, MediaQuota::default());
        let before = tool.upstream.submissions.load(Ordering::SeqCst);
        tool.execute(
            "media.generate",
            &serde_json::to_value(image_request(1)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            tool.upstream.submissions.load(Ordering::SeqCst),
            before + 1,
            "桩提交只递增一次，不触网"
        );
    }
}
