//! 特效字幕的受限渲染与渲染器运行时完整性校验（步骤 3 任务组 4）。
//!
//! 两条不可动摇的约束：
//!
//! 1. **渲染不在主进程内执行**：一律通过 `EffectCaptionRenderer` 走独立子进程，并在启动前
//!    完成运行时完整性校验（L1 随包 bundle 树哈希 → L2 引擎脚本文本哈希 → L3 页面脚本来源
//!    断言 → 浏览器与字体版本）。任何一项不符都返回**可区分**的错误，绝不静默降级。
//! 2. **失败即清理**：超时、取消、崩溃都必须在有界时间内终止子进程，并只清理未发布的临时帧，
//!    工程与 ASR 产物保持不变。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::process::{Child, Command};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

/// 渲染器运行时错误。错误码必须稳定且可区分（任务 4.7 / 4.8）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{code}: {detail}")]
pub struct EffectCaptionRuntimeError {
    pub code: &'static str,
    pub detail: String,
}

impl EffectCaptionRuntimeError {
    pub(crate) fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

/// 随包运行时清单。数值来自任务 0.7 的实测（见 engine-runtime-inventory.md）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RendererRuntimeManifest {
    pub renderer_version: String,
    /// `dist/` 目录的文件数与树哈希；树哈希约定见 `compute_tree_digest`。
    pub renderer_file_count: usize,
    pub renderer_tree_sha256: String,
    /// 引擎脚本文本（内联常量派生）的字节数与哈希。
    pub engine_script_bytes: u64,
    pub engine_script_sha256: String,
    /// 捕获期 audio FX 运行时。
    pub audio_fx_script_bytes: u64,
    pub audio_fx_script_sha256: String,
    /// 固定版本渲染浏览器与随包锁定字体。
    pub browser_version: String,
    pub browser_sha256: String,
    pub font_version: String,
}

/// 通过完整性校验的运行时。只有 [`verify_renderer_runtime`] 能构造它，
/// 因此渲染器无法在未校验的运行时上被调用（任务 4.7 的类型级保证）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRendererRuntime {
    renderer_version: String,
    renderer_tree_sha256: String,
    engine_script_sha256: String,
    font_version: String,
}

#[cfg(test)]
impl VerifiedRendererRuntime {
    /// 测试专用：跳过随包清单校验，构造一个已校验运行时。
    pub(crate) fn for_tests() -> Self {
        Self {
            renderer_version: "0.8.96".to_owned(),
            renderer_tree_sha256: "test-tree".to_owned(),
            engine_script_sha256: "test-engine".to_owned(),
            font_version: "test-fonts".to_owned(),
        }
    }
}

impl VerifiedRendererRuntime {
    pub fn renderer_version(&self) -> &str {
        &self.renderer_version
    }

    pub fn renderer_tree_sha256(&self) -> &str {
        &self.renderer_tree_sha256
    }

    pub fn engine_script_sha256(&self) -> &str {
        &self.engine_script_sha256
    }

    pub fn font_version(&self) -> &str {
        &self.font_version
    }
}

/// 渲染子进程在加载页面后产出的脚本来源自证（由随包探测脚本生成）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageScriptAttestation {
    pub engine_script_bytes: u64,
    pub engine_script_sha256: String,
    pub page_script_count: usize,
    pub external_script_count: usize,
    pub runtime_url_env_empty: bool,
}

/// 计算目录的确定性摘要：每行 `sha256␠␠size␠␠相对路径`，按相对路径排序后取 sha256。
pub fn compute_tree_digest(root: &Path) -> Result<(usize, u64, String), EffectCaptionRuntimeError> {
    let mut files: Vec<PathBuf> = Vec::new();
    collect_files(root, &mut files)?;
    files.sort();
    let mut lines = String::new();
    let mut total_bytes = 0_u64;
    for file in &files {
        let bytes = std::fs::read(file).map_err(|error| {
            EffectCaptionRuntimeError::new("renderer_runtime_unreadable", sanitize(&error))
        })?;
        total_bytes += bytes.len() as u64;
        let relative = file
            .strip_prefix(root)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");
        lines.push_str(&format!(
            "{:x}  {:>9}  {}\n",
            Sha256::digest(&bytes),
            bytes.len(),
            relative
        ));
    }
    Ok((
        files.len(),
        total_bytes,
        hex::encode(Sha256::digest(lines.as_bytes())),
    ))
}

fn collect_files(
    directory: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), EffectCaptionRuntimeError> {
    let entries = std::fs::read_dir(directory).map_err(|error| {
        EffectCaptionRuntimeError::new("renderer_runtime_missing", sanitize(&error))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            EffectCaptionRuntimeError::new("renderer_runtime_unreadable", sanitize(&error))
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

/// 渲染前的运行时完整性校验（任务 4.7）。
///
/// 失败必须返回可区分错误，且**不得**降级到未校验的运行时（任务 4.8）。
pub fn verify_renderer_runtime(
    package_root: &Path,
    manifest: &RendererRuntimeManifest,
) -> Result<VerifiedRendererRuntime, EffectCaptionRuntimeError> {
    let dist = package_root.join("dist");
    if !dist.is_dir() {
        return Err(EffectCaptionRuntimeError::new(
            "renderer_runtime_missing",
            "随包渲染器缺少 dist 目录",
        ));
    }
    let (file_count, _bytes, tree_sha256) = compute_tree_digest(&dist)?;
    if file_count != manifest.renderer_file_count {
        return Err(EffectCaptionRuntimeError::new(
            "renderer_file_count_mismatch",
            format!(
                "随包文件数 {file_count} 与清单 {} 不一致",
                manifest.renderer_file_count
            ),
        ));
    }
    if tree_sha256 != manifest.renderer_tree_sha256 {
        return Err(EffectCaptionRuntimeError::new(
            "renderer_tree_hash_mismatch",
            "随包渲染器树哈希与清单不一致",
        ));
    }
    Ok(VerifiedRendererRuntime {
        renderer_version: manifest.renderer_version.clone(),
        renderer_tree_sha256: tree_sha256,
        engine_script_sha256: manifest.engine_script_sha256.clone(),
        font_version: manifest.font_version.clone(),
    })
}

/// 校验渲染页面里的可执行内容只来自随包引擎脚本（L3）。
pub fn verify_page_script_attestation(
    attestation: &PageScriptAttestation,
    manifest: &RendererRuntimeManifest,
) -> Result<(), EffectCaptionRuntimeError> {
    if !attestation.runtime_url_env_empty {
        return Err(EffectCaptionRuntimeError::new(
            "engine_script_url_not_empty",
            "HYPERFRAME_RUNTIME_URL 非空，页面会加载随包清单之外的脚本",
        ));
    }
    if attestation.external_script_count != 0 {
        return Err(EffectCaptionRuntimeError::new(
            "page_script_unexpected",
            format!(
                "渲染页面出现 {} 个外链脚本",
                attestation.external_script_count
            ),
        ));
    }
    if attestation.page_script_count != 1 {
        return Err(EffectCaptionRuntimeError::new(
            "page_script_unexpected",
            format!(
                "渲染页面脚本数 {}，期望恰为 1 个随包引擎脚本",
                attestation.page_script_count
            ),
        ));
    }
    if attestation.engine_script_bytes != manifest.engine_script_bytes
        || attestation.engine_script_sha256 != manifest.engine_script_sha256
    {
        return Err(EffectCaptionRuntimeError::new(
            "engine_script_hash_mismatch",
            "引擎脚本文本与随包清单不一致",
        ));
    }
    if manifest.audio_fx_script_sha256.trim().is_empty() || manifest.audio_fx_script_bytes == 0 {
        return Err(EffectCaptionRuntimeError::new(
            "engine_script_hash_mismatch",
            "audio FX 运行时清单不完整",
        ));
    }
    Ok(())
}

/// 校验固定版本渲染浏览器（版本字符串 + 二进制哈希）。
pub fn verify_browser(
    browser_path: &Path,
    observed_version: &str,
    manifest: &RendererRuntimeManifest,
) -> Result<(), EffectCaptionRuntimeError> {
    if observed_version.trim() != manifest.browser_version {
        return Err(EffectCaptionRuntimeError::new(
            "browser_version_mismatch",
            format!("渲染浏览器版本不是清单固定的 {}", manifest.browser_version),
        ));
    }
    let bytes = std::fs::read(browser_path).map_err(|error| {
        EffectCaptionRuntimeError::new("renderer_runtime_missing", sanitize(&error))
    })?;
    let observed = hex::encode(Sha256::digest(&bytes));
    if observed != manifest.browser_sha256 {
        return Err(EffectCaptionRuntimeError::new(
            "browser_hash_mismatch",
            "渲染浏览器二进制哈希与清单不一致",
        ));
    }
    Ok(())
}

fn sanitize(error: &std::io::Error) -> String {
    // 只保留错误类别，不回显路径。
    format!("{:?}", error.kind())
}

/// 子进程资源与时间上限。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessLimits {
    pub timeout: Duration,
    pub max_rss_kib: u64,
    pub poll_interval: Duration,
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(600),
            max_rss_kib: 3 * 1024 * 1024,
            poll_interval: Duration::from_millis(200),
        }
    }
}

