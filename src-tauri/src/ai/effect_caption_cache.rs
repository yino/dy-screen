//! 特效字幕渲染缓存的复用与清理（步骤 3 任务组 9）。
//!
//! 复用的唯一依据是缓存键——组合版本、组合指纹、字体版本、浏览器版本、渲染器版本、
//! 引擎脚本版本、分辨率与帧率**全部一致**才复用；任一字段变化都会落到新的尝试行。
//! 复用前还会重新校验磁盘上的帧序列，避免"记录说 ready、帧已被删除"的假命中。

use std::path::Path;

use tokio_util::sync::CancellationToken;

use super::clip_export::{ClipExportFailure, verify_transparent_frame_sequence};
use super::domain::{AiEffectCaptionRenderAttempt, NewAiEffectCaptionRenderAttempt};
use super::effect_caption_render::{
    EffectCaptionRenderer, EffectCaptionRuntimeError, RenderInvocation, VerifiedRendererRuntime,
    verify_render_outcome,
};
use super::repository::AiRepository;

/// 一次渲染请求的结果：命中缓存或真的渲染过。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderCacheOutcome {
    Reused(AiEffectCaptionRenderAttempt),
    Rendered(AiEffectCaptionRenderAttempt),
}

impl RenderCacheOutcome {
    pub fn attempt(&self) -> &AiEffectCaptionRenderAttempt {
        match self {
            Self::Reused(attempt) | Self::Rendered(attempt) => attempt,
        }
    }

    pub fn reused(&self) -> bool {
        matches!(self, Self::Reused(_))
    }
}

/// 命中缓存时必须重新校验磁盘产物：帧序列必须存在、连续、带 Alpha 且确实透明。
fn cached_frames_are_usable(invocation: &RenderInvocation) -> bool {
    if verify_render_outcome(&invocation.frames_dir, &invocation.limits).is_err() {
        return false;
    }
    verify_transparent_frame_sequence(&invocation.frames_dir, 3).is_ok()
}

/// 带缓存地渲染一条高光（任务 9.1–9.3）。
///
/// 1. 先按缓存键登记尝试；`ready` 且磁盘帧仍可用 → 直接复用，**不调用渲染器**；
/// 2. 记录存在但帧被删除/损坏 → 记为失败并按新的一次渲染重跑；
/// 3. 渲染成功 → 标记 `ready`；失败 → 记下可区分的失败码，帧序列由渲染器侧负责清理。
pub async fn render_with_cache(
    repository: &AiRepository,
    renderer: &dyn EffectCaptionRenderer,
    runtime: &VerifiedRendererRuntime,
    invocation: &RenderInvocation,
    request: NewAiEffectCaptionRenderAttempt,
    cancellation: &CancellationToken,
) -> Result<RenderCacheOutcome, EffectCaptionRuntimeError> {
    let (attempt, cached) = repository.begin_render_attempt(request).map_err(|error| {
        EffectCaptionRuntimeError::new("render_cache_unavailable", error.to_string())
    })?;
    if cached && cached_frames_are_usable(invocation) {
        return Ok(RenderCacheOutcome::Reused(attempt));
    }
    if cached {
        // 记录说 ready，但磁盘产物已不可用：必须先撤销 ready，再老老实实重渲染。
        repository
            .mark_render_attempt_failed(attempt.id, "render_cache_missing")
            .map_err(|error| {
                EffectCaptionRuntimeError::new("render_cache_unavailable", error.to_string())
            })?;
        let (attempt, _) = repository
            .begin_render_attempt(request_for(&attempt))
            .map_err(|error| {
                EffectCaptionRuntimeError::new("render_cache_unavailable", error.to_string())
            })?;
        return render_and_record(
            repository,
            renderer,
            runtime,
            invocation,
            attempt,
            cancellation,
        )
        .await;
    }
    render_and_record(
        repository,
        renderer,
        runtime,
        invocation,
        attempt,
        cancellation,
    )
    .await
}

