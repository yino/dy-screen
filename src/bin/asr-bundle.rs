//! 离线暂存和校验单平台 ASR 随包资源的发行工具。
//!
//! 工具拒绝符号链接、绝对 RPATH、缺失完整性记录和跨平台混装，并使用原子目录替换发布。

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand, ValueEnum};
use dy_screen::asr::{AsrBundleManifest, AsrFileIntegrityManifest, AsrPlatformManifest};
use dy_screen::runtime_resources::{
    RuntimeArchive, RuntimeComponent, RuntimeFile, RuntimeLicense, RuntimeManifest,
    RuntimeManifestSignature, RuntimePlatformManifest, validate_base_url,
};
use sha2::{Digest, Sha256};

const STAGE_MARKER: &str = ".dy-screen-asr-stage";

/// 为 Tauri 安装包准备单一平台的完整 ASR 资源目录。
#[derive(Debug, Parser)]
#[command(
    name = "asr-bundle",
    version,
    about = "准备并校验切片智能体随包 ASR 资源"
)]
struct Cli {
    #[command(subcommand)]
    command: BundleCommand,
}

#[derive(Debug, Subcommand)]
enum BundleCommand {
    /// 从已下载的可信资源目录复制当前发行平台需要的文件，不执行网络下载。
    Stage {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        target: PathBuf,
        #[arg(long, value_enum)]
        platform: BundlePlatform,
        #[arg(long, default_value = "stable")]
        channel: String,
    },
    /// 校验一个待打包目录的 manifest、文件、哈希、权限和引擎版本。
    Verify {
        #[arg(long)]
        root: PathBuf,
        #[arg(long, value_enum)]
        platform: BundlePlatform,
    },
    /// 输出待签名 manifest 的规范字节，不读取或保存私钥。
    ManifestPayload {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        key_id: String,
    },
    /// 应用外部 Ed25519 签名，并使用应用内嵌公钥立即校验。
    ApplySignature {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        signature_file: PathBuf,
        #[arg(long)]
        key_id: String,
    },
    /// 校验 manifest 的 Ed25519 正式签名。
    VerifySignature {
        #[arg(long)]
        root: PathBuf,
    },
    /// 将单平台 staging 发布为可由客户端逐文件下载的静态目录。
    Publish {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        target: PathBuf,
        #[arg(long, value_enum)]
        platform: BundlePlatform,
        #[arg(long)]
        channel: String,
        #[arg(long)]
        app_version: String,
        #[arg(long)]
        resource_base_url: String,
        #[arg(long, default_value_t = false)]
        allow_unsigned_development: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BundlePlatform {
    MacosAarch64,
    MacosX86_64,
    WindowsX86_64,
}

impl BundlePlatform {
    fn target(self) -> (&'static str, &'static str) {
        match self {
            Self::MacosAarch64 => ("macos", "aarch64"),
            Self::MacosX86_64 => ("macos", "x86_64"),
            Self::WindowsX86_64 => ("windows", "x86_64"),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::MacosAarch64 => "macos-aarch64",
            Self::MacosX86_64 => "macos-x86-64",
            Self::WindowsX86_64 => "windows-x86_64",
        }
    }
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        BundleCommand::Stage {
            source,
            target,
            platform,
            channel,
        } => stage_bundle_for_channel(&source, &target, platform, &channel),
        BundleCommand::Verify { root, platform } => verify_bundle(&root, platform),
        BundleCommand::ManifestPayload {
            root,
            output,
            key_id,
        } => write_manifest_payload(&root, &output, &key_id),
        BundleCommand::ApplySignature {
            root,
            signature_file,
            key_id,
        } => apply_manifest_signature(&root, &signature_file, &key_id),
        BundleCommand::VerifySignature { root } => verify_manifest_signature(&root),
        BundleCommand::Publish {
            root,
            target,
            platform,
            channel,
            app_version,
            resource_base_url,
            allow_unsigned_development,
        } => publish_bundle(
            &root,
            &target,
            platform,
            &channel,
            &app_version,
            &resource_base_url,
            allow_unsigned_development,
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ASR 资源处理失败：{error}");
            ExitCode::FAILURE
        }
    }
}

fn write_manifest_payload(root: &Path, output: &Path, key_id: &str) -> Result<(), String> {
    if output.exists() {
        return Err("签名 payload 输出文件已经存在".to_owned());
    }
    if key_id.trim().is_empty() || key_id.contains(['/', '\\']) {
        return Err("签名 key ID 无效".to_owned());
    }
    let mut manifest = load_runtime_manifest(root)?;
    manifest.signature.key_id = key_id.to_owned();
    manifest.signature.value.clear();
    let payload = manifest
        .canonical_bytes()
        .map_err(|error| format!("无法生成 manifest 规范字节：{error}"))?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("无法创建签名 payload 目录：{error}"))?;
    }
    fs::write(output, payload).map_err(|error| format!("无法写入签名 payload：{error}"))
}

fn verify_manifest_signature(root: &Path) -> Result<(), String> {
    load_runtime_manifest(root)?
        .verify_signature()
        .map_err(|_| "Runtime manifest 的 Ed25519 签名无效".to_owned())
}

fn apply_manifest_signature(
    root: &Path,
    signature_file: &Path,
    key_id: &str,
) -> Result<(), String> {
    if key_id.trim().is_empty() || key_id.contains(['/', '\\']) {
        return Err("签名 key ID 无效".to_owned());
    }
    verify_regular_path(signature_file, "Ed25519 签名文件")?;
    let signature = fs::read_to_string(signature_file)
        .map_err(|error| format!("无法读取 Ed25519 签名：{error}"))?;
    let signature = signature.trim();
    if signature.is_empty() || signature.chars().any(char::is_whitespace) {
        return Err("Ed25519 签名必须是单行 Base64".to_owned());
    }
    let mut manifest = load_runtime_manifest(root)?;
    manifest.signature.key_id = key_id.to_owned();
    manifest.signature.value = signature.to_owned();
    manifest
        .verify_signature()
        .map_err(|_| "Ed25519 签名与应用内嵌公钥不匹配".to_owned())?;
    let bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("无法序列化已签名 manifest：{error}"))?;
    let target = root.join("runtime-manifest.json");
    let temporary = root.join("runtime-manifest.json.part");
    if temporary.exists() {
        return Err("存在未清理的 manifest 签名临时文件".to_owned());
    }
    fs::write(&temporary, [bytes, b"\n".to_vec()].concat())
        .map_err(|error| format!("无法写入已签名 manifest：{error}"))?;
    fs::rename(&temporary, &target).map_err(|error| format!("无法原子发布已签名 manifest：{error}"))
}