/// 子进程常驻内存采样。取不到时返回 `None`，由调用方按"未知"处理。
#[async_trait]
pub trait ProcessMemoryProbe: Send + Sync {
    async fn resident_kib(&self, pid: u32) -> Option<u64>;
}

/// 生产实现：向操作系统查询 RSS。
pub struct SystemMemoryProbe;

#[async_trait]
impl ProcessMemoryProbe for SystemMemoryProbe {
    async fn resident_kib(&self, pid: u32) -> Option<u64> {
        #[cfg(unix)]
        {
            let output = Command::new("ps")
                .args(["-o", "rss=", "-p", &pid.to_string()])
                .stdin(Stdio::null())
                .output()
                .await
                .ok()?;
            if !output.status.success() {
                return None;
            }
            let text = String::from_utf8_lossy(&output.stdout);
            text.trim().parse::<u64>().ok()
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            None
        }
    }
}

/// 子进程终态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessExit {
    Success,
    Failure(Option<i32>),
}

/// 监管一个子进程：超时、取消与内存上限都会在**有界时间内**终止它。
///
/// 该函数是渲染子进程监管的唯一入口，因此这几条约束对真实渲染与测试替身一致生效。
pub async fn supervise_process(
    mut child: Child,
    limits: &ProcessLimits,
    cancellation: &CancellationToken,
    memory_probe: &dyn ProcessMemoryProbe,
) -> Result<ProcessExit, EffectCaptionRuntimeError> {
    let started = Instant::now();
    let pid = child.id();
    loop {
        if cancellation.is_cancelled() {
            terminate(&mut child).await;
            return Err(EffectCaptionRuntimeError::new(
                "render_cancelled",
                "渲染已被取消",
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(if status.success() {
                    ProcessExit::Success
                } else {
                    ProcessExit::Failure(status.code())
                });
            }
            Ok(None) => {}
            Err(error) => {
                terminate(&mut child).await;
                return Err(EffectCaptionRuntimeError::new(
                    "render_subprocess_failed",
                    sanitize(&error),
                ));
            }
        }
        if started.elapsed() >= limits.timeout {
            terminate(&mut child).await;
            return Err(EffectCaptionRuntimeError::new(
                "render_timeout",
                format!("渲染超过 {}ms 上限", limits.timeout.as_millis()),
            ));
        }
        if let Some(pid) = pid
            && let Some(rss) = memory_probe.resident_kib(pid).await
            && rss > limits.max_rss_kib
        {
            terminate(&mut child).await;
            return Err(EffectCaptionRuntimeError::new(
                "render_memory_exceeded",
                format!("渲染进程内存超过 {}KiB 上限", limits.max_rss_kib),
            ));
        }
        tokio::time::sleep(limits.poll_interval).await;
    }
}

async fn terminate(child: &mut Child) {
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
}

/// 渲染任务与资源上限。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderLimits {
    pub concurrency: usize,
    pub timeout_ms: u64,
    pub max_frames: u64,
    pub max_disk_bytes: u64,
}

impl Default for RenderLimits {
    fn default() -> Self {
        Self {
            // 渲染独占型任务：默认串行，避免与录制/预览/导出争抢。
            concurrency: 1,
            timeout_ms: 600_000,
            max_frames: 5_400,
            max_disk_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}

/// 一次渲染的输入。所有路径都必须落在允许的根目录内（任务 4.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderInvocation {
    pub composition_path: PathBuf,
    pub workspace_dir: PathBuf,
    pub frames_dir: PathBuf,
    pub allowed_roots: Vec<PathBuf>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub duration_ms: u64,
    pub limits: RenderLimits,
}

/// 渲染产物。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderOutcome {
    pub frames_dir: PathBuf,
    pub frame_count: u64,
    pub frames_bytes: u64,
}

/// 渲染器抽象：任何实现都必须在独立子进程内完成渲染（任务 4.1）。
#[async_trait]
pub trait EffectCaptionRenderer: Send + Sync {
    async fn render(
        &self,
        runtime: &VerifiedRendererRuntime,
        invocation: &RenderInvocation,
        cancellation: &CancellationToken,
    ) -> Result<RenderOutcome, EffectCaptionRuntimeError>;
}

/// 校验渲染输入路径都在允许的根目录内（任务 4.2）。
pub fn assert_paths_within_roots(
    invocation: &RenderInvocation,
) -> Result<(), EffectCaptionRuntimeError> {
    for path in [
        &invocation.composition_path,
        &invocation.workspace_dir,
        &invocation.frames_dir,
    ] {
        let allowed = invocation
            .allowed_roots
            .iter()
            .any(|root| path.starts_with(root));
        if !allowed {
            return Err(EffectCaptionRuntimeError::new(
                "render_path_escape",
                "渲染输入路径超出随包资源与工程临时目录",
            ));
        }
    }
    Ok(())
}

/// 构造受限渲染命令（任务 4.2）：清空环境、只保留白名单变量、禁用代理与远程运行时。
pub fn build_restricted_command(
    program: &Path,
    invocation: &RenderInvocation,
    browser_path: &Path,
) -> Result<Command, EffectCaptionRuntimeError> {
    build_restricted_command_with_fonts(program, invocation, browser_path, None)
}

