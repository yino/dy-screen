//! Runtime Resource Pack v2 的跨平台协议、校验、安装和下载核心。
//!
//! 该模块不依赖 Tauri 或业务数据库。桌面端只负责把应用数据目录、固定发行地址和事件
//! 发布器注入进来；WebView 永远不能提交资源 URL、可执行文件路径或校验绕过开关。

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use url::Url;

pub const RUNTIME_MANIFEST_SCHEMA: u32 = 2;
pub const RUNTIME_RESOURCE_CONNECT_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(30);
/// 应用发布时替换为正式 Ed25519 公钥。该值固定在二进制中，永远不从远端 manifest 读取。
pub const EMBEDDED_MANIFEST_PUBLIC_KEY: [u8; 32] = [
    0x6a, 0x1f, 0x2c, 0x7b, 0x88, 0x5e, 0x13, 0x42, 0x19, 0x9d, 0x73, 0x04, 0x6f, 0x2a, 0x91, 0x55,
    0x3e, 0xa0, 0xb4, 0x17, 0x2d, 0xc8, 0x61, 0x0e, 0x77, 0x38, 0xf2, 0x49, 0x5b, 0x83, 0x16, 0x20,
];

#[derive(Debug, Error)]
pub enum RuntimeResourceError {
    #[error("资源清单无效：{0}")]
    InvalidManifest(String),
    #[error("资源签名无效")]
    InvalidSignature,
    #[error("资源地址无效：{0}")]
    InvalidUrl(String),
    #[error("资源路径无效：{0}")]
    InvalidPath(String),
    #[error("资源文件缺失：{0}")]
    MissingFile(String),
    #[error("资源完整性校验失败：{0}")]
    Integrity(String),
    #[error("资源平台不支持：{0}")]
    UnsupportedPlatform(String),
    #[error("资源版本不满足应用要求")]
    VersionTooOld,
    #[error("资源下载失败：{0}")]
    Download(String),
    #[error("资源安装失败：{0}")]
    Install(String),
    #[error("资源操作已取消")]
    Cancelled,
}

impl RuntimeResourceError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidManifest(_) => "invalid_manifest",
            Self::InvalidSignature => "invalid_signature",
            Self::InvalidUrl(_) => "invalid_url",
            Self::InvalidPath(_) => "invalid_path",
            Self::MissingFile(_) => "missing_file",
            Self::Integrity(_) => "integrity_failed",
            Self::UnsupportedPlatform(_) => "unsupported_platform",
            Self::VersionTooOld => "version_too_old",
            Self::Download(_) => "download_failed",
            Self::Install(_) => "install_failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeManifest {
    pub schema_version: u32,
    pub app_min_version: String,
    pub bundle_version: String,
    pub channel: String,
    pub platforms: Vec<RuntimePlatformManifest>,
    pub components: Vec<RuntimeComponent>,
    pub archive: RuntimeArchive,
    #[serde(default)]
    pub licenses: Vec<RuntimeLicense>,
    pub signature: RuntimeManifestSignature,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimePlatformManifest {
    pub os: String,
    pub arch: String,
    pub minimum_free_disk_bytes: u64,
    pub minimum_memory_bytes: u64,
}

/// 组件所属能力组（任务 10.2）。
///
/// 基础能力（录制、浏览、预览、封面、本地 ASR）只依赖 `Base` 组件；
/// 增强能力（特效字幕渲染）依赖 `Enhancement` 组件。任一增强组件缺失或损坏
/// MUST NOT 影响基础能力。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeCapability {
    #[default]
    Base,
    Enhancement,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeComponent {
    pub id: String,
    pub version: String,
    pub required: bool,
    pub files: Vec<RuntimeFile>,
    /// 组件所属能力组；旧清单缺省视为基础能力。
    #[serde(default)]
    pub capability: RuntimeCapability,
    /// 组件内以文本形式内联的可执行脚本条目（渲染器引擎脚本）。
    ///
    /// 引擎脚本内联在随包 JS 文件里、没有独立文件，因此必须以
    /// （版本 + 字节数 + SHA-256 + 承载文件）四元组独立声明，供启动前校验。
    #[serde(default)]
    pub scripts: Vec<RuntimeScriptEntry>,
}

/// 组件内联脚本条目。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeScriptEntry {
    pub id: String,
    pub version: String,
    pub size_bytes: u64,
    pub sha256: String,
    /// 承载该脚本的相对路径，必须在本组件的 `files` 中声明过。
    pub host_file: String,
}

/// 增强能力的组件 ID（任务 10.1）。
pub const RENDER_RUNTIME_COMPONENT: &str = "render.runtime";
pub const RENDER_BROWSER_COMPONENT: &str = "render.browser";
pub const RENDER_ENGINE_SCRIPT_COMPONENT: &str = "render.engine-script";
pub const LOCKED_FONTS_COMPONENT: &str = "render.locked-fonts";
pub const HARNESS_RUNTIME_COMPONENT: &str = "harness.runtime";
pub const HARNESS_PROFILE_COMPONENT: &str = "harness.profile";
pub const HARNESS_SKILL_COMPONENT: &str = "harness.skill";

/// 增强能力需要的全部组件。
pub const ENHANCEMENT_COMPONENTS: &[&str] = &[
    RENDER_RUNTIME_COMPONENT,
    RENDER_BROWSER_COMPONENT,
    RENDER_ENGINE_SCRIPT_COMPONENT,
    LOCKED_FONTS_COMPONENT,
    HARNESS_RUNTIME_COMPONENT,
    HARNESS_PROFILE_COMPONENT,
    HARNESS_SKILL_COMPONENT,
];

/// 单个组件的诊断结果（可独立诊断，任务 10.1）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ComponentDiagnostic {
    pub id: String,
    pub version: String,
    pub capability: RuntimeCapability,
    pub required: bool,
    pub available: bool,
    pub error_code: Option<String>,
}

/// 能力快照：基础能力与增强能力分别报告。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CapabilitySnapshot {
    pub base_available: bool,
    pub enhancement_available: bool,
    pub enhancement_error_code: Option<String>,
    pub components: Vec<ComponentDiagnostic>,
}

