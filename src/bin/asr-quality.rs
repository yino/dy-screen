//! 真实中文直播 ASR 样本的脱敏采集与质量评估工具。
//!
//! `collect` 逐个调用独立的 ASR 可执行文件，保存去除本地路径后的结构化输出，并记录
//! 输入前后哈希、墙钟耗时和 stdout/stderr 哈希。`evaluate` 只读取这些不可变证据计算指标，
//! 不重新访问原始视频，也不绑定 `whisper.cpp`，因此未来其他 `AsrEngine` Adapter 可以沿用。

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use clap::{Args, Parser, Subcommand};
use dy_screen::asr::{
    AsrQualityCollectionEvidence, AsrQualityDataset, AsrQualityError, AsrQualityOutput,
    evaluate_quality_results, render_quality_markdown,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Error)]
enum QualityCliError {
    #[error("参数或本地验收输入无效：{0}")]
    InvalidInput(String),
    #[error(transparent)]
    Quality(#[from] AsrQualityError),
    #[error("无法完成本地验收文件操作")]
    Io(#[from] std::io::Error),
    #[error("无法生成质量验收 JSON")]
    Json(#[from] serde_json::Error),
}

type QualityResult<T> = Result<T, QualityCliError>;

/// 采集和评估真实中文直播 ASR 质量证据。
#[derive(Debug, Parser)]
#[command(name = "asr-quality", version)]
struct Cli {
    #[command(subcommand)]
    command: QualityCommand,
}

#[derive(Debug, Subcommand)]
enum QualityCommand {
    /// 顺序调用 ASR CLI，并保存不含本地路径和 stderr 原文的样本证据。
    Collect(CollectArgs),
    /// 从已采集证据计算召回率、时间误差、静音幻觉、失败率和实时因子。
    Evaluate(EvaluateArgs),
}

#[derive(Debug, Args)]
struct CollectArgs {
    /// 质量数据集 JSON；相对视频路径以该文件所在目录为基准。
    #[arg(long)]
    dataset: PathBuf,
    /// 新建或为空的结果目录；已有同名样本证据时拒绝覆盖。
    #[arg(long)]
    results: PathBuf,
    /// 要调用的 `dy-screen` 或兼容 ASR CLI 可执行文件。
    #[arg(long)]
    asr_binary: PathBuf,
    /// 开发阶段的受控 ASR 资源根；安装后自定位资源时可以省略。
    #[arg(long)]
    resource_root: Option<PathBuf>,
    /// 语言提示。
    #[arg(long, default_value = "zh")]
    language: String,
    /// 所有样本共同使用的真实项目热词；不会自动把参考答案作为热词。
    #[arg(long = "hotword")]
    hotwords: Vec<String>,
    /// 防止用过小数据集误当真实验收；调试合成数据时可显式降低。
    #[arg(long, default_value_t = 10)]
    minimum_samples: usize,
}

#[derive(Debug, Args)]
struct EvaluateArgs {
    /// 与采集阶段相同的数据集 JSON。
    #[arg(long)]
    dataset: PathBuf,
    /// `collect` 生成的结果目录。
    #[arg(long)]
    results: PathBuf,
    /// 新生成的机器可读报告路径。
    #[arg(long)]
    json_report: PathBuf,
    /// 新生成的 Markdown 报告路径。
    #[arg(long)]
    markdown_report: PathBuf,
    /// 正式验收默认要求至少十个样本。
    #[arg(long, default_value_t = 10)]
    minimum_samples: usize,
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        QualityCommand::Collect(args) => collect(args),
        QualityCommand::Evaluate(args) => evaluate(args),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ASR 质量验收失败：{error}");
            ExitCode::FAILURE
        }
    }
}

fn collect(args: CollectArgs) -> QualityResult<()> {
    if args.language.trim().is_empty() || args.minimum_samples == 0 {
        return Err(QualityCliError::InvalidInput(
            "语言提示不能为空，minimum-samples 必须大于零".to_owned(),
        ));
    }
    let dataset = AsrQualityDataset::from_path(&args.dataset)?;
    dataset.validate(args.minimum_samples)?;
    let dataset_sha256 = sha256_file(&args.dataset)?;
    ensure_regular_file(&args.asr_binary, "ASR 可执行文件")?;
    let asr_binary_sha256 = sha256_file(&args.asr_binary)?;
    let resource_manifest_sha256 = args
        .resource_root
        .as_ref()
        .map(|root| root.join("manifest.json"))
        .map(|manifest| {
            ensure_regular_file(&manifest, "ASR 资源 manifest")?;
            sha256_file(&manifest)
        })
        .transpose()?;
    let dataset_root = args.dataset.parent().unwrap_or_else(|| Path::new("."));
    validate_distinct_sample_sources(&dataset, dataset_root)?;
    prepare_directory(&args.results)?;
    reject_existing_sample_outputs(&dataset, &args.results)?;

    let mut failed_samples = 0_usize;
    for sample in &dataset.samples {
        let video = if sample.video.is_absolute() {
            sample.video.clone()
        } else {
            dataset_root.join(&sample.video)
        };
        let outcome = collect_sample(
            sample.id.as_str(),
            &video,
            &args,
            &dataset_sha256,
            &asr_binary_sha256,
            resource_manifest_sha256.as_deref(),
        )?;
        if !outcome {
            failed_samples += 1;
        }
    }

    println!(
        "ASR 质量样本采集完成：样本={}，采集失败={}，结果目录未记录本地路径或 stderr 原文",
        dataset.samples.len(),
        failed_samples
    );
    Ok(())
}

fn validate_distinct_sample_sources(
    dataset: &AsrQualityDataset,
    dataset_root: &Path,
) -> QualityResult<()> {
    let mut hashes = HashSet::with_capacity(dataset.samples.len());
    for sample in &dataset.samples {
        let video = if sample.video.is_absolute() {
            sample.video.clone()
        } else {
            dataset_root.join(&sample.video)
        };
        ensure_regular_file(&video, "样本视频")?;
        let digest = sha256_file(&video)?;
        if !hashes.insert(digest) {
            return Err(QualityCliError::InvalidInput(format!(
                "样本 {} 与另一个样本内容完全相同；正式验收需要不同直播内容",
                sample.id
            )));
        }
    }
    Ok(())
}

fn collect_sample(
    sample_id: &str,
    video: &Path,
    args: &CollectArgs,
    dataset_sha256: &str,
    asr_binary_sha256: &str,
    resource_manifest_sha256: Option<&str>,
) -> QualityResult<bool> {
    ensure_regular_file(video, "样本视频")?;
    let source_size_bytes_before = fs::metadata(video)?.len();
    let source_sha256_before = sha256_file(video)?;

    let mut command = Command::new(&args.asr_binary);
    command
        .arg("asr")
        .arg(video)
        .arg("--language")
        .arg(&args.language)
        .arg("--json");
    if let Some(resource_root) = &args.resource_root {
        command.arg("--resource-root").arg(resource_root);
    }
    for hotword in &args.hotwords {
        command.arg("--hotword").arg(hotword);
    }

    let started = Instant::now();
    let process = command.output();
    let wall_ms = elapsed_milliseconds(started);
    let (exit_code, stdout, stderr) = match process {
        Ok(output) => (output.status.code(), output.stdout, output.stderr),
        Err(_) => (None, Vec::new(), Vec::new()),
    };

    // 即使 ASR 失败，也重新读取整个源文件。只有内容与大小都相同才允许验收通过。
    let source_after = source_evidence(video).ok();
    let source_size_bytes_after = source_after.as_ref().map(|value| value.0);
    let source_sha256_after = source_after.as_ref().map(|value| value.1.clone());
    let source_unchanged = source_size_bytes_after == Some(source_size_bytes_before)
        && source_sha256_after.as_deref() == Some(source_sha256_before.as_str());

    let stdout_sha256 = sha256_bytes(&stdout);
    let stderr_sha256 = sha256_bytes(&stderr);
    let parsed = serde_json::from_slice::<AsrQualityOutput>(&stdout).ok();
    let sanitized_result = if let Some(mut output) = parsed.clone() {
        // 原始 CLI 的 `video` 字段可能是绝对路径；质量证据永远不持久化该字段。
        output.video = None;
        let mut bytes = serde_json::to_vec_pretty(&output)?;
        bytes.push(b'\n');
        atomic_write_new(&args.results.join(format!("{sample_id}.json")), &bytes)?;
        Some(bytes)
    } else {
        None
    };

    let evidence = AsrQualityCollectionEvidence {
        schema_version: 1,
        dataset_sha256: dataset_sha256.to_owned(),
        sample_id: sample_id.to_owned(),
        exit_code,
        wall_ms,
        source_size_bytes_before,
        source_size_bytes_after,
        source_sha256_before,
        source_sha256_after,
        source_unchanged,
        asr_binary_sha256: asr_binary_sha256.to_owned(),
        resource_manifest_sha256: resource_manifest_sha256.map(str::to_owned),
        stdout_valid_json: parsed.is_some(),
        stdout_size_bytes: saturating_u64(stdout.len()),
        stdout_sha256,
        result_size_bytes: sanitized_result
            .as_ref()
            .map(|bytes| saturating_u64(bytes.len())),
        result_sha256: sanitized_result.as_deref().map(sha256_bytes),
        stderr_size_bytes: saturating_u64(stderr.len()),
        stderr_sha256,
    };
    let mut evidence_bytes = serde_json::to_vec_pretty(&evidence)?;
    evidence_bytes.push(b'\n');
    atomic_write_new(
        &args.results.join(format!("{sample_id}.evidence.json")),
        &evidence_bytes,
    )?;

    let success = exit_code == Some(0) && parsed.is_some() && source_unchanged;
    println!(
        "样本 {sample_id}：exit={}，JSON={}，源文件未变={}",
        exit_code
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unavailable".to_owned()),
        parsed.is_some(),
        source_unchanged
    );
    Ok(success)
}

fn evaluate(args: EvaluateArgs) -> QualityResult<()> {
    if args.minimum_samples == 0 || args.json_report == args.markdown_report {
        return Err(QualityCliError::InvalidInput(
            "minimum-samples 必须大于零，且两个报告路径不能相同".to_owned(),
        ));
    }
    ensure_directory(&args.results, "采集结果目录")?;
    reject_existing_file(&args.json_report, "JSON 报告")?;
    reject_existing_file(&args.markdown_report, "Markdown 报告")?;
    let dataset = AsrQualityDataset::from_path(&args.dataset)?;
    let dataset_sha256 = sha256_file(&args.dataset)?;
    let report = evaluate_quality_results(
        &dataset,
        &dataset_sha256,
        &args.results,
        args.minimum_samples,
    )?;

    let mut json = serde_json::to_vec_pretty(&report)?;
    json.push(b'\n');
    atomic_write_new(&args.json_report, &json)?;
    atomic_write_new(
        &args.markdown_report,
        render_quality_markdown(&report).as_bytes(),
    )?;
    println!(
        "ASR 质量评估完成：样本={}，成功={}，失败={}，失败率={:.2}%",
        report.sample_count,
        report.successful_samples,
        report.failed_samples,
        report.failure_rate * 100.0
    );
    Ok(())
}

fn reject_existing_sample_outputs(
    dataset: &AsrQualityDataset,
    results: &Path,
) -> QualityResult<()> {
    for sample in &dataset.samples {
        for suffix in [".json", ".evidence.json"] {
            if results.join(format!("{}{suffix}", sample.id)).exists() {
                return Err(QualityCliError::InvalidInput(format!(
                    "样本 {} 已存在采集证据；请使用新的结果目录",
                    sample.id
                )));
            }
        }
    }
    Ok(())
}

fn source_evidence(path: &Path) -> QualityResult<(u64, String)> {
    ensure_regular_file(path, "样本视频")?;
    Ok((fs::metadata(path)?.len(), sha256_file(path)?))
}

fn ensure_regular_file(path: &Path, label: &str) -> QualityResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| QualityCliError::InvalidInput(format!("{label}不存在或不可读取")))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(QualityCliError::InvalidInput(format!(
            "{label}必须是普通文件，不能是符号链接"
        )));
    }
    Ok(())
}