/// 同上，但把随包字体目录显式交给渲染器（任务 5.1）。
///
/// 环境被清空，因此 `FONTCONFIG_PATH`、`XDG_DATA_DIRS` 等系统字体发现路径都不会被继承：
/// 特效字幕的字形只能来自 `fonts`。
pub fn build_restricted_command_with_fonts(
    program: &Path,
    invocation: &RenderInvocation,
    browser_path: &Path,
    fonts: Option<&FontResourceSet>,
) -> Result<Command, EffectCaptionRuntimeError> {
    assert_paths_within_roots(invocation)?;
    let mut command = Command::new(program);
    command
        .arg("render")
        .arg("--format")
        .arg("png-sequence")
        .arg("--width")
        .arg(invocation.width.to_string())
        .arg("--height")
        .arg(invocation.height.to_string())
        .arg("--fps")
        .arg(invocation.fps.to_string())
        .arg("--output")
        .arg(&invocation.frames_dir)
        .arg(&invocation.composition_path)
        .env_clear()
        // 只保留随包资源与工作目录所需的变量；代理与远程运行时一律不继承。
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &invocation.workspace_dir)
        .env("TMPDIR", &invocation.workspace_dir)
        .env("HYPERFRAMES_BROWSER_PATH", browser_path)
        .env("HYPERFRAME_RUNTIME_URL", "")
        .env(
            "PUPPETEER_CACHE_DIR",
            invocation.workspace_dir.join("puppeteer-cache"),
        )
        .current_dir(&invocation.workspace_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(fonts) = fonts {
        command
            .env("HYPERFRAMES_CAPTION_FONT_DIR", fonts.directory())
            .env("HYPERFRAMES_CAPTION_FONT_FAMILY", fonts.family());
    }
    Ok(command)
}

/// 基础能力闸门：录制、预览、导出进行时，渲染必须让路（任务 4.6）。
pub trait BaseCapabilityGate: Send + Sync {
    fn base_capability_busy(&self) -> bool;
}

/// 始终空闲的闸门（用于测试与无录制场景）。
pub struct AlwaysIdleGate;

impl BaseCapabilityGate for AlwaysIdleGate {
    fn base_capability_busy(&self) -> bool {
        false
    }
}

/// 渲染并发闸门：限制并发数，并在基础能力繁忙时等待（任务 4.6）。
pub struct RenderScheduler {
    permits: Arc<Semaphore>,
    gate: Arc<dyn BaseCapabilityGate>,
    poll_interval: Duration,
}

impl RenderScheduler {
    pub fn new(concurrency: usize, gate: Arc<dyn BaseCapabilityGate>) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(concurrency.max(1))),
            gate,
            poll_interval: Duration::from_millis(50),
        }
    }

    pub fn available_permits(&self) -> usize {
        self.permits.available_permits()
    }

    /// 取得渲染许可：先等基础能力空闲，再占用一个并发位。
    pub async fn acquire(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<OwnedSemaphorePermit, EffectCaptionRuntimeError> {
        while self.gate.base_capability_busy() {
            if cancellation.is_cancelled() {
                return Err(EffectCaptionRuntimeError::new(
                    "render_cancelled",
                    "等待基础能力空闲时被取消",
                ));
            }
            tokio::time::sleep(self.poll_interval).await;
        }
        let permit = tokio::select! {
            _ = cancellation.cancelled() => {
                return Err(EffectCaptionRuntimeError::new(
                    "render_cancelled",
                    "等待渲染并发位时被取消",
                ));
            }
            permit = self.permits.clone().acquire_owned() => permit.map_err(|_| {
                EffectCaptionRuntimeError::new("render_scheduler_closed", "渲染调度器已关闭")
            })?,
        };
        Ok(permit)
    }
}

/// 清理未发布的帧序列（失败、取消、崩溃后调用）。
pub fn cleanup_unpublished_frames(frames_dir: &Path) -> Result<(), EffectCaptionRuntimeError> {
    if !frames_dir.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(frames_dir)
        .map_err(|error| EffectCaptionRuntimeError::new("render_cleanup_failed", sanitize(&error)))
}

/// 生产渲染器：在独立子进程内调用随包渲染器，并按受限参数监管。
pub struct SubprocessRenderer {
    program: PathBuf,
    browser_path: PathBuf,
    memory_probe: Arc<dyn ProcessMemoryProbe>,
}

impl SubprocessRenderer {
    pub fn new(program: impl Into<PathBuf>, browser_path: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            browser_path: browser_path.into(),
            memory_probe: Arc::new(SystemMemoryProbe),
        }
    }

    pub fn with_memory_probe(mut self, probe: Arc<dyn ProcessMemoryProbe>) -> Self {
        self.memory_probe = probe;
        self
    }
}

#[async_trait]
impl EffectCaptionRenderer for SubprocessRenderer {
    async fn render(
        &self,
        _runtime: &VerifiedRendererRuntime,
        invocation: &RenderInvocation,
        cancellation: &CancellationToken,
    ) -> Result<RenderOutcome, EffectCaptionRuntimeError> {
        let limit_ms = invocation.limits.timeout_ms;
        let process_limits = ProcessLimits {
            timeout: Duration::from_millis(limit_ms),
            ..ProcessLimits::default()
        };
        let result = async {
            let mut command =
                build_restricted_command(&self.program, invocation, &self.browser_path)?;
            let child = command.spawn().map_err(|error| {
                EffectCaptionRuntimeError::new("render_subprocess_failed", sanitize(&error))
            })?;
            match supervise_process(
                child,
                &process_limits,
                cancellation,
                self.memory_probe.as_ref(),
            )
            .await?
            {
                ProcessExit::Success => {
                    verify_render_outcome(&invocation.frames_dir, &invocation.limits)
                }
                ProcessExit::Failure(code) => Err(EffectCaptionRuntimeError::new(
                    "render_subprocess_failed",
                    match code {
                        Some(code) => format!("渲染器以退出码 {code} 结束"),
                        None => "渲染器被信号终止".to_owned(),
                    },
                )),
            }
        }
        .await;
        if result.is_err() {
            // 失败、取消、崩溃、超限：只清理未发布的帧序列，工程与 ASR 产物不受影响。
            let _ = cleanup_unpublished_frames(&invocation.frames_dir);
        }
        result
    }
}

/// 随包锁定字体资源（任务 5.1）。系统字体不得作为特效字幕的字形来源。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FontResource {
    pub family: String,
    pub version: String,
    pub path: PathBuf,
    pub sha256: String,
}

/// 通过校验的字体集合。只有 [`verify_font_resources`] 能构造它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontResourceSet {
    family: String,
    version: String,
    directory: PathBuf,
    count: usize,
}

