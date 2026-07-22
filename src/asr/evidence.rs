//! OpenSpec ASR 最终完成证据的离线、跨报告一致性审计。
//!
//! 单个脚本成功不足以证明 `83/83`：Windows 目标测试、双平台发行、双平台 8 GB 性能和
//! 十场直播质量报告必须同时存在，并且二进制、资源 manifest、数据集及样本证据哈希能够
//! 相互关联。本模块只输出哈希和布尔结论，不把输入文件路径或报告原文写入结果。

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::DateTime;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{
    AsrQualityCollectionEvidence, AsrQualityDataset, RecallMetrics, evaluate_quality_results,
    render_quality_markdown,
};

const MAX_EVIDENCE_BYTES: u64 = 16 * 1024 * 1024;
const COMPLETION_TASKS: [&str; 6] = ["1.6", "9.2", "9.4", "9.5", "9.8", "9.9"];

/// 最终证据审计需要的仓库外输入。
#[derive(Debug, Clone)]
pub struct AsrCompletionEvidencePaths {
    /// Windows x64 功能目标报告。
    pub windows_target_report: PathBuf,
    /// 正式 macOS 发行报告。
    pub macos_release_report: PathBuf,
    /// 正式 Windows 发行报告。
    pub windows_release_report: PathBuf,
    /// 严格 8 GB macOS arm64 性能报告。
    pub macos_performance_report: PathBuf,
    /// 严格 8 GB Windows x64 性能报告。
    pub windows_performance_report: PathBuf,
    /// 至少十场授权直播的质量数据集 JSON。
    pub quality_dataset: PathBuf,
    /// `asr-quality collect` 生成的不可变样本证据目录。
    pub quality_results: PathBuf,
    /// `asr-quality evaluate` 生成的 JSON 报告。
    pub quality_json_report: PathBuf,
    /// 与 JSON 报告同次生成的 Markdown 报告。
    pub quality_markdown_report: PathBuf,
}

/// 一个 OpenSpec 外部门禁的聚合状态。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AsrCompletionTaskStatus {
    /// OpenSpec `tasks.md` 中的任务编号。
    pub task_id: String,
    /// 与任务相关的所有检查是否均通过。
    pub ready: bool,
    /// 参与此任务判定的检查数量。
    pub check_count: usize,
}

/// 一项不会泄漏证据路径或原始日志的稳定检查结论。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AsrCompletionEvidenceCheck {
    /// 此检查约束的一个或多个 OpenSpec 任务。
    pub task_ids: Vec<String>,
    /// 供自动化和人工复核稳定匹配的代码。
    pub code: String,
    /// 检查是否通过。
    pub passed: bool,
    /// 不包含本地路径、凭据或报告原文的中文结论。
    pub message: String,
    /// 被检查报告本体的 SHA-256；跨报告检查没有单一文件时为 `None`。
    pub evidence_sha256: Option<String>,
}

/// 最终 `83/83` 的机器可读审计结果。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AsrCompletionEvidenceReport {
    /// 审计输出格式版本。
    pub schema_version: u32,
    /// 六个外部任务是否都具有完整且相互一致的真实证据。
    pub ready_to_complete: bool,
    /// 每个外部任务的聚合状态。
    pub tasks: Vec<AsrCompletionTaskStatus>,
    /// 全部原子检查，失败时可据此定位缺失证据类别。
    pub checks: Vec<AsrCompletionEvidenceCheck>,
}

struct LoadedJson {
    value: Value,
    sha256: String,
}

struct ReleaseLinks {
    asr_cli_sha256: String,
    manifest_sha256: String,
    runtime_sha256: Option<String>,
}

struct TargetLinks {
    manifest_sha256: String,
    runtime_sha256: String,
}

struct PerformanceLinks {
    asr_binary_sha256: String,
    manifest_sha256: String,
}

struct QualityLinks {
    asr_binary_sha256: String,
    manifest_sha256: String,
}

#[derive(Default)]
struct Auditor {
    checks: Vec<AsrCompletionEvidenceCheck>,
}

impl Auditor {
    fn check(
        &mut self,
        task_ids: &[&str],
        code: &str,
        passed: bool,
        messages: (&str, &str),
        evidence_sha256: Option<&str>,
    ) {
        self.checks.push(AsrCompletionEvidenceCheck {
            task_ids: task_ids.iter().map(|value| (*value).to_owned()).collect(),
            code: code.to_owned(),
            passed,
            message: if passed { messages.0 } else { messages.1 }.to_owned(),
            evidence_sha256: evidence_sha256.map(str::to_owned),
        });
    }