/// 按清单校验已安装组件，并给出分能力组的状态（任务 10.1 / 10.2）。
///
/// `installed_root` 是运行资源根目录。基础能力只看 `Base` 组件，
/// 增强能力只看 `Enhancement` 组件，两者互不影响。
pub fn capability_snapshot(
    manifest: &RuntimeManifest,
    installed_root: &Path,
) -> CapabilitySnapshot {
    let mut components = Vec::with_capacity(manifest.components.len());
    let mut base_available = true;
    let mut enhancement_available = true;
    let mut enhancement_error: Option<String> = None;
    let mut enhancement_seen: Vec<&str> = Vec::new();
    for component in &manifest.components {
        let mut error_code: Option<String> = None;
        for file in &component.files {
            let path = installed_root.join(&file.path);
            let verified = sha256_file(&path)
                .map(|(size, sha256)| size == file.size_bytes && sha256 == file.sha256)
                .unwrap_or(false);
            if !verified {
                error_code = Some("runtime_component_corrupt".to_owned());
                break;
            }
        }
        if error_code.is_none() && component.capability == RuntimeCapability::Enhancement {
            enhancement_seen.push(component.id.as_str());
            // 引擎脚本必须显式声明条目，否则增强能力不可用。
            if component.id == RENDER_ENGINE_SCRIPT_COMPONENT && component.scripts.is_empty() {
                error_code = Some("enhancement_engine_script_missing".to_owned());
            }
        }
        let available = error_code.is_none();
        match component.capability {
            RuntimeCapability::Base => {
                if component.required && !available {
                    base_available = false;
                }
            }
            RuntimeCapability::Enhancement => {
                if !available {
                    enhancement_available = false;
                    enhancement_error.get_or_insert_with(|| {
                        error_code
                            .clone()
                            .unwrap_or_else(|| "enhancement_component_unavailable".to_owned())
                    });
                }
            }
        }
        components.push(ComponentDiagnostic {
            id: component.id.clone(),
            version: component.version.clone(),
            capability: component.capability,
            required: component.required,
            available,
            error_code,
        });
    }
    let missing: Vec<&str> = ENHANCEMENT_COMPONENTS
        .iter()
        .copied()
        .filter(|id| !enhancement_seen.contains(id))
        .collect();
    if !missing.is_empty() {
        enhancement_available = false;
        enhancement_error.get_or_insert_with(|| "enhancement_component_missing".to_owned());
    }
    CapabilitySnapshot {
        base_available,
        enhancement_available,
        enhancement_error_code: enhancement_error,
        components,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeFile {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
    #[serde(default)]
    pub executable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeArchive {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub download_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeLicense {
    pub id: String,
    pub path: String,
    pub license: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeManifestSignature {
    pub algorithm: String,
    pub key_id: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResourceStatus {
    Ready,
    Downloading,
    Verifying,
    Failed,
    Cancelled,
    Unsupported,
    Rollback,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceStatusSnapshot {
    pub status: ResourceStatus,
    pub bundle_version: Option<String>,
    pub manifest_sha256: Option<String>,
    pub current_component: Option<String>,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceProgress {
    pub phase: String,
    pub component_id: Option<String>,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ResolvedRuntimeResources {
    pub root: PathBuf,
    pub bundle_version: String,
    pub manifest_sha256: String,
    pub components: BTreeMap<String, Vec<PathBuf>>,
}

impl RuntimeManifest {
    pub fn from_json(input: &str) -> Result<Self, RuntimeResourceError> {
        let manifest: Self = serde_json::from_str(input)
            .map_err(|error| RuntimeResourceError::InvalidManifest(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), RuntimeResourceError> {
        if self.schema_version != RUNTIME_MANIFEST_SCHEMA
            || self.app_min_version.trim().is_empty()
            || self.bundle_version.trim().is_empty()
            || self.channel.trim().is_empty()
            || self.platforms.is_empty()
            || self.components.is_empty()
            || self.signature.algorithm != "ed25519"
            || self.signature.key_id.trim().is_empty()
        {
            return Err(RuntimeResourceError::InvalidManifest("基础字段缺失".into()));
        }
        validate_hash(&self.archive.sha256)?;
        validate_relative_path(&self.archive.path)?;
        validate_absolute_or_relative_url(&self.archive.download_url)?;
        let mut platform_keys = HashSet::new();
        for platform in &self.platforms {
            if platform.os.trim().is_empty()
                || platform.arch.trim().is_empty()
                || platform.minimum_free_disk_bytes == 0
                || platform.minimum_memory_bytes == 0
                || !platform_keys.insert((&platform.os, &platform.arch))
            {
                return Err(RuntimeResourceError::InvalidManifest(
                    "平台重复或约束无效".into(),
                ));
            }
        }
        let mut component_ids = HashSet::new();
        let mut paths = HashSet::new();
        for component in &self.components {
            if component.id.trim().is_empty()
                || component.version.trim().is_empty()
                || component.files.is_empty()
                || !component_ids.insert(component.id.as_str())
            {
                return Err(RuntimeResourceError::InvalidManifest(
                    "组件重复或为空".into(),
                ));
            }
            let declared: HashSet<&str> = component
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect();
            for file in &component.files {
                validate_relative_path(&file.path)?;
                validate_hash(&file.sha256)?;
                if file.size_bytes == 0 || !paths.insert(file.path.as_str()) {
                    return Err(RuntimeResourceError::InvalidManifest(
                        "文件重复或大小为零".into(),
                    ));
                }
            }
            for script in &component.scripts {
                if script.id.trim().is_empty() || script.version.trim().is_empty() {
                    return Err(RuntimeResourceError::InvalidManifest(
                        "内联脚本条目缺少 id 或版本".into(),
                    ));
                }
                validate_hash(&script.sha256)?;
                validate_relative_path(&script.host_file)?;
                if script.size_bytes == 0 {
                    return Err(RuntimeResourceError::InvalidManifest(
                        "内联脚本条目大小为零".into(),
                    ));
                }
                if !declared.contains(script.host_file.as_str()) {
                    return Err(RuntimeResourceError::InvalidManifest(
                        "内联脚本的承载文件未在组件中声明".into(),
                    ));
                }
            }
        }
        let mut license_ids = HashSet::new();
        for license in &self.licenses {
            if license.id.trim().is_empty() || !license_ids.insert(license.id.as_str()) {
                return Err(RuntimeResourceError::InvalidManifest(
                    "许可证 ID 重复".into(),
                ));
            }
            validate_relative_path(&license.path)?;
            if license.license.trim().is_empty() || license.source.trim().is_empty() {
                return Err(RuntimeResourceError::InvalidManifest(
                    "许可证信息不完整".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn for_current_platform(&self) -> Result<&RuntimePlatformManifest, RuntimeResourceError> {
        self.platforms
            .iter()
            .find(|platform| {
                platform.os == std::env::consts::OS && platform.arch == std::env::consts::ARCH
            })
            .ok_or_else(|| {
                RuntimeResourceError::UnsupportedPlatform(format!(
                    "{}-{}",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ))
            })
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, RuntimeResourceError> {
        let mut unsigned = self.clone();
        unsigned.signature.value.clear();
        serde_json::to_vec(&unsigned)
            .map_err(|error| RuntimeResourceError::InvalidManifest(error.to_string()))
    }

    pub fn manifest_sha256(&self) -> Result<String, RuntimeResourceError> {
        Ok(hex::encode(Sha256::digest(self.canonical_bytes()?)))
    }

    pub fn require_app_version(&self, current_version: &str) -> Result<(), RuntimeResourceError> {
        if compare_versions(current_version, &self.app_min_version).is_lt() {
            return Err(RuntimeResourceError::VersionTooOld);
        }
        Ok(())
    }

    pub fn verify_signature(&self) -> Result<(), RuntimeResourceError> {
        if self.signature.value.trim().is_empty() {
            return Err(RuntimeResourceError::InvalidSignature);
        }
        let signature = BASE64
            .decode(self.signature.value.as_bytes())
            .map_err(|_| RuntimeResourceError::InvalidSignature)?;
        UnparsedPublicKey::new(&ED25519, &EMBEDDED_MANIFEST_PUBLIC_KEY)
            .verify(&self.canonical_bytes()?, &signature)
            .map_err(|_| RuntimeResourceError::InvalidSignature)
    }
}

pub fn validate_base_url(base_url: &str) -> Result<Url, RuntimeResourceError> {
    let url = Url::parse(base_url)
        .map_err(|error| RuntimeResourceError::InvalidUrl(error.to_string()))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
    {
        return Err(RuntimeResourceError::InvalidUrl(
            "资源服务器必须使用 HTTPS 且不能包含凭据".into(),
        ));
    }
    Ok(url)
}

fn validate_absolute_or_relative_url(value: &str) -> Result<(), RuntimeResourceError> {
    if value.trim().is_empty() || value.contains('\\') || value.starts_with("//") {
        return Err(RuntimeResourceError::InvalidUrl(value.into()));
    }
    if value.contains("://") {
        let url = Url::parse(value).map_err(|_| RuntimeResourceError::InvalidUrl(value.into()))?;
        if url.scheme() != "https" || url.username() != "" || url.password().is_some() {
            return Err(RuntimeResourceError::InvalidUrl(value.into()));
        }
    }
    Ok(())
}

pub fn validate_relative_path(value: &str) -> Result<(), RuntimeResourceError> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || value.contains('\\')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(RuntimeResourceError::InvalidPath(value.into()));
    }
    Ok(())
}

fn validate_hash(value: &str) -> Result<(), RuntimeResourceError> {
    if value.len() != 64 || hex::decode(value).map_or(true, |bytes| bytes.len() != 32) {
        return Err(RuntimeResourceError::InvalidManifest(
            "SHA-256 格式无效".into(),
        ));
    }
    Ok(())
}

pub fn compare_versions(left: &str, right: &str) -> std::cmp::Ordering {
    let parse = |value: &str| {
        value
            .split(['.', '-', '+'])
            .take(3)
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    let left = parse(left);
    let right = parse(right);
    (0..3)
        .map(|index| {
            (
                left.get(index).copied().unwrap_or(0),
                right.get(index).copied().unwrap_or(0),
            )
        })
        .find_map(|(left, right)| (left != right).then_some(left.cmp(&right)))
        .unwrap_or(std::cmp::Ordering::Equal)
}

pub fn sha256_file(path: &Path) -> Result<(u64, String), RuntimeResourceError> {
    let mut file = File::open(path)
        .map_err(|_| RuntimeResourceError::MissingFile(path.display().to_string()))?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| RuntimeResourceError::Integrity(error.to_string()))?;
        if read == 0 {
            break;
        }
        size = size.saturating_add(read as u64);
        hasher.update(&buffer[..read]);
    }
    Ok((size, hex::encode(hasher.finalize())))
}

pub fn verify_file(
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<(), RuntimeResourceError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| RuntimeResourceError::MissingFile(path.display().to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(RuntimeResourceError::Integrity(path.display().to_string()));
    }
    let (size, hash) = sha256_file(path)?;
    if size != expected_size || hash != expected_sha256 {
        return Err(RuntimeResourceError::Integrity(path.display().to_string()));
    }
    Ok(())
}

pub fn resolve_runtime_resources(
    root: &Path,
    manifest: &RuntimeManifest,
) -> Result<ResolvedRuntimeResources, RuntimeResourceError> {
    manifest.validate()?;
    manifest.for_current_platform()?;
    let mut components = BTreeMap::new();
    for component in &manifest.components {
        let mut files = Vec::new();
        for file in &component.files {
            let path = root.join(&file.path);
            verify_file(&path, file.size_bytes, &file.sha256)?;
            files.push(path);
        }
        components.insert(component.id.clone(), files);
    }
    for license in &manifest.licenses {
        let path = root.join(&license.path);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| RuntimeResourceError::MissingFile(license.path.clone()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(RuntimeResourceError::Integrity(license.path.clone()));
        }
    }
    Ok(ResolvedRuntimeResources {
        root: root.to_path_buf(),
        bundle_version: manifest.bundle_version.clone(),
        manifest_sha256: manifest.manifest_sha256()?,
        components,
    })
}

pub struct RuntimeInstaller {
    root: PathBuf,
}

impl RuntimeInstaller {
    pub fn new(root: PathBuf) -> Result<Self, RuntimeResourceError> {
        fs::create_dir_all(&root)
            .map_err(|error| RuntimeResourceError::Install(error.to_string()))?;
        reject_symlink_dir(&root)?;
        Ok(Self { root })
    }

    pub fn install_directory(
        &self,
        source: &Path,
        manifest: &RuntimeManifest,
    ) -> Result<ResolvedRuntimeResources, RuntimeResourceError> {
        manifest.validate()?;
        let platform = manifest.for_current_platform()?;
        reject_symlink_dir(source)?;
        let available = fs2::available_space(&self.root)
            .map_err(|error| RuntimeResourceError::Install(error.to_string()))?;
        if available < platform.minimum_free_disk_bytes {
            return Err(RuntimeResourceError::Install(
                "资源安装目录可用磁盘空间不足".to_owned(),
            ));
        }
        let version = safe_version(&manifest.bundle_version)?;
        let part = self.root.join(format!("{version}.part"));
        let target = self.root.join(&version);
        remove_if_exists(&part)?;
        fs::create_dir_all(&part)
            .map_err(|error| RuntimeResourceError::Install(error.to_string()))?;
        let result = (|| {
            for component in &manifest.components {
                for file in &component.files {
                    let from = source.join(&file.path);
                    verify_file(&from, file.size_bytes, &file.sha256)?;
                    let to = part.join(&file.path);
                    copy_atomic(&from, &to)?;
                }
            }
            for license in &manifest.licenses {
                let from = source.join(&license.path);
                let to = part.join(&license.path);
                copy_atomic(&from, &to)?;
            }
            write_atomic(
                &part.join("runtime-manifest.json"),
                serde_json::to_vec_pretty(manifest)
                    .map_err(|error| RuntimeResourceError::Install(error.to_string()))?,
            )?;
            let marker = serde_json::json!({
                "bundleVersion": manifest.bundle_version,
                "manifestSha256": manifest.manifest_sha256()?,
                "installedAt": chrono::Utc::now().to_rfc3339(),
            });
            let marker_path = part.join("installed.json");
            write_atomic(
                &marker_path,
                serde_json::to_vec_pretty(&marker)
                    .map_err(|error| RuntimeResourceError::Install(error.to_string()))?,
            )?;
            sync_dir(&part)?;
            remove_if_exists(&target)?;
            fs::rename(&part, &target)
                .map_err(|error| RuntimeResourceError::Install(error.to_string()))?;
            write_atomic(&self.root.join("current"), version.as_bytes())?;
            self.cleanup_old_versions(&version)?;
            resolve_runtime_resources(&target, manifest)
        })();
        if result.is_err() {
            let _ = remove_if_exists(&part);
        }
        result
    }

    pub fn current_root(&self) -> Option<PathBuf> {
        let version = fs::read_to_string(self.root.join("current")).ok()?;
        let version = version.trim();
        if version.is_empty() || safe_version(version).is_err() {
            return None;
        }
        let root = self.root.join(version);
        root.join("installed.json").is_file().then_some(root)
    }

    pub fn cleanup_temporary(&self) -> Result<(), RuntimeResourceError> {
        for entry in fs::read_dir(&self.root)
            .map_err(|error| RuntimeResourceError::Install(error.to_string()))?
        {
            let entry = entry.map_err(|error| RuntimeResourceError::Install(error.to_string()))?;
            if entry
                .file_type()
                .map_err(|error| RuntimeResourceError::Install(error.to_string()))?
                .is_dir()
                && entry.file_name().to_string_lossy().ends_with(".part")
            {
                remove_if_exists(&entry.path())?;
            }
        }
        Ok(())
    }

    fn cleanup_old_versions(&self, current: &str) -> Result<(), RuntimeResourceError> {
        let mut versions = Vec::new();
        for entry in fs::read_dir(&self.root)
            .map_err(|error| RuntimeResourceError::Install(error.to_string()))?
        {
            let entry = entry.map_err(|error| RuntimeResourceError::Install(error.to_string()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry
                .file_type()
                .map_err(|error| RuntimeResourceError::Install(error.to_string()))?
                .is_dir()
                && name != current
                && !name.ends_with(".part")
                && entry.path().join("installed.json").is_file()
            {
                versions.push((
                    entry.metadata().and_then(|m| m.modified()).ok(),
                    entry.path(),
                ));
            }
        }
        versions.sort_by_key(|(modified, _)| *modified);
        while versions.len() > 1 {
            let (_, path) = versions.remove(0);
            remove_if_exists(&path)?;
        }
        Ok(())
    }
}

pub struct RuntimeDownloader {
    client: reqwest::Client,
    base_url: Url,
    max_bytes: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct DownloadMetadata {
    etag: Option<String>,
    last_modified: Option<String>,
}

impl RuntimeDownloader {
    pub fn new(base_url: &str, max_bytes: u64) -> Result<Self, RuntimeResourceError> {
        let base_url = validate_base_url(base_url)?;
        let client = reqwest::Client::builder()
            .connect_timeout(RUNTIME_RESOURCE_CONNECT_TIMEOUT)
            .timeout(std::time::Duration::from_secs(120))
            .user_agent("dy-screen-runtime-resource/1")
            .build()
            .map_err(|error| RuntimeResourceError::Download(error.to_string()))?;
        Ok(Self {
            client,
            base_url,
            max_bytes,
        })
    }

    pub async fn fetch_manifest(
        &self,
        relative_or_absolute: &str,
        cancellation: &CancellationToken,
    ) -> Result<RuntimeManifest, RuntimeResourceError> {
        let url = self.resolve_url(relative_or_absolute)?;
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|error| RuntimeResourceError::Download(error.to_string()))?;
        if !response.status().is_success() {
            return Err(RuntimeResourceError::Download(format!(
                "HTTP {}",
                response.status()
            )));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| RuntimeResourceError::Download(error.to_string()))?;
        if cancellation.is_cancelled() {
            return Err(RuntimeResourceError::Cancelled);
        }
        if bytes.len() as u64 > self.max_bytes {
            return Err(RuntimeResourceError::Download(
                "manifest 超过大小限制".into(),
            ));
        }
        let manifest =
            RuntimeManifest::from_json(std::str::from_utf8(&bytes).map_err(|_| {
                RuntimeResourceError::InvalidManifest("manifest 不是 UTF-8".into())
            })?)?;
        manifest.verify_signature()?;
        Ok(manifest)
    }

    pub async fn download_file<F>(
        &self,
        relative_or_absolute: &str,
        destination: &Path,
        expected_size: u64,
        expected_sha256: &str,
        cancellation: &CancellationToken,
        mut progress: F,
    ) -> Result<(), RuntimeResourceError>
    where
        F: FnMut(ResourceProgress) + Send,
    {
        let url = self.resolve_url(relative_or_absolute)?;
        let part = part_path(destination);
        let metadata_path = part.with_file_name(format!(
            "{}.meta",
            part.file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("resource.part")
        ));
        if let Some(parent) = part.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| RuntimeResourceError::Download(error.to_string()))?;
        }
        let existing = tokio::fs::metadata(&part)
            .await
            .map(|meta| meta.len())
            .unwrap_or(0);
        let mut request = self.client.get(url);
        if existing > 0 && existing < expected_size {
            request = request.header(reqwest::header::RANGE, format!("bytes={existing}-"));
            if let Ok(bytes) = tokio::fs::read(&metadata_path).await
                && let Ok(metadata) = serde_json::from_slice::<DownloadMetadata>(&bytes)
            {
                if let Some(etag) = metadata.etag {
                    request = request.header(reqwest::header::IF_RANGE, etag);
                } else if let Some(last_modified) = metadata.last_modified {
                    request = request.header(reqwest::header::IF_RANGE, last_modified);
                }
            }
        }
        let response = request
            .send()
            .await
            .map_err(|error| RuntimeResourceError::Download(error.to_string()))?;
        let append = existing > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
        if !append && existing > 0 {
            let _ = tokio::fs::remove_file(&part).await;
        }
        let mut downloaded = if append { existing } else { 0 };
        let mut file = if append {
            tokio::fs::OpenOptions::new().append(true).open(&part).await
        } else {
            tokio::fs::File::create(&part).await
        }
        .map_err(|error| RuntimeResourceError::Download(error.to_string()))?;
        let mut stream = response;
        while let Some(chunk) = stream
            .chunk()
            .await
            .map_err(|error| RuntimeResourceError::Download(error.to_string()))?
        {
            if cancellation.is_cancelled() {
                return Err(RuntimeResourceError::Cancelled);
            }
            downloaded = downloaded.saturating_add(chunk.len() as u64);
            if downloaded > expected_size || downloaded > self.max_bytes {
                return Err(RuntimeResourceError::Download("资源超过大小限制".into()));
            }
            file.write_all(&chunk)
                .await
                .map_err(|error| RuntimeResourceError::Download(error.to_string()))?;
            progress(ResourceProgress {
                phase: "downloading".into(),
                component_id: None,
                downloaded_bytes: downloaded,
                total_bytes: expected_size,
            });
        }
        file.flush()
            .await
            .map_err(|error| RuntimeResourceError::Download(error.to_string()))?;
        drop(file);
        let (size, hash) = sha256_file(&part)?;
        if size != expected_size || hash != expected_sha256 {
            return Err(RuntimeResourceError::Integrity(
                destination.display().to_string(),
            ));
        }
        let metadata = DownloadMetadata {
            etag: response_headers_etag(&stream),
            last_modified: response_headers_last_modified(&stream),
        };
        let _ = tokio::fs::write(
            &metadata_path,
            serde_json::to_vec(&metadata).unwrap_or_default(),
        )
        .await;
        tokio::fs::rename(&part, destination)
            .await
            .map_err(|error| RuntimeResourceError::Download(error.to_string()))?;
        let _ = tokio::fs::remove_file(&metadata_path).await;
        Ok(())
    }

    fn resolve_url(&self, value: &str) -> Result<Url, RuntimeResourceError> {
        if value.contains("://") {
            let url = Url::parse(value)
                .map_err(|error| RuntimeResourceError::InvalidUrl(error.to_string()))?;
            if url.scheme() != "https" || url.host_str() != self.base_url.host_str() {
                return Err(RuntimeResourceError::InvalidUrl(
                    "资源地址不在固定服务器上".into(),
                ));
            }
            return Ok(url);
        }
        validate_relative_path(value)?;
        self.base_url
            .join(value)
            .map_err(|error| RuntimeResourceError::InvalidUrl(error.to_string()))
    }
}

fn safe_version(version: &str) -> Result<String, RuntimeResourceError> {
    validate_relative_path(version)?;
    Ok(version.to_owned())
}

fn reject_symlink_dir(path: &Path) -> Result<(), RuntimeResourceError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| RuntimeResourceError::Install(error.to_string()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(RuntimeResourceError::Install(
            "资源根目录不能是符号链接".into(),
        ));
    }
    Ok(())
}

fn copy_atomic(from: &Path, to: &Path) -> Result<(), RuntimeResourceError> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| RuntimeResourceError::Install(error.to_string()))?;
    }
    let metadata = fs::symlink_metadata(from)
        .map_err(|_| RuntimeResourceError::MissingFile(from.display().to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(RuntimeResourceError::InvalidPath(
            from.display().to_string(),
        ));
    }
    let part = part_path(to);
    fs::copy(from, &part).map_err(|error| RuntimeResourceError::Install(error.to_string()))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&part)
        .map_err(|error| RuntimeResourceError::Install(error.to_string()))?;
    file.sync_all()
        .map_err(|error| RuntimeResourceError::Install(error.to_string()))?;
    drop(file);
    fs::rename(part, to).map_err(|error| RuntimeResourceError::Install(error.to_string()))
}

fn write_atomic(path: &Path, bytes: impl AsRef<[u8]>) -> Result<(), RuntimeResourceError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| RuntimeResourceError::Install(error.to_string()))?;
    }
    let part = part_path(path);
    let mut file =
        File::create(&part).map_err(|error| RuntimeResourceError::Install(error.to_string()))?;
    file.write_all(bytes.as_ref())
        .map_err(|error| RuntimeResourceError::Install(error.to_string()))?;
    file.sync_all()
        .map_err(|error| RuntimeResourceError::Install(error.to_string()))?;
    drop(file);
    fs::rename(part, path).map_err(|error| RuntimeResourceError::Install(error.to_string()))
}

fn sync_dir(path: &Path) -> Result<(), RuntimeResourceError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| RuntimeResourceError::Install(error.to_string()))
}

fn part_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("resource");
    path.with_file_name(format!("{file_name}.part"))
}

fn response_headers_etag(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn response_headers_last_modified(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get(reqwest::header::LAST_MODIFIED)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn remove_if_exists(path: &Path) -> Result<(), RuntimeResourceError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)
        }
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
    .map_err(|error| RuntimeResourceError::Install(error.to_string()))
}

#[derive(Clone)]
pub struct ResourceProgressPublisher(pub Arc<dyn Fn(ResourceProgress) + Send + Sync>);

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn runtime_resource_connect_timeout_is_thirty_seconds() {
        assert_eq!(
            RUNTIME_RESOURCE_CONNECT_TIMEOUT,
            std::time::Duration::from_secs(30)
        );
    }

    fn manifest(root: &Path) -> RuntimeManifest {
        let file = root.join("bin/tool");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"tool").unwrap();
        let (_, sha256) = sha256_file(&file).unwrap();
        RuntimeManifest {
            schema_version: 2,
            app_min_version: "0.2.0".into(),
            bundle_version: "2026.07.26".into(),
            channel: "stable".into(),
            platforms: vec![RuntimePlatformManifest {
                os: std::env::consts::OS.into(),
                arch: std::env::consts::ARCH.into(),
                minimum_free_disk_bytes: 1,
                minimum_memory_bytes: 1,
            }],
            components: vec![RuntimeComponent {
                id: "media.ffmpeg".into(),
                version: "1".into(),
                required: true,
                capability: RuntimeCapability::Base,
                scripts: Vec::new(),
                files: vec![RuntimeFile {
                    path: "bin/tool".into(),
                    size_bytes: 4,
                    sha256,
                    executable: true,
                }],
            }],
            archive: RuntimeArchive {
                path: "archives/runtime.tar".into(),
                size_bytes: 1,
                sha256: "00".repeat(32),
                download_url: "archives/runtime.tar".into(),
            },
            licenses: vec![],
            signature: RuntimeManifestSignature {
                algorithm: "ed25519".into(),
                key_id: "test".into(),
                value: String::new(),
            },
        }
    }

    #[test]
    fn rejects_traversal_and_absolute_paths() {
        assert!(validate_relative_path("../tool").is_err());
        assert!(validate_relative_path("/tmp/tool").is_err());
        assert!(validate_relative_path("bin\\tool").is_err());
    }

    #[test]
    fn verifies_and_installs_declared_files_atomically() {
        let source = tempdir().unwrap();
        let manifest = manifest(source.path());
        let target = tempdir().unwrap();
        let installer = RuntimeInstaller::new(target.path().join("resources")).unwrap();
        let resolved = installer
            .install_directory(source.path(), &manifest)
            .unwrap();
        assert_eq!(resolved.bundle_version, "2026.07.26");
        assert!(installer.current_root().unwrap().join("bin/tool").is_file());
        assert!(target.path().join("resources/current").is_file());
    }

    #[test]
    fn signed_manifest_requires_signature() {
        let root = tempdir().unwrap();
        let manifest = manifest(root.path());
        assert!(matches!(
            manifest.verify_signature(),
            Err(RuntimeResourceError::InvalidSignature)
        ));
    }

    #[test]
    fn validates_https_and_minimum_version() {
        assert!(validate_base_url("http://example.com/resources").is_err());
        assert!(validate_base_url("https://example.com/resources").is_ok());
        let root = tempdir().unwrap();
        let manifest = manifest(root.path());
        assert!(manifest.require_app_version("0.2.0").is_ok());
        assert!(matches!(
            manifest.require_app_version("0.1.9"),
            Err(RuntimeResourceError::VersionTooOld)
        ));
    }

    #[test]
    fn compares_numeric_resource_versions() {
        assert!(compare_versions("2026.07.10", "2026.07.3").is_gt());
        assert!(compare_versions("2026.08.1", "2026.07.99").is_gt());
        assert!(compare_versions("2026.07.3", "2026.07.3").is_eq());
    }

    #[test]
    fn rejects_missing_target_platform_before_file_resolution() {
        let root = tempdir().unwrap();
        let mut manifest = manifest(root.path());
        manifest.platforms[0].os = "unsupported-os".into();
        assert!(matches!(
            manifest.for_current_platform(),
            Err(RuntimeResourceError::UnsupportedPlatform(_))
        ));
    }

    #[test]
    fn refuses_tampered_files_and_symlinks_during_install() {
        let source = tempdir().unwrap();
        let manifest = manifest(source.path());
        fs::write(source.path().join("bin/tool"), b"tampered").unwrap();
        let target = tempdir().unwrap();
        let installer = RuntimeInstaller::new(target.path().join("resources")).unwrap();
        assert!(matches!(
            installer.install_directory(source.path(), &manifest),
            Err(RuntimeResourceError::Integrity(_))
        ));

        fs::write(source.path().join("bin/tool"), b"tool").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let mut manifest = manifest;
            let linked = source.path().join("bin/linked");
            symlink("tool", &linked).unwrap();
            manifest.components[0].files.push(RuntimeFile {
                path: "bin/linked".into(),
                size_bytes: 4,
                sha256: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08".into(),
                executable: false,
            });
            assert!(matches!(
                installer.install_directory(source.path(), &manifest),
                Err(RuntimeResourceError::MissingFile(_))
                    | Err(RuntimeResourceError::InvalidPath(_))
                    | Err(RuntimeResourceError::Integrity(_))
            ));
        }
    }

    #[test]
    fn rejects_archive_hash_mismatch_before_installing() {
        let source = tempdir().unwrap();
        let mut manifest = manifest(source.path());
        let archive = source.path().join("runtime-bundle.tar.zst");
        fs::write(&archive, b"archive").unwrap();
        manifest.archive.size_bytes = 7;
        manifest.archive.sha256 = "00".repeat(32);
        let target = tempdir().unwrap();
        let installer = RuntimeInstaller::new(target.path().join("resources")).unwrap();
        let error = verify_file(
            &archive,
            manifest.archive.size_bytes,
            &manifest.archive.sha256,
        )
        .expect_err("归档哈希错误必须被拒绝");
        assert!(matches!(error, RuntimeResourceError::Integrity(_)));
        assert!(installer.current_root().is_none());
    }

    #[test]
    fn rejects_insufficient_disk_space_before_writing_part_directory() {
        let source = tempdir().unwrap();
        let mut manifest = manifest(source.path());
        manifest.platforms[0].minimum_free_disk_bytes = u64::MAX;
        let target = tempdir().unwrap();
        let resource_root = target.path().join("resources");
        let installer = RuntimeInstaller::new(resource_root.clone()).unwrap();
        let error = installer
            .install_directory(source.path(), &manifest)
            .expect_err("磁盘空间不足必须在事务开始前失败");
        assert!(matches!(error, RuntimeResourceError::Install(_)));
        assert!(!resource_root.join("2026.07.26.part").exists());
        assert!(installer.current_root().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_source_directory() {
        use std::os::unix::fs::symlink;

        let source = tempdir().unwrap();
        let actual = tempdir().unwrap();
        let linked = source.path().join("linked-source");
        symlink(actual.path(), &linked).unwrap();
        let target = tempdir().unwrap();
        let installer = RuntimeInstaller::new(target.path().join("resources")).unwrap();
        let error = installer
            .install_directory(&linked, &manifest(actual.path()))
            .expect_err("源目录符号链接必须被拒绝");
        assert!(matches!(error, RuntimeResourceError::Install(_)));
    }

    /// 写入一个文件并返回其组件文件条目。
    fn component_file(root: &Path, relative: &str, bytes: &[u8]) -> (RuntimeFile, PathBuf) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        let (size_bytes, sha256) = sha256_file(&path).unwrap();
        (
            RuntimeFile {
                path: relative.into(),
                size_bytes,
                sha256,
                executable: false,
            },
            path,
        )
    }

    /// 基础组件 + 五个增强组件（含引擎脚本条目）的完整清单。
    fn manifest_with_enhancement(root: &Path) -> RuntimeManifest {
        let mut manifest = manifest(root);
        let enhancement = |id: &str, relative: &str, bytes: &[u8]| {
            let (file, _) = component_file(root, relative, bytes);
            RuntimeComponent {
                id: id.into(),
                version: "0.8.96".into(),
                required: false,
                capability: RuntimeCapability::Enhancement,
                scripts: Vec::new(),
                files: vec![file],
            }
        };
        manifest.components.push(enhancement(
            RENDER_RUNTIME_COMPONENT,
            "render/runtime/cli.js",
            b"renderer",
        ));
        manifest.components.push(enhancement(
            RENDER_BROWSER_COMPONENT,
            "render/browser/chrome",
            b"browser",
        ));
        let (engine_file, _) = component_file(root, "render/runtime/chunk.js", b"engine-host");
        let (engine_size, engine_sha) = sha256_file(&root.join("render/runtime/chunk.js")).unwrap();
        assert_eq!(engine_size, engine_file.size_bytes);
        manifest.components.push(RuntimeComponent {
            id: RENDER_ENGINE_SCRIPT_COMPONENT.into(),
            version: "0.8.96".into(),
            required: false,
            capability: RuntimeCapability::Enhancement,
            scripts: vec![RuntimeScriptEntry {
                id: "engine-script".into(),
                version: "0.8.96".into(),
                size_bytes: 480_863,
                sha256: engine_sha,
                host_file: "render/runtime/chunk.js".into(),
            }],
            files: vec![engine_file],
        });
        manifest.components.push(enhancement(
            LOCKED_FONTS_COMPONENT,
            "render/fonts/caption-cjk.ttf",
            b"font",
        ));
        manifest.components.push(enhancement(
            HARNESS_RUNTIME_COMPONENT,
            "harness/runtime.js",
            b"harness",
        ));
        manifest.components.push(enhancement(
            HARNESS_PROFILE_COMPONENT,
            "harness/profile.yaml",
            b"profile",
        ));
        manifest.components.push(enhancement(
            HARNESS_SKILL_COMPONENT,
            "harness/skills/generic-hook.skill.json",
            b"skill",
        ));
        manifest
    }

    #[test]
    fn enhancement_components_are_reported_independently() {
        let directory = tempdir().unwrap();
        let manifest = manifest_with_enhancement(directory.path());
        manifest.validate().unwrap();
        let snapshot = capability_snapshot(&manifest, directory.path());
        assert!(snapshot.base_available);
        assert!(snapshot.enhancement_available);
        assert!(snapshot.enhancement_error_code.is_none());
        // 每个增强组件都必须独立出现在诊断里。
        for id in ENHANCEMENT_COMPONENTS {
            let diagnostic = snapshot
                .components
                .iter()
                .find(|component| component.id == *id)
                .unwrap_or_else(|| panic!("缺少组件诊断 {id}"));
            assert!(diagnostic.available, "组件 {id} 应当可用");
            assert_eq!(diagnostic.capability, RuntimeCapability::Enhancement);
            assert_eq!(diagnostic.version, "0.8.96");
        }
    }

    #[test]
    fn broken_enhancement_component_only_disables_enhancement() {
        let directory = tempdir().unwrap();
        let manifest = manifest_with_enhancement(directory.path());
        // 损坏渲染浏览器：基础能力必须保持可用。
        fs::write(directory.path().join("render/browser/chrome"), b"corrupted").unwrap();
        let snapshot = capability_snapshot(&manifest, directory.path());
        assert!(snapshot.base_available, "基础能力不得受增强组件影响");
        assert!(!snapshot.enhancement_available);
        assert_eq!(
            snapshot.enhancement_error_code.as_deref(),
            Some("runtime_component_corrupt")
        );
        let base = snapshot
            .components
            .iter()
            .find(|component| component.id == "media.ffmpeg")
            .unwrap();
        assert!(base.available, "基础组件诊断必须仍为可用");
        let browser = snapshot
            .components
            .iter()
            .find(|component| component.id == RENDER_BROWSER_COMPONENT)
            .unwrap();
        assert!(!browser.available);
        assert_eq!(
            browser.error_code.as_deref(),
            Some("runtime_component_corrupt")
        );
    }

    #[test]
    fn missing_enhancement_components_disable_only_enhancement() {
        let directory = tempdir().unwrap();
        // 只装基础组件的旧清单：增强能力不可用，基础能力照常。
        let manifest = manifest(directory.path());
        let snapshot = capability_snapshot(&manifest, directory.path());
        assert!(snapshot.base_available);
        assert!(!snapshot.enhancement_available);
        assert_eq!(
            snapshot.enhancement_error_code.as_deref(),
            Some("enhancement_component_missing")
        );
    }

    #[test]
    fn engine_script_entry_must_be_declared_and_present() {
        let directory = tempdir().unwrap();
        let mut manifest = manifest_with_enhancement(directory.path());
        // 去掉引擎脚本条目 → 增强能力不可用，基础能力不受影响。
        for component in &mut manifest.components {
            if component.id == RENDER_ENGINE_SCRIPT_COMPONENT {
                component.scripts.clear();
            }
        }
        let snapshot = capability_snapshot(&manifest, directory.path());
        assert!(snapshot.base_available);
        assert!(!snapshot.enhancement_available);
        assert_eq!(
            snapshot.enhancement_error_code.as_deref(),
            Some("enhancement_engine_script_missing")
        );

        // 脚本条目的承载文件必须在本组件的 files 中声明。
        let mut invalid = manifest_with_enhancement(directory.path());
        for component in &mut invalid.components {
            if component.id == RENDER_ENGINE_SCRIPT_COMPONENT {
                component.scripts[0].host_file = "render/runtime/undeclared.js".into();
            }
        }
        assert!(invalid.validate().is_err(), "未声明的承载文件必须被拒绝");

        // 哈希非法、大小为 0 同样必须被拒绝。
        let mut bad_hash = manifest_with_enhancement(directory.path());
        for component in &mut bad_hash.components {
            if component.id == RENDER_ENGINE_SCRIPT_COMPONENT {
                component.scripts[0].sha256 = "not-a-hash".into();
            }
        }
        assert!(bad_hash.validate().is_err());
        let mut zero_size = manifest_with_enhancement(directory.path());
        for component in &mut zero_size.components {
            if component.id == RENDER_ENGINE_SCRIPT_COMPONENT {
                component.scripts[0].size_bytes = 0;
            }
        }
        assert!(zero_size.validate().is_err());
    }

    /// 任务 1.2（步骤 2）：Harness 运行时、Profile 与 Skill 组件类型可独立诊断，
    /// 缺失时增强能力不可用，且不误伤基础能力。
    #[test]
    fn harness_components_are_registered_and_independently_diagnosable() {
        for id in [
            HARNESS_RUNTIME_COMPONENT,
            HARNESS_PROFILE_COMPONENT,
            HARNESS_SKILL_COMPONENT,
        ] {
            assert!(
                ENHANCEMENT_COMPONENTS.contains(&id),
                "{id} 必须登记进增强组件集合"
            );
        }
        let directory = tempdir().unwrap();
        let full = manifest_with_enhancement(directory.path());
        let snapshot = capability_snapshot(&full, directory.path());
        for id in [
            HARNESS_RUNTIME_COMPONENT,
            HARNESS_PROFILE_COMPONENT,
            HARNESS_SKILL_COMPONENT,
        ] {
            let diagnostic = snapshot
                .components
                .iter()
                .find(|component| component.id == id)
                .unwrap_or_else(|| panic!("缺少组件诊断 {id}"));
            assert!(diagnostic.available, "{id} 应当可用");
            assert_eq!(diagnostic.capability, RuntimeCapability::Enhancement);
        }
        assert!(snapshot.enhancement_available);

        // 只含基础组件的旧清单：缺失 Harness 组件使增强不可用、基础照常。
        let base = manifest(directory.path());
        let base_snapshot = capability_snapshot(&base, directory.path());
        assert!(base_snapshot.base_available);
        assert!(!base_snapshot.enhancement_available);
    }

    #[test]
    fn legacy_manifests_without_capability_default_to_base() {
        // 旧清单没有 capability/scripts 字段：必须仍可解析，并归入基础能力。
        let directory = tempdir().unwrap();
        let manifest = manifest(directory.path());
        manifest.validate().unwrap();
        let json = serde_json::to_string(&manifest).unwrap();
        let parsed = RuntimeManifest::from_json(&json).unwrap();
        assert_eq!(parsed.components[0].capability, RuntimeCapability::Base);
        assert!(parsed.components[0].scripts.is_empty());
        let snapshot = capability_snapshot(&parsed, directory.path());
        assert!(snapshot.base_available);
        assert!(!snapshot.enhancement_available);
    }
}