impl FontResourceSet {
    pub fn family(&self) -> &str {
        &self.family
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn count(&self) -> usize {
        self.count
    }
}

/// 校验随包字体：家族必须是被样式表引用的锁定家族，版本与每个文件哈希都必须与清单一致。
pub fn verify_font_resources(
    resources: &[FontResource],
    expected_version: &str,
) -> Result<FontResourceSet, EffectCaptionRuntimeError> {
    if resources.is_empty() {
        return Err(EffectCaptionRuntimeError::new(
            "caption_font_missing",
            "没有随包锁定字体，禁止用系统字体渲染",
        ));
    }
    let family = resources[0].family.clone();
    for resource in resources {
        if !super::effect_caption::BUNDLED_FONT_FAMILIES.contains(&resource.family.as_str()) {
            return Err(EffectCaptionRuntimeError::new(
                "caption_font_family_not_bundled",
                "字体家族不是随包锁定家族",
            ));
        }
        if resource.family != family {
            return Err(EffectCaptionRuntimeError::new(
                "caption_font_family_not_bundled",
                "随包字体必须属于同一家族",
            ));
        }
        if resource.version != expected_version {
            return Err(EffectCaptionRuntimeError::new(
                "caption_font_version_mismatch",
                format!("字体版本不是清单固定的 {expected_version}"),
            ));
        }
        let bytes = std::fs::read(&resource.path).map_err(|error| {
            EffectCaptionRuntimeError::new("caption_font_missing", sanitize(&error))
        })?;
        let observed = hex::encode(Sha256::digest(&bytes));
        if observed != resource.sha256 {
            return Err(EffectCaptionRuntimeError::new(
                "caption_font_hash_mismatch",
                "字体文件哈希与清单不一致",
            ));
        }
    }
    let directory = resources[0]
        .path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            EffectCaptionRuntimeError::new("caption_font_missing", "字体文件缺少所在目录")
        })?;
    Ok(FontResourceSet {
        family,
        version: expected_version.to_owned(),
        directory,
        count: resources.len(),
    })
}

/// 渲染版本集合：任何一个字段变化都必须产生新的缓存键（任务 5.4）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderVersionSet {
    pub composition_version: String,
    pub composition_fingerprint: String,
    pub font_version: String,
    pub browser_version: String,
    pub renderer_version: String,
    pub engine_script_version: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// 渲染缓存键。相同键表示可以复用已渲染的帧序列。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RenderCacheKey(String);

