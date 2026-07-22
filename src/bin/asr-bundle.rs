//! 离线暂存和校验单平台 ASR 随包资源的发行工具。
//!
//! 工具拒绝符号链接、绝对 RPATH、缺失完整性记录和跨平台混装，并使用原子目录替换发布。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::{Parser, Subcommand, ValueEnum};
use dy_screen::asr::{AsrBundleManifest, AsrFileIntegrityManifest, AsrPlatformManifest};
use sha2::{Digest, Sha256};

const STAGE_MARKER: &str = ".dy-screen-asr-stage";

/// 为 Tauri 安装包准备单一平台的完整 ASR 资源目录。
#[derive(Debug, Parser)]
#[command(
    name = "asr-bundle",
    version,
    about = "准备并校验直播管家随包 ASR 资源"
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
    },
    /// 校验一个待打包目录的 manifest、文件、哈希、权限和引擎版本。
    Verify {
        #[arg(long)]
        root: PathBuf,
        #[arg(long, value_enum)]
        platform: BundlePlatform,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BundlePlatform {
    MacosAarch64,
    WindowsX86_64,
}

impl BundlePlatform {
    fn target(self) -> (&'static str, &'static str) {
        match self {
            Self::MacosAarch64 => ("macos", "aarch64"),
            Self::WindowsX86_64 => ("windows", "x86_64"),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::MacosAarch64 => "macos-aarch64",
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
        } => stage_bundle(&source, &target, platform),
        BundleCommand::Verify { root, platform } => verify_bundle(&root, platform),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ASR 资源处理失败：{error}");
            ExitCode::FAILURE
        }
    }
}

fn stage_bundle(source: &Path, target: &Path, platform: BundlePlatform) -> Result<(), String> {
    if source == target {
        return Err("源目录与目标目录不能相同".to_owned());
    }
    let mut manifest = load_manifest(source)?;
    let mut selected = selected_platform(&manifest, platform)?.clone();
    selected.resource_integrity = seal_platform_resources(source, &selected)?;
    let temporary = sibling_part_path(target)?;
    prepare_owned_directory(&temporary)?;

    let stage_result = (|| {
        copy_and_verify_common_resources(source, &temporary, &manifest)?;
        copy_plain_resources(source, &temporary, &manifest.license_files)?;
        copy_platform_resources(source, &temporary, &selected)?;
        manifest.platforms = vec![selected];
        let json = serde_json::to_string_pretty(&manifest)
            .map_err(|error| format!("无法序列化资源清单：{error}"))?;
        fs::write(temporary.join("manifest.json"), format!("{json}\n"))
            .map_err(|error| format!("无法写入资源清单：{error}"))?;
        verify_bundle(&temporary, platform)
    })();
    stage_result?;

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

fn verify_bundle(root: &Path, platform: BundlePlatform) -> Result<(), String> {
    let manifest = load_manifest(root)?;
    if manifest.platforms.len() != 1 {
        return Err("待打包资源清单必须只包含一个目标平台".to_owned());
    }
    let selected = selected_platform(&manifest, platform)?;
    verify_common_resources(root, &manifest)?;
    verify_plain_resources(root, &manifest.license_files)?;
    verify_platform_resources(root, selected)?;
    verify_macos_relocatability_when_applicable(root, selected)?;
    verify_engine_version_when_runnable(root, &manifest, selected)?;
    println!(
        "ASR 资源校验通过：平台={}，目录={}",
        platform.label(),
        root.display()
    );
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
        fs::write(root.join("models/model.bin"), model).unwrap();
        fs::write(root.join("models/vad.bin"), vad).unwrap();
        fs::write(root.join("normalization/map.txt"), normalization).unwrap();
        fs::write(root.join("lib/macos-aarch64/libfake.dylib"), b"library").unwrap();
        for name in ["whisper-cli", "vad", "ffmpeg", "ffprobe"] {
            let body = if name == "whisper-cli" {
                "#!/bin/sh\nprintf '%s\\n' 'whisper.cpp version: 1.9.1'\n"
            } else {
                "#!/bin/sh\nexit 0\n"
            };
            write_executable(&root.join("bin/macos-aarch64").join(name), body);
        }
        let manifest = serde_json::json!({
            "schemaVersion":1,"bundleVersion":"test",
            "engine":{"id":"whisper.cpp","version":"v1.9.1","sourceCommit":"test"},
            "model":{"logicalId":"small","version":"1","file":"models/model.bin","sizeBytes":model.len(),"sha256":hex::encode(Sha256::digest(model)),"source":"test","license":"MIT"},
            "vad":{"logicalId":"vad","version":"1","file":"models/vad.bin","sizeBytes":vad.len(),"sha256":hex::encode(Sha256::digest(vad)),"source":"test","license":"MIT"},
            "normalization":{"logicalId":"map","version":"1","file":"normalization/map.txt","sizeBytes":normalization.len(),"sha256":hex::encode(Sha256::digest(normalization)),"source":"test","license":"Apache-2.0"},
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
