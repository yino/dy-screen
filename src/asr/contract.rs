//! 与供应商无关的 ASR 请求、能力、进度和结果契约。
//!
//! 业务层只能依赖本模块公开的类型。新的本地模型或云端 Provider 必须通过
//! [`AsrEngine`] 适配，不能把命令行参数、凭据、资源路径或供应商私有字段泄漏进项目、
//! 缓存和 UI 数据模型。

#![deny(missing_docs)]

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::error::{AsrError, AsrResult as EngineResult};

/// 已准备音频的中立格式。首版只接受 Whisper 通用的 16 kHz 单声道 PCM。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PreparedAudioFormat {
    /// 带 RIFF/WAVE 容器头的 16-bit little-endian PCM 文件。
    PcmS16LeWav,
    /// 不带容器头的 16-bit little-endian PCM 字节流。
    PcmS16LeRaw,
}

/// 媒体层交给 ASR 引擎的短期音频资产。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreparedAudio {
    /// 受控临时音频路径；其生命周期由媒体准备层管理。
    pub path: PathBuf,
    /// 音频容器或字节流格式。
    pub format: PreparedAudioFormat,
    /// 采样率；当前中立契约固定要求 16 kHz。
    pub sample_rate_hz: u32,
    /// 声道数；当前中立契约固定要求单声道。
    pub channels: u16,
    /// 与源视频时间基准一致的音频总时长。
    pub duration_ms: u64,
}

/// VAD 返回并映射到源视频时间的有效人声范围。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SpeechRegion {
    /// 人声区间在源音频中的起始毫秒，包含该时间点。
    pub start_ms: u64,
    /// 人声区间在源音频中的结束毫秒，不包含该时间点。
    pub end_ms: u64,
}

impl SpeechRegion {
    /// 校验区间非空且没有超出已准备音频的时长。
    pub fn validate(self, duration_ms: u64) -> EngineResult<()> {
        if self.start_ms >= self.end_ms || self.end_ms > duration_ms {
            return Err(AsrError::invalid_input(
                "invalid_speech_region",
                "人声时间范围无效",
            ));
        }
        Ok(())
    }
}

/// 业务层需要的时间戳精度，不映射任何供应商命令行开关。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimestampPolicy {
    /// 返回可定位的句段级时间范围。
    Segment,
}

/// 统一 ASR 请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AsrRequest {
    /// 调用方生成的稳定请求标识，用于进度关联和诊断，不包含本地路径。
    pub request_id: String,
    /// 经过媒体层标准化且具有受控生命周期的音频。
    pub audio: PreparedAudio,
    /// 可选 BCP-47 风格语言提示；适配器不支持时必须显式拒绝。
    pub language_hint: Option<String>,
    /// 主播名、品牌名或商品名等用户热词。
    pub hotwords: Vec<String>,
    /// VAD 已确认并映射回源时间的人声区间。
    pub speech_regions: Vec<SpeechRegion>,
    /// 业务层要求的时间戳精度。
    pub timestamp_policy: TimestampPolicy,
    /// 此次识别允许使用的最大工作线程数。
    pub max_threads: usize,
}

impl AsrRequest {
    /// 在进入适配器之前执行供应商无关的强校验。
    pub fn validate(&self) -> EngineResult<()> {
        if self.request_id.trim().is_empty() {
            return Err(AsrError::invalid_input(
                "missing_request_id",
                "识别请求缺少请求标识",
            ));
        }
        if self.audio.sample_rate_hz != 16_000 || self.audio.channels != 1 {
            return Err(AsrError::invalid_input(
                "unsupported_audio_format",
                "语音识别只接受 16 kHz 单声道 PCM 音频",
            ));
        }
        if self.audio.duration_ms == 0 {
            return Err(AsrError::invalid_input(
                "empty_audio",
                "待识别音频时长必须大于零",
            ));
        }
        if self.max_threads == 0 {
            return Err(AsrError::invalid_input(
                "invalid_thread_limit",
                "识别线程上限必须大于零",
            ));
        }
        let mut previous_end = 0;
        for region in &self.speech_regions {
            region.validate(self.audio.duration_ms)?;
            if region.start_ms < previous_end {
                return Err(AsrError::invalid_input(
                    "overlapping_speech_regions",
                    "人声时间范围必须按顺序排列且不能重叠",
                ));
            }
            previous_end = region.end_ms;
        }
        if self.hotwords.iter().any(|word| word.trim().is_empty()) {
            return Err(AsrError::invalid_input("empty_hotword", "识别热词不能为空"));
        }
        Ok(())
    }
}