fn prepare_directory(path: &Path) -> QualityResult<()> {
    if path.exists() {
        return ensure_directory(path, "结果目录");
    }
    fs::create_dir_all(path)?;
    ensure_directory(path, "结果目录")
}

fn ensure_directory(path: &Path, label: &str) -> QualityResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| QualityCliError::InvalidInput(format!("{label}不存在或不可读取")))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(QualityCliError::InvalidInput(format!(
            "{label}必须是普通目录，不能是符号链接"
        )));
    }
    Ok(())
}

fn reject_existing_file(path: &Path, label: &str) -> QualityResult<()> {
    if path.exists() {
        return Err(QualityCliError::InvalidInput(format!(
            "{label}已存在；为保护既有证据不会覆盖文件"
        )));
    }
    Ok(())
}

fn sha256_file(path: &Path) -> QualityResult<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn elapsed_milliseconds(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn saturating_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// 使用同目录临时文件、`sync_all` 和 rename 发布新证据。
///
/// 目标文件必须不存在，因此 Unix 与 Windows 都不会遇到覆盖语义差异；重复采集必须选择
/// 新目录，从流程上保留每次验收的不可变证据。
fn atomic_write_new(path: &Path, bytes: &[u8]) -> QualityResult<()> {
    reject_existing_file(path, "验收输出")?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    ensure_directory(parent, "验收输出目录")?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| QualityCliError::InvalidInput("验收输出文件名无效".to_owned()))?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.part",
        std::process::id(),
        sequence
    ));
    let result = (|| -> QualityResult<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        sync_parent_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> QualityResult<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> QualityResult<()> {
    Ok(())
}