async fn render_and_record(
    repository: &AiRepository,
    renderer: &dyn EffectCaptionRenderer,
    runtime: &VerifiedRendererRuntime,
    invocation: &RenderInvocation,
    attempt: AiEffectCaptionRenderAttempt,
    cancellation: &CancellationToken,
) -> Result<RenderCacheOutcome, EffectCaptionRuntimeError> {
    match renderer.render(runtime, invocation, cancellation).await {
        Ok(outcome) => {
            let updated = repository
                .mark_render_attempt_ready(attempt.id, outcome.frame_count)
                .map_err(|error| {
                    EffectCaptionRuntimeError::new("render_cache_unavailable", error.to_string())
                })?;
            Ok(RenderCacheOutcome::Rendered(updated))
        }
        Err(error) => {
            let _ = repository.mark_render_attempt_failed(attempt.id, error.code);
            Err(error)
        }
    }
}

fn request_for(attempt: &AiEffectCaptionRenderAttempt) -> NewAiEffectCaptionRenderAttempt {
    NewAiEffectCaptionRenderAttempt {
        project_id: attempt.project_id,
        candidate_id: attempt.candidate_id,
        cache_key: attempt.cache_key.clone(),
        composition_version: attempt.composition_version.clone(),
        composition_fingerprint: attempt.composition_fingerprint.clone(),
        font_version: attempt.font_version.clone(),
        browser_version: attempt.browser_version.clone(),
        renderer_version: attempt.renderer_version.clone(),
        engine_script_version: attempt.engine_script_version.clone(),
        width: attempt.width,
        height: attempt.height,
        fps: attempt.fps,
        frames_dir: attempt.frames_dir.clone(),
    }
}

/// 清理某候选的渲染缓存（任务 9.3）。
///
/// 返回被清理的条目数。只删除缓存行与其帧目录；工程、ASR 产物与已导出成片不在其中，
/// 因此清理是安全的。
pub fn clear_render_cache(
    repository: &AiRepository,
    candidate_id: i64,
) -> Result<usize, ClipExportFailure> {
    let directories = repository
        .delete_render_attempts_for_candidate(candidate_id)
        .map_err(|_| failure("render_cache_unavailable", "无法删除渲染缓存条目"))?;
    for directory in &directories {
        let path = Path::new(directory);
        if path.as_os_str().is_empty() {
            continue;
        }
        let _ = std::fs::remove_dir_all(path);
    }
    Ok(directories.len())
}

/// 清理指定时间之前的缓存条目（例如按保留期回收）。
pub fn prune_render_cache(
    repository: &AiRepository,
    cutoff: &str,
) -> Result<usize, ClipExportFailure> {
    let directories = repository
        .prune_render_attempts_created_before(cutoff)
        .map_err(|_| failure("render_cache_unavailable", "无法回收渲染缓存条目"))?;
    for directory in &directories {
        let path = Path::new(directory);
        if path.as_os_str().is_empty() {
            continue;
        }
        let _ = std::fs::remove_dir_all(path);
    }
    Ok(directories.len())
}