    fn load_json(&mut self, task_ids: &[&str], code: &str, path: &Path) -> Option<LoadedJson> {
        let bytes = read_regular_file(path).ok();
        let loaded = bytes.as_deref().and_then(|bytes| {
            serde_json::from_slice::<Value>(bytes)
                .ok()
                .filter(|value| !contains_private_material(value))
                .map(|value| LoadedJson {
                    value,
                    sha256: sha256_bytes(bytes),
                })
        });
        self.check(
            task_ids,
            code,
            loaded.is_some(),
            (
                "证据文件可读取、JSON 合法且不包含本地路径或日志原文",
                "证据文件缺失、过大、不是普通文件、JSON 无效或包含敏感字段",
            ),
            loaded.as_ref().map(|value| value.sha256.as_str()),
        );
        loaded
    }

    fn finish(self) -> AsrCompletionEvidenceReport {
        let tasks = COMPLETION_TASKS
            .iter()
            .map(|task_id| {
                let related = self
                    .checks
                    .iter()
                    .filter(|check| check.task_ids.iter().any(|value| value == task_id))
                    .collect::<Vec<_>>();
                AsrCompletionTaskStatus {
                    task_id: (*task_id).to_owned(),
                    ready: !related.is_empty() && related.iter().all(|check| check.passed),
                    check_count: related.len(),
                }
            })
            .collect::<Vec<_>>();
        AsrCompletionEvidenceReport {
            schema_version: 1,
            ready_to_complete: tasks.iter().all(|task| task.ready),
            tasks,
            checks: self.checks,
        }
    }
}

/// 审计六个剩余 OpenSpec 任务的真实证据和跨报告哈希关系。
///
/// 缺失或损坏输入会产生 `ready_to_complete=false` 的完整报告，便于一次看到全部缺口。
/// 函数不会修改证据、OpenSpec 任务或任何媒体文件。
pub fn audit_asr_completion_evidence(
    paths: &AsrCompletionEvidencePaths,
) -> AsrCompletionEvidenceReport {
    let mut auditor = Auditor::default();
    let windows_target = auditor
        .load_json(
            &["1.6"],
            "windows_target_file",
            &paths.windows_target_report,
        )
        .and_then(|report| validate_windows_target(&mut auditor, &report));
    let macos_release = auditor
        .load_json(&["9.4"], "macos_release_file", &paths.macos_release_report)
        .and_then(|report| validate_macos_release(&mut auditor, &report));
    let windows_release = auditor
        .load_json(
            &["9.2", "9.5"],
            "windows_release_file",
            &paths.windows_release_report,
        )
        .and_then(|report| validate_windows_release(&mut auditor, &report));
    let macos_performance = auditor
        .load_json(
            &["9.8"],
            "macos_performance_file",
            &paths.macos_performance_report,
        )
        .and_then(|report| validate_performance(&mut auditor, &report, "macos", "arm64"));
    let windows_performance = auditor
        .load_json(
            &["9.8"],
            "windows_performance_file",
            &paths.windows_performance_report,
        )
        .and_then(|report| validate_performance(&mut auditor, &report, "windows", "x64"));
    let quality = validate_quality(&mut auditor, paths);

    auditor.check(
        &["1.6", "9.2"],
        "windows_target_matches_release",
        windows_target
            .as_ref()
            .zip(windows_release.as_ref())
            .is_some_and(|(target, release)| {
                target.manifest_sha256 == release.manifest_sha256
                    && release.runtime_sha256.as_deref() == Some(target.runtime_sha256.as_str())
            }),
        (
            "Windows 目标验收与正式发行使用相同 manifest 和运行库",
            "Windows 目标验收与正式发行缺失或资源哈希不一致",
        ),
        None,
    );
    cross_release_check(
        &mut auditor,
        "macos_performance_matches_release",
        macos_performance.as_ref(),
        macos_release.as_ref(),
        "macOS",
    );
    cross_release_check(
        &mut auditor,
        "windows_performance_matches_release",
        windows_performance.as_ref(),
        windows_release.as_ref(),
        "Windows",
    );
    auditor.check(
        &["9.9"],
        "quality_matches_release",
        quality.as_ref().is_some_and(|quality| {
            [macos_release.as_ref(), windows_release.as_ref()]
                .into_iter()
                .flatten()
                .any(|release| {
                    quality.asr_binary_sha256 == release.asr_cli_sha256
                        && quality.manifest_sha256 == release.manifest_sha256
                })
        }),
        (
            "质量样本使用的 ASR CLI 和资源可追溯到一个正式发行平台",
            "质量样本缺失、混用了二进制/资源或无法追溯到正式发行",
        ),
        None,
    );
    auditor.finish()
}