/// 引擎身份与模型身份会进入识别配置指纹和结果元数据。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AsrEngineIdentity {
    /// 稳定引擎逻辑标识，例如 `whisper.cpp`。
    pub engine_id: String,
    /// 引擎实现或 sidecar 的锁定版本。
    pub engine_version: String,
    /// 稳定模型逻辑标识，不包含本地文件路径。
    pub model_id: String,
    /// 模型版本及必要的内容指纹。
    pub model_version: String,
}

/// 置信信息的语义，避免业务层把不可比较的供应商数值混在一起。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AsrConfidenceKind {
    /// 可解释为 `0.0..=1.0` 的概率值。
    Probability,
}

/// 引擎能力用于显式拒绝不支持的配置，而不是静默忽略。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AsrCapabilities {
    /// 引擎明确支持的时间戳策略。
    pub timestamp_policies: Vec<TimestampPolicy>,
    /// 是否接受语言提示。
    pub supports_language_hint: bool,
    /// 是否接受热词提示。
    pub supports_hotwords: bool,
    /// 置信信息的可比较语义；不提供时为 `None`。
    pub confidence_kind: Option<AsrConfidenceKind>,
    /// 引擎允许调用方请求的最大线程数。
    pub maximum_threads: usize,
}

/// 任务前环境诊断结果。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AsrEnvironmentReport {
    /// 所有强制检查是否通过。
    pub ready: bool,
    /// 当前受支持的平台和架构标识。
    pub platform: String,
    /// 本次诊断对应的引擎和模型身份。
    pub identity: AsrEngineIdentity,
    /// 可直接安全展示给用户的逐项检查结果。
    pub checks: Vec<AsrEnvironmentCheck>,
}

/// 单项环境检查只返回中文安全说明，不返回本地路径。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AsrEnvironmentCheck {
    /// 供 UI 和测试稳定匹配的检查代码。
    pub code: String,
    /// 此项检查是否通过。
    pub passed: bool,
    /// 不包含本地路径、凭据或原始 stderr 的中文说明。
    pub message: String,
}

/// 引擎输出的句段。稳定业务句段 ID 在产物事务发布时生成。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AsrSegment {
    /// 句段在源视频时间基准内的起始毫秒。
    pub start_ms: u64,
    /// 句段在源视频时间基准内的结束毫秒。
    pub end_ms: u64,
    /// 引擎原始识别文本；规范化在业务后处理层完成。
    pub text: String,
    /// 引擎提供且语义可解释时的置信概率。
    pub confidence: Option<f32>,
}

impl AsrSegment {
    /// 校验句段范围、非空文本和可选置信概率。
    pub fn validate(&self, duration_ms: u64) -> EngineResult<()> {
        if self.start_ms >= self.end_ms || self.end_ms > duration_ms {
            return Err(AsrError::new(
                super::error::AsrErrorKind::MalformedOutput,
                "invalid_segment_range",
                "识别引擎返回了无效时间范围",
                false,
            ));
        }
        if self.text.trim().is_empty() {
            return Err(AsrError::new(
                super::error::AsrErrorKind::MalformedOutput,
                "empty_segment_text",
                "识别引擎返回了空文本句段",
                false,
            ));
        }
        if let Some(confidence) = self.confidence
            && !(0.0..=1.0).contains(&confidence)
        {
            return Err(AsrError::new(
                super::error::AsrErrorKind::MalformedOutput,
                "invalid_confidence",
                "识别引擎返回了无效置信信息",
                false,
            ));
        }
        Ok(())
    }
}

