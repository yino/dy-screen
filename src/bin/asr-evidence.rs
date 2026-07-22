//! 汇总并审计 `add-user-triggered-ai-asr` 六个仓库外完成门禁。

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use dy_screen::asr::{AsrCompletionEvidencePaths, audit_asr_completion_evidence};

/// 验证 Windows 目标、双平台发行、双平台 8 GB 性能和十场直播质量证据。
#[derive(Debug, Parser)]
#[command(name = "asr-evidence", version)]
struct Cli {
    /// Windows x64 功能目标报告。
    #[arg(long)]
    windows_target: PathBuf,
    /// 正式 macOS 发行报告。
    #[arg(long)]
    macos_release: PathBuf,
    /// 正式 Windows 发行报告。
    #[arg(long)]
    windows_release: PathBuf,
    /// 严格 8 GB macOS arm64 性能报告。
    #[arg(long)]
    macos_performance: PathBuf,
    /// 严格 8 GB Windows x64 性能报告。
    #[arg(long)]
    windows_performance: PathBuf,
    /// 至少十场授权直播的质量数据集。
    #[arg(long)]
    quality_dataset: PathBuf,
    /// `asr-quality collect` 的样本证据目录。
    #[arg(long)]
    quality_results: PathBuf,
    /// `asr-quality evaluate` 的 JSON 报告。
    #[arg(long)]
    quality_json: PathBuf,
    /// `asr-quality evaluate` 的 Markdown 报告。
    #[arg(long)]
    quality_markdown: PathBuf,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let report = audit_asr_completion_evidence(&AsrCompletionEvidencePaths {
        windows_target_report: cli.windows_target,
        macos_release_report: cli.macos_release,
        windows_release_report: cli.windows_release,
        macos_performance_report: cli.macos_performance,
        windows_performance_report: cli.windows_performance,
        quality_dataset: cli.quality_dataset,
        quality_results: cli.quality_results,
        quality_json_report: cli.quality_json,
        quality_markdown_report: cli.quality_markdown,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).expect("completion evidence report is serializable")
    );
    if report.ready_to_complete {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