fn cross_release_check(
    auditor: &mut Auditor,
    code: &str,
    performance: Option<&PerformanceLinks>,
    release: Option<&ReleaseLinks>,
    platform: &str,
) {
    let passed = performance
        .zip(release)
        .is_some_and(|(performance, release)| {
            performance.asr_binary_sha256 == release.asr_cli_sha256
                && performance.manifest_sha256 == release.manifest_sha256
        });
    auditor.check(
        &["9.8"],
        code,
        passed,
        (
            if platform == "macOS" {
                "macOS 8 GB 性能报告可追溯到正式发行 CLI 和资源"
            } else {
                "Windows 8 GB 性能报告可追溯到正式发行 CLI 和资源"
            },
            if platform == "macOS" {
                "macOS 8 GB 性能报告缺失或未使用正式发行 CLI/资源"
            } else {
                "Windows 8 GB 性能报告缺失或未使用正式发行 CLI/资源"
            },
        ),
        None,
    );
}

fn validate_windows_target(auditor: &mut Auditor, report: &LoadedJson) -> Option<TargetLinks> {
    let value = &report.value;
    let common = common_report(value, "windows", "x64");
    auditor.check(
        &["1.6"],
        "windows_target_identity",
        common,
        (
            "Windows x64 目标报告身份和时间有效",
            "Windows 目标报告版本、平台、架构或采集时间无效",
        ),
        Some(&report.sha256),
    );
    let gates = bool_fields(
        value,
        &[
            "allPassed",
            "cpuBaselineDeclared",
            "runtimeDeclared",
            "runningCancellationPassed",
            "unicodeAndSpacePathPassed",
            "structuredOutputPassed",
            "sourceUnchanged",
        ],
    ) && zero_fields(
        value,
        &[
            "buildExitCode",
            "adapterTestsExitCode",
            "powershellParserTestsExitCode",
            "integrationTestsExitCode",
            "cliStageTestsExitCode",
            "cliExitCode",
        ],
    );
    auditor.check(
        &["1.6"],
        "windows_target_gates",
        gates,
        (
            "Windows CPU、Unicode、取消、CLI 阶段和运行库目标门禁通过",
            "Windows 目标报告至少一个真实行为门禁未通过",
        ),
        Some(&report.sha256),
    );
    let manifest = sha_field(value, "manifestSha256");
    let runtime = sha_field(value, "runtimeSha256");
    let hashes = manifest.is_some()
        && sha_field(value, "sidecarSha256").is_some()
        && runtime.is_some()
        && sha_field(value, "sourceSha256Before").is_some()
        && sha_field(value, "sourceSha256Before") == sha_field(value, "sourceSha256After");
    auditor.check(
        &["1.6"],
        "windows_target_hashes",
        hashes,
        (
            "Windows 目标报告的资源、运行库和源文件哈希有效",
            "Windows 目标报告缺少有效哈希或源文件前后不一致",
        ),
        Some(&report.sha256),
    );
    (common && gates && hashes).then(|| TargetLinks {
        manifest_sha256: manifest.unwrap().to_owned(),
        runtime_sha256: runtime.unwrap().to_owned(),
    })
}