fn publish_bundle(
    root: &Path,
    target: &Path,
    platform: BundlePlatform,
    channel: &str,
    app_version: &str,
    resource_base_url: &str,
    allow_unsigned_development: bool,
) -> Result<(), String> {
    if !matches!(platform, BundlePlatform::WindowsX86_64) {
        return Err("该发布命令当前只接受 Windows x64 staging".to_owned());
    }
    verify_bundle(root, platform)?;
    validate_release_segment(channel, "channel")?;
    validate_release_segment(app_version, "应用版本")?;
    let manifest = load_runtime_manifest(root)?;
    validate_release_segment(&manifest.bundle_version, "资源版本")?;
    if allow_unsigned_development && channel.eq_ignore_ascii_case("stable") {
        return Err("无签名开发资源不得发布到 stable 渠道".to_owned());
    }
    if manifest.channel != channel {
        return Err("Runtime manifest channel 与发布 channel 不一致".to_owned());
    }
    if !allow_unsigned_development {
        if manifest.signature.key_id.contains("placeholder")
            || manifest.signature.value.trim().is_empty()
        {
            return Err("正式资源发布拒绝占位或空签名".to_owned());
        }
        manifest
            .verify_signature()
            .map_err(|_| "正式资源 manifest 的 Ed25519 签名无效".to_owned())?;
    }
    let base_url = validate_base_url(resource_base_url)
        .map_err(|error| format!("Windows 资源基础 URL 无效：{error}"))?;
    let suffix = format!(
        "/{channel}/{app_version}/windows/x86_64/{}/",
        manifest.bundle_version
    );
    if !base_url.path().ends_with(&suffix) {
        return Err(format!("资源基础 URL 必须直接指向平台版本目录：{suffix}"));
    }

    let app_root = target.join(channel).join(app_version);
    let release_root = app_root
        .join("windows")
        .join("x86_64")
        .join(&manifest.bundle_version);
    let index_path = app_root.join("index.json");
    if release_root.exists() || index_path.exists() {
        return Err("拒绝覆盖已发布的 Windows 资源版本或 index".to_owned());
    }
    let part = release_root.with_file_name(format!("{}.part", manifest.bundle_version));
    if part.exists() {
        return Err("存在未清理的 Windows 资源发布临时目录".to_owned());
    }
    if let Some(parent) = part.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("无法创建 Windows 资源发布目录：{error}"))?;
    }
    copy_directory_tree(root, &part)?;
    if let Err(error) = verify_bundle(&part, platform) {
        let _ = fs::remove_dir_all(&part);
        return Err(error);
    }
    fs::rename(&part, &release_root)
        .map_err(|error| format!("无法原子发布 Windows 资源目录：{error}"))?;

    let index = serde_json::json!({
        "channel": channel,
        "appVersion": app_version,
        "platform": "windows",
        "arch": "x86_64",
        "bundleVersion": manifest.bundle_version,
        "manifest": "runtime-manifest.json",
        "resourceBaseUrl": resource_base_url,
        "official": !allow_unsigned_development,
    });
    if let Err(error) = write_new_file(
        &index_path,
        [
            serde_json::to_vec_pretty(&index)
                .map_err(|error| format!("无法序列化 Windows 资源 index：{error}"))?,
            b"\n".to_vec(),
        ]
        .concat(),
    ) {
        let _ = fs::remove_dir_all(&release_root);
        return Err(error);
    }
    println!(
        "Windows Runtime Resource Pack 已发布：{}",
        release_root.display()
    );
    Ok(())
}

fn load_runtime_manifest(root: &Path) -> Result<RuntimeManifest, String> {
    let text = fs::read_to_string(root.join("runtime-manifest.json"))
        .map_err(|error| format!("无法读取 runtime-manifest.json：{error}"))?;
    RuntimeManifest::from_json(&text)
        .map_err(|error| format!("Runtime Resource Pack 清单无效：{error}"))
}

fn validate_release_segment(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(format!(
            "{label} 只能包含 ASCII 字母、数字、点、横线和下划线"
        ));
    }
    Ok(())
}

fn verify_regular_path(path: &Path, label: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| format!("{label} 不存在"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("{label} 必须是普通文件"));
    }
    Ok(())
}

fn copy_directory_tree(source: &Path, target: &Path) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(source).map_err(|error| format!("无法读取发布源目录：{error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("发布源必须是普通目录".to_owned());
    }
    fs::create_dir_all(target).map_err(|error| format!("无法创建发布临时目录：{error}"))?;
    for entry in fs::read_dir(source).map_err(|error| format!("无法读取发布源目录：{error}"))?
    {
        let entry = entry.map_err(|error| format!("无法读取发布源目录项：{error}"))?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("无法读取发布源元数据：{error}"))?;
        if metadata.file_type().is_symlink() {
            return Err(format!("发布源包含符号链接：{}", entry.path().display()));
        }
        let destination = target.join(entry.file_name());
        if metadata.is_dir() {
            copy_directory_tree(&entry.path(), &destination)?;
        } else if metadata.is_file() {
            fs::copy(entry.path(), destination)
                .map_err(|error| format!("无法复制发布资源：{error}"))?;
        } else {
            return Err(format!("发布源包含非普通文件：{}", entry.path().display()));
        }
    }
    Ok(())
}

fn write_new_file(path: &Path, bytes: Vec<u8>) -> Result<(), String> {
    use std::io::Write as _;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("无法创建 index 目录：{error}"))?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("无法创建 Windows 资源 index：{error}"))?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|error| format!("无法写入 Windows 资源 index：{error}"))
}

#[cfg(test)]
fn stage_bundle(source: &Path, target: &Path, platform: BundlePlatform) -> Result<(), String> {
    stage_bundle_for_channel(source, target, platform, "stable")
}

