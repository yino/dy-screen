//! ASR Adapter 必须使用的稳定、可脱敏错误契约。

#![deny(missing_docs)]

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// ASR 各实现必须映射到的稳定错误分类。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AsrErrorKind {
    /// 当前操作系统或 CPU 架构没有可用实现。
    UnsupportedPlatform,
    /// sidecar、模型、运行库或最低资源检查未通过。
    EnvironmentUnavailable,
    /// 调用方提交的中立请求不符合契约。
    InvalidInput,
    /// VAD 没有检测到可识别人声。
    NoSpeech,
    /// 用户、应用退出或调度器主动取消任务。
    Cancelled,
    /// 受控媒体或模型子进程启动失败或异常退出。
    ProcessFailed,
    /// Adapter 无法把供应商输出解析为合法中立结果。
    MalformedOutput,
    /// 内存、磁盘、线程或其他受控资源不足。
    ResourceExhausted,
    /// 访问受控本地文件或进程管道失败。
    Io,
    /// 不属于以上稳定类别的内部不变量错误。
    Internal,
}

/// 可安全返回给界面、事件和普通日志的 ASR 错误。
///
/// 该类型故意不保存原始 stderr、媒体路径或模型路径。适配器需要诊断细节时，应写入受控
/// 诊断日志，并在构造此错误前完成脱敏与分类。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Error)]
#[error("{safe_message}")]
#[serde(rename_all = "camelCase")]
pub struct AsrError {
    /// 跨 Adapter 稳定的错误大类。
    pub kind: AsrErrorKind,
    /// 供 UI、测试和重试策略稳定匹配的机器代码。
    pub code: String,
    /// 可直接展示且不包含路径、凭据或原始 stderr 的中文消息。
    pub safe_message: String,
    /// 修复环境或稍后重试是否可能成功。
    pub retryable: bool,
}

impl AsrError {
    /// 创建一个不包含路径或供应商输出的稳定错误。
    pub fn new(
        kind: AsrErrorKind,
        code: impl Into<String>,
        safe_message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            kind,
            code: code.into(),
            safe_message: safe_message.into(),
            retryable,
        }
    }

    /// 创建统一的请求校验错误。
    pub fn invalid_input(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(AsrErrorKind::InvalidInput, code, message, false)
    }

    /// 创建统一的用户取消错误。
    pub fn cancelled() -> Self {
        Self::new(
            AsrErrorKind::Cancelled,
            "asr_cancelled",
            "语音识别已取消",
            true,
        )
    }
}

/// ASR 核心和所有 Adapter 统一使用的结果类型。
pub type AsrResult<T> = std::result::Result<T, AsrError>;