fn validate_macos_release(auditor: &mut Auditor, report: &LoadedJson) -> Option<ReleaseLinks> {
    let value = &report.value;
    let common = common_report(value, "macos", "arm64");
    auditor.check(
        &["9.4"],
        "macos_release_identity",
        common,
        (
            "macOS arm64 正式发行报告身份和时间有效",
            "macOS 发行报告版本、平台、架构或采集时间无效",
        ),
        Some(&report.sha256),
    );
    let gates = bool_fields(
        value,
        &[
            "allPassed",
            "appCodeSignatureValid",
            "developerIdSigned",
            "hardenedRuntime",
            "allMachOSigned",
            "appStapled",
            "dmgStapled",
            "gatekeeperAppPassed",
            "gatekeeperDmgPassed",
            "dmgVerified",
            "resourceBundleValid",
            "installedUnderApplications",
            "offlineObserved",
            "appLaunchPassed",
            "asrJsonValid",
            "asrExpectedTextPassed",
            "sourceUnchanged",
        ],
    ) && zero_fields(value, &["asrExitCode"])
        && positive_equal_fields(value, "machOCount", "signedMachOCount");
    auditor.check(
        &["9.4"],
        "macos_release_gates",
        gates,
        (
            "Developer ID、公证、Gatekeeper、安装、离线 ASR 与启动门禁通过",
            "macOS 正式发行至少一个签名、公证、安装或离线行为门禁未通过",
        ),
        Some(&report.sha256),
    );
    let hashes = [
        "dmgSha256",
        "mainBinarySha256",
        "resourceManifestSha256",
        "asrCliSha256",
        "asrBundleVerifierSha256",
        "asrStdoutSha256",
        "asrStderrSha256",
        "sourceSha256Before",
        "sourceSha256After",
    ]
    .iter()
    .all(|field| sha_field(value, field).is_some())
        && sha_field(value, "sourceSha256Before") == sha_field(value, "sourceSha256After");
    auditor.check(
        &["9.4"],
        "macos_release_hashes",
        hashes,
        (
            "macOS 发行包、CLI、资源、日志和源文件哈希完整",
            "macOS 发行报告缺少有效哈希或源文件前后不一致",
        ),
        Some(&report.sha256),
    );
    (common && gates && hashes).then(|| ReleaseLinks {
        asr_cli_sha256: sha_field(value, "asrCliSha256").unwrap().to_owned(),
        manifest_sha256: sha_field(value, "resourceManifestSha256")
            .unwrap()
            .to_owned(),
        runtime_sha256: None,
    })
}

fn validate_windows_release(auditor: &mut Auditor, report: &LoadedJson) -> Option<ReleaseLinks> {
    let value = &report.value;
    let common = common_report(value, "windows", "x64");
    auditor.check(
        &["9.2", "9.5"],
        "windows_release_identity",
        common,
        (
            "Windows x64 正式发行报告身份和时间有效",
            "Windows 发行报告版本、平台、架构或采集时间无效",
        ),
        Some(&report.sha256),
    );
    let smart_screen =
        string_field(value, "smartScreenEvidenceId").is_some_and(|value| !is_placeholder(value));
    let gates = bool_fields(
        value,
        &[
            "allPassed",
            "installerSignatureValid",
            "allInstalledBinariesSigned",
            "runtimeSignatureValid",
            "resourceBundleValid",
            "vcRuntimeInstalled",
            "webView2Installed",
            "offlineObserved",
            "unicodeUserProfile",
            "appLaunchPassed",
            "cliJsonValid",
            "asrExpectedTextPassed",
            "sourceUnchanged",
            "uninstallRemovedInstallDirectory",
        ],
    ) && zero_fields(
        value,
        &["installExitCode", "cliExitCode", "uninstallExitCode"],
    ) && positive_equal_fields(value, "signedFileCount", "validSignedFileCount")
        && smart_screen;
    auditor.check(
        &["9.2", "9.5"],
        "windows_release_gates",
        gates,
        (
            "Authenticode、SmartScreen、安装、运行库、离线 ASR 和卸载门禁通过",
            "Windows 正式发行至少一个签名、安装、运行库、离线行为或卸载门禁未通过",
        ),
        Some(&report.sha256),
    );
    let hashes = [
        "installerSha256",
        "mainBinarySha256",
        "resourceManifestSha256",
        "runtimeSha256",
        "asrCliSha256",
        "asrBundleVerifierSha256",
        "cliStdoutSha256",
        "cliStderrSha256",
        "sourceSha256Before",
        "sourceSha256After",
    ]
    .iter()
    .all(|field| sha_field(value, field).is_some())
        && sha_field(value, "sourceSha256Before") == sha_field(value, "sourceSha256After");
    auditor.check(
        &["9.2", "9.5"],
        "windows_release_hashes",
        hashes,
        (
            "Windows 安装包、程序、CLI、资源、运行库、日志和源文件哈希完整",
            "Windows 发行报告缺少有效哈希或源文件前后不一致",
        ),
        Some(&report.sha256),
    );
    (common && gates && hashes).then(|| ReleaseLinks {
        asr_cli_sha256: sha_field(value, "asrCliSha256").unwrap().to_owned(),
        manifest_sha256: sha_field(value, "resourceManifestSha256")
            .unwrap()
            .to_owned(),
        runtime_sha256: sha_field(value, "runtimeSha256").map(str::to_owned),
    })
}