fn stage_bundle_for_channel(
    source: &Path,
    target: &Path,
    platform: BundlePlatform,
    channel: &str,
) -> Result<(), String> {
    if source == target {
        return Err("源目录与目标目录不能相同".to_owned());
    }
    validate_release_segment(channel, "channel")?;
    let mut manifest = load_manifest(source)?;
    let mut selected = selected_platform(&manifest, platform)?.clone();
    selected.resource_integrity = seal_platform_resources(source, &selected)?;
    let temporary = sibling_part_path(target)?;
    prepare_owned_directory(&temporary)?;

    let stage_result = (|| {
        copy_and_verify_common_resources(source, &temporary, &manifest)?;
        copy_plain_resources(source, &temporary, &manifest.license_files)?;
        copy_platform_resources(source, &temporary, &selected)?;
        let runtime_manifest = build_runtime_manifest(&manifest, &selected, channel)?;
        manifest.platforms = vec![selected];
        let json = serde_json::to_string_pretty(&manifest)
            .map_err(|error| format!("无法序列化资源清单：{error}"))?;
        fs::write(temporary.join("manifest.json"), format!("{json}\n"))
            .map_err(|error| format!("无法写入资源清单：{error}"))?;
        let runtime_json = serde_json::to_string_pretty(&runtime_manifest)
            .map_err(|error| format!("无法序列化 Runtime Resource Pack 清单：{error}"))?;
        fs::write(
            temporary.join("runtime-manifest.json"),
            format!("{runtime_json}\n"),
        )
        .map_err(|error| format!("无法写入 runtime-manifest.json：{error}"))?;
        verify_bundle(&temporary, platform)
    })();
    if let Err(error) = stage_result {
        let _ = remove_owned_directory(&temporary);
        return Err(error);
    }

    if target.exists() {
        remove_owned_directory(target)?;
    }
    fs::rename(&temporary, target).map_err(|error| format!("无法发布资源目录：{error}"))?;
    println!(
        "ASR 资源已准备：平台={}，目录={}",
        platform.label(),
        target.display()
    );
    Ok(())
}

fn build_runtime_manifest(
    manifest: &AsrBundleManifest,
    platform: &AsrPlatformManifest,
    channel: &str,
) -> Result<RuntimeManifest, String> {
    let mut components = Vec::new();
    let file_for = |path: &str| -> Result<RuntimeFile, String> {
        let integrity = platform
            .resource_integrity
            .iter()
            .find(|item| item.file == path)
            .ok_or_else(|| format!("缺少平台资源哈希：{path}"))?;
        Ok(RuntimeFile {
            path: path.to_owned(),
            size_bytes: integrity.size_bytes,
            sha256: integrity.sha256.clone(),
            executable: path == platform.sidecar
                || path == platform.vad_sidecar
                || path == platform.ffmpeg
                || path == platform.ffprobe,
        })
    };
    for (id, path) in [
        ("media.ffmpeg", platform.ffmpeg.as_str()),
        ("media.ffprobe", platform.ffprobe.as_str()),
        ("asr.whisper", platform.sidecar.as_str()),
        ("asr.vad-sidecar", platform.vad_sidecar.as_str()),
    ] {
        components.push(RuntimeComponent {
            id: id.to_owned(),
            version: manifest.engine.version.clone(),
            required: true,
            files: vec![file_for(path)?],
        });
    }
    let common = [
        (
            "asr.model",
            &manifest.model.file,
            manifest.model.size_bytes,
            &manifest.model.sha256,
            manifest.model.version.clone(),
        ),
        (
            "asr.vad-model",
            &manifest.vad.file,
            manifest.vad.size_bytes,
            &manifest.vad.sha256,
            manifest.vad.version.clone(),
        ),
        (
            "asr.normalization",
            &manifest.normalization.file,
            manifest.normalization.size_bytes,
            &manifest.normalization.sha256,
            manifest.normalization.version.clone(),
        ),
    ];
    for (id, path, size_bytes, sha256, version) in common {
        components.push(RuntimeComponent {
            id: id.to_owned(),
            version,
            required: true,
            files: vec![RuntimeFile {
                path: path.clone(),
                size_bytes,
                sha256: sha256.clone(),
                executable: false,
            }],
        });
    }
    for library in &platform.libraries {
        components.push(RuntimeComponent {
            id: format!("platform.library.{}", components.len()),
            version: manifest.engine.version.clone(),
            required: true,
            files: vec![file_for(library)?],
        });
    }
    if let Some(runtime_file) = &platform.runtime_file {
        components.push(RuntimeComponent {
            id: "platform.windows.vc-runtime".to_owned(),
            version: manifest.engine.version.clone(),
            required: platform.os == "windows",
            files: vec![file_for(runtime_file)?],
        });
    }
    Ok(RuntimeManifest {
        schema_version: 2,
        app_min_version: env!("CARGO_PKG_VERSION").to_owned(),
        bundle_version: manifest.bundle_version.clone(),
        channel: channel.to_owned(),
        platforms: vec![RuntimePlatformManifest {
            os: platform.os.clone(),
            arch: platform.arch.clone(),
            minimum_free_disk_bytes: platform.minimum_free_disk_bytes,
            minimum_memory_bytes: platform.minimum_memory_bytes,
        }],
        components,
        archive: RuntimeArchive {
            path: "runtime-bundle.tar.zst".to_owned(),
            size_bytes: 1,
            sha256: "00".repeat(32),
            download_url: "runtime-bundle.tar.zst".to_owned(),
        },
        licenses: manifest
            .license_files
            .iter()
            .map(|path| RuntimeLicense {
                id: Path::new(path)
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("license")
                    .to_owned(),
                path: path.clone(),
                license: "see-file".to_owned(),
                source: "resources/asr-source".to_owned(),
            })
            .collect(),
        signature: RuntimeManifestSignature {
            algorithm: "ed25519".to_owned(),
            key_id: "release-key-placeholder".to_owned(),
            value: String::new(),
        },
    })
}

fn verify_bundle(root: &Path, platform: BundlePlatform) -> Result<(), String> {
    let manifest = load_manifest(root)?;
    if manifest.platforms.len() != 1 {
        return Err("待打包资源清单必须只包含一个目标平台".to_owned());
    }
    let selected = selected_platform(&manifest, platform)?;
    verify_common_resources(root, &manifest)?;
    if manifest.license_files.is_empty() {
        return Err("待打包资源必须声明至少一个许可证文件".to_owned());
    }
    verify_plain_resources(root, &manifest.license_files)?;
    verify_platform_resources(root, selected)?;
    let runtime_manifest_path = root.join("runtime-manifest.json");
    let runtime_manifest = fs::read_to_string(&runtime_manifest_path)
        .map_err(|error| format!("无法读取 runtime-manifest.json：{error}"))?;
    let runtime_manifest = RuntimeManifest::from_json(&runtime_manifest)
        .map_err(|error| format!("Runtime Resource Pack 清单无效：{error}"))?;
    verify_runtime_manifest_contract(root, &manifest, selected, &runtime_manifest)?;
    verify_macos_architecture_when_applicable(root, selected)?;
    verify_macos_relocatability_when_applicable(root, selected)?;
    verify_windows_resources_when_applicable(root, selected)?;
    verify_engine_version_when_runnable(root, &manifest, selected)?;
    println!(
        "ASR 资源校验通过：平台={}，目录={}",
        platform.label(),
        root.display()
    );
    Ok(())
}