/// 可稳定展示或记录的非致命警告。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AsrWarning {
    /// 供 UI、日志和测试稳定匹配的非致命警告代码。
    pub code: String,
    /// 可安全展示的中文警告说明。
    pub message: String,
}

/// 统一识别结果，不包含 sidecar 路径或 Whisper 专属字段。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AsrResult {
    /// 与输入请求完全一致的请求标识。
    pub request_id: String,
    /// 实际执行识别的引擎和模型身份。
    pub identity: AsrEngineIdentity,
    /// 引擎检测到的语言；无法可靠判断时为 `None`。
    pub detected_language: Option<String>,
    /// 用于校验所有句段边界的音频总时长。
    pub audio_duration_ms: u64,
    /// 按起始时间排序且互不重叠的原始识别句段。
    pub segments: Vec<AsrSegment>,
    /// 不影响产物发布的可展示警告。
    pub warnings: Vec<AsrWarning>,
}

impl AsrResult {
    /// 校验所有句段都位于音频范围内且按时间有序、互不重叠。
    pub fn validate(&self) -> EngineResult<()> {
        let mut previous_end = 0;
        for segment in &self.segments {
            segment.validate(self.audio_duration_ms)?;
            if segment.start_ms < previous_end {
                return Err(AsrError::new(
                    super::error::AsrErrorKind::MalformedOutput,
                    "overlapping_segments",
                    "识别引擎返回了重叠句段",
                    false,
                ));
            }
            previous_end = segment.end_ms;
        }
        Ok(())
    }
}

/// 与具体供应商进度格式无关的识别阶段。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AsrProgressStage {
    /// 校验资源、平台和请求约束。
    ValidatingEnvironment,
    /// 启动引擎并加载模型。
    LoadingModel,
    /// 执行音频转写。
    Transcribing,
    /// 解析、校验并清理临时输出。
    Finalizing,
}

/// 引擎通过可注入 sink 发布的中立进度事件。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AsrProgressEvent {
    /// 与 [`AsrRequest::request_id`] 对应的请求标识。
    pub request_id: String,
    /// 当前执行阶段。
    pub stage: AsrProgressStage,
    /// 当前阶段已完成的工作单位。
    pub completed_units: u64,
    /// 当前阶段总工作单位；无法估算时为 `None`。
    pub total_units: Option<u64>,
    /// 可安全展示且不包含供应商原始输出的中文进度说明。
    pub message: String,
}

/// 进度发布边界可由 CLI、Tauri 事件或测试替身分别实现。
pub trait AsrProgressSink: Send + Sync {
    /// 发布一次进度快照；实现不得阻塞模型进程的 stdout/stderr 消费。
    fn publish(&self, event: AsrProgressEvent);
}

/// 与具体模型和进程实现解耦的语音识别接口。
#[async_trait]
pub trait AsrEngine: Send + Sync {
    /// 返回会进入识别配置指纹和产物元数据的稳定身份。
    fn identity(&self) -> AsrEngineIdentity;

    /// 返回引擎显式支持的能力，调用方据此拒绝不兼容配置。
    fn capabilities(&self) -> AsrCapabilities;

    /// 在启动媒体或模型进程前诊断平台、资源完整性和最低运行条件。
    async fn diagnose(&self) -> EngineResult<AsrEnvironmentReport>;

    /// 执行一次可取消识别，并通过中立进度 sink 发布状态。
    ///
    /// 实现必须先调用 [`AsrRequest::validate`]，退出前回收子进程和临时输出，并把任何
    /// 供应商错误映射为不泄漏路径或原始 stderr 的稳定 [`AsrError`]。
    async fn transcribe(
        &self,
        request: AsrRequest,
        progress: Arc<dyn AsrProgressSink>,
        cancellation: CancellationToken,
    ) -> EngineResult<AsrResult>;
}