fn validate_performance(
    auditor: &mut Auditor,
    report: &LoadedJson,
    platform: &str,
    architecture: &str,
) -> Option<PerformanceLinks> {
    let value = &report.value;
    let identity = common_report(value, platform, architecture);
    let strict_memory = u64_field(value, "physicalMemoryBytes")
        .is_some_and(|bytes| (7 * 1024_u64.pow(3)..=9 * 1024_u64.pow(3)).contains(&bytes))
        && bool_field(value, "strict8GbDeviceRequired")
        && bool_field(value, "strict8GbDeviceGatePassed");
    let peak_field = if platform == "macos" {
        "peakResidentSetBytes"
    } else {
        "peakWorkingSetBytes"
    };
    let metrics = zero_fields(value, &["exitCode", "lingeringChildProcessCount"])
        && bool_fields(
            value,
            &["sourceUnchanged", "resourcesReleased", "stdoutValidJson"],
        )
        && positive_fields(
            value,
            &[
                "wallMs",
                "audioDurationMs",
                peak_field,
                "maximumThreadCount",
                "maximumProcessCount",
            ],
        )
        && finite_positive_number(value, "realtimeFactor")
        && string_field(value, "thermalStateBefore").is_some()
        && string_field(value, "thermalStateAfter").is_some();
    let hashes = [
        "inputSha256Before",
        "inputSha256After",
        "asrBinarySha256",
        "resourceManifestSha256",
        "stdoutSha256",
        "stderrSha256",
    ]
    .iter()
    .all(|field| sha_field(value, field).is_some())
        && sha_field(value, "inputSha256Before") == sha_field(value, "inputSha256After");
    auditor.check(
        &["9.8"],
        &format!("{platform}_performance_identity"),
        identity && strict_memory,
        (
            if platform == "macos" {
                "macOS arm64 报告来自严格 8 GB 设备"
            } else {
                "Windows x64 报告来自严格 8 GB 设备"
            },
            "性能报告平台、架构、时间或严格 8 GB 门禁无效",
        ),
        Some(&report.sha256),
    );
    auditor.check(
        &["9.8"],
        &format!("{platform}_performance_metrics"),
        metrics && hashes,
        (
            "性能、热状态、资源释放和完整性指标有效",
            "性能报告缺少有效指标、遗留子进程、源文件变化或哈希无效",
        ),
        Some(&report.sha256),
    );
    (identity && strict_memory && metrics && hashes).then(|| PerformanceLinks {
        asr_binary_sha256: sha_field(value, "asrBinarySha256").unwrap().to_owned(),
        manifest_sha256: sha_field(value, "resourceManifestSha256")
            .unwrap()
            .to_owned(),
    })
}