fn verify_runtime_manifest_contract(
    root: &Path,
    manifest: &AsrBundleManifest,
    platform: &AsrPlatformManifest,
    runtime: &RuntimeManifest,
) -> Result<(), String> {
    if runtime.bundle_version != manifest.bundle_version {
        return Err("Runtime manifest 的资源版本与 ASR manifest 不一致".to_owned());
    }
    let [runtime_platform] = runtime.platforms.as_slice() else {
        return Err("Runtime manifest 必须且只能声明一个目标平台".to_owned());
    };
    if runtime_platform.os != platform.os
        || runtime_platform.arch != platform.arch
        || runtime_platform.minimum_free_disk_bytes != platform.minimum_free_disk_bytes
        || runtime_platform.minimum_memory_bytes != platform.minimum_memory_bytes
    {
        return Err("Runtime manifest 的平台或资源约束与 ASR manifest 不一致".to_owned());
    }

    let mut expected = BTreeMap::<String, (u64, String, bool)>::new();
    for (path, size, sha256) in [
        (
            manifest.model.file.as_str(),
            manifest.model.size_bytes,
            manifest.model.sha256.as_str(),
        ),
        (
            manifest.vad.file.as_str(),
            manifest.vad.size_bytes,
            manifest.vad.sha256.as_str(),
        ),
        (
            manifest.normalization.file.as_str(),
            manifest.normalization.size_bytes,
            manifest.normalization.sha256.as_str(),
        ),
    ] {
        expected.insert(path.to_owned(), (size, sha256.to_owned(), false));
    }
    for (path, executable) in platform_paths(platform) {
        let integrity = platform
            .integrity_for(path)
            .map_err(|error| error.safe_message)?;
        expected.insert(
            path.to_owned(),
            (integrity.size_bytes, integrity.sha256.clone(), executable),
        );
    }

    let mut declared = BTreeMap::<String, (u64, String, bool)>::new();
    for component in &runtime.components {
        for file in &component.files {
            verify_hashed_file(root, &file.path, file.size_bytes, &file.sha256)?;
            declared.insert(
                file.path.clone(),
                (file.size_bytes, file.sha256.clone(), file.executable),
            );
        }
    }
    if declared != expected {
        return Err(
            "Runtime manifest 的组件路径、大小、哈希或可执行标记与 staging 不一致".to_owned(),
        );
    }

    let expected_licenses = manifest.license_files.iter().collect::<HashSet<_>>();
    let declared_licenses = runtime
        .licenses
        .iter()
        .map(|license| &license.path)
        .collect::<HashSet<_>>();
    if declared_licenses != expected_licenses {
        return Err("Runtime manifest 的许可证列表与 ASR manifest 不一致".to_owned());
    }
    for license in &runtime.licenses {
        verify_regular_file(root, &license.path, false)?;
    }
    Ok(())
}

fn copy_plain_resources(source: &Path, target: &Path, resources: &[String]) -> Result<(), String> {
    verify_plain_resources(source, resources)?;
    for relative in resources {
        copy_relative_file(source, target, relative)?;
    }
    Ok(())
}

fn verify_plain_resources(root: &Path, resources: &[String]) -> Result<(), String> {
    for relative in resources {
        verify_regular_file(root, relative, false)?;
    }
    Ok(())
}

fn load_manifest(root: &Path) -> Result<AsrBundleManifest, String> {
    let text = fs::read_to_string(root.join("manifest.json"))
        .map_err(|error| format!("无法读取 manifest.json：{error}"))?;
    AsrBundleManifest::from_json(&text).map_err(|error| error.safe_message)
}

fn selected_platform(
    manifest: &AsrBundleManifest,
    platform: BundlePlatform,
) -> Result<&AsrPlatformManifest, String> {
    let (os, arch) = platform.target();
    manifest
        .platform(os, arch)
        .map_err(|_| format!("资源清单不包含目标平台 {}", platform.label()))
}

fn copy_and_verify_common_resources(
    source: &Path,
    target: &Path,
    manifest: &AsrBundleManifest,
) -> Result<(), String> {
    for resource in [
        (
            manifest.model.file.as_str(),
            manifest.model.size_bytes,
            manifest.model.sha256.as_str(),
        ),
        (
            manifest.vad.file.as_str(),
            manifest.vad.size_bytes,
            manifest.vad.sha256.as_str(),
        ),
        (
            manifest.normalization.file.as_str(),
            manifest.normalization.size_bytes,
            manifest.normalization.sha256.as_str(),
        ),
    ] {
        verify_hashed_file(source, resource.0, resource.1, resource.2)?;
        copy_relative_file(source, target, resource.0)?;
    }
    Ok(())
}

fn verify_common_resources(root: &Path, manifest: &AsrBundleManifest) -> Result<(), String> {
    for resource in [
        (
            manifest.model.file.as_str(),
            manifest.model.size_bytes,
            manifest.model.sha256.as_str(),
        ),
        (
            manifest.vad.file.as_str(),
            manifest.vad.size_bytes,
            manifest.vad.sha256.as_str(),
        ),
        (
            manifest.normalization.file.as_str(),
            manifest.normalization.size_bytes,
            manifest.normalization.sha256.as_str(),
        ),
    ] {
        verify_hashed_file(root, resource.0, resource.1, resource.2)?;
    }
    Ok(())
}

fn platform_paths(platform: &AsrPlatformManifest) -> Vec<(&str, bool)> {
    let mut paths = vec![
        (platform.sidecar.as_str(), true),
        (platform.vad_sidecar.as_str(), true),
        (platform.ffmpeg.as_str(), true),
        (platform.ffprobe.as_str(), true),
    ];
    if let Some(runtime_file) = &platform.runtime_file {
        paths.push((runtime_file.as_str(), false));
    }
    paths.extend(
        platform
            .libraries
            .iter()
            .map(|library| (library.as_str(), false)),
    );
    paths
}

fn copy_platform_resources(
    source: &Path,
    target: &Path,
    platform: &AsrPlatformManifest,
) -> Result<(), String> {
    verify_platform_resources(source, platform)?;
    for (relative, _) in platform_paths(platform) {
        copy_relative_file(source, target, relative)?;
    }
    Ok(())
}

