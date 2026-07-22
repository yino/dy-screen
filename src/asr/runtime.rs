//! 从受控应用资源目录解析 ASR 文件，并执行平台、哈希、内存和磁盘 preflight。
//!
//! 普通生产配置不能向本模块注入任意 sidecar 或模型路径。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::{
    AsrBundleManifest, AsrEngineIdentity, AsrEnvironmentCheck, AsrEnvironmentReport, AsrError,
    AsrErrorKind, AsrPlatformManifest, EngineResult,
};

#[derive(Debug, Clone)]
pub struct ResolvedAsrPlatformResource {
    pub code: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
    pub executable: bool,
}

#[derive(Debug, Clone)]
pub struct ResolvedAsrResources {
    pub root: PathBuf,
    pub whisper_sidecar: PathBuf,
    pub vad_sidecar: PathBuf,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub platform_resources: Vec<ResolvedAsrPlatformResource>,
    pub model: PathBuf,
    pub vad_model: PathBuf,
    pub normalization_mapping: PathBuf,
    pub license_files: Vec<PathBuf>,
    pub identity: AsrEngineIdentity,
    pub model_size_bytes: u64,
    pub model_sha256: String,
    pub vad_size_bytes: u64,
    pub vad_sha256: String,
    pub normalization_size_bytes: u64,
    pub normalization_sha256: String,
    pub platform: String,
    pub use_gpu: bool,
    pub minimum_memory_bytes: u64,
    pub minimum_free_disk_bytes: u64,
    pub maximum_threads: usize,
}

/// 只从应用控制的资源根目录和严格 manifest 解析 sidecar，不接受单独可执行路径。
pub struct AsrResourceResolver;

impl AsrResourceResolver {
    pub fn resolve(root: &Path, manifest_json: &str) -> EngineResult<ResolvedAsrResources> {
        let manifest = AsrBundleManifest::from_json(manifest_json)?;
        let os = current_os();
        let arch = current_arch();
        let platform = manifest.platform(os, arch)?.clone();
        platform.require_complete_integrity()?;
        let platform_resources = platform_resource_specs(&platform)
            .into_iter()
            .map(|(code, file, executable)| {
                let integrity = platform.integrity_for(file)?;
                Ok(ResolvedAsrPlatformResource {
                    code,
                    path: root.join(file),
                    size_bytes: integrity.size_bytes,
                    sha256: integrity.sha256.clone(),
                    executable,
                })
            })
            .collect::<EngineResult<Vec<_>>>()?;
        Ok(ResolvedAsrResources {
            root: root.to_path_buf(),
            whisper_sidecar: root.join(&platform.sidecar),
            vad_sidecar: root.join(&platform.vad_sidecar),
            ffmpeg: root.join(&platform.ffmpeg),
            ffprobe: root.join(&platform.ffprobe),
            platform_resources,
            model: root.join(&manifest.model.file),
            vad_model: root.join(&manifest.vad.file),
            normalization_mapping: root.join(&manifest.normalization.file),
            license_files: manifest
                .license_files
                .iter()
                .map(|license_file| root.join(license_file))
                .collect(),
            identity: AsrEngineIdentity {
                engine_id: manifest.engine.id,
                engine_version: manifest.engine.version,
                model_id: manifest.model.logical_id,
                model_version: manifest.model.version,
            },
            model_size_bytes: manifest.model.size_bytes,
            model_sha256: manifest.model.sha256,
            vad_size_bytes: manifest.vad.size_bytes,
            vad_sha256: manifest.vad.sha256,
            normalization_size_bytes: manifest.normalization.size_bytes,
            normalization_sha256: manifest.normalization.sha256,
            platform: format!("{os}-{arch}"),
            use_gpu: platform.accelerator == "metal",
            minimum_memory_bytes: platform.minimum_memory_bytes,
            minimum_free_disk_bytes: platform.minimum_free_disk_bytes,
            maximum_threads: platform.maximum_threads,
        })
    }
}

pub trait SystemResourceProbe: Send + Sync {
    fn total_memory_bytes(&self) -> Option<u64>;
    fn available_memory_bytes(&self) -> Option<u64>;
    fn available_disk_bytes(&self, path: &Path) -> Option<u64>;
}

#[derive(Debug, Default)]
pub struct NativeSystemResourceProbe;

impl SystemResourceProbe for NativeSystemResourceProbe {
    fn total_memory_bytes(&self) -> Option<u64> {
        #[cfg(target_os = "macos")]
        {
            return macos_total_memory();
        }
        #[cfg(target_os = "windows")]
        {
            return powershell_memory("TotalVisibleMemorySize");
        }
        #[allow(unreachable_code)]
        None
    }

    fn available_memory_bytes(&self) -> Option<u64> {
        #[cfg(target_os = "macos")]
        {
            return macos_available_memory();
        }
        #[cfg(target_os = "windows")]
        {
            return powershell_memory("FreePhysicalMemory");
        }
        #[allow(unreachable_code)]
        None
    }

    fn available_disk_bytes(&self, path: &Path) -> Option<u64> {
        fs2::available_space(path).ok()
    }
}