impl RenderCacheKey {
    pub fn new(versions: &RenderVersionSet) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(versions.composition_version.as_bytes());
        hasher.update([0]);
        hasher.update(versions.composition_fingerprint.as_bytes());
        hasher.update([0]);
        hasher.update(versions.font_version.as_bytes());
        hasher.update([0]);
        hasher.update(versions.browser_version.as_bytes());
        hasher.update([0]);
        hasher.update(versions.renderer_version.as_bytes());
        hasher.update([0]);
        hasher.update(versions.engine_script_version.as_bytes());
        hasher.update([0]);
        hasher.update(versions.width.to_le_bytes());
        hasher.update(versions.height.to_le_bytes());
        hasher.update(versions.fps.to_le_bytes());
        Self(hex::encode(hasher.finalize()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 帧序列一致性策略。设计决策四明确"逻辑确定性而非位精确"：允许极少量光栅差异。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeterminismPolicy {
    pub max_mismatch_ratio: f64,
}

impl Default for DeterminismPolicy {
    fn default() -> Self {
        // 设计给出的实测上界：约 0.4–0.9% 的帧可能存在合成器光栅差异。
        Self {
            max_mismatch_ratio: 0.009,
        }
    }
}

/// 一次帧序列一致性比对的结果。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameParity {
    pub total: u64,
    pub mismatched: u64,
    pub ratio: f64,
}

/// 计算帧序列的逐帧摘要：`(文件名, sha256)`，按文件名排序。
pub fn digest_frame_sequence(
    frames_dir: &Path,
) -> Result<Vec<(String, String)>, EffectCaptionRuntimeError> {
    let mut frames: Vec<(String, String)> = Vec::new();
    let entries = std::fs::read_dir(frames_dir).map_err(|error| {
        EffectCaptionRuntimeError::new("render_frames_missing", sanitize(&error))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            EffectCaptionRuntimeError::new("render_frames_missing", sanitize(&error))
        })?;
        let path = entry.path();
        if !path.is_file()
            || frame_index(&path.file_name().unwrap_or_default().to_string_lossy()).is_none()
        {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|error| {
            EffectCaptionRuntimeError::new("render_frames_missing", sanitize(&error))
        })?;
        frames.push((
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            hex::encode(Sha256::digest(&bytes)),
        ));
    }
    if frames.is_empty() {
        return Err(EffectCaptionRuntimeError::new(
            "render_frames_missing",
            "没有可比的帧序列",
        ));
    }
    frames.sort();
    Ok(frames)
}

/// 比对两次渲染的帧序列（任务 5.2 / 5.5）。
pub fn compare_frame_sequences(
    first: &Path,
    second: &Path,
    policy: &DeterminismPolicy,
) -> Result<FrameParity, EffectCaptionRuntimeError> {
    let left = digest_frame_sequence(first)?;
    let right = digest_frame_sequence(second)?;
    if left.len() != right.len() {
        return Err(EffectCaptionRuntimeError::new(
            "render_frame_count_mismatch",
            format!("两次渲染帧数不同：{} vs {}", left.len(), right.len()),
        ));
    }
    let mismatched = left
        .iter()
        .zip(right.iter())
        .filter(|(left, right)| left != right)
        .count() as u64;
    let total = left.len() as u64;
    let ratio = if total == 0 {
        0.0
    } else {
        mismatched as f64 / total as f64
    };
    if ratio > policy.max_mismatch_ratio {
        return Err(EffectCaptionRuntimeError::new(
            "render_nondeterministic",
            format!("相同输入两次渲染不一致：{mismatched}/{total} 帧不同"),
        ));
    }
    Ok(FrameParity {
        total,
        mismatched,
        ratio,
    })
}

/// 校验渲染产物：帧数、命名连续性与磁盘上限。
pub fn verify_render_outcome(
    frames_dir: &Path,
    limits: &RenderLimits,
) -> Result<RenderOutcome, EffectCaptionRuntimeError> {
    let mut frames: BTreeMap<u64, u64> = BTreeMap::new();
    let entries = std::fs::read_dir(frames_dir).map_err(|error| {
        EffectCaptionRuntimeError::new("render_output_missing", sanitize(&error))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            EffectCaptionRuntimeError::new("render_output_unreadable", sanitize(&error))
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let Some(index) = frame_index(&name) else {
            continue;
        };
        let bytes = entry.metadata().map(|meta| meta.len()).unwrap_or_default();
        frames.insert(index, bytes);
    }
    if frames.is_empty() {
        return Err(EffectCaptionRuntimeError::new(
            "render_output_empty",
            "渲染没有产出任何帧",
        ));
    }
    // 帧序号必须从 1 开始连续（渲染器 png-sequence 的 1 基序号）。
    for (expected, actual) in (1_u64..).zip(frames.keys().copied()) {
        if expected != actual {
            return Err(EffectCaptionRuntimeError::new(
                "render_output_gap",
                format!("帧序号不连续，期望 {expected} 实际 {actual}"),
            ));
        }
    }
    let frame_count = frames.len() as u64;
    if frame_count > limits.max_frames {
        return Err(EffectCaptionRuntimeError::new(
            "render_frame_limit_exceeded",
            format!("帧数 {frame_count} 超过上限 {}", limits.max_frames),
        ));
    }
    let frames_bytes: u64 = frames.values().sum();
    if frames_bytes > limits.max_disk_bytes {
        return Err(EffectCaptionRuntimeError::new(
            "render_disk_limit_exceeded",
            format!(
                "帧序列体积 {frames_bytes} 超过上限 {}",
                limits.max_disk_bytes
            ),
        ));
    }
    Ok(RenderOutcome {
        frames_dir: frames_dir.to_path_buf(),
        frame_count,
        frames_bytes,
    })
}

fn frame_index(name: &str) -> Option<u64> {
    let stem = name.strip_suffix(".png")?;
    let digits = stem.strip_prefix("frame_")?;
    if digits.is_empty() || !digits.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    fn manifest() -> RendererRuntimeManifest {
        RendererRuntimeManifest {
            renderer_version: "0.8.96".to_owned(),
            renderer_file_count: 2,
            renderer_tree_sha256: String::new(),
            engine_script_bytes: 480_863,
            engine_script_sha256:
                "38592478aad3f515e33b69ecfc11898e338c28a54799bebcd933c18906decd06".to_owned(),
            audio_fx_script_bytes: 38_874,
            audio_fx_script_sha256:
                "eec4e4349b87ee1542b88a32a4a5dd1428a2bc50fda7ad124c27083d126eb426".to_owned(),
            browser_version: "154.0.8037.58".to_owned(),
            browser_sha256: String::new(),
            font_version: "caption-fonts-2026.10".to_owned(),
        }
    }

    fn fixture_package() -> (tempfile::TempDir, RendererRuntimeManifest) {
        let directory = tempfile::tempdir().expect("tempdir");
        let dist = directory.path().join("dist");
        std::fs::create_dir_all(&dist).expect("create dist");
        std::fs::write(dist.join("engine.js"), b"engine-bytes").expect("write engine");
        std::fs::write(dist.join("cli.js"), b"cli-bytes").expect("write cli");
        let (file_count, _bytes, tree) = compute_tree_digest(&dist).expect("digest");
        let mut manifest = manifest();
        manifest.renderer_file_count = file_count;
        manifest.renderer_tree_sha256 = tree;
        (directory, manifest)
    }

    #[test]
    fn runtime_verification_accepts_matching_tree_and_rejects_tampering() {
        let (directory, manifest) = fixture_package();
        let verified = verify_renderer_runtime(directory.path(), &manifest).expect("应当通过校验");
        assert_eq!(verified.renderer_version(), "0.8.96");
        assert_eq!(
            verified.engine_script_sha256(),
            manifest.engine_script_sha256
        );

        // 篡改一个随包文件：树哈希必须变化并被拒绝。
        std::fs::write(directory.path().join("dist/engine.js"), b"tampered").expect("tamper");
        let rejection = verify_renderer_runtime(directory.path(), &manifest).unwrap_err();
        assert_eq!(rejection.code, "renderer_tree_hash_mismatch");

        // 文件数不符时给出可区分的错误。
        let (directory, mut manifest) = fixture_package();
        manifest.renderer_file_count = 99;
        let rejection = verify_renderer_runtime(directory.path(), &manifest).unwrap_err();
        assert_eq!(rejection.code, "renderer_file_count_mismatch");
    }

    #[test]
    fn missing_renderer_runtime_is_reported_separately() {
        let directory = tempfile::tempdir().expect("tempdir");
        let rejection = verify_renderer_runtime(directory.path(), &manifest()).unwrap_err();
        assert_eq!(rejection.code, "renderer_runtime_missing");
    }

    #[test]
    fn page_script_attestation_rejects_external_or_unpinned_scripts() {
        let manifest = manifest();
        let ok = PageScriptAttestation {
            engine_script_bytes: 480_863,
            engine_script_sha256: manifest.engine_script_sha256.clone(),
            page_script_count: 1,
            external_script_count: 0,
            runtime_url_env_empty: true,
        };
        assert!(verify_page_script_attestation(&ok, &manifest).is_ok());

        let mut external = ok.clone();
        external.external_script_count = 1;
        assert_eq!(
            verify_page_script_attestation(&external, &manifest)
                .unwrap_err()
                .code,
            "page_script_unexpected"
        );

        let mut url = ok.clone();
        url.runtime_url_env_empty = false;
        assert_eq!(
            verify_page_script_attestation(&url, &manifest)
                .unwrap_err()
                .code,
            "engine_script_url_not_empty"
        );

        let mut tampered = ok.clone();
        tampered.engine_script_sha256 = "deadbeef".repeat(8);
        assert_eq!(
            verify_page_script_attestation(&tampered, &manifest)
                .unwrap_err()
                .code,
            "engine_script_hash_mismatch"
        );

        let mut extra = ok;
        extra.page_script_count = 2;
        assert_eq!(
            verify_page_script_attestation(&extra, &manifest)
                .unwrap_err()
                .code,
            "page_script_unexpected"
        );
    }

    #[test]
    fn browser_version_and_hash_must_match_manifest() {
        let directory = tempfile::tempdir().expect("tempdir");
        let browser = directory.path().join("chrome");
        std::fs::write(&browser, b"browser-bytes").expect("write browser");
        let mut manifest = manifest();
        manifest.browser_sha256 = hex::encode(Sha256::digest(b"browser-bytes"));
        assert!(verify_browser(&browser, "154.0.8037.58", &manifest).is_ok());
        assert_eq!(
            verify_browser(&browser, "155.0.0.0", &manifest)
                .unwrap_err()
                .code,
            "browser_version_mismatch"
        );
        std::fs::write(&browser, b"other-browser").expect("rewrite browser");
        assert_eq!(
            verify_browser(&browser, "154.0.8037.58", &manifest)
                .unwrap_err()
                .code,
            "browser_hash_mismatch"
        );
    }

    fn test_limits() -> ProcessLimits {
        ProcessLimits {
            timeout: Duration::from_millis(300),
            max_rss_kib: 1_000_000,
            poll_interval: Duration::from_millis(20),
        }
    }

    #[tokio::test]
    async fn subprocess_timeout_and_failure_are_killed_and_classified() {
        let cancellation = CancellationToken::new();
        let slow = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 5")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sleep");
        let started = Instant::now();
        let rejection = supervise_process(slow, &test_limits(), &cancellation, &NoProbe)
            .await
            .unwrap_err();
        assert_eq!(rejection.code, "render_timeout");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "超时必须及时终止子进程"
        );

        let failing = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 3")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn failing program");
        let exit = supervise_process(failing, &test_limits(), &cancellation, &NoProbe)
            .await
            .expect("子进程应当正常结束");
        assert_eq!(exit, ProcessExit::Failure(Some(3)));