fn verify_platform_resources(root: &Path, platform: &AsrPlatformManifest) -> Result<(), String> {
    platform
        .require_complete_integrity()
        .map_err(|error| error.safe_message)?;
    for (relative, executable) in platform_paths(platform) {
        verify_regular_file(root, relative, executable && platform.os != "windows")?;
        let integrity = platform
            .integrity_for(relative)
            .map_err(|error| error.safe_message)?;
        verify_hashed_file(root, relative, integrity.size_bytes, &integrity.sha256)?;
    }
    Ok(())
}

fn seal_platform_resources(
    root: &Path,
    platform: &AsrPlatformManifest,
) -> Result<Vec<AsrFileIntegrityManifest>, String> {
    platform_paths(platform)
        .into_iter()
        .map(|(relative, executable)| {
            verify_regular_file(root, relative, executable && platform.os != "windows")?;
            let path = root.join(relative);
            let bytes = fs::read(&path)
                .map_err(|error| format!("无法读取待封存资源 {}：{error}", path.display()))?;
            if bytes.is_empty() {
                return Err(format!("待封存资源 {} 不能为空", path.display()));
            }
            Ok(AsrFileIntegrityManifest {
                file: relative.to_owned(),
                size_bytes: bytes.len() as u64,
                sha256: hex::encode(Sha256::digest(bytes)),
            })
        })
        .collect()
}

fn verify_macos_architecture_when_applicable(
    root: &Path,
    platform: &AsrPlatformManifest,
) -> Result<(), String> {
    if std::env::consts::OS != "macos" || platform.os != "macos" {
        return Ok(());
    }
    let expected = match platform.arch.as_str() {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        _ => return Err("macOS 资源清单声明了不支持的架构".to_owned()),
    };
    for (relative, _) in platform_paths(platform) {
        let path = root.join(relative);
        if !is_macho(&path)? {
            return Err(format!(
                "macOS 原生资源 {} 不是 Mach-O 文件",
                path.display()
            ));
        }
        let output = Command::new("lipo")
            .arg("-archs")
            .arg(&path)
            .output()
            .map_err(|error| format!("无法检查 {} 的 Mach-O 架构：{error}", path.display()))?;
        if !output.status.success() {
            return Err(format!("无法读取 {} 的 Mach-O 架构", path.display()));
        }
        let architectures = String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if architectures.as_slice() != [expected] {
            return Err(format!(
                "macOS 原生资源 {} 的架构 {:?} 与目标 {} 不一致",
                path.display(),
                architectures,
                platform.arch
            ));
        }
    }
    Ok(())
}

fn verify_macos_relocatability_when_applicable(
    root: &Path,
    platform: &AsrPlatformManifest,
) -> Result<(), String> {
    if std::env::consts::OS != "macos"
        || platform.os != "macos"
        || !is_macho(&root.join(&platform.sidecar))?
    {
        return Ok(());
    }
    for (relative, _) in platform_paths(platform) {
        let path = root.join(relative);
        if !is_macho(&path)? {
            return Err(format!(
                "macOS 原生资源 {} 不是 Mach-O 文件",
                path.display()
            ));
        }
        let dependencies = Command::new("otool")
            .arg("-L")
            .arg(&path)
            .output()
            .map_err(|error| format!("无法检查 {} 的动态依赖：{error}", path.display()))?;
        if !dependencies.status.success() {
            return Err(format!("无法读取 {} 的动态依赖", path.display()));
        }
        let dependencies = String::from_utf8_lossy(&dependencies.stdout);
        for dependency in dependencies.lines().skip(1).filter_map(|line| {
            line.split_whitespace()
                .next()
                .filter(|value| !value.is_empty())
        }) {
            if !is_allowed_macos_dependency(dependency) {
                return Err(format!(
                    "macOS 原生资源 {} 引用了包外动态库 {}",
                    path.display(),
                    dependency
                ));
            }
        }

        let load_commands = Command::new("otool")
            .arg("-l")
            .arg(&path)
            .output()
            .map_err(|error| format!("无法检查 {} 的 RPATH：{error}", path.display()))?;
        if !load_commands.status.success() {
            return Err(format!("无法读取 {} 的 RPATH", path.display()));
        }
        let load_commands = String::from_utf8_lossy(&load_commands.stdout);
        let mut reading_rpath = false;
        for line in load_commands.lines().map(str::trim) {
            if line == "cmd LC_RPATH" {
                reading_rpath = true;
                continue;
            }
            if reading_rpath && line.starts_with("path ") {
                let rpath = line
                    .strip_prefix("path ")
                    .and_then(|value| value.split_whitespace().next())
                    .unwrap_or_default();
                if !rpath.starts_with('@') {
                    return Err(format!(
                        "macOS 原生资源 {} 包含绝对 RPATH {}",
                        path.display(),
                        rpath
                    ));
                }
                reading_rpath = false;
            }
        }
    }
    Ok(())
}

fn is_allowed_macos_dependency(dependency: &str) -> bool {
    dependency.starts_with("/System/")
        || dependency.starts_with("/usr/lib/")
        || dependency.starts_with("@loader_path/")
        || dependency.starts_with("@executable_path/")
        || dependency.starts_with("@rpath/")
}

fn is_macho(path: &Path) -> Result<bool, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("无法读取原生资源 {}：{error}", path.display()))?;
    let magic = bytes.get(..4);
    Ok(matches!(
        magic,
        Some([0xfe, 0xed, 0xfa, 0xce])
            | Some([0xce, 0xfa, 0xed, 0xfe])
            | Some([0xfe, 0xed, 0xfa, 0xcf])
            | Some([0xcf, 0xfa, 0xed, 0xfe])
            | Some([0xca, 0xfe, 0xba, 0xbe])
            | Some([0xbe, 0xba, 0xfe, 0xca])
    ))
}