fn failure(code: &'static str, message: &str) -> ClipExportFailure {
    ClipExportFailure {
        code,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use async_trait::async_trait;

    use crate::ai::domain::AiRenderAttemptStatus;
    use crate::ai::effect_caption_render::{
        EffectCaptionRuntimeError, RenderLimits, RenderOutcome,
    };
    use crate::database::Database;

    struct RecordingRenderer {
        calls: Arc<AtomicU64>,
        frames: u64,
        fail: bool,
    }

    #[async_trait]
    impl EffectCaptionRenderer for RecordingRenderer {
        async fn render(
            &self,
            _runtime: &VerifiedRendererRuntime,
            invocation: &RenderInvocation,
            _cancellation: &CancellationToken,
        ) -> Result<RenderOutcome, EffectCaptionRuntimeError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(EffectCaptionRuntimeError::new(
                    "render_subprocess_failed",
                    "测试用失败",
                ));
            }
            std::fs::create_dir_all(&invocation.frames_dir).expect("frames dir");
            let mut bytes = 0_u64;
            for index in 1..=self.frames {
                let pixels = vec![0_u8; 8 * 8 * 4];
                let file = std::fs::File::create(
                    invocation.frames_dir.join(format!("frame_{index:06}.png")),
                )
                .expect("frame file");
                let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), 8, 8);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header().expect("png header");
                writer.write_image_data(&pixels).expect("png data");
                bytes += 8 * 8 * 4;
            }
            Ok(RenderOutcome {
                frames_dir: invocation.frames_dir.clone(),
                frame_count: self.frames,
                frames_bytes: bytes,
            })
        }
    }

    fn runtime() -> VerifiedRendererRuntime {
        VerifiedRendererRuntime::for_tests()
    }

    fn invocation(directory: &Path, limits: RenderLimits) -> RenderInvocation {
        RenderInvocation {
            composition_path: directory.join("composition.html"),
            workspace_dir: directory.to_path_buf(),
            frames_dir: directory.join("frames"),
            allowed_roots: vec![directory.to_path_buf()],
            width: 1080,
            height: 1920,
            fps: 30,
            duration_ms: 15_000,
            limits,
        }
    }

    fn request(
        directory: &Path,
        cache_key: &str,
        frames_dir: PathBuf,
    ) -> NewAiEffectCaptionRenderAttempt {
        let _ = directory;
        NewAiEffectCaptionRenderAttempt {
            project_id: 1,
            candidate_id: 5,
            cache_key: cache_key.to_owned(),
            composition_version: "composition-v1".to_owned(),
            composition_fingerprint: "composition-fp".to_owned(),
            font_version: "caption-fonts-2026.10".to_owned(),
            browser_version: "154.0.8037.58".to_owned(),
            renderer_version: "0.8.96".to_owned(),
            engine_script_version: "38592478aad3f515".to_owned(),
            width: 1080,
            height: 1920,
            fps: 30,
            frames_dir: frames_dir.to_string_lossy().into_owned(),
        }
    }

    fn repository() -> AiRepository {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate");
        AiRepository::new(database)
    }

    #[tokio::test]
    async fn identical_versions_reuse_frames_without_rendering_again() {
        let repository = repository();
        let directory = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicU64::new(0));
        let renderer = RecordingRenderer {
            calls: calls.clone(),
            frames: 3,
            fail: false,
        };
        let limits = RenderLimits {
            max_frames: 10,
            ..RenderLimits::default()
        };
        let invocation = invocation(directory.path(), limits.clone());

        let first = render_with_cache(
            &repository,
            &renderer,
            &runtime(),
            &invocation,
            request(directory.path(), "cache-key", invocation.frames_dir.clone()),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(matches!(first, RenderCacheOutcome::Rendered(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // 同一缓存键、帧仍在磁盘 → 复用且不再调用渲染器。
        let second = render_with_cache(
            &repository,
            &renderer,
            &runtime(),
            &invocation,
            request(directory.path(), "cache-key", invocation.frames_dir.clone()),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(second.reused(), "全部一致时必须复用");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "复用时不得重新渲染");
        assert_eq!(second.attempt().frame_count, 3);
    }

    #[tokio::test]
    async fn cache_hit_with_missing_frames_renders_again() {
        let repository = repository();
        let directory = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicU64::new(0));
        let renderer = RecordingRenderer {
            calls: calls.clone(),
            frames: 2,
            fail: false,
        };
        let limits = RenderLimits {
            max_frames: 10,
            ..RenderLimits::default()
        };
        let invocation = invocation(directory.path(), limits.clone());
        render_with_cache(
            &repository,
            &renderer,
            &runtime(),
            &invocation,
            request(directory.path(), "cache-key", invocation.frames_dir.clone()),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        // 帧被外部删除（磁盘回收/用户清理）→ 记录虽是 ready，也必须重渲染。
        std::fs::remove_dir_all(&invocation.frames_dir).unwrap();
        let second = render_with_cache(
            &repository,
            &renderer,
            &runtime(),
            &invocation,
            request(directory.path(), "cache-key", invocation.frames_dir.clone()),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(matches!(second, RenderCacheOutcome::Rendered(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(second.attempt().status, AiRenderAttemptStatus::Ready);
    }

    #[tokio::test]
    async fn any_version_field_change_creates_a_new_attempt() {
        let repository = repository();
        let directory = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicU64::new(0));
        let renderer = RecordingRenderer {
            calls: calls.clone(),
            frames: 2,
            fail: false,
        };
        let limits = RenderLimits {
            max_frames: 10,
            ..RenderLimits::default()
        };
        let invocation = invocation(directory.path(), limits.clone());
        let first = render_with_cache(
            &repository,
            &renderer,
            &runtime(),
            &invocation,
            request(directory.path(), "key-a", invocation.frames_dir.clone()),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        // 仅引擎脚本版本不同 → 新缓存键 → 新尝试行，且必须重新渲染。
        let mut changed = request(directory.path(), "key-b", invocation.frames_dir.clone());
        changed.engine_script_version = "deadbeefdeadbeef".to_owned();
        let second = render_with_cache(
            &repository,
            &renderer,
            &runtime(),
            &invocation,
            changed,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(!second.reused());
        assert_ne!(second.attempt().id, first.attempt().id);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(repository.render_attempt_count().unwrap(), 2);
    }

    #[tokio::test]
    async fn failed_render_is_recorded_with_a_distinguishable_code() {
        let repository = repository();
        let directory = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicU64::new(0));
        let renderer = RecordingRenderer {
            calls: calls.clone(),
            frames: 0,
            fail: true,
        };
        let invocation = invocation(directory.path(), RenderLimits::default());
        let rejection = render_with_cache(
            &repository,
            &renderer,
            &runtime(),
            &invocation,
            request(directory.path(), "key-fail", invocation.frames_dir.clone()),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(rejection.code, "render_subprocess_failed");
        let attempt = repository
            .find_render_attempt_by_key("key-fail")
            .unwrap()
            .expect("attempt recorded");
        assert_eq!(attempt.status, AiRenderAttemptStatus::Failed);
        assert_eq!(
            attempt.failure_code.as_deref(),
            Some("render_subprocess_failed")
        );
        assert!(
            repository
                .find_ready_render_attempt("key-fail")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn clearing_the_cache_keeps_project_asr_and_exported_artifacts() {
        let repository = repository();
        let directory = tempfile::tempdir().unwrap();
        let frames_dir = directory.path().join("frames");
        std::fs::create_dir_all(&frames_dir).unwrap();
        std::fs::write(frames_dir.join("frame_000001.png"), b"x").unwrap();
        let (attempt, _) = repository
            .begin_render_attempt(request(directory.path(), "key-c", frames_dir.clone()))
            .unwrap();
        repository.mark_render_attempt_ready(attempt.id, 1).unwrap();
        let project_artifact = directory.path().join("project.json");
        let asr_artifact = directory.path().join("asr.json");
        let exported = directory.path().join("exported.mp4");
        for path in [&project_artifact, &asr_artifact, &exported] {
            std::fs::write(path, b"keep").unwrap();
        }

        let cleared = clear_render_cache(&repository, 5).unwrap();
        assert_eq!(cleared, 1);
        assert!(!frames_dir.exists(), "缓存帧目录必须被清理");
        assert_eq!(repository.render_attempt_count().unwrap(), 0);
        for path in [&project_artifact, &asr_artifact, &exported] {
            assert!(path.exists(), "清理缓存不得影响 {path:?}");
        }
        // 幂等：再清理一次不报错也不误删。
        assert_eq!(clear_render_cache(&repository, 5).unwrap(), 0);
    }
}
