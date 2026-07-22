//! 从不可变转写产物生成不泄漏路径的 UI、复制及 TXT/JSON 只读投影。

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    AiInputSourceKind, AiInputStatus, AiProject, AiRepository, AiRepositoryError, TranscriptSegment,
};

#[derive(Debug, Error)]
pub enum AiProjectionError {
    #[error(transparent)]
    Repository(#[from] AiRepositoryError),
    #[error("找不到项目输入")]
    InputNotFound,
    #[error("无法生成转写 JSON")]
    Serialization,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiTranscriptProjection {
    pub project: AiProject,
    pub inputs: Vec<AiTranscriptInputProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiTranscriptInputProjection {
    pub input_id: i64,
    pub position: i64,
    pub source_kind: AiInputSourceKind,
    pub video_id: Option<i64>,
    pub display_name: String,
    pub duration_ms: Option<u64>,
    pub project_offset_ms: Option<u64>,
    pub status: AiInputStatus,
    pub progress_percent: u8,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    /// 失败、取消或无人声输入在项目逻辑时间轴上保留的空洞。
    pub gap_duration_ms: Option<u64>,
    pub segments: Vec<AiTranscriptSegmentProjection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AiTranscriptSegmentProjection {
    pub stable_segment_id: String,
    pub input_id: i64,
    pub video_id: Option<i64>,
    pub source_start_ms: u64,
    pub source_end_ms: u64,
    pub project_start_ms: Option<u64>,
    pub project_end_ms: Option<u64>,
    pub raw_text: String,
    pub normalized_text: String,
    pub confidence: Option<f32>,
}

impl AiTranscriptProjection {
    pub fn load(repository: &AiRepository, project_id: i64) -> Result<Self, AiProjectionError> {
        let detail = repository.get_project(project_id)?;
        let mut inputs = Vec::with_capacity(detail.inputs.len());
        for input in detail.inputs {
            let source_segments = repository.list_segments_for_input(input.id)?;
            let segments = source_segments
                .into_iter()
                .map(|segment| project_segment(&input, segment))
                .collect::<Vec<_>>();
            let gap_duration_ms = if segments.is_empty()
                && matches!(
                    input.status,
                    AiInputStatus::Failed | AiInputStatus::Cancelled | AiInputStatus::Skipped
                ) {
                input.duration_ms
            } else {
                None
            };
            inputs.push(AiTranscriptInputProjection {
                input_id: input.id,
                position: input.position,
                source_kind: input.source_kind,
                video_id: input.video_id,
                display_name: safe_display_name(&input.display_name),
                duration_ms: input.duration_ms,
                project_offset_ms: input.project_offset_ms,
                status: input.status,
                progress_percent: input.progress_percent,
                error_code: input.last_error_code,
                error_message: input.last_error_message,
                gap_duration_ms,
                segments,
            });
        }
        Ok(Self {
            project: detail.project,
            inputs,
        })
    }

    pub fn copy_input_text(&self, input_id: i64) -> Result<String, AiProjectionError> {
        let input = self
            .inputs
            .iter()
            .find(|input| input.input_id == input_id)
            .ok_or(AiProjectionError::InputNotFound)?;
        Ok(input
            .segments
            .iter()
            .map(|segment| segment.normalized_text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"))
    }

    pub fn copy_segment_text(&self, stable_segment_id: &str) -> Result<String, AiProjectionError> {
        self.inputs
            .iter()
            .flat_map(|input| &input.segments)
            .find(|segment| segment.stable_segment_id == stable_segment_id)
            .map(|segment| segment.normalized_text.clone())
            .ok_or(AiProjectionError::InputNotFound)
    }

    pub fn copy_project_text(&self) -> String {
        self.inputs
            .iter()
            .map(|input| {
                let text = input
                    .segments
                    .iter()
                    .map(|segment| segment.normalized_text.trim())
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() {
                    format!("【{}】\n（无可用转写）", input.display_name)
                } else {
                    format!("【{}】\n{text}", input.display_name)
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    pub fn to_txt(&self) -> String {
        let mut lines = vec![format!("项目：{}", self.project.name), String::new()];
        for input in &self.inputs {
            lines.push(format!(
                "=== 视频 {}：{} ===",
                input.position.saturating_add(1),
                input.display_name
            ));
            if input.segments.is_empty() {
                lines.push(format!(
                    "（无可用转写：{}）",
                    input
                        .error_message
                        .as_deref()
                        .unwrap_or("没有检测到有效文本")
                ));
            } else {
                lines.extend(input.segments.iter().map(|segment| {
                    format!(
                        "[{} - {}] {}",
                        format_timestamp(segment.source_start_ms),
                        format_timestamp(segment.source_end_ms),
                        segment.normalized_text.trim()
                    )
                }));
            }
            lines.push(String::new());
        }
        lines.join("\n")
    }

    pub fn to_json(&self) -> Result<String, AiProjectionError> {
        serde_json::to_string_pretty(self).map_err(|_| AiProjectionError::Serialization)
    }
}

fn project_segment(
    input: &super::AiProjectInput,
    segment: TranscriptSegment,
) -> AiTranscriptSegmentProjection {
    AiTranscriptSegmentProjection {
        stable_segment_id: segment.id,
        input_id: input.id,
        video_id: input.video_id,
        source_start_ms: segment.source_start_ms,
        source_end_ms: segment.source_end_ms,
        project_start_ms: input
            .project_offset_ms
            .map(|offset| offset.saturating_add(segment.source_start_ms)),
        project_end_ms: input
            .project_offset_ms
            .map(|offset| offset.saturating_add(segment.source_end_ms)),
        raw_text: segment.raw_text,
        normalized_text: segment.normalized_text,
        confidence: segment.confidence,
    }
}

fn safe_display_name(value: &str) -> String {
    value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("未命名视频")
        .replace(['\r', '\n'], " ")
}

fn format_timestamp(milliseconds: u64) -> String {
    let hours = milliseconds / 3_600_000;
    let minutes = (milliseconds / 60_000) % 60;
    let seconds = (milliseconds / 1_000) % 60;
    let millis = milliseconds % 1_000;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
    } else {
        format!("{minutes:02}:{seconds:02}.{millis:03}")
    }
}

#[cfg(test)]
mod tests {
    use super::{format_timestamp, safe_display_name};

    #[test]
    fn timestamp_and_display_name_are_portable() {
        assert_eq!(format_timestamp(61_234), "01:01.234");
        assert_eq!(format_timestamp(3_661_234), "01:01:01.234");
        assert_eq!(safe_display_name("C:\\用户\\主播\\视频.mp4"), "视频.mp4");
        assert_eq!(safe_display_name("/Users/test/视频.mp4"), "视频.mp4");
    }
}