fn verify_windows_resources_when_applicable(
    root: &Path,
    platform: &AsrPlatformManifest,
) -> Result<(), String> {
    if platform.os != "windows" {
        return Ok(());
    }
    let Some(runtime_file) = platform.runtime_file.as_deref() else {
        return Err("Windows 资源必须声明 x86_64、CPU、SSE4.2 和 VC++ 运行库".to_owned());
    };
    if platform.arch != "x86_64"
        || platform.accelerator != "cpu"
        || !platform
            .minimum_cpu_features
            .iter()
            .any(|feature| feature.eq_ignore_ascii_case("sse4.2"))
        || platform
            .runtime
            .as_deref()
            .is_none_or(|runtime| runtime.trim().is_empty())
    {
        return Err("Windows 资源必须声明 x86_64、CPU、SSE4.2 和 VC++ 运行库".to_owned());
    }

    let runtime_file = runtime_file.replace('\\', "/");
    let declared = platform_paths(platform)
        .into_iter()
        .map(|(path, _)| path.replace('\\', "/"))
        .collect::<HashSet<_>>();
    for relative in &declared {
        let extension = Path::new(relative)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        if !matches!(extension.to_ascii_lowercase().as_str(), "exe" | "dll") {
            return Err(format!("Windows 原生资源路径不是 PE 文件：{relative}"));
        }
        if relative == &runtime_file {
            verify_windows_runtime_bootstrapper_pe(&root.join(relative))?;
        } else {
            verify_x64_pe(&root.join(relative))?;
        }
    }

    let mut native_files = Vec::new();
    collect_native_files(root, root, &mut native_files)?;
    for relative in native_files {
        if !declared.contains(&relative) {
            return Err(format!("Windows staging 包含未声明原生文件：{relative}"));
        }
    }
    Ok(())
}

fn collect_native_files(
    root: &Path,
    directory: &Path,
    output: &mut Vec<String>,
) -> Result<(), String> {
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("无法审计资源目录 {}：{error}", directory.display()))?
    {
        let entry = entry.map_err(|error| format!("无法读取资源目录项：{error}"))?;
        let metadata = entry
            .metadata()
            .map_err(|error| format!("无法读取资源元数据：{error}"))?;
        if metadata.is_dir() {
            collect_native_files(root, &entry.path(), output)?;
            continue;
        }
        let extension = entry
            .path()
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(extension.as_str(), "exe" | "dll" | "dylib" | "so") {
            let relative = entry
                .path()
                .strip_prefix(root)
                .map_err(|_| "资源路径越界".to_owned())?
                .to_string_lossy()
                .replace('\\', "/");
            output.push(relative);
        }
    }
    Ok(())
}

fn verify_x64_pe(path: &Path) -> Result<(), String> {
    if read_pe_machine(path)? != 0x8664 {
        return Err(format!(
            "Windows 原生资源 {} 不是 x64 PE 文件",
            path.display()
        ));
    }
    Ok(())
}

fn verify_windows_runtime_bootstrapper_pe(path: &Path) -> Result<(), String> {
    if !matches!(read_pe_machine(path)?, 0x014c | 0x8664) {
        return Err(format!(
            "Windows VC++ x64 运行库 {} 不是受支持的 x86/x64 PE bootstrapper",
            path.display()
        ));
    }
    Ok(())
}

fn read_pe_machine(path: &Path) -> Result<u16, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("无法读取 Windows 原生资源 {}：{error}", path.display()))?;
    if bytes.len() < 0x46 || bytes.get(..2) != Some(b"MZ") {
        return Err(format!("Windows 原生资源 {} 不是 PE 文件", path.display()));
    }
    let pe_offset = u32::from_le_bytes(
        bytes[0x3c..0x40]
            .try_into()
            .map_err(|_| format!("Windows 原生资源 {} 的 DOS 头无效", path.display()))?,
    ) as usize;
    if pe_offset < 0x40
        || pe_offset.checked_add(6).is_none_or(|end| end > bytes.len())
        || bytes.get(pe_offset..pe_offset + 4) != Some(b"PE\0\0")
    {
        return Err(format!("Windows 原生资源 {} 不是 PE 文件", path.display()));
    }
    Ok(u16::from_le_bytes([
        bytes[pe_offset + 4],
        bytes[pe_offset + 5],
    ]))
}

fn verify_hashed_file(
    root: &Path,
    relative: &str,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<(), String> {
    verify_regular_file(root, relative, false)?;
    let path = root.join(relative);
    let bytes = fs::read(&path).map_err(|error| format!("无法读取 {}：{error}", path.display()))?;
    if bytes.len() as u64 != expected_size {
        return Err(format!("{} 的文件大小与 manifest 不一致", path.display()));
    }
    let actual = hex::encode(Sha256::digest(&bytes));
    if actual != expected_sha256 {
        return Err(format!("{} 的 SHA-256 与 manifest 不一致", path.display()));
    }
    Ok(())
}

fn verify_regular_file(root: &Path, relative: &str, _executable: bool) -> Result<(), String> {
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("缺少待打包资源 {}：{error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "{} 是符号链接，发行包必须包含独立文件",
            path.display()
        ));
    }
    if !metadata.is_file() {
        return Err(format!("{} 不是普通文件", path.display()));
    }
    #[cfg(unix)]
    if _executable {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!("{} 没有可执行权限", path.display()));
        }
    }
    Ok(())
}

fn copy_relative_file(source: &Path, target: &Path, relative: &str) -> Result<(), String> {
    let from = source.join(relative);
    let to = target.join(relative);
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("无法创建目录 {}：{error}", parent.display()))?;
    }
    fs::copy(&from, &to).map_err(|error| {
        format!(
            "无法复制资源 {} 到 {}：{error}",
            from.display(),
            to.display()
        )
    })?;
    Ok(())
}

fn verify_engine_version_when_runnable(
    root: &Path,
    manifest: &AsrBundleManifest,
    platform: &AsrPlatformManifest,
) -> Result<(), String> {
    let host_matches = match std::env::consts::OS {
        "macos" => platform.os == "macos" && std::env::consts::ARCH == platform.arch,
        "windows" => platform.os == "windows" && std::env::consts::ARCH == platform.arch,
        _ => false,
    };
    if !host_matches {
        return Ok(());
    }
    let output = Command::new(root.join(&platform.sidecar))
        .arg("--version")
        .output()
        .map_err(|error| format!("无法启动 whisper.cpp 版本检查：{error}"))?;
    if !output.status.success() {
        return Err("whisper.cpp 版本检查异常退出".to_owned());
    }
    let version = String::from_utf8_lossy(&output.stdout);
    if !version.contains(manifest.engine.version.trim_start_matches('v')) {
        return Err("whisper.cpp 版本与 manifest 不一致".to_owned());
    }
    Ok(())
}

fn sibling_part_path(target: &Path) -> Result<PathBuf, String> {
    let name = target
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "目标目录名称无效".to_owned())?;
    Ok(target.with_file_name(format!("{name}.part")))
}

