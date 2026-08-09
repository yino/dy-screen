//! 剪辑工作台 LLM 工作流的有界进度事件。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClipWorkflowProgress {
    pub workflow: String,
    pub clip_project_id: i64,
    pub run_id: Option<i64>,
    pub stage: String,
    pub completed: usize,
    pub total: usize,
    pub message: String,
}

pub trait ClipWorkflowPublisher: Send + Sync {
    fn publish(&self, progress: &ClipWorkflowProgress);
}

#[derive(Default)]
pub struct SilentClipWorkflowPublisher;

impl ClipWorkflowPublisher for SilentClipWorkflowPublisher {
    fn publish(&self, _progress: &ClipWorkflowProgress) {}
}
