//! ASR 真实样本质量验收的数据契约与指标计算。
//!
//! 本模块刻意不启动 `whisper.cpp`，也不依赖 Tauri。采集器只需把任意
//! [`AsrEngine`](super::AsrEngine) 的结果投影为 [`AsrQualityOutput`]，即可复用同一套
//! 商品名、金额、主播名、时间戳、静音幻觉和实时因子指标。这样后续增加云端 Adapter 时
//! 不需要复制或修改质量口径。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AsrQualityError {
    #[error("质量数据集无效：{0}")]
    InvalidDataset(String),
    #[error("无法读取质量验收文件")]
    Io(#[from] std::io::Error),
    #[error("质量验收 JSON 格式无效")]
    Json(#[from] serde_json::Error),
}

/// 一次质量验收的数据集。视频路径只在本机采集阶段使用，不会写入结果报告。

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AsrQualityDataset {
    pub schema_version: u32,
    pub dataset_id: String,
    pub samples: Vec<AsrQualitySample>,
}

/// 一个经过授权的真实样本及其人工参考标注。

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AsrQualitySample {
    pub id: String,
    pub authorization_reference: String,
    pub video: PathBuf,
    pub duration_ms: u64,
    #[serde(default)]
    pub speech_regions: Vec<AsrReferenceRegion>,
    #[serde(default)]
    pub terms: AsrQualityTerms,
    #[serde(default)]
    pub anchors: Vec<AsrQualityAnchor>,
}

/// 按业务重要性分组的参考词。

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AsrQualityTerms {
    #[serde(default)]
    pub products: Vec<AsrQualityTerm>,
    #[serde(default)]
    pub amounts: Vec<AsrQualityTerm>,
    #[serde(default)]
    pub streamers: Vec<AsrQualityTerm>,
}

/// 一个参考词及 ASR 可接受的等价写法。

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AsrQualityTerm {
    pub label: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// 用于度量句段起始时间误差的人工锚点。

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AsrQualityAnchor {
    pub label: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub start_ms: u64,
    pub tolerance_ms: u64,
}

/// 人工标注的人声区间；区间必须按时间排序且互不重叠。

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AsrReferenceRegion {
    pub start_ms: u64,
    pub end_ms: u64,
}

/// 完整质量报告。所有比率均使用 `0.0..=1.0`。

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AsrQualityReport {
    pub schema_version: u32,
    pub dataset_id: String,
    pub dataset_sha256: String,
    pub sample_count: usize,
    pub successful_samples: usize,
    pub failed_samples: usize,
    pub failure_rate: f64,
    pub products: RecallMetrics,
    pub amounts: RecallMetrics,
    pub streamers: RecallMetrics,
    pub timestamps: TimestampMetrics,
    pub silence: SilenceMetrics,
    pub performance: PerformanceMetrics,
    pub samples: Vec<SampleQualityReport>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecallMetrics {
    pub expected: usize,
    pub matched: usize,
    pub recall: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimestampMetrics {
    pub expected: usize,
    pub matched: usize,
    pub within_tolerance: usize,
    pub mean_absolute_error_ms: Option<f64>,
    pub p95_absolute_error_ms: Option<u64>,
    pub within_tolerance_rate: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SilenceMetrics {
    pub transcribed_segments: usize,
    pub hallucinated_segments: usize,
    pub segment_hallucination_rate: Option<f64>,
    pub reference_silence_ms: u64,
    pub hallucinated_segment_ms: u64,
    pub duration_hallucination_rate: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PerformanceMetrics {
    pub measured_samples: usize,
    pub mean_realtime_factor: Option<f64>,
    pub maximum_realtime_factor: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleQualityReport {
    pub id: String,
    pub status: &'static str,
    pub failure_code: Option<String>,
    pub product_expected: usize,
    pub product_matched: usize,
    pub amount_expected: usize,
    pub amount_matched: usize,
    pub streamer_expected: usize,
    pub streamer_matched: usize,
    pub anchor_expected: usize,
    pub anchor_matched: usize,
    pub mean_timestamp_error_ms: Option<f64>,
    pub hallucinated_segments: usize,
    pub realtime_factor: Option<f64>,
}

/// 质量采集器保存的脱敏 ASR 输出。
///
/// `video` 只用于兼容 `dy-screen asr --json` 的原始 stdout，序列化时会被丢弃，避免
/// 把用户本地绝对路径写入验收证据。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AsrQualityOutput {
    #[serde(default, skip_serializing)]
    pub video: Option<String>,
    pub engine_id: String,
    pub engine_version: String,
    pub model_id: String,
    pub model_version: String,
    pub language: Option<String>,
    pub duration_ms: u64,
    #[serde(default)]
    pub segments: Vec<AsrQualitySegment>,
    #[serde(default)]
    pub text: String,
}

/// 质量评测所需的最小句段投影。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AsrQualitySegment {
    pub start_ms: u64,
    pub end_ms: u64,
    #[serde(default)]
    pub raw_text: String,
    #[serde(default)]
    pub normalized_text: String,
    #[serde(default)]
    pub confidence: Option<f32>,
}

/// 单个样本的可审计采集证据。
///
/// 只保存哈希、大小、状态和耗时，不保存命令行、stderr 原文或本地路径。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AsrQualityCollectionEvidence {
    pub schema_version: u32,
    pub dataset_sha256: String,
    pub sample_id: String,
    pub exit_code: Option<i32>,
    pub wall_ms: u64,
    pub source_size_bytes_before: u64,
    pub source_size_bytes_after: Option<u64>,
    pub source_sha256_before: String,
    pub source_sha256_after: Option<String>,
    pub source_unchanged: bool,
    pub asr_binary_sha256: String,
    pub resource_manifest_sha256: Option<String>,
    pub stdout_valid_json: bool,
    pub stdout_size_bytes: u64,
    pub stdout_sha256: String,
    pub result_size_bytes: Option<u64>,
    pub result_sha256: Option<String>,
    pub stderr_size_bytes: u64,
    pub stderr_sha256: String,
}

impl AsrQualityDataset {
    pub fn from_path(path: &Path) -> Result<Self, AsrQualityError> {
        let dataset: Self = serde_json::from_slice(&fs::read(path)?)?;
        Ok(dataset)
    }

    pub fn validate(&self, minimum_samples: usize) -> Result<(), AsrQualityError> {
        if self.schema_version != 1
            || !valid_sample_id(&self.dataset_id)
            || self.samples.len() < minimum_samples
        {
            return Err(AsrQualityError::InvalidDataset(format!(
                "需要合法 datasetId、至少 {minimum_samples} 个样本且 schemaVersion 必须为 1"
            )));
        }
        let mut ids = HashSet::new();
        let mut videos = HashSet::new();
        let mut product_terms = 0_usize;
        let mut amount_terms = 0_usize;
        let mut streamer_terms = 0_usize;
        let mut anchors = 0_usize;
        let mut reference_silence = 0_u64;
        for sample in &self.samples {
            if !valid_sample_id(&sample.id)
                || !ids.insert(sample.id.as_str())
                || !videos.insert(&sample.video)
                || sample.authorization_reference.trim().is_empty()
                || sample
                    .authorization_reference
                    .trim()
                    .to_ascii_lowercase()
                    .starts_with("replace-with-")
                || sample.authorization_reference.len() > 256
                || sample.authorization_reference.contains(['\r', '\n'])
                || sample.video.as_os_str().is_empty()
                || sample.duration_ms == 0
            {
                return Err(AsrQualityError::InvalidDataset(format!(
                    "样本 {} 的 ID、视频或时长无效",
                    sample.id
                )));
            }
            validate_regions(sample)?;
            product_terms += sample.terms.products.len();
            amount_terms += sample.terms.amounts.len();
            streamer_terms += sample.terms.streamers.len();
            anchors += sample.anchors.len();
            reference_silence = reference_silence.saturating_add(reference_silence_ms(sample));
            for term in sample
                .terms
                .products
                .iter()
                .chain(&sample.terms.amounts)
                .chain(&sample.terms.streamers)
            {
                validate_term(&sample.id, &term.label, &term.aliases)?;
            }
            for anchor in &sample.anchors {
                validate_term(&sample.id, &anchor.label, &anchor.aliases)?;
                if anchor.start_ms >= sample.duration_ms || anchor.tolerance_ms == 0 {
                    return Err(AsrQualityError::InvalidDataset(format!(
                        "样本 {} 的时间锚点无效",
                        sample.id
                    )));
                }
            }
        }
        if product_terms == 0
            || amount_terms == 0
            || streamer_terms == 0
            || anchors == 0
            || reference_silence == 0
        {
            return Err(AsrQualityError::InvalidDataset(
                "数据集必须包含商品、金额、主播、时间锚点和静音区间参考标注".to_owned(),
            ));
        }
        Ok(())
    }
}

pub fn evaluate_quality_results(
    dataset: &AsrQualityDataset,
    dataset_sha256: &str,
    results_root: &Path,
    minimum_samples: usize,
) -> Result<AsrQualityReport, AsrQualityError> {
    dataset.validate(minimum_samples)?;
    if !valid_sha256(dataset_sha256) {
        return Err(AsrQualityError::InvalidDataset(
            "数据集 SHA-256 无效".to_owned(),
        ));
    }
    let mut report = AsrQualityReport {
        schema_version: 1,
        dataset_id: dataset.dataset_id.clone(),
        dataset_sha256: dataset_sha256.to_owned(),
        sample_count: dataset.samples.len(),
        successful_samples: 0,
        failed_samples: 0,
        failure_rate: 0.0,
        products: RecallMetrics::default(),
        amounts: RecallMetrics::default(),
        streamers: RecallMetrics::default(),
        timestamps: TimestampMetrics::default(),
        silence: SilenceMetrics::default(),
        performance: PerformanceMetrics::default(),
        samples: Vec::with_capacity(dataset.samples.len()),
    };
    let mut timestamp_errors = Vec::new();
    let mut realtime_factors = Vec::new();

    for sample in &dataset.samples {
        let output_path = results_root.join(format!("{}.json", sample.id));
        let evidence_path = results_root.join(format!("{}.evidence.json", sample.id));
        let evidence = fs::read(&evidence_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<AsrQualityCollectionEvidence>(&bytes).ok());
        let output_bytes = fs::read(&output_path).ok();
        let output = output_bytes
            .as_deref()
            .and_then(|bytes| serde_json::from_slice::<AsrQualityOutput>(bytes).ok());
        let failure_code = sample_failure(
            sample,
            dataset_sha256,
            output_bytes.as_deref(),
            output.as_ref(),
            evidence.as_ref(),
        );
        if let Some(code) = failure_code {
            report.failed_samples += 1;
            report.products.expected += sample.terms.products.len();
            report.amounts.expected += sample.terms.amounts.len();
            report.streamers.expected += sample.terms.streamers.len();
            report.timestamps.expected += sample.anchors.len();
            report.samples.push(SampleQualityReport {
                id: sample.id.clone(),
                status: "failed",
                failure_code: Some(code),
                product_expected: sample.terms.products.len(),
                product_matched: 0,
                amount_expected: sample.terms.amounts.len(),
                amount_matched: 0,
                streamer_expected: sample.terms.streamers.len(),
                streamer_matched: 0,
                anchor_expected: sample.anchors.len(),
                anchor_matched: 0,
                mean_timestamp_error_ms: None,
                hallucinated_segments: 0,
                realtime_factor: None,
            });
            continue;
        }

        let output = output.expect("successful sample must have parsed output");
        report.successful_samples += 1;
        let full_text = if output.text.trim().is_empty() {
            output
                .segments
                .iter()
                .map(|segment| segment.normalized_text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            output.text.clone()
        };
        let product_matched = matched_terms(&sample.terms.products, &full_text);
        let amount_matched = matched_terms(&sample.terms.amounts, &full_text);
        let streamer_matched = matched_terms(&sample.terms.streamers, &full_text);
        update_recall(
            &mut report.products,
            sample.terms.products.len(),
            product_matched,
        );
        update_recall(
            &mut report.amounts,
            sample.terms.amounts.len(),
            amount_matched,
        );
        update_recall(
            &mut report.streamers,
            sample.terms.streamers.len(),
            streamer_matched,
        );

        let sample_errors = anchor_errors(sample, &output.segments);
        report.timestamps.expected += sample.anchors.len();
        report.timestamps.matched += sample_errors.len();
        report.timestamps.within_tolerance += sample_errors
            .iter()
            .filter(|(error, tolerance)| error <= tolerance)
            .count();
        timestamp_errors.extend(sample_errors.iter().map(|(error, _)| *error));

        let (hallucinated_segments, hallucinated_ms) =
            silence_hallucinations(sample, &output.segments);
        report.silence.transcribed_segments += output.segments.len();
        report.silence.hallucinated_segments += hallucinated_segments;
        report.silence.hallucinated_segment_ms = report
            .silence
            .hallucinated_segment_ms
            .saturating_add(hallucinated_ms);
        report.silence.reference_silence_ms = report
            .silence
            .reference_silence_ms
            .saturating_add(reference_silence_ms(sample));

        let realtime_factor = evidence.and_then(|evidence| {
            (output.duration_ms > 0).then_some(evidence.wall_ms as f64 / output.duration_ms as f64)
        });
        if let Some(value) = realtime_factor {
            realtime_factors.push(value);
        }
        report.samples.push(SampleQualityReport {
            id: sample.id.clone(),
            status: "ok",
            failure_code: None,
            product_expected: sample.terms.products.len(),
            product_matched,
            amount_expected: sample.terms.amounts.len(),
            amount_matched,
            streamer_expected: sample.terms.streamers.len(),
            streamer_matched,
            anchor_expected: sample.anchors.len(),
            anchor_matched: sample_errors.len(),
            mean_timestamp_error_ms: mean_u64(sample_errors.iter().map(|(error, _)| *error)),
            hallucinated_segments,
            realtime_factor,
        });
    }

    report.failure_rate = ratio(report.failed_samples, report.sample_count).unwrap_or(0.0);
    finalize_recall(&mut report.products);
    finalize_recall(&mut report.amounts);
    finalize_recall(&mut report.streamers);
    report.timestamps.mean_absolute_error_ms = mean_u64(timestamp_errors.iter().copied());
    report.timestamps.p95_absolute_error_ms = percentile_95(&mut timestamp_errors);
    report.timestamps.within_tolerance_rate = ratio(
        report.timestamps.within_tolerance,
        report.timestamps.expected,
    );
    report.silence.segment_hallucination_rate = ratio(
        report.silence.hallucinated_segments,
        report.silence.transcribed_segments,
    );
    report.silence.duration_hallucination_rate = ratio_u64(
        report.silence.hallucinated_segment_ms,
        report.silence.reference_silence_ms,
    );
    report.performance.measured_samples = realtime_factors.len();
    report.performance.mean_realtime_factor = mean_f64(&realtime_factors);
    report.performance.maximum_realtime_factor = realtime_factors.iter().copied().reduce(f64::max);
    Ok(report)
}

pub fn render_quality_markdown(report: &AsrQualityReport) -> String {
    let mut markdown = String::from("# ASR 中文直播质量验收\n\n");
    markdown.push_str(&format!("数据集：`{}`\n\n", report.dataset_id));
    markdown.push_str("| 指标 | 数值 |\n| --- | ---: |\n");
    markdown.push_str(&format!("| 样本数 | {} |\n", report.sample_count));
    markdown.push_str(&format!(
        "| 失败率 | {:.2}% |\n",
        report.failure_rate * 100.0
    ));
    markdown.push_str(&format!(
        "| 商品名召回率 | {} |\n",
        format_percent(report.products.recall)
    ));
    markdown.push_str(&format!(
        "| 金额召回率 | {} |\n",
        format_percent(report.amounts.recall)
    ));
    markdown.push_str(&format!(
        "| 主播名召回率 | {} |\n",
        format_percent(report.streamers.recall)
    ));
    markdown.push_str(&format!(
        "| 平均时间误差 | {} |\n",
        report
            .timestamps
            .mean_absolute_error_ms
            .map(|value| format!("{value:.1} ms"))
            .unwrap_or_else(|| "N/A".to_owned())
    ));
    markdown.push_str(&format!(
        "| 静音句段幻觉率 | {} |\n",
        format_percent(report.silence.segment_hallucination_rate)
    ));
    markdown.push_str(&format!(
        "| 平均实时因子 | {} |\n\n",
        report
            .performance
            .mean_realtime_factor
            .map(|value| format!("{value:.3}"))
            .unwrap_or_else(|| "N/A".to_owned())
    ));
    markdown.push_str("## 分样本结果\n\n");
    markdown.push_str("| 样本 | 状态 | 商品 | 金额 | 主播 | 时间锚点 | 静音幻觉句段 | RTF |\n");
    markdown.push_str("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for sample in &report.samples {
        markdown.push_str(&format!(
            "| {} | {} | {}/{} | {}/{} | {}/{} | {}/{} | {} | {} |\n",
            sample.id,
            sample.status,
            sample.product_matched,
            sample.product_expected,
            sample.amount_matched,
            sample.amount_expected,
            sample.streamer_matched,
            sample.streamer_expected,
            sample.anchor_matched,
            sample.anchor_expected,
            sample.hallucinated_segments,
            sample
                .realtime_factor
                .map(|value| format!("{value:.3}"))
                .unwrap_or_else(|| "N/A".to_owned())
        ));
    }
    markdown
}

fn sample_failure(
    sample: &AsrQualitySample,
    dataset_sha256: &str,
    output_bytes: Option<&[u8]>,
    output: Option<&AsrQualityOutput>,
    evidence: Option<&AsrQualityCollectionEvidence>,
) -> Option<String> {
    let evidence = match evidence {
        Some(evidence)
            if evidence.schema_version == 1
                && evidence.dataset_sha256 == dataset_sha256
                && evidence.sample_id == sample.id
                && valid_sha256(&evidence.source_sha256_before)
                && evidence
                    .source_sha256_after
                    .as_deref()
                    .is_none_or(valid_sha256)
                && valid_sha256(&evidence.asr_binary_sha256)
                && evidence
                    .resource_manifest_sha256
                    .as_deref()
                    .is_none_or(valid_sha256)
                && valid_sha256(&evidence.stdout_sha256)
                && evidence.result_sha256.as_deref().is_none_or(valid_sha256)
                && valid_sha256(&evidence.stderr_sha256) =>
        {
            evidence
        }
        _ => return Some("evidence_missing_or_invalid".to_owned()),
    };
    if evidence.exit_code != Some(0) {
        return Some("asr_process_failed".to_owned());
    }
    if !evidence.source_unchanged
        || evidence.source_size_bytes_after != Some(evidence.source_size_bytes_before)
        || evidence.source_sha256_after.as_deref() != Some(evidence.source_sha256_before.as_str())
    {
        return Some("source_modified".to_owned());
    }
    if !evidence.stdout_valid_json {
        return Some("result_missing_or_invalid".to_owned());
    }
    let Some(output_bytes) = output_bytes else {
        return Some("result_missing_or_invalid".to_owned());
    };
    let output_digest = sha256_bytes(output_bytes);
    if evidence.result_size_bytes != Some(output_bytes.len() as u64)
        || evidence.result_sha256.as_deref() != Some(output_digest.as_str())
    {
        return Some("result_integrity_mismatch".to_owned());
    }
    let Some(output) = output else {
        return Some("result_missing_or_invalid".to_owned());
    };
    if output.engine_id.trim().is_empty()
        || output.engine_version.trim().is_empty()
        || output.model_id.trim().is_empty()
        || output.model_version.trim().is_empty()
    {
        return Some("result_identity_missing".to_owned());
    }
    let tolerance = 1_000_u64.max(sample.duration_ms / 50);
    if output.duration_ms.abs_diff(sample.duration_ms) > tolerance
        || output.segments.iter().any(|segment| {
            segment.end_ms <= segment.start_ms || segment.end_ms > output.duration_ms
        })
    {
        return Some("invalid_timeline".to_owned());
    }
    None
}

fn matched_terms(terms: &[AsrQualityTerm], full_text: &str) -> usize {
    let searchable = searchable_text(full_text);
    terms
        .iter()
        .filter(|term| aliases(&term.label, &term.aliases).any(|alias| searchable.contains(&alias)))
        .count()
}

fn anchor_errors(sample: &AsrQualitySample, segments: &[AsrQualitySegment]) -> Vec<(u64, u64)> {
    sample
        .anchors
        .iter()
        .filter_map(|anchor| {
            let aliases = aliases(&anchor.label, &anchor.aliases).collect::<Vec<_>>();
            segments
                .iter()
                .filter(|segment| {
                    let text = searchable_text(&segment.normalized_text);
                    aliases.iter().any(|alias| text.contains(alias))
                })
                .map(|segment| segment.start_ms.abs_diff(anchor.start_ms))
                .min()
                .map(|error| (error, anchor.tolerance_ms))
        })
        .collect()
}

fn silence_hallucinations(
    sample: &AsrQualitySample,
    segments: &[AsrQualitySegment],
) -> (usize, u64) {
    let hallucinated = segments.iter().filter(|segment| {
        !segment.normalized_text.trim().is_empty()
            && !sample
                .speech_regions
                .iter()
                .any(|region| segment.start_ms < region.end_ms && segment.end_ms > region.start_ms)
    });
    let mut count = 0;
    let mut duration = 0_u64;
    for segment in hallucinated {
        count += 1;
        duration = duration.saturating_add(segment.end_ms.saturating_sub(segment.start_ms));
    }
    (count, duration)
}

fn reference_silence_ms(sample: &AsrQualitySample) -> u64 {
    let speech = sample.speech_regions.iter().fold(0_u64, |total, region| {
        total.saturating_add(region.end_ms - region.start_ms)
    });
    sample.duration_ms.saturating_sub(speech)
}

fn validate_regions(sample: &AsrQualitySample) -> Result<(), AsrQualityError> {
    let mut previous_end = 0;
    for region in &sample.speech_regions {
        if region.end_ms <= region.start_ms
            || region.end_ms > sample.duration_ms
            || region.start_ms < previous_end
        {
            return Err(AsrQualityError::InvalidDataset(format!(
                "样本 {} 的人声区间无效或重叠",
                sample.id
            )));
        }
        previous_end = region.end_ms;
    }
    Ok(())
}

fn validate_term(id: &str, label: &str, aliases: &[String]) -> Result<(), AsrQualityError> {
    if searchable_text(label).is_empty()
        || aliases
            .iter()
            .any(|alias| searchable_text(alias).is_empty())
    {
        return Err(AsrQualityError::InvalidDataset(format!(
            "样本 {id} 包含空白术语或别名"
        )));
    }
    Ok(())
}

fn aliases<'a>(label: &'a str, aliases: &'a [String]) -> impl Iterator<Item = String> + 'a {
    std::iter::once(label)
        .chain(aliases.iter().map(String::as_str))
        .map(searchable_text)
}

fn searchable_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn valid_sample_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.chars().next().is_some_and(char::is_alphanumeric)
        && id.chars().next_back().is_some_and(char::is_alphanumeric)
        && id
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_' | '.'))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn update_recall(metrics: &mut RecallMetrics, expected: usize, matched: usize) {
    metrics.expected += expected;
    metrics.matched += matched;
}

fn finalize_recall(metrics: &mut RecallMetrics) {
    metrics.recall = ratio(metrics.matched, metrics.expected);
}

fn ratio(numerator: usize, denominator: usize) -> Option<f64> {
    (denominator > 0).then_some(numerator as f64 / denominator as f64)
}

fn ratio_u64(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator > 0).then_some(numerator as f64 / denominator as f64)
}

fn mean_u64(values: impl Iterator<Item = u64>) -> Option<f64> {
    let (sum, count) = values.fold((0_u128, 0_u128), |(sum, count), value| {
        (sum + u128::from(value), count + 1)
    });
    (count > 0).then_some(sum as f64 / count as f64)
}

fn mean_f64(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

fn percentile_95(values: &mut [u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let index = (values.len() * 95).div_ceil(100).saturating_sub(1);
    values.get(index).copied()
}

fn format_percent(value: Option<f64>) -> String {
    value
        .map(|value| format!("{:.2}%", value * 100.0))
        .unwrap_or_else(|| "N/A".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn dataset() -> AsrQualityDataset {
        AsrQualityDataset {
            schema_version: 1,
            dataset_id: "authorized-zh-live-v1".to_owned(),
            samples: vec![AsrQualitySample {
                id: "sample-01".to_owned(),
                authorization_reference: "consent-ticket-001".to_owned(),
                video: "sample-01.mp4".into(),
                duration_ms: 10_000,
                speech_regions: vec![AsrReferenceRegion {
                    start_ms: 1_000,
                    end_ms: 5_000,
                }],
                terms: AsrQualityTerms {
                    products: vec![AsrQualityTerm {
                        label: "测试商品".to_owned(),
                        aliases: Vec::new(),
                    }],
                    amounts: vec![AsrQualityTerm {
                        label: "99元".to_owned(),
                        aliases: vec!["九十九元".to_owned()],
                    }],
                    streamers: vec![AsrQualityTerm {
                        label: "小明".to_owned(),
                        aliases: Vec::new(),
                    }],
                },
                anchors: vec![AsrQualityAnchor {
                    label: "测试商品".to_owned(),
                    aliases: Vec::new(),
                    start_ms: 1_200,
                    tolerance_ms: 500,
                }],
            }],
        }
    }

    fn evidence() -> AsrQualityCollectionEvidence {
        AsrQualityCollectionEvidence {
            schema_version: 1,
            dataset_sha256: "f".repeat(64),
            sample_id: "sample-01".to_owned(),
            exit_code: Some(0),
            wall_ms: 5_000,
            source_size_bytes_before: 42,
            source_size_bytes_after: Some(42),
            source_sha256_before: "a".repeat(64),
            source_sha256_after: Some("a".repeat(64)),
            source_unchanged: true,
            asr_binary_sha256: "c".repeat(64),
            resource_manifest_sha256: Some("d".repeat(64)),
            stdout_valid_json: true,
            stdout_size_bytes: 100,
            stdout_sha256: "b".repeat(64),
            result_size_bytes: Some(100),
            result_sha256: Some("b".repeat(64)),
            stderr_size_bytes: 0,
            stderr_sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                .to_owned(),
        }
    }

    #[test]
    fn evaluates_recall_timestamps_silence_failure_and_rtf() {
        let root = tempdir().unwrap();
        let output = serde_json::to_vec(&serde_json::json!({
                "engineId": "whisper.cpp",
                "engineVersion": "1.9.1",
                "modelId": "whisper-small-multilingual-q5_1",
                "modelVersion": "v3-turbo",
                "language": "zh",
                "durationMs": 10_000,
                "text": "小明介绍测试商品，价格九十九元",
                "segments": [
                    {"startMs": 1_300, "endMs": 4_800, "normalizedText": "小明介绍测试商品，价格九十九元"},
                    {"startMs": 7_000, "endMs": 7_500, "normalizedText": "虚假文本"}
                ]
            }))
            .unwrap();
        fs::write(root.path().join("sample-01.json"), &output).unwrap();
        let mut evidence = evidence();
        evidence.result_size_bytes = Some(output.len() as u64);
        evidence.result_sha256 = Some(sha256_bytes(&output));
        fs::write(
            root.path().join("sample-01.evidence.json"),
            serde_json::to_vec(&evidence).unwrap(),
        )
        .unwrap();

        let report = evaluate_quality_results(&dataset(), &"f".repeat(64), root.path(), 1).unwrap();
        assert_eq!(report.successful_samples, 1);
        assert_eq!(report.products.recall, Some(1.0));
        assert_eq!(report.amounts.recall, Some(1.0));
        assert_eq!(report.streamers.recall, Some(1.0));
        assert_eq!(report.timestamps.mean_absolute_error_ms, Some(100.0));
        assert_eq!(report.silence.hallucinated_segments, 1);
        assert_eq!(report.performance.mean_realtime_factor, Some(0.5));
        assert!(render_quality_markdown(&report).contains("商品名召回率"));

        let mut tampered = output;
        tampered.push(b' ');
        fs::write(root.path().join("sample-01.json"), tampered).unwrap();
        let tampered_report =
            evaluate_quality_results(&dataset(), &"f".repeat(64), root.path(), 1).unwrap();
        assert_eq!(
            tampered_report.samples[0].failure_code.as_deref(),
            Some("result_integrity_mismatch")
        );
    }

    #[test]
    fn rejects_small_duplicate_or_overlapping_datasets() {
        let mut value = dataset();
        assert!(value.validate(10).is_err());
        value.samples.push(value.samples[0].clone());
        assert!(value.validate(1).is_err());
        value.samples.pop();
        let mut duplicate_source = value.samples[0].clone();
        duplicate_source.id = "sample-02".to_owned();
        duplicate_source.authorization_reference = "consent-ticket-002".to_owned();
        value.samples.push(duplicate_source);
        assert!(value.validate(1).is_err());
        value.samples.pop();
        value.samples[0].speech_regions.push(AsrReferenceRegion {
            start_ms: 4_000,
            end_ms: 6_000,
        });
        assert!(value.validate(1).is_err());
    }

    #[test]
    fn missing_or_invalid_collection_files_are_failures_instead_of_panics() {
        let root = tempdir().unwrap();
        let report = evaluate_quality_results(&dataset(), &"f".repeat(64), root.path(), 1).unwrap();
        assert_eq!(report.failed_samples, 1);
        assert_eq!(
            report.samples[0].failure_code.as_deref(),
            Some("evidence_missing_or_invalid")
        );

        fs::write(
            root.path().join("sample-01.evidence.json"),
            serde_json::to_vec(&evidence()).unwrap(),
        )
        .unwrap();
        let report = evaluate_quality_results(&dataset(), &"f".repeat(64), root.path(), 1).unwrap();
        assert_eq!(
            report.samples[0].failure_code.as_deref(),
            Some("result_missing_or_invalid")
        );
    }

    #[test]
    fn integer_mean_does_not_overflow() {
        assert_eq!(
            mean_u64([u64::MAX, u64::MAX].into_iter()),
            Some(u64::MAX as f64)
        );
    }
}