fn validate_quality(
    auditor: &mut Auditor,
    paths: &AsrCompletionEvidencePaths,
) -> Option<QualityLinks> {
    let dataset_bytes = read_regular_file(&paths.quality_dataset).ok();
    let dataset_sha256 = dataset_bytes.as_deref().map(sha256_bytes);
    let dataset = dataset_bytes
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<AsrQualityDataset>(bytes).ok())
        .filter(|dataset| dataset.validate(10).is_ok());
    auditor.check(
        &["9.9"],
        "quality_dataset",
        dataset.is_some(),
        (
            "质量数据集包含至少十场不同路径、授权引用和完整人工标注",
            "质量数据集缺失、少于十场、授权占位或人工标注不完整",
        ),
        dataset_sha256.as_deref(),
    );
    let results_directory = fs::symlink_metadata(&paths.quality_results)
        .ok()
        .is_some_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
    auditor.check(
        &["9.9"],
        "quality_results_directory",
        results_directory,
        (
            "质量样本证据目录存在且不是符号链接",
            "质量样本证据目录缺失、不是目录或是符号链接",
        ),
        None,
    );
    let dataset = dataset?;
    let dataset_sha256 = dataset_sha256.unwrap();
    let recomputed = results_directory
        .then(|| evaluate_quality_results(&dataset, &dataset_sha256, &paths.quality_results, 10))
        .transpose()
        .ok()
        .flatten();
    let quality_json = auditor.load_json(&["9.9"], "quality_json_file", &paths.quality_json_report);
    let json_matches =
        recomputed
            .as_ref()
            .zip(quality_json.as_ref())
            .is_some_and(|(report, provided)| {
                serde_json::to_value(report).ok().as_ref() == Some(&provided.value)
            });
    let markdown = read_regular_file(&paths.quality_markdown_report).ok();
    let markdown_matches =
        recomputed
            .as_ref()
            .zip(markdown.as_ref())
            .is_some_and(|(report, provided)| {
                render_quality_markdown(report).as_bytes() == provided.as_slice()
            });
    auditor.check(
        &["9.9"],
        "quality_recomputed_reports",
        json_matches && markdown_matches,
        (
            "质量 JSON 与 Markdown 可从不可变样本证据逐字重新生成",
            "质量报告缺失、被修改或无法从样本证据重新计算",
        ),
        quality_json.as_ref().map(|value| value.sha256.as_str()),
    );
    let metrics = recomputed.as_ref().is_some_and(|report| {
        report.sample_count >= 10
            && report.successful_samples > 0
            && report.successful_samples + report.failed_samples == report.sample_count
            && report.samples.len() == report.sample_count
            && valid_ratio(report.failure_rate)
            && valid_recall(&report.products)
            && valid_recall(&report.amounts)
            && valid_recall(&report.streamers)
            && report.timestamps.expected > 0
            && report
                .timestamps
                .within_tolerance_rate
                .is_some_and(valid_ratio)
            && report.silence.reference_silence_ms > 0
            && report
                .silence
                .duration_hallucination_rate
                .is_some_and(valid_ratio)
            && report.performance.measured_samples == report.successful_samples
            && report
                .performance
                .mean_realtime_factor
                .is_some_and(valid_positive)
            && report
                .performance
                .maximum_realtime_factor
                .is_some_and(valid_positive)
    });
    auditor.check(
        &["9.9"],
        "quality_metrics",
        metrics,
        (
            "十场以上直播已记录召回、时间、静音、失败率和 RTF",
            "质量报告样本不足、没有成功样本或至少一类要求指标缺失",
        ),
        quality_json.as_ref().map(|value| value.sha256.as_str()),
    );

    let mut binaries = HashSet::new();
    let mut manifests = HashSet::new();
    let mut source_hashes = HashSet::new();
    let evidence_valid = dataset.samples.iter().all(|sample| {
        let path = paths
            .quality_results
            .join(format!("{}.evidence.json", sample.id));
        let evidence = read_regular_file(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<AsrQualityCollectionEvidence>(&bytes).ok());
        evidence.is_some_and(|evidence| {
            let manifest = evidence.resource_manifest_sha256.as_deref();
            let valid = evidence.schema_version == 1
                && evidence.dataset_sha256 == dataset_sha256
                && evidence.sample_id == sample.id
                && evidence.source_unchanged
                && evidence.source_size_bytes_after == Some(evidence.source_size_bytes_before)
                && evidence.source_sha256_after.as_deref()
                    == Some(evidence.source_sha256_before.as_str())
                && is_sha256(&evidence.source_sha256_before)
                && is_sha256(&evidence.asr_binary_sha256)
                && manifest.is_some_and(is_sha256)
                && is_sha256(&evidence.stdout_sha256)
                && is_sha256(&evidence.stderr_sha256);
            if valid {
                binaries.insert(evidence.asr_binary_sha256);
                manifests.insert(manifest.unwrap().to_owned());
                source_hashes.insert(evidence.source_sha256_before);
            }
            valid
        })
    }) && source_hashes.len() == dataset.samples.len()
        && binaries.len() == 1
        && manifests.len() == 1;
    auditor.check(
        &["9.9"],
        "quality_sample_evidence",
        evidence_valid,
        (
            "全部直播样本使用同一 ASR CLI/资源且源内容互不重复",
            "样本证据缺失、哈希无效、源内容重复或混用了 ASR CLI/资源",
        ),
        None,
    );
    (json_matches && markdown_matches && metrics && evidence_valid).then(|| QualityLinks {
        asr_binary_sha256: binaries.into_iter().next().unwrap(),
        manifest_sha256: manifests.into_iter().next().unwrap(),
    })
}