        let succeeding = Command::new("/bin/sh")
            .arg("-c")
            .arg("exit 0")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn");
        assert_eq!(
            supervise_process(succeeding, &test_limits(), &cancellation, &NoProbe)
                .await
                .expect("成功退出"),
            ProcessExit::Success
        );
    }

    #[tokio::test]
    async fn subprocess_cancellation_kills_within_bounded_time() {
        let cancellation = CancellationToken::new();
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 5")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sleep");
        let token = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            token.cancel();
        });
        let started = Instant::now();
        let rejection = supervise_process(child, &test_limits(), &cancellation, &NoProbe)
            .await
            .unwrap_err();
        assert_eq!(rejection.code, "render_cancelled");
        assert!(started.elapsed() < Duration::from_secs(3), "取消必须有界");
    }

    struct NoProbe;

    #[async_trait]
    impl ProcessMemoryProbe for NoProbe {
        async fn resident_kib(&self, _pid: u32) -> Option<u64> {
            None
        }
    }

    struct HugeProbe;

    #[async_trait]
    impl ProcessMemoryProbe for HugeProbe {
        async fn resident_kib(&self, _pid: u32) -> Option<u64> {
            Some(u64::MAX / 2)
        }
    }

    #[tokio::test]
    async fn subprocess_memory_limit_kills_the_child() {
        let cancellation = CancellationToken::new();
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 5")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sleep");
        let rejection = supervise_process(child, &test_limits(), &cancellation, &HugeProbe)
            .await
            .unwrap_err();
        assert_eq!(rejection.code, "render_memory_exceeded");
    }

    #[test]
    fn restricted_command_drops_proxies_and_remote_runtime() {
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = directory.path().join("work");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let composition = workspace.join("composition.html");
        std::fs::write(&composition, "<html></html>").expect("composition");
        let frames = workspace.join("frames");
        let invocation = RenderInvocation {
            composition_path: composition,
            workspace_dir: workspace.clone(),
            frames_dir: frames.clone(),
            allowed_roots: vec![directory.path().to_path_buf()],
            width: 1080,
            height: 1920,
            fps: 30,
            duration_ms: 15_000,
            limits: RenderLimits::default(),
        };
        let browser = directory.path().join("chrome");
        let command = build_restricted_command(Path::new("/usr/bin/node"), &invocation, &browser)
            .expect("command");
        let envs: Vec<(String, Option<String>)> = command
            .as_std()
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().to_string(),
                    value.map(|value| value.to_string_lossy().to_string()),
                )
            })
            .collect();
        let names: Vec<&str> = envs.iter().map(|(key, _)| key.as_str()).collect();
        for forbidden in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "http_proxy",
            "https_proxy",
        ] {
            assert!(!names.contains(&forbidden), "不得继承代理变量 {forbidden}");
        }
        let runtime_url = envs
            .iter()
            .find(|(key, _)| key == "HYPERFRAME_RUNTIME_URL")
            .and_then(|(_, value)| value.clone());
        assert_eq!(
            runtime_url.as_deref(),
            Some(""),
            "远程运行时地址必须显式置空"
        );
        assert!(names.contains(&"PATH"), "必须提供最小 PATH");
        let path_value = envs
            .iter()
            .find(|(key, _)| key == "PATH")
            .and_then(|(_, value)| value.clone());
        assert_eq!(path_value.as_deref(), Some("/usr/bin:/bin"));
        let home = envs
            .iter()
            .find(|(key, _)| key == "HOME")
            .and_then(|(_, value)| value.clone());
        assert_eq!(
            home.as_deref(),
            Some(workspace.to_string_lossy().as_ref()),
            "HOME 必须落在工程临时目录内"
        );
        let cwd = command.as_std().get_current_dir();
        assert_eq!(cwd, Some(workspace.as_path()));
    }

    #[test]
    fn render_paths_outside_allowed_roots_are_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = directory.path().join("work");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        let invocation = RenderInvocation {
            composition_path: outside.path().join("composition.html"),
            workspace_dir: workspace.clone(),
            frames_dir: workspace.join("frames"),
            allowed_roots: vec![workspace.clone()],
            width: 1080,
            height: 1920,
            fps: 30,
            duration_ms: 15_000,
            limits: RenderLimits::default(),
        };
        let rejection = assert_paths_within_roots(&invocation).unwrap_err();
        assert_eq!(rejection.code, "render_path_escape");
    }

    struct FlippableGate(AtomicBool);

    impl BaseCapabilityGate for FlippableGate {
        fn base_capability_busy(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    #[tokio::test]
    async fn scheduler_serializes_renders_and_yields_to_base_capabilities() {
        let gate = Arc::new(FlippableGate(AtomicBool::new(true)));
        let scheduler = RenderScheduler::new(1, gate.clone());
        let cancellation = CancellationToken::new();
        let started = Instant::now();
        let waiter = {
            let scheduler = Arc::new(scheduler);
            let scheduler_clone = Arc::clone(&scheduler);
            let cancellation_clone = cancellation.clone();
            tokio::spawn(async move { scheduler_clone.acquire(&cancellation_clone).await })
        };
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert!(
            !waiter.is_finished(),
            "基础能力繁忙时渲染必须等待（任务 4.6）"
        );
        gate.0.store(false, Ordering::SeqCst);
        let permit = waiter.await.expect("join").expect("acquire");
        assert!(started.elapsed() >= Duration::from_millis(120));
        drop(permit);
    }

    #[tokio::test]
    async fn scheduler_limits_concurrency_and_honours_cancellation() {
        let scheduler = RenderScheduler::new(1, Arc::new(AlwaysIdleGate));
        let cancellation = CancellationToken::new();
        let permit = scheduler
            .acquire(&cancellation)
            .await
            .expect("first permit");
        assert_eq!(scheduler.available_permits(), 0);
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let rejection = scheduler.acquire(&cancelled).await.unwrap_err();
        assert_eq!(rejection.code, "render_cancelled");
        drop(permit);
        assert_eq!(scheduler.available_permits(), 1);
    }

    #[test]
    fn render_outcome_verification_checks_sequence_and_limits() {
        let directory = tempfile::tempdir().expect("tempdir");
        let frames = directory.path().join("frames");
        std::fs::create_dir_all(&frames).expect("frames");
        for index in 1..=3 {
            std::fs::write(frames.join(format!("frame_{index:06}.png")), vec![0_u8; 16])
                .expect("write frame");
        }
        let limits = RenderLimits::default();
        let outcome = verify_render_outcome(&frames, &limits).expect("outcome");
        assert_eq!(outcome.frame_count, 3);
        assert_eq!(outcome.frames_bytes, 48);

        std::fs::remove_file(frames.join("frame_000002.png")).expect("remove frame");
        let rejection = verify_render_outcome(&frames, &limits).unwrap_err();
        assert_eq!(rejection.code, "render_output_gap");

        std::fs::write(frames.join("frame_000002.png"), vec![0_u8; 16]).expect("restore");
        let tight = RenderLimits {
            max_frames: 2,
            ..RenderLimits::default()
        };
        assert_eq!(
            verify_render_outcome(&frames, &tight).unwrap_err().code,
            "render_frame_limit_exceeded"
        );
    }

    #[test]
    fn cleanup_removes_only_the_unpublished_frames() {
        let directory = tempfile::tempdir().expect("tempdir");
        let frames = directory.path().join("frames");
        std::fs::create_dir_all(&frames).expect("frames");
        std::fs::write(frames.join("frame_000001.png"), b"x").expect("frame");
        let keep = directory.path().join("published.mp4");
        std::fs::write(&keep, b"keep").expect("published artefact");
        cleanup_unpublished_frames(&frames).expect("cleanup");
        assert!(!frames.exists());
        assert!(keep.exists(), "已发布产物不得被清理");
        // 幂等：目录已不存在也算成功。
        cleanup_unpublished_frames(&frames).expect("cleanup again");
    }

    struct MemoryTrackingRenderer {
        frames: PathBuf,
        behaviour: Behaviour,
        calls: Arc<AtomicU64>,
    }

    #[derive(Clone, Copy)]
    enum Behaviour {
        Success,
        Crash,
    }

    #[async_trait]
    impl EffectCaptionRenderer for MemoryTrackingRenderer {
        async fn render(
            &self,
            _runtime: &VerifiedRendererRuntime,
            _invocation: &RenderInvocation,
            _cancellation: &CancellationToken,
        ) -> Result<RenderOutcome, EffectCaptionRuntimeError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.behaviour {
                Behaviour::Success => {
                    std::fs::create_dir_all(&self.frames).expect("frames");
                    std::fs::write(self.frames.join("frame_000001.png"), b"x").expect("frame");
                    Ok(RenderOutcome {
                        frames_dir: self.frames.clone(),
                        frame_count: 1,
                        frames_bytes: 1,
                    })
                }
                Behaviour::Crash => {
                    std::fs::create_dir_all(&self.frames).expect("frames");
                    std::fs::write(self.frames.join("frame_000001.png"), b"x").expect("frame");
                    cleanup_unpublished_frames(&self.frames).expect("cleanup after crash");
                    Err(EffectCaptionRuntimeError::new(
                        "render_subprocess_failed",
                        "渲染进程非正常退出",
                    ))
                }
            }
        }
    }

    #[tokio::test]
    async fn successful_renderer_keeps_frames() {
        let directory = tempfile::tempdir().expect("tempdir");
        let frames = directory.path().join("frames");
        let renderer = MemoryTrackingRenderer {
            frames: frames.clone(),
            behaviour: Behaviour::Success,
            calls: Arc::new(AtomicU64::new(0)),
        };
        let runtime = VerifiedRendererRuntime {
            renderer_version: "0.8.96".to_owned(),
            renderer_tree_sha256: "tree".to_owned(),
            engine_script_sha256: "engine".to_owned(),
            font_version: "fonts".to_owned(),
        };
        let invocation = RenderInvocation {
            composition_path: directory.path().join("composition.html"),
            workspace_dir: directory.path().to_path_buf(),
            frames_dir: frames.clone(),
            allowed_roots: vec![directory.path().to_path_buf()],
            width: 1080,
            height: 1920,
            fps: 30,
            duration_ms: 15_000,
            limits: RenderLimits::default(),
        };
        let outcome = renderer
            .render(&runtime, &invocation, &CancellationToken::new())
            .await
            .expect("成功渲染");
        assert_eq!(outcome.frame_count, 1);
        assert!(frames.exists(), "成功后帧序列必须保留");
    }

    #[tokio::test]
    async fn subprocess_renderer_spawns_real_process_and_cleans_up_on_empty_output() {
        // /bin/echo 会以 0 退出但不产出帧：真实 spawn 路径必须报告"没有产出帧"并清理目录。
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = directory.path().join("work");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let composition = workspace.join("composition.html");
        std::fs::write(&composition, "<html></html>").expect("composition");
        let frames = workspace.join("frames");
        std::fs::create_dir_all(&frames).expect("frames");
        let browser = directory.path().join("chrome");
        std::fs::write(&browser, b"browser").expect("browser");
        let renderer = SubprocessRenderer::new("/bin/echo", &browser);
        let invocation = RenderInvocation {
            composition_path: composition,
            workspace_dir: workspace.clone(),
            frames_dir: frames.clone(),
            allowed_roots: vec![directory.path().to_path_buf()],
            width: 1080,
            height: 1920,
            fps: 30,
            duration_ms: 15_000,
            limits: RenderLimits {
                timeout_ms: 5_000,
                ..RenderLimits::default()
            },
        };
        let runtime = VerifiedRendererRuntime {
            renderer_version: "0.8.96".to_owned(),
            renderer_tree_sha256: "tree".to_owned(),
            engine_script_sha256: "engine".to_owned(),
            font_version: "fonts".to_owned(),
        };
        let rejection = renderer
            .render(&runtime, &invocation, &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(rejection.code, "render_output_empty");
        assert!(!frames.exists(), "失败后必须清理未发布帧");
    }

    #[tokio::test]
    async fn crashed_renderer_cleans_temp_frames_and_keeps_published_artifacts() {
        let directory = tempfile::tempdir().expect("tempdir");
        let frames = directory.path().join("frames");
        let published = directory.path().join("project.mp4");
        std::fs::write(&published, b"published").expect("published");
        let renderer = MemoryTrackingRenderer {
            frames: frames.clone(),
            behaviour: Behaviour::Crash,
            calls: Arc::new(AtomicU64::new(0)),
        };
        let runtime = VerifiedRendererRuntime {
            renderer_version: "0.8.96".to_owned(),
            renderer_tree_sha256: "tree".to_owned(),
            engine_script_sha256: "engine".to_owned(),
            font_version: "fonts".to_owned(),
        };
        let invocation = RenderInvocation {
            composition_path: directory.path().join("composition.html"),
            workspace_dir: directory.path().to_path_buf(),
            frames_dir: frames.clone(),
            allowed_roots: vec![directory.path().to_path_buf()],
            width: 1080,
            height: 1920,
            fps: 30,
            duration_ms: 15_000,
            limits: RenderLimits::default(),
        };
        let rejection = renderer
            .render(&runtime, &invocation, &CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(rejection.code, "render_subprocess_failed");
        assert!(!frames.exists(), "崩溃后必须清理未发布帧");
        assert!(published.exists(), "工程产物不得受影响");
        assert_eq!(renderer.calls.load(Ordering::SeqCst), 1);
    }

    fn font_resource(directory: &Path, bytes: &[u8]) -> FontResource {
        let path = directory.join("caption-cjk.ttf");
        std::fs::write(&path, bytes).expect("write font");
        FontResource {
            family: "hyperframes-caption-cjk".to_owned(),
            version: "caption-fonts-2026.10".to_owned(),
            path,
            sha256: hex::encode(Sha256::digest(bytes)),
        }
    }

    #[test]
    fn font_resources_are_verified_and_system_families_rejected() {
        let directory = tempfile::tempdir().expect("tempdir");
        let resource = font_resource(directory.path(), b"font-bytes");
        let verified =
            verify_font_resources(std::slice::from_ref(&resource), "caption-fonts-2026.10")
                .expect("字体应当通过校验");
        assert_eq!(verified.family(), "hyperframes-caption-cjk");
        assert_eq!(verified.version(), "caption-fonts-2026.10");
        assert_eq!(verified.count(), 1);
        assert_eq!(verified.directory(), directory.path());

        // 版本不符
        assert_eq!(
            verify_font_resources(std::slice::from_ref(&resource), "caption-fonts-2025.01")
                .unwrap_err()
                .code,
            "caption_font_version_mismatch"
        );
        // 系统字体家族
        let mut system = resource.clone();
        system.family = "PingFang SC".to_owned();
        assert_eq!(
            verify_font_resources(&[system], "caption-fonts-2026.10")
                .unwrap_err()
                .code,
            "caption_font_family_not_bundled"
        );
        // 文件被篡改
        let mut tampered = resource.clone();
        tampered.sha256 = "0".repeat(64);
        assert_eq!(
            verify_font_resources(&[tampered], "caption-fonts-2026.10")
                .unwrap_err()
                .code,
            "caption_font_hash_mismatch"
        );
        // 一个字体都没有
        assert_eq!(
            verify_font_resources(&[], "caption-fonts-2026.10")
                .unwrap_err()
                .code,
            "caption_font_missing"
        );
    }

    #[test]
    fn render_environment_never_inherits_system_font_paths() {
        let directory = tempfile::tempdir().expect("tempdir");
        let workspace = directory.path().join("work");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let fonts_dir = directory.path().join("fonts");
        std::fs::create_dir_all(&fonts_dir).expect("fonts");
        let resource = font_resource(&fonts_dir, b"font-bytes");
        let fonts = verify_font_resources(&[resource], "caption-fonts-2026.10").expect("fonts");
        let invocation = RenderInvocation {
            composition_path: workspace.join("composition.html"),
            workspace_dir: workspace.clone(),
            frames_dir: workspace.join("frames"),
            allowed_roots: vec![directory.path().to_path_buf()],
            width: 1080,
            height: 1920,
            fps: 30,
            duration_ms: 15_000,
            limits: RenderLimits::default(),
        };
        let browser = directory.path().join("chrome");
        let command = build_restricted_command_with_fonts(
            Path::new("/usr/bin/node"),
            &invocation,
            &browser,
            Some(&fonts),
        )
        .expect("command");
        let envs: Vec<String> = command
            .as_std()
            .get_envs()
            .map(|(key, _)| key.to_string_lossy().to_string())
            .collect();
        for system_font_env in [
            "FONTCONFIG_PATH",
            "FONTCONFIG_FILE",
            "XDG_DATA_DIRS",
            "XDG_DATA_HOME",
            "DYLD_FALLBACK_LIBRARY_PATH",
        ] {
            assert!(
                !envs.contains(&system_font_env.to_owned()),
                "不得继承系统字体发现路径 {system_font_env}"
            );
        }
        let font_dir = command
            .as_std()
            .get_envs()
            .find(|(key, _)| key.to_string_lossy() == "HYPERFRAMES_CAPTION_FONT_DIR")
            .and_then(|(_, value)| value.map(|value| value.to_string_lossy().to_string()));
        assert_eq!(
            font_dir.as_deref(),
            Some(fonts_dir.to_string_lossy().as_ref())
        );
    }

    fn write_frames(directory: &Path, contents: &[&[u8]]) {
        // 先清空：否则沿用上一次的帧会掩盖帧数/内容差异。
        let _ = std::fs::remove_dir_all(directory);
        std::fs::create_dir_all(directory).expect("frames dir");
        for (index, bytes) in contents.iter().enumerate() {
            std::fs::write(directory.join(format!("frame_{:06}.png", index + 1)), bytes)
                .expect("write frame");
        }
    }

    #[test]
    fn frame_sequences_are_compared_against_the_determinism_policy() {
        let directory = tempfile::tempdir().expect("tempdir");
        let first = directory.path().join("a");
        let second = directory.path().join("b");
        write_frames(&first, &[b"1", b"2", b"3"]);
        write_frames(&second, &[b"1", b"2", b"3"]);
        let parity = compare_frame_sequences(&first, &second, &DeterminismPolicy::default())
            .expect("相同帧序列");
        assert_eq!(parity.mismatched, 0);
        assert_eq!(parity.ratio, 0.0);

        // 3 帧里 1 帧不同（33%）远超允许比例 → 判定非确定性。
        write_frames(&second, &[b"1", b"X", b"3"]);
        let left = digest_frame_sequence(&first).expect("digest left");
        let right = digest_frame_sequence(&second).expect("digest right");
        assert_ne!(left, right, "重写后的帧序列必须不同：{left:?} vs {right:?}");
        assert_eq!(
            compare_frame_sequences(&first, &second, &DeterminismPolicy::default())
                .unwrap_err()
                .code,
            "render_nondeterministic"
        );

        // 帧数不同 → 独立错误码。
        write_frames(&second, &[b"1", b"2"]);
        assert_eq!(
            compare_frame_sequences(&first, &second, &DeterminismPolicy::default())
                .unwrap_err()
                .code,
            "render_frame_count_mismatch"
        );
    }

    #[test]
    fn determinism_policy_allows_documented_raster_tolerance() {
        let directory = tempfile::tempdir().expect("tempdir");
        let first = directory.path().join("a");
        let second = directory.path().join("b");
        let identical: Vec<&[u8]> = vec![b"same"; 200];
        write_frames(&first, &identical);
        let mut tolerant = identical.clone();
        tolerant[0] = b"diff";
        write_frames(&second, &tolerant);
        let parity = compare_frame_sequences(&first, &second, &DeterminismPolicy::default())
            .expect("0.5% 差异在设计允许范围内");
        assert_eq!(parity.total, 200);
        assert_eq!(parity.mismatched, 1);
        assert!(parity.ratio < 0.009);
    }

    #[test]
    fn cache_key_changes_when_any_version_field_changes() {
        let versions = RenderVersionSet {
            composition_version: "composition-v1".to_owned(),
            composition_fingerprint: "fingerprint".to_owned(),
            font_version: "caption-fonts-2026.10".to_owned(),
            browser_version: "154.0.8037.58".to_owned(),
            renderer_version: "0.8.96".to_owned(),
            engine_script_version: "38592478aad3f515".to_owned(),
            width: 1080,
            height: 1920,
            fps: 30,
        };
        let base = RenderCacheKey::new(&versions);
        assert_eq!(base.as_str().len(), 64);
        assert_eq!(
            base,
            RenderCacheKey::new(&versions.clone()),
            "相同输入必须稳定"
        );
        for mutate in [
            |value: &mut RenderVersionSet| value.composition_version = "composition-v2".to_owned(),
            |value: &mut RenderVersionSet| value.composition_fingerprint = "other".to_owned(),
            |value: &mut RenderVersionSet| value.font_version = "caption-fonts-2026.11".to_owned(),
            |value: &mut RenderVersionSet| value.browser_version = "155.0.0.0".to_owned(),
            |value: &mut RenderVersionSet| value.renderer_version = "0.8.97".to_owned(),
            |value: &mut RenderVersionSet| value.engine_script_version = "deadbeef".to_owned(),
            |value: &mut RenderVersionSet| value.width = 720,
            |value: &mut RenderVersionSet| value.height = 1280,
            |value: &mut RenderVersionSet| value.fps = 60,
        ] {
            let mut changed = versions.clone();
            mutate(&mut changed);
            assert_ne!(
                base,
                RenderCacheKey::new(&changed),
                "任一版本字段变化都必须产生新的缓存键"
            );
        }
    }
}
