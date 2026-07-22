//! ASR 随包资源 manifest、平台选择和完整性集合约束。
//!
//! 发行暂存和运行时都依赖同一份契约，避免按文件名信任 sidecar、模型或原生运行库。

use std::collections::HashSet;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

use super::{AsrError, AsrErrorKind, EngineResult};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AsrBundleManifest {
    pub schema_version: u32,
    pub bundle_version: String,
    pub engine: AsrEngineManifest,
    pub model: AsrModelManifest,
    pub vad: AsrVadManifest,
    pub normalization: AsrNormalizationManifest,
    #[serde(default)]
    pub license_files: Vec<String>,
    pub platforms: Vec<AsrPlatformManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AsrEngineManifest {
    pub id: String,
    pub version: String,
    pub source_commit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AsrModelManifest {
    pub logical_id: String,
    pub version: String,
    pub file: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub source: String,
    pub license: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AsrVadManifest {
    pub logical_id: String,
    pub version: String,
    pub file: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub source: String,
    pub license: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AsrNormalizationManifest {
    pub logical_id: String,
    pub version: String,
    pub file: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub source: String,
    pub license: String,
}

/// 安装包内单个平台文件的不可变完整性记录。
///
/// 平台二进制由各自构建机产生，基础 manifest 只描述路径；发行暂存时会把实际文件大小和
/// SHA-256 封入单平台 manifest。运行时只接受完整封存过的 manifest，避免只按文件名信任
/// `whisper.cpp`、FFmpeg、动态库或 Windows 原生运行库。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AsrFileIntegrityManifest {
    pub file: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AsrPlatformManifest {
    pub os: String,
    pub arch: String,
    pub accelerator: String,
    pub sidecar: String,
    pub vad_sidecar: String,
    pub ffmpeg: String,
    pub ffprobe: String,
    #[serde(default)]
    pub libraries: Vec<String>,
    pub minimum_memory_bytes: u64,
    pub minimum_free_disk_bytes: u64,
    pub maximum_threads: usize,
    #[serde(default)]
    pub minimum_cpu_features: Vec<String>,
    pub runtime: Option<String>,
    #[serde(default)]
    pub runtime_file: Option<String>,
    #[serde(default)]
    pub resource_integrity: Vec<AsrFileIntegrityManifest>,
}

impl AsrBundleManifest {
    /// 解析并验证 manifest 的结构约束。文件本体哈希在资源诊断阶段校验。
    pub fn from_json(json: &str) -> EngineResult<Self> {
        let manifest: Self = serde_json::from_str(json).map_err(|_| {
            AsrError::new(
                AsrErrorKind::EnvironmentUnavailable,
                "invalid_resource_manifest",
                "本地 ASR 资源清单格式无效，请重新安装应用",
                false,
            )
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> EngineResult<()> {
        if self.schema_version != 1
            || self.bundle_version.trim().is_empty()
            || self.engine.id.trim().is_empty()
            || self.engine.version.trim().is_empty()
        {
            return Err(invalid_manifest());
        }
        validate_resource(
            &self.model.logical_id,
            &self.model.file,
            self.model.size_bytes,
            &self.model.sha256,
        )?;
        validate_resource(
            &self.vad.logical_id,
            &self.vad.file,
            self.vad.size_bytes,
            &self.vad.sha256,
        )?;
        validate_resource(
            &self.normalization.logical_id,
            &self.normalization.file,
            self.normalization.size_bytes,
            &self.normalization.sha256,
        )?;
        for license_file in &self.license_files {
            validate_relative_path(license_file)?;
        }
        let mut targets = HashSet::new();
        for platform in &self.platforms {
            if !targets.insert((&platform.os, &platform.arch))
                || platform.os.trim().is_empty()
                || platform.arch.trim().is_empty()
                || platform.accelerator.trim().is_empty()
                || platform.maximum_threads == 0
                || platform.minimum_memory_bytes == 0
                || platform.minimum_free_disk_bytes == 0
                || platform.runtime.is_some() != platform.runtime_file.is_some()
                || platform
                    .runtime
                    .as_ref()
                    .is_some_and(|runtime| runtime.trim().is_empty())
            {
                return Err(invalid_manifest());
            }
            let mut paths = HashSet::new();
            for path in platform.required_resource_paths() {
                validate_relative_path(path)?;
                if !paths.insert(path) {
                    return Err(invalid_manifest());
                }
            }
            let mut cpu_features = HashSet::new();
            if platform
                .minimum_cpu_features
                .iter()
                .any(|feature| feature.trim().is_empty() || !cpu_features.insert(feature.as_str()))
            {
                return Err(invalid_manifest());
            }
            if !platform.resource_integrity.is_empty() {
                platform.require_complete_integrity()?;
            }
        }
        if self.platforms.is_empty() {
            return Err(invalid_manifest());
        }
        Ok(())
    }

    /// 按编译目标选择唯一平台资源，不根据用户输入拼接任意路径。
    pub fn platform(&self, os: &str, arch: &str) -> EngineResult<&AsrPlatformManifest> {
        self.platforms
            .iter()
            .find(|platform| platform.os == os && platform.arch == arch)
            .ok_or_else(|| {
                AsrError::new(
                    AsrErrorKind::UnsupportedPlatform,
                    "unsupported_asr_platform",
                    "当前系统不支持本地语音识别",
                    false,
                )
            })
    }
}

impl AsrPlatformManifest {
    /// 返回此平台安装包必须携带的全部原生文件路径。
    pub fn required_resource_paths(&self) -> Vec<&str> {
        let mut paths = vec![
            self.sidecar.as_str(),
            self.vad_sidecar.as_str(),
            self.ffmpeg.as_str(),
            self.ffprobe.as_str(),
        ];
        paths.extend(self.libraries.iter().map(String::as_str));
        if let Some(runtime_file) = &self.runtime_file {
            paths.push(runtime_file);
        }
        paths
    }

    /// 要求完整性记录与平台文件集合一一对应，不允许缺项、重复项或额外文件。
    pub fn require_complete_integrity(&self) -> EngineResult<()> {
        let required = self
            .required_resource_paths()
            .into_iter()
            .collect::<HashSet<_>>();
        if self.resource_integrity.len() != required.len() {
            return Err(invalid_manifest());
        }
        let mut sealed = HashSet::new();
        for resource in &self.resource_integrity {
            validate_file_integrity(resource)?;
            if !required.contains(resource.file.as_str()) || !sealed.insert(resource.file.as_str())
            {
                return Err(invalid_manifest());
            }
        }
        Ok(())
    }

    pub fn integrity_for(&self, file: &str) -> EngineResult<&AsrFileIntegrityManifest> {
        self.resource_integrity
            .iter()
            .find(|resource| resource.file == file)
            .ok_or_else(invalid_manifest)
    }
}

fn validate_resource(id: &str, file: &str, size: u64, sha256: &str) -> EngineResult<()> {
    if id.trim().is_empty()
        || size == 0
        || sha256.len() != 64
        || hex::decode(sha256).map_or(true, |bytes| bytes.len() != 32)
    {
        return Err(invalid_manifest());
    }
    validate_relative_path(file)
}

fn validate_file_integrity(resource: &AsrFileIntegrityManifest) -> EngineResult<()> {
    if resource.size_bytes == 0
        || resource.sha256.len() != 64
        || hex::decode(&resource.sha256).map_or(true, |bytes| bytes.len() != 32)
    {
        return Err(invalid_manifest());
    }
    validate_relative_path(&resource.file)
}

fn validate_relative_path(path: &str) -> EngineResult<()> {
    let parsed = Path::new(path);
    if path.trim().is_empty()
        || path.contains('\\')
        || parsed.is_absolute()
        || parsed
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid_manifest());
    }
    Ok(())
}

fn invalid_manifest() -> AsrError {
    AsrError::new(
        AsrErrorKind::EnvironmentUnavailable,
        "invalid_resource_manifest",
        "本地 ASR 资源清单无效，请重新安装应用",
        false,
    )
}