fn read_regular_file(path: &Path) -> Result<Vec<u8>, ()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ())?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > MAX_EVIDENCE_BYTES
    {
        return Err(());
    }
    fs::read(path).map_err(|_| ())
}

fn common_report(value: &Value, platform: &str, architecture: &str) -> bool {
    u64_field(value, "schemaVersion") == Some(1)
        && string_field(value, "platform") == Some(platform)
        && string_field(value, "architecture") == Some(architecture)
        && string_field(value, "collectedAtUtc").is_some_and(valid_timestamp)
}

fn valid_timestamp(value: &str) -> bool {
    DateTime::parse_from_rfc3339(value).is_ok()
}

fn bool_fields(value: &Value, fields: &[&str]) -> bool {
    fields.iter().all(|field| bool_field(value, field))
}

fn bool_field(value: &Value, field: &str) -> bool {
    value.get(field).and_then(Value::as_bool) == Some(true)
}

fn zero_fields(value: &Value, fields: &[&str]) -> bool {
    fields
        .iter()
        .all(|field| value.get(field).and_then(Value::as_i64) == Some(0))
}

fn positive_fields(value: &Value, fields: &[&str]) -> bool {
    fields
        .iter()
        .all(|field| u64_field(value, field).is_some_and(|number| number > 0))
}

fn positive_equal_fields(value: &Value, left: &str, right: &str) -> bool {
    u64_field(value, left)
        .zip(u64_field(value, right))
        .is_some_and(|(left, right)| left > 0 && left == right)
}

fn u64_field(value: &Value, field: &str) -> Option<u64> {
    value.get(field).and_then(Value::as_u64)
}

fn string_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn sha_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    string_field(value, field).filter(|value| is_sha256(value))
}

fn finite_positive_number(value: &Value, field: &str) -> bool {
    value
        .get(field)
        .and_then(Value::as_f64)
        .is_some_and(valid_positive)
}

fn valid_positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

fn valid_ratio(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

fn valid_recall(value: &RecallMetrics) -> bool {
    value.expected > 0 && value.matched <= value.expected && value.recall.is_some_and(valid_ratio)
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_placeholder(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase();
    value.is_empty()
        || ["replace", "placeholder", "example", "smoke", "test-only"]
            .iter()
            .any(|prefix| value.starts_with(prefix))
}

fn contains_private_material(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            let normalized = key.to_ascii_lowercase();
            matches!(
                normalized.as_str(),
                "video"
                    | "videopath"
                    | "resourceroot"
                    | "installerpath"
                    | "installdirectory"
                    | "apppath"
                    | "asrstdouttext"
                    | "asrstderrtext"
                    | "clistdouttext"
                    | "clistderrtext"
                    | "stdouttext"
                    | "stderrtext"
            ) || contains_private_material(value)
        }),
        Value::Array(values) => values.iter().any(contains_private_material),
        Value::String(value) => looks_like_absolute_path(value),
        _ => false,
    }
}

fn looks_like_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.starts_with('/')
        || value.starts_with("~/")
        || value.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[1] == b':'
            && bytes[0].is_ascii_alphabetic()
            && matches!(bytes[2], b'\\' | b'/'))
        || value.starts_with("file://")
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn privacy_scan_rejects_paths_and_raw_process_output_fields() {
        assert!(contains_private_material(&serde_json::json!({
            "nested": { "stderrText": "failure" }
        })));
        assert!(contains_private_material(&serde_json::json!({
            "osVersion": "/private/tmp/version"
        })));
        assert!(!contains_private_material(&serde_json::json!({
            "platform": "windows",
            "stderrSha256": "a".repeat(64),
            "smartScreenEvidenceId": "SEC-2026-0001"
        })));
    }

    #[test]
    fn placeholder_evidence_references_are_rejected() {
        assert!(is_placeholder("replace-with-ticket"));
        assert!(is_placeholder("smoke-report"));
        assert!(!is_placeholder("SEC-2026-0001"));
    }

    #[test]
    fn completion_task_set_is_stable() {
        assert_eq!(
            BTreeSet::from(COMPLETION_TASKS),
            BTreeSet::from(["1.6", "9.2", "9.4", "9.5", "9.8", "9.9"])
        );
    }
}
