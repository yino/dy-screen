//! 把独立视频的不可变句段装配为可回溯的项目逻辑时间轴。
//!
//! 装配只计算偏移、空洞和有限边界去重，不拼接或修改任何媒体。

/// 单个项目输入的来源。只有同一录像会话的相邻分片允许边界去重。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptInputSource {
    External,
    RecordingSession { session_id: i64 },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptSourceSegment {
    pub id: String,
    pub source_start_ms: u64,
    pub source_end_ms: u64,
    pub raw_text: String,
    pub normalized_text: String,
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptInput {
    pub input_id: i64,
    pub video_id: Option<i64>,
    pub source: TranscriptInputSource,
    pub duration_ms: u64,
    pub completed: bool,
    pub segments: Vec<TranscriptSourceSegment>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectTranscriptSegment {
    pub id: String,
    pub input_id: i64,
    pub video_id: Option<i64>,
    pub source_start_ms: u64,
    pub source_end_ms: u64,
    pub project_start_ms: u64,
    pub project_end_ms: u64,
    pub raw_text: String,
    pub normalized_text: String,
    pub project_text: String,
    pub deduplicated_prefix_chars: usize,
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTimelineGap {
    pub input_id: i64,
    pub project_start_ms: u64,
    pub project_end_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssembledTranscript {
    pub duration_ms: u64,
    pub segments: Vec<ProjectTranscriptSegment>,
    pub gaps: Vec<ProjectTimelineGap>,
}

/// 把独立视频的不可变句段映射到项目逻辑时间，不创建任何合并媒体。
pub struct TranscriptAssembler {
    maximum_boundary_overlap_chars: usize,
    minimum_boundary_overlap_chars: usize,
}

impl Default for TranscriptAssembler {
    fn default() -> Self {
        Self {
            maximum_boundary_overlap_chars: 200,
            minimum_boundary_overlap_chars: 4,
        }
    }
}

impl TranscriptAssembler {
    pub fn assemble(&self, inputs: &[TranscriptInput]) -> AssembledTranscript {
        let mut project_offset_ms = 0_u64;
        let mut output = AssembledTranscript {
            duration_ms: 0,
            segments: Vec::new(),
            gaps: Vec::new(),
        };
        let mut previous_source: Option<&TranscriptInputSource> = None;
        let mut previous_tail = String::new();

        for input in inputs {
            if !input.completed {
                output.gaps.push(ProjectTimelineGap {
                    input_id: input.input_id,
                    project_start_ms: project_offset_ms,
                    project_end_ms: project_offset_ms.saturating_add(input.duration_ms),
                });
                project_offset_ms = project_offset_ms.saturating_add(input.duration_ms);
                previous_source = Some(&input.source);
                previous_tail.clear();
                continue;
            }

            let shares_context = matches!(
                (previous_source, &input.source),
                (
                    Some(TranscriptInputSource::RecordingSession { session_id: previous }),
                    TranscriptInputSource::RecordingSession { session_id: current }
                ) if previous == current
            );
            for segment in &input.segments {
                let deduplicated_prefix_chars = if shares_context {
                    bounded_overlap_chars(
                        &previous_tail,
                        &segment.normalized_text,
                        self.minimum_boundary_overlap_chars,
                        self.maximum_boundary_overlap_chars,
                    )
                } else {
                    0
                };
                let project_text = segment
                    .normalized_text
                    .chars()
                    .skip(deduplicated_prefix_chars)
                    .collect::<String>()
                    .trim_start_matches(['，', '。', '！', '？', ',', '.', '!', '?', ' '])
                    .to_owned();
                output.segments.push(ProjectTranscriptSegment {
                    id: segment.id.clone(),
                    input_id: input.input_id,
                    video_id: input.video_id,
                    source_start_ms: segment.source_start_ms,
                    source_end_ms: segment.source_end_ms,
                    project_start_ms: project_offset_ms.saturating_add(segment.source_start_ms),
                    project_end_ms: project_offset_ms.saturating_add(segment.source_end_ms),
                    raw_text: segment.raw_text.clone(),
                    normalized_text: segment.normalized_text.clone(),
                    project_text,
                    deduplicated_prefix_chars,
                    confidence: segment.confidence,
                });
            }
            previous_tail = input
                .segments
                .iter()
                .map(|segment| segment.normalized_text.as_str())
                .collect::<String>()
                .chars()
                .rev()
                .take(self.maximum_boundary_overlap_chars)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            previous_source = Some(&input.source);
            project_offset_ms = project_offset_ms.saturating_add(input.duration_ms);
        }
        output.duration_ms = project_offset_ms;
        output
    }
}

fn bounded_overlap_chars(previous: &str, current: &str, minimum: usize, maximum: usize) -> usize {
    let previous: Vec<char> = previous.chars().collect();
    let current: Vec<char> = current.chars().collect();
    let limit = previous.len().min(current.len()).min(maximum);
    for overlap in (minimum..=limit).rev() {
        if previous[previous.len() - overlap..] == current[..overlap] {
            return overlap;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::{
        TranscriptAssembler, TranscriptInput, TranscriptInputSource, TranscriptSourceSegment,
    };

    fn segment(id: &str, start: u64, end: u64, text: &str) -> TranscriptSourceSegment {
        TranscriptSourceSegment {
            id: id.to_owned(),
            source_start_ms: start,
            source_end_ms: end,
            raw_text: text.to_owned(),
            normalized_text: text.to_owned(),
            confidence: None,
        }
    }

    #[test]
    fn maps_source_and_project_time_with_failed_input_gap() {
        let output = TranscriptAssembler::default().assemble(&[
            TranscriptInput {
                input_id: 1,
                video_id: Some(11),
                source: TranscriptInputSource::External,
                duration_ms: 4_000,
                completed: true,
                segments: vec![segment("seg-a", 100, 1_000, "第一段")],
            },
            TranscriptInput {
                input_id: 2,
                video_id: Some(12),
                source: TranscriptInputSource::External,
                duration_ms: 3_000,
                completed: false,
                segments: Vec::new(),
            },
            TranscriptInput {
                input_id: 3,
                video_id: None,
                source: TranscriptInputSource::External,
                duration_ms: 2_000,
                completed: true,
                segments: vec![segment("seg-c", 200, 1_500, "第三段")],
            },
        ]);
        assert_eq!(output.duration_ms, 9_000);
        assert_eq!(output.segments[0].project_start_ms, 100);
        assert_eq!(output.gaps[0].project_start_ms, 4_000);
        assert_eq!(output.gaps[0].project_end_ms, 7_000);
        assert_eq!(output.segments[1].project_start_ms, 7_200);
        assert_eq!(output.segments[1].id, "seg-c");
    }

    #[test]
    fn deduplicates_only_adjacent_segments_from_same_recording_session() {
        let same_session = TranscriptAssembler::default().assemble(&[
            TranscriptInput {
                input_id: 1,
                video_id: Some(11),
                source: TranscriptInputSource::RecordingSession { session_id: 7 },
                duration_ms: 4_000,
                completed: true,
                segments: vec![segment("seg-a", 0, 4_000, "现在开始介绍这款商品")],
            },
            TranscriptInput {
                input_id: 2,
                video_id: Some(12),
                source: TranscriptInputSource::RecordingSession { session_id: 7 },
                duration_ms: 4_000,
                completed: true,
                segments: vec![segment("seg-b", 0, 4_000, "现在开始介绍这款商品，库存不多")],
            },
        ]);
        assert_eq!(same_session.segments[1].project_text, "库存不多");
        assert!(same_session.segments[1].deduplicated_prefix_chars > 0);
        assert_eq!(
            same_session.segments[1].normalized_text,
            "现在开始介绍这款商品，库存不多"
        );

        let external = TranscriptAssembler::default().assemble(&[
            TranscriptInput {
                input_id: 1,
                video_id: None,
                source: TranscriptInputSource::External,
                duration_ms: 4_000,
                completed: true,
                segments: vec![segment("seg-a", 0, 4_000, "重复文本")],
            },
            TranscriptInput {
                input_id: 2,
                video_id: None,
                source: TranscriptInputSource::External,
                duration_ms: 4_000,
                completed: true,
                segments: vec![segment("seg-b", 0, 4_000, "重复文本")],
            },
        ]);
        assert_eq!(external.segments[1].project_text, "重复文本");
        assert_eq!(external.segments[1].deduplicated_prefix_chars, 0);
    }
}