#[cfg(target_os = "macos")]
fn macos_total_memory() -> Option<u64> {
    command_number("/usr/sbin/sysctl", &["-n", "hw.memsize"]).or_else(macos_system_profiler_memory)
}

#[cfg(target_os = "macos")]
fn command_number(program: &str, arguments: &[&str]) -> Option<u64> {
    let output = Command::new(program).args(arguments).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

#[cfg(target_os = "macos")]
fn macos_system_profiler_memory() -> Option<u64> {
    let output = Command::new("/usr/sbin/system_profiler")
        .args(["SPHardwareDataType", "-json"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let capacity = value
        .get("SPHardwareDataType")?
        .as_array()?
        .first()?
        .get("physical_memory")?
        .as_str()?;
    parse_memory_capacity(capacity)
}

#[cfg(target_os = "macos")]
fn parse_memory_capacity(value: &str) -> Option<u64> {
    let mut fields = value.split_whitespace();
    let amount = fields.next()?.parse::<f64>().ok()?;
    let unit = fields.next()?.to_ascii_uppercase();
    if !amount.is_finite() || amount <= 0.0 || fields.next().is_some() {
        return None;
    }
    let multiplier = match unit.as_str() {
        "KB" => 1024_f64,
        "MB" => 1024_f64.powi(2),
        "GB" => 1024_f64.powi(3),
        "TB" => 1024_f64.powi(4),
        _ => return None,
    };
    let bytes = amount * multiplier;
    (bytes <= u64::MAX as f64).then_some(bytes.round() as u64)
}

#[cfg(target_os = "macos")]
fn macos_available_memory() -> Option<u64> {
    let output = Command::new("/usr/bin/vm_stat").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let page_size = text
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    let mut pages = 0_u64;
    for label in [
        "Pages free",
        "Pages inactive",
        "Pages speculative",
        "Pages purgeable",
    ] {
        let value = text
            .lines()
            .find(|line| line.starts_with(label))
            .and_then(|line| line.split(':').nth(1))
            .map(|value| value.trim().trim_end_matches('.'))
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        pages = pages.saturating_add(value);
    }
    Some(pages.saturating_mul(page_size))
}

#[cfg(target_os = "windows")]
fn powershell_memory(property: &str) -> Option<u64> {
    let script = format!("[UInt64](Get-CimInstance Win32_OperatingSystem).{property} * 1KB");
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

/// 任务前资源检查。所有失败都保持项目草稿，不启动媒体或模型进程。
#[derive(Clone)]
pub struct ResourcePreflight {
    probe: Arc<dyn SystemResourceProbe>,
}

impl ResourcePreflight {
    pub fn native() -> Self {
        Self {
            probe: Arc::new(NativeSystemResourceProbe),
        }
    }

    pub fn new(probe: Arc<dyn SystemResourceProbe>) -> Self {
        Self { probe }
    }

    pub fn diagnose(&self, resources: &ResolvedAsrResources) -> EngineResult<AsrEnvironmentReport> {
        let mut checks = resources
            .platform_resources
            .iter()
            .map(|resource| {
                file_check(
                    &resource.code,
                    &resource.path,
                    Some(resource.size_bytes),
                    Some(&resource.sha256),
                    resource.executable,
                )
            })
            .collect::<Vec<_>>();
        checks.extend([
            file_check(
                "asr_model",
                &resources.model,
                Some(resources.model_size_bytes),
                Some(&resources.model_sha256),
                false,
            ),
            file_check(
                "vad_model",
                &resources.vad_model,
                Some(resources.vad_size_bytes),
                Some(&resources.vad_sha256),
                false,
            ),
            file_check(
                "normalization_mapping",
                &resources.normalization_mapping,
                Some(resources.normalization_size_bytes),
                Some(&resources.normalization_sha256),
                false,
            ),
            version_check(resources),
        ]);
        checks.extend(
            resources
                .license_files
                .iter()
                .enumerate()
                .map(|(index, path)| {
                    file_check(
                        &format!("license_file_{}", index.saturating_add(1)),
                        path,
                        None,
                        None,
                        false,
                    )
                }),
        );

        let total_memory = self.probe.total_memory_bytes();
        checks.push(AsrEnvironmentCheck {
            code: "total_memory".to_owned(),
            passed: total_memory.is_some_and(|value| value >= resources.minimum_memory_bytes),
            message: match total_memory {
                Some(value) if value >= resources.minimum_memory_bytes => {
                    "设备内存满足 8 GB 基线".to_owned()
                }
                Some(_) => "设备物理内存低于 8 GB 基线".to_owned(),
                None => "无法读取设备物理内存".to_owned(),
            },
        });
        let required_available = resources
            .model_size_bytes
            .saturating_mul(4)
            .max(1_073_741_824);
        let available_memory = self.probe.available_memory_bytes();
        checks.push(AsrEnvironmentCheck {
            code: "available_memory".to_owned(),
            passed: available_memory.is_some_and(|value| value >= required_available),
            message: match available_memory {
                Some(value) if value >= required_available => "当前可用内存满足识别要求".to_owned(),
                Some(_) => "当前可用内存不足，请关闭其他占用较大的应用".to_owned(),
                None => "无法读取当前可用内存".to_owned(),
            },
        });
        let available_disk = self.probe.available_disk_bytes(&resources.root);
        checks.push(AsrEnvironmentCheck {
            code: "available_disk".to_owned(),
            passed: available_disk.is_some_and(|value| value >= resources.minimum_free_disk_bytes),
            message: match available_disk {
                Some(value) if value >= resources.minimum_free_disk_bytes => {
                    "磁盘空间满足临时音频要求".to_owned()
                }
                Some(_) => "磁盘空间不足，无法安全创建临时音频".to_owned(),
                None => "无法读取磁盘可用空间".to_owned(),
            },
        });

        let ready = checks.iter().all(|check| check.passed);
        Ok(AsrEnvironmentReport {
            ready,
            platform: resources.platform.clone(),
            identity: resources.identity.clone(),
            checks,
        })
    }
}

fn file_check(
    code: &str,
    path: &Path,
    expected_size: Option<u64>,
    expected_sha256: Option<&str>,
    executable: bool,
) -> AsrEnvironmentCheck {
    let result = validate_file(path, expected_size, expected_sha256, executable);
    AsrEnvironmentCheck {
        code: code.to_owned(),
        passed: result.is_ok(),
        message: match result {
            Ok(()) => format!("{}完整", resource_label(code)),
            Err(error) => error.safe_message,
        },
    }
}

fn validate_file(
    path: &Path,
    expected_size: Option<u64>,
    expected_sha256: Option<&str>,
    _executable: bool,
) -> EngineResult<()> {
    let metadata = path.metadata().map_err(|_| resource_missing())?;
    if !metadata.is_file() || expected_size.is_some_and(|size| size != metadata.len()) {
        return Err(resource_corrupt());
    }
    #[cfg(unix)]
    if _executable {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(resource_corrupt());
        }
    }
    if let Some(expected) = expected_sha256 {
        let bytes = std::fs::read(path).map_err(|_| resource_corrupt())?;
        let actual = hex::encode(Sha256::digest(bytes));
        if actual != expected {
            return Err(resource_corrupt());
        }
    }
    Ok(())
}

fn version_check(resources: &ResolvedAsrResources) -> AsrEnvironmentCheck {
    let output = Command::new(&resources.whisper_sidecar)
        .arg("--version")
        .output();
    let passed = output
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|text| {
            text.contains(resources.identity.engine_version.trim_start_matches('v'))
        });
    AsrEnvironmentCheck {
        code: "engine_version".to_owned(),
        passed,
        message: if passed {
            "识别引擎版本匹配资源清单".to_owned()
        } else {
            "识别引擎版本不匹配，请重新安装应用".to_owned()
        },
    }
}

fn resource_label(code: &str) -> &'static str {
    match code {
        "whisper_sidecar" => "Whisper 组件",
        "vad_sidecar" => "VAD 组件",
        "ffmpeg" => "FFmpeg 组件",
        "ffprobe" => "FFprobe 组件",
        "asr_model" => "ASR 模型",
        "vad_model" => "VAD 模型",
        "normalization_mapping" => "中文规范化映射",
        "native_runtime_installer" => "Windows 原生运行库安装器",
        _ => "本地资源",
    }
}

fn platform_resource_specs(platform: &AsrPlatformManifest) -> Vec<(String, &str, bool)> {
    let mut resources = vec![
        (
            "whisper_sidecar".to_owned(),
            platform.sidecar.as_str(),
            true,
        ),
        (
            "vad_sidecar".to_owned(),
            platform.vad_sidecar.as_str(),
            true,
        ),
        ("ffmpeg".to_owned(), platform.ffmpeg.as_str(), true),
        ("ffprobe".to_owned(), platform.ffprobe.as_str(), true),
    ];
    resources.extend(platform.libraries.iter().enumerate().map(|(index, file)| {
        (
            format!("runtime_library_{}", index.saturating_add(1)),
            file.as_str(),
            false,
        )
    }));
    if let Some(runtime_file) = &platform.runtime_file {
        resources.push((
            "native_runtime_installer".to_owned(),
            runtime_file.as_str(),
            false,
        ));
    }
    resources
}

fn resource_missing() -> AsrError {
    AsrError::new(
        AsrErrorKind::EnvironmentUnavailable,
        "asr_resource_missing",
        "本地 ASR 资源缺失，请重新安装应用",
        false,
    )
}

fn resource_corrupt() -> AsrError {
    AsrError::new(
        AsrErrorKind::EnvironmentUnavailable,
        "asr_resource_corrupt",
        "本地 ASR 资源损坏，请重新安装应用",
        false,
    )
}

fn current_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        other => other,
    }
}

fn current_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => other,
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::parse_memory_capacity;

    #[test]
    fn parses_system_profiler_memory_without_locale_dependent_labels() {
        assert_eq!(parse_memory_capacity("8 GB"), Some(8 * 1024 * 1024 * 1024));
        assert_eq!(parse_memory_capacity("1.5 TB"), Some(1_649_267_441_664));
        assert_eq!(parse_memory_capacity("unknown"), None);
    }
}