fn prepare_owned_directory(path: &Path) -> Result<(), String> {
    if path.exists() {
        remove_owned_directory(path)?;
    }
    fs::create_dir_all(path)
        .map_err(|error| format!("无法创建资源暂存目录 {}：{error}", path.display()))?;
    fs::write(path.join(STAGE_MARKER), b"dy-screen asr stage\n")
        .map_err(|error| format!("无法写入资源暂存标记：{error}"))
}

fn remove_owned_directory(path: &Path) -> Result<(), String> {
    if !path.join(STAGE_MARKER).is_file() {
        return Err(format!(
            "拒绝覆盖未标记为 ASR 暂存目录的路径：{}",
            path.display()
        ));
    }
    fs::remove_dir_all(path)
        .map_err(|error| format!("无法清理旧资源暂存目录 {}：{error}", path.display()))
}

#[cfg(all(test, unix, target_arch = "aarch64", target_os = "macos"))]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_executable(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;

        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn source_bundle() -> tempfile::TempDir {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let model = b"model";
        let vad = b"vad";
        let normalization = b"map";
        fs::create_dir_all(root.join("models")).unwrap();
        fs::create_dir_all(root.join("normalization")).unwrap();
        fs::create_dir_all(root.join("lib/macos-aarch64")).unwrap();
        fs::create_dir_all(root.join("licenses")).unwrap();
        fs::write(root.join("models/model.bin"), model).unwrap();
        fs::write(root.join("models/vad.bin"), vad).unwrap();
        fs::write(root.join("normalization/map.txt"), normalization).unwrap();
        fs::write(root.join("licenses/license.txt"), b"license").unwrap();
        let source = root.join("fixture.c");
        let binary = root.join("fixture");
        fs::write(
            &source,
            b"#include <stdio.h>\nint main(void) { puts(\"whisper.cpp version: 1.9.1\"); return 0; }\n",
        )
        .unwrap();
        let status = Command::new("clang")
            .args(["-arch", "arm64"])
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .status()
            .unwrap();
        assert!(status.success());
        fs::copy(&binary, root.join("lib/macos-aarch64/libfake.dylib")).unwrap();
        for name in ["whisper-cli", "vad", "ffmpeg", "ffprobe"] {
            let target = root.join("bin/macos-aarch64").join(name);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::copy(&binary, target).unwrap();
        }
        let manifest = serde_json::json!({
            "schemaVersion":1,"bundleVersion":"test",
            "engine":{"id":"whisper.cpp","version":"v1.9.1","sourceCommit":"test"},
            "model":{"logicalId":"small","version":"1","file":"models/model.bin","sizeBytes":model.len(),"sha256":hex::encode(Sha256::digest(model)),"source":"test","license":"MIT"},
            "vad":{"logicalId":"vad","version":"1","file":"models/vad.bin","sizeBytes":vad.len(),"sha256":hex::encode(Sha256::digest(vad)),"source":"test","license":"MIT"},
            "normalization":{"logicalId":"map","version":"1","file":"normalization/map.txt","sizeBytes":normalization.len(),"sha256":hex::encode(Sha256::digest(normalization)),"source":"test","license":"Apache-2.0"},
            "licenseFiles":["licenses/license.txt"],
            "platforms":[{"os":"macos","arch":"aarch64","accelerator":"metal","sidecar":"bin/macos-aarch64/whisper-cli","vadSidecar":"bin/macos-aarch64/vad","ffmpeg":"bin/macos-aarch64/ffmpeg","ffprobe":"bin/macos-aarch64/ffprobe","libraries":["lib/macos-aarch64/libfake.dylib"],"minimumMemoryBytes":8589934592_u64,"minimumFreeDiskBytes":2147483648_u64,"maximumThreads":4}]
        });
        fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        directory
    }

    #[test]
    fn stages_one_platform_and_rejects_corrupt_hashes() {
        let source = source_bundle();
        let output = tempdir().unwrap();
        let target = output.path().join("asr-stage");
        stage_bundle(source.path(), &target, BundlePlatform::MacosAarch64).unwrap();
        assert!(target.join("bin/macos-aarch64/whisper-cli").is_file());
        assert!(target.join("lib/macos-aarch64/libfake.dylib").is_file());
        assert!(target.join(STAGE_MARKER).is_file());
        let manifest = load_manifest(&target).unwrap();
        assert_eq!(manifest.platforms.len(), 1);
        assert_eq!(manifest.platforms[0].resource_integrity.len(), 5);
        write_executable(
            &target.join("bin/macos-aarch64/whisper-cli"),
            "#!/bin/sh\nprintf '%s\\n' 'tampered'\n",
        );
        assert!(verify_bundle(&target, BundlePlatform::MacosAarch64).is_err());

        stage_bundle(source.path(), &target, BundlePlatform::MacosAarch64).unwrap();
        fs::write(target.join("models/model.bin"), b"corrupt").unwrap();
        assert!(verify_bundle(&target, BundlePlatform::MacosAarch64).is_err());
    }
}

#[cfg(test)]
mod windows_resource_tests {
    use super::*;
    use tempfile::tempdir;

    fn fake_pe(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0u8; 0x80];
        bytes[0..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&(0x40u32).to_le_bytes());
        bytes[0x40..0x44].copy_from_slice(b"PE\0\0");
        bytes[0x44..0x46].copy_from_slice(&machine.to_le_bytes());
        bytes
    }

    fn fake_x64_pe() -> Vec<u8> {
        fake_pe(0x8664)
    }

    fn fake_x86_pe() -> Vec<u8> {
        fake_pe(0x014c)
    }

    fn write_file(root: &Path, relative: &str, bytes: &[u8]) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn windows_source(include_license: bool) -> tempfile::TempDir {
        let directory = tempdir().unwrap();
        let root = directory.path();
        let model = b"model";
        let vad = b"vad";
        let normalization = b"map";
        write_file(root, "models/model.bin", model);
        write_file(root, "models/vad.bin", vad);
        write_file(root, "normalization/map.txt", normalization);
        if include_license {
            write_file(root, "licenses/license.txt", b"license");
        }
        for relative in [
            "bin/windows-x86_64/whisper-cli.exe",
            "bin/windows-x86_64/vad-speech-segments.exe",
            "bin/windows-x86_64/ffmpeg.exe",
            "bin/windows-x86_64/ffprobe.exe",
        ] {
            write_file(root, relative, &fake_x64_pe());
        }
        write_file(
            root,
            "runtime/windows-x86_64/vc_redist.x64.exe",
            &fake_x86_pe(),
        );
        let manifest = serde_json::json!({
            "schemaVersion":1,
            "bundleVersion":"windows-test",
            "engine":{"id":"whisper.cpp","version":"v1.9.1","sourceCommit":"test"},
            "model":{"logicalId":"small","version":"1","file":"models/model.bin","sizeBytes":model.len(),"sha256":hex::encode(Sha256::digest(model)),"source":"test","license":"MIT"},
            "vad":{"logicalId":"vad","version":"1","file":"models/vad.bin","sizeBytes":vad.len(),"sha256":hex::encode(Sha256::digest(vad)),"source":"test","license":"MIT"},
            "normalization":{"logicalId":"map","version":"1","file":"normalization/map.txt","sizeBytes":normalization.len(),"sha256":hex::encode(Sha256::digest(normalization)),"source":"test","license":"Apache-2.0"},
            "licenseFiles": if include_license { vec!["licenses/license.txt"] } else { Vec::<&str>::new() },
            "platforms":[{
                "os":"windows","arch":"x86_64","accelerator":"cpu",
                "sidecar":"bin/windows-x86_64/whisper-cli.exe",
                "vadSidecar":"bin/windows-x86_64/vad-speech-segments.exe",
                "ffmpeg":"bin/windows-x86_64/ffmpeg.exe",
                "ffprobe":"bin/windows-x86_64/ffprobe.exe",
                "minimumMemoryBytes":8589934592_u64,
                "minimumFreeDiskBytes":2147483648_u64,
                "maximumThreads":4,
                "minimumCpuFeatures":["sse4.2"],
                "runtime":"Microsoft Visual C++ 2015-2022 Redistributable x64",
                "runtimeFile":"runtime/windows-x86_64/vc_redist.x64.exe"
            }]
        });
        fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        directory
    }

    #[test]
    fn stages_only_declared_x64_windows_resources() {
        let source = windows_source(true);
        let output = tempdir().unwrap();
        let target = output.path().join("windows-stage");
        stage_bundle(source.path(), &target, BundlePlatform::WindowsX86_64).unwrap();
        verify_bundle(&target, BundlePlatform::WindowsX86_64).unwrap();
        assert!(target.join("bin/windows-x86_64/ffmpeg.exe").is_file());

        write_file(&target, "bin/windows-x86_64/undeclared.dll", &fake_x64_pe());
        let error = verify_bundle(&target, BundlePlatform::WindowsX86_64).unwrap_err();
        assert!(error.contains("未声明原生文件"), "{error}");
    }

    #[test]
    fn rejects_runtime_manifest_hashes_that_do_not_match_the_staged_files() {
        let source = windows_source(true);
        let output = tempdir().unwrap();
        let target = output.path().join("windows-stage");
        stage_bundle(source.path(), &target, BundlePlatform::WindowsX86_64).unwrap();

        let path = target.join("runtime-manifest.json");
        let mut runtime: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        runtime["components"][0]["files"][0]["sha256"] = serde_json::Value::String("00".repeat(32));
        fs::write(&path, serde_json::to_vec_pretty(&runtime).unwrap()).unwrap();

        let error = verify_bundle(&target, BundlePlatform::WindowsX86_64).unwrap_err();
        assert!(
            error.contains("SHA-256") || error.contains("staging 不一致"),
            "{error}"
        );
    }

    #[test]
    fn rejects_non_x64_or_unlicensed_windows_sources_without_leaving_part_directory() {
        let source = windows_source(true);
        fs::write(
            source.path().join("bin/windows-x86_64/ffmpeg.exe"),
            b"not a PE",
        )
        .unwrap();
        let output = tempdir().unwrap();
        let target = output.path().join("windows-stage");
        let error =
            stage_bundle(source.path(), &target, BundlePlatform::WindowsX86_64).unwrap_err();
        assert!(error.contains("PE 文件"), "{error}");
        assert!(!target.exists());
        assert!(!target.with_file_name("windows-stage.part").exists());

        let unlicensed = windows_source(false);
        let error =
            stage_bundle(unlicensed.path(), &target, BundlePlatform::WindowsX86_64).unwrap_err();
        assert!(error.contains("许可证"), "{error}");
    }

    #[test]
    fn publishes_unsigned_windows_resources_only_to_explicit_development_directory() {
        let source = windows_source(true);
        let stage_parent = tempdir().unwrap();
        let stage = stage_parent.path().join("windows-stage");
        stage_bundle_for_channel(
            source.path(),
            &stage,
            BundlePlatform::WindowsX86_64,
            "development",
        )
        .unwrap();
        let stable_release = tempdir().unwrap();
        let stable_url = "https://resources.example/stable/0.2.0/windows/x86_64/windows-test/";
        let error = publish_bundle(
            &stage,
            stable_release.path(),
            BundlePlatform::WindowsX86_64,
            "stable",
            "0.2.0",
            stable_url,
            true,
        )
        .unwrap_err();
        assert!(error.contains("不得发布到 stable"), "{error}");

        let release = tempdir().unwrap();
        let base_url = "https://resources.example/development/0.2.0/windows/x86_64/windows-test/";
        publish_bundle(
            &stage,
            release.path(),
            BundlePlatform::WindowsX86_64,
            "development",
            "0.2.0",
            base_url,
            true,
        )
        .unwrap();
        let published = release
            .path()
            .join("development/0.2.0/windows/x86_64/windows-test");
        verify_bundle(&published, BundlePlatform::WindowsX86_64).unwrap();
        let index: serde_json::Value = serde_json::from_slice(
            &fs::read(release.path().join("development/0.2.0/index.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(index["resourceBaseUrl"], base_url);
        assert_eq!(index["official"], false);

        let stable_stage_parent = tempdir().unwrap();
        let stable_stage = stable_stage_parent.path().join("windows-stage");
        stage_bundle(source.path(), &stable_stage, BundlePlatform::WindowsX86_64).unwrap();
        let second_release = tempdir().unwrap();
        let error = publish_bundle(
            &stable_stage,
            second_release.path(),
            BundlePlatform::WindowsX86_64,
            "stable",
            "0.2.0",
            stable_url,
            false,
        )
        .unwrap_err();
        assert!(error.contains("占位或空签名"), "{error}");

        let third_release = tempdir().unwrap();
        let error = publish_bundle(
            &stage,
            third_release.path(),
            BundlePlatform::WindowsX86_64,
            "development",
            "0.2.0",
            "http://resources.example/development/0.2.0/windows/x86_64/windows-test/",
            true,
        )
        .unwrap_err();
        assert!(error.contains("基础 URL 无效"), "{error}");
    }
}
