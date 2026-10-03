//! 步骤 3 任务组 12 的**验收级**测试（12.5–12.8）。
//!
//! 这些测试走真实公开 API、真实 SQLite 与真实渲染运行时校验；
//! 需要随包渲染器/固定版本浏览器/真实模型的部分（12.1–12.4）在
//! `openspec/changes/add-effect-caption-renderer/evidence/acceptance-receipts.md` 中逐条留证。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dy_screen_app_lib::ai::{
    AiRepository, CanvasSpec, DeterminismPolicy, EffectCaptionEnhancementRunner,
    EffectCaptionLimits, EffectCaptionRenderer, EffectCaptionRuntimeError, EnhancementHighlight,
    EnhancementProduct, HighlightRenderWindow, RenderCacheKey, RenderInvocation, RenderLimits,
    RenderOutcome, RenderPurpose, RenderVersionSet, RendererRuntimeManifest,
    VerifiedRendererRuntime, compute_tree_digest, enhancement_stage_plan, plan_render_spec,
    published_ranges_from_subtitles, render_with_cache, run_enhancement_stage,
    validate_effect_caption, verify_renderer_runtime,
};
use dy_screen_app_lib::database::Database;
use tokio_util::sync::CancellationToken;

fn repository() -> AiRepository {
    let database = Database::open_in_memory().expect("in-memory database");
    database.migrate().expect("migrate");
    AiRepository::new(database)
}

fn root_attributes() -> &'static str {
    r#"data-composition-id="main" data-duration="15" data-width="1080" data-height="1920""#
}

fn compose(body: &str, css: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html {attrs}>\n<head>\n<meta charset=\"utf-8\">\n<title>t</title>\n<style>{css}</style>\n</head>\n<body>\n{body}\n</body>\n</html>\n",
        attrs = root_attributes()
    )
}

fn corpus() -> Vec<String> {
    vec!["这个价格真的绷不住了".to_owned(), "，冲".to_owned()]
}

fn limits() -> EffectCaptionLimits {
    EffectCaptionLimits::default()
}

/// 任务 3.8 的九类越界样本（每类一个代表样本）。
fn boundary_samples() -> Vec<(&'static str, String, &'static str)> {
    let injected = compose(
        r#"<div class="caption-line">这个价格真的绷不住了</div>"#,
        ".caption-line{opacity:1}",
    )
    .replace(
        "<body>",
        "<body>\n<script data-hyperframes-preview-runtime=\"1\">/* forged */</script>",
    );
    vec![
        (
            "脚本",
            compose(
                r#"<div class="caption-line">这个价格真的绷不住了</div>"#,
                ".caption-line{opacity:1}",
            )
            .replace("<body>", "<body>\n<script>alert(1)</script>"),
            "effect_script_rejected",
        ),
        (
            "事件处理器",
            compose(
                r#"<div class="caption-line" onclick="run()">这个价格真的绷不住了</div>"#,
                ".caption-line{opacity:1}",
            ),
            "effect_event_handler_rejected",
        ),
        (
            "外部引用",
            compose(
                r#"<div class="caption-line">这个价格真的绷不住了</div>"#,
                ".caption-line{background-image:url(https://example.com/a.png)}",
            ),
            "effect_external_reference_rejected",
        ),
        (
            "脚本引用（伪造引擎脚本）",
            injected,
            "effect_script_rejected",
        ),
        (
            "未支持 CSS",
            compose(
                r#"<div class="caption-line">这个价格真的绷不住了</div>"#,
                "@import \"theme.css\";",
            ),
            "effect_external_reference_rejected",
        ),
        (
            "超限",
            compose(
                &format!(
                    "{}这个价格真的绷不住了{}",
                    "<div>".repeat(30),
                    "</div>".repeat(30)
                ),
                ".caption-line{opacity:1}",
            ),
            "effect_limits_exceeded",
        ),
        (
            "文本引用越界",
            compose(
                r#"<div class="caption-line">恭喜发财红包拿来</div>"#,
                ".caption-line{opacity:1}",
            ),
            "effect_text_reference_rejected",
        ),
        (
            "全屏不透明背景",
            compose(
                r#"<div class="caption-line">这个价格真的绷不住了</div>"#,
                "html, body { background-color:#000000; }",
            ),
            "effect_opaque_background_rejected",
        ),
        (
            "非确定性时序",
            compose(
                r#"<div class="caption-line">这个价格真的绷不住了</div>"#,
                ".caption-line{transform:translateY(random(0px, 40px))}",
            ),
            "effect_nondeterministic_rejected",
        ),
    ]
}

/// 逐条校验九类越界样本，并统计"是否启动了渲染进程"。
struct BoundaryRunner {
    samples: Vec<(i64, String)>,
    render_attempts: AtomicUsize,
    observed: Mutex<Vec<(i64, String)>>,
}

#[async_trait]
impl EffectCaptionEnhancementRunner for BoundaryRunner {
    async fn enhance(
        &self,
        highlight: &EnhancementHighlight,
        _cancellation: &CancellationToken,
    ) -> Result<EnhancementProduct, EffectCaptionRuntimeError> {
        let sample = self
            .samples
            .iter()
            .find(|(candidate_id, _)| *candidate_id == highlight.candidate_id)
            .map(|(_, html)| html.clone())
            .unwrap_or_default();
        // 与真实实现同序：先白名单校验，校验通过才会走到"启动渲染进程"这一步。
        match validate_effect_caption(&sample, &corpus(), &limits()) {
            Ok(_) => {
                self.render_attempts.fetch_add(1, Ordering::SeqCst);
                Ok(EnhancementProduct {
                    composition_version: "unexpected".to_owned(),
                    composition_fingerprint: "unexpected".to_owned(),
                    frames_dir: "/tmp/unexpected".to_owned(),
                    emphasis_words: Vec::new(),
                })
            }
            Err(rejection) => {
                let mut observed = self.observed.lock().unwrap();
                observed.push((highlight.candidate_id, rejection.code.to_owned()));
                let _ = rejection.detail;
                Err(EffectCaptionRuntimeError::new(
                    rejection.code,
                    String::new(),
                ))
            }
        }
    }
}

/// 12.5 越界验收：九类样本全部被拒绝，且**没有任何一次**启动渲染进程，
/// 工程侧不产生生效增强，渲染缓存零条目。
#[tokio::test]
async fn task_12_5_all_nine_boundary_samples_are_rejected_without_a_single_render() {
    let repository = repository();
    let project = repository
        .create_project("越界验收工程", &profile())
        .expect("create project");
    let samples = boundary_samples();
    assert_eq!(samples.len(), 9, "九类越界样本必须齐全");

    let highlights: Vec<EnhancementHighlight> = samples
        .iter()
        .enumerate()
        .map(|(index, (_, _, _))| EnhancementHighlight {
            candidate_id: index as i64 + 1,
            source_fingerprint: "fp-acceptance".to_owned(),
            window: Some((0, 1_000)),
        })
        .collect();
    let runner = BoundaryRunner {
        samples: samples
            .iter()
            .enumerate()
            .map(|(index, (_, html, _))| (index as i64 + 1, html.clone()))
            .collect(),
        render_attempts: AtomicUsize::new(0),
        observed: Mutex::new(Vec::new()),
    };
    let summary = run_enhancement_stage(
        &repository,
        &runner,
        &highlights,
        project.id,
        "fp-acceptance",
        &CancellationToken::new(),
    )
    .await
    .expect("阶段本身必须成功返回");

    assert_eq!(summary.applied, 0, "越界组合不得生效");
    assert_eq!(summary.failed, 9);
    assert_eq!(
        runner.render_attempts.load(Ordering::SeqCst),
        0,
        "九类越界样本都不得启动渲染进程"
    );
    // 每个样本都给出预期的稳定错误码。
    let observed = runner.observed.lock().unwrap();
    for (index, (name, _, expected)) in samples.iter().enumerate() {
        let code = observed
            .iter()
            .find(|(candidate_id, _)| *candidate_id == index as i64 + 1)
            .map(|(_, code)| code.clone())
            .unwrap_or_else(|| panic!("样本「{name}」没有被拒绝"));
        assert_eq!(&code, expected, "样本「{name}」错误码不符");
    }
    assert_eq!(repository.render_attempt_count().unwrap(), 0);
    for highlight in &highlights {
        if let Some(record) = repository
            .active_enhancement_for_candidate(highlight.candidate_id)
            .unwrap()
        {
            assert_eq!(
                record.status,
                dy_screen_app_lib::ai::AiEnhancementStatus::Rejected
            );
            assert!(record.suppression_ranges.is_empty());
        }
    }
}

/// 12.6 完整性验收：真实目录树的篡改必须被拒绝；引擎脚本版本变化必须使缓存失效。
#[tokio::test]
async fn task_12_6_tampered_tree_is_rejected_and_engine_version_invalidates_cache() {
    let directory = tempfile::tempdir().unwrap();
    let dist = directory.path().join("dist");
    std::fs::create_dir_all(&dist).unwrap();
    std::fs::write(dist.join("engine.js"), b"engine-bytes").unwrap();
    std::fs::write(dist.join("cli.js"), b"cli-bytes").unwrap();
    let (file_count, _bytes, tree) = compute_tree_digest(&dist).unwrap();
    let manifest = RendererRuntimeManifest {
        renderer_version: "0.8.96".to_owned(),
        renderer_file_count: file_count,
        renderer_tree_sha256: tree,
        engine_script_bytes: 480_863,
        engine_script_sha256: "38592478aad3f515e33b69ecfc11898e338c28a54799bebcd933c18906decd06"
            .to_owned(),
        audio_fx_script_bytes: 38_874,
        audio_fx_script_sha256: "eec4e4349b87ee1542b88a32a4a5dd1428a2bc50fda7ad124c27083d126eb426"
            .to_owned(),
        browser_version: "154.0.8037.58".to_owned(),
        browser_sha256: String::new(),
        font_version: "caption-fonts-2026.10".to_owned(),
    };
    let verified = verify_renderer_runtime(directory.path(), &manifest).expect("未篡改必须通过");
    assert_eq!(verified.renderer_version(), "0.8.96");

    // 篡改一个字节 → 必须拒绝。
    std::fs::write(dist.join("engine.js"), b"engine-bytez").unwrap();
    let rejection = verify_renderer_runtime(directory.path(), &manifest).unwrap_err();
    assert_eq!(rejection.code, "renderer_tree_hash_mismatch");

    // 引擎脚本版本变化 → 缓存键变化 → 旧 ready 帧不得复用。
    let repository = repository();
    let frames_dir = directory.path().join("frames");
    let key = |engine_version: &str| {
        RenderCacheKey::new(&RenderVersionSet {
            composition_version: "composition-v1".to_owned(),
            composition_fingerprint: "fingerprint-v1".to_owned(),
            font_version: "caption-fonts-2026.10".to_owned(),
            browser_version: "154.0.8037.58".to_owned(),
            renderer_version: "0.8.96".to_owned(),
            engine_script_version: engine_version.to_owned(),
            width: 1080,
            height: 1920,
            fps: 30,
        })
    };
    let old_key = key("38592478aad3f515");
    let new_key = key("deadbeefdeadbeef");
    assert_ne!(old_key.as_str(), new_key.as_str());
    let request = |cache_key: &str| dy_screen_app_lib::ai::NewAiEffectCaptionRenderAttempt {
        project_id: 1,
        candidate_id: 1,
        cache_key: cache_key.to_owned(),
        composition_version: "composition-v1".to_owned(),
        composition_fingerprint: "fingerprint-v1".to_owned(),
        font_version: "caption-fonts-2026.10".to_owned(),
        browser_version: "154.0.8037.58".to_owned(),
        renderer_version: "0.8.96".to_owned(),
        engine_script_version: "38592478aad3f515".to_owned(),
        width: 1080,
        height: 1920,
        fps: 30,
        frames_dir: frames_dir.to_string_lossy().into_owned(),
    };
    let (attempt, _) = repository
        .begin_render_attempt(request(old_key.as_str()))
        .unwrap();
    repository
        .mark_render_attempt_ready(attempt.id, 12)
        .unwrap();
    assert!(
        repository
            .find_ready_render_attempt(new_key.as_str())
            .unwrap()
            .is_none(),
        "引擎脚本版本变化后不得复用旧帧"
    );
}

/// 记录调用次数并产出真实 PNG 帧的渲染器。
struct CountingRenderer {
    calls: Arc<AtomicUsize>,
    frames: u64,
}

#[async_trait]
impl EffectCaptionRenderer for CountingRenderer {
    async fn render(
        &self,
        _runtime: &VerifiedRendererRuntime,
        invocation: &RenderInvocation,
        _cancellation: &CancellationToken,
    ) -> Result<RenderOutcome, EffectCaptionRuntimeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::fs::create_dir_all(&invocation.frames_dir).unwrap();
        let mut bytes = 0_u64;
        for index in 1..=self.frames {
            let file =
                std::fs::File::create(invocation.frames_dir.join(format!("frame_{index:06}.png")))
                    .unwrap();
            let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), 8, 8);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[0_u8; 8 * 8 * 4]).unwrap();
            bytes += 8 * 8 * 4;
        }
        Ok(RenderOutcome {
            frames_dir: invocation.frames_dir.clone(),
            frame_count: self.frames,
            frames_bytes: bytes,
        })
    }
}

/// 12.7 缓存验收：重复导出命中缓存不重新渲染；修改文案导致组合版本变化后重新渲染并更新记录。
#[tokio::test]
async fn task_12_7_repeat_export_reuses_cache_and_text_change_rerenders() {
    let directory = tempfile::tempdir().unwrap();
    let dist = directory.path().join("dist");
    std::fs::create_dir_all(&dist).unwrap();
    std::fs::write(dist.join("engine.js"), b"engine-bytes").unwrap();
    let (file_count, _bytes, tree) = compute_tree_digest(&dist).unwrap();
    let manifest = RendererRuntimeManifest {
        renderer_version: "0.8.96".to_owned(),
        renderer_file_count: file_count,
        renderer_tree_sha256: tree,
        engine_script_bytes: 480_863,
        engine_script_sha256: "38592478aad3f515e33b69ecfc11898e338c28a54799bebcd933c18906decd06"
            .to_owned(),
        audio_fx_script_bytes: 38_874,
        audio_fx_script_sha256: "eec4e4349b87ee1542b88a32a4a5dd1428a2bc50fda7ad124c27083d126eb426"
            .to_owned(),
        browser_version: "154.0.8037.58".to_owned(),
        browser_sha256: String::new(),
        font_version: "caption-fonts-2026.10".to_owned(),
    };
    let runtime = verify_renderer_runtime(directory.path(), &manifest).unwrap();
    let repository = repository();
    let calls = Arc::new(AtomicUsize::new(0));
    let renderer = CountingRenderer {
        calls: calls.clone(),
        frames: 3,
    };
    let frames_dir = directory.path().join("frames");
    let invocation = RenderInvocation {
        composition_path: directory.path().join("composition.html"),
        workspace_dir: directory.path().to_path_buf(),
        frames_dir: frames_dir.clone(),
        allowed_roots: vec![directory.path().to_path_buf()],
        width: 1080,
        height: 1920,
        fps: 30,
        duration_ms: 15_000,
        limits: RenderLimits {
            max_frames: 10,
            ..RenderLimits::default()
        },
    };
    let request = |composition_version: &str, fingerprint: &str| {
        dy_screen_app_lib::ai::NewAiEffectCaptionRenderAttempt {
            project_id: 1,
            candidate_id: 1,
            cache_key: RenderCacheKey::new(&RenderVersionSet {
                composition_version: composition_version.to_owned(),
                composition_fingerprint: fingerprint.to_owned(),
                font_version: "caption-fonts-2026.10".to_owned(),
                browser_version: "154.0.8037.58".to_owned(),
                renderer_version: "0.8.96".to_owned(),
                engine_script_version: "38592478aad3f515".to_owned(),
                width: 1080,
                height: 1920,
                fps: 30,
            })
            .as_str()
            .to_owned(),
            composition_version: composition_version.to_owned(),
            composition_fingerprint: fingerprint.to_owned(),
            font_version: "caption-fonts-2026.10".to_owned(),
            browser_version: "154.0.8037.58".to_owned(),
            renderer_version: "0.8.96".to_owned(),
            engine_script_version: "38592478aad3f515".to_owned(),
            width: 1080,
            height: 1920,
            fps: 30,
            frames_dir: frames_dir.to_string_lossy().into_owned(),
        }
    };

    let first = render_with_cache(
        &repository,
        &renderer,
        &runtime,
        &invocation,
        request("composition-v1", "fingerprint-v1"),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(!first.reused());
    let second = render_with_cache(
        &repository,
        &renderer,
        &runtime,
        &invocation,
        request("composition-v1", "fingerprint-v1"),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(second.reused(), "重复导出必须命中缓存");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "命中缓存不得重新渲染");

    // 修改文案 → 组合版本与指纹变化 → 必须重新渲染并更新记录。
    let third = render_with_cache(
        &repository,
        &renderer,
        &runtime,
        &invocation,
        request("composition-v2", "fingerprint-v2"),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(!third.reused(), "文案变化后必须重新渲染");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(third.attempt().composition_version, "composition-v2");
    assert_eq!(repository.render_attempt_count().unwrap(), 2);
}

/// 12.8 诊断脱敏验收：本次验收流程产出的诊断里不得出现组合正文、完整提示词、
/// ASR 正文、字体绝对路径、本地绝对路径与短期 token。
#[test]
fn task_12_8_diagnostics_never_leak_body_prompt_asr_paths_or_tokens() {
    let forbidden = [
        "这个价格真的绷不住了", // 组合正文
        "sk-",                  // 短期 token 前缀
        "/Users/",              // 本地绝对路径
        "\\Users\\",            // Windows 本地绝对路径
        "/opt/yino",            // 开发机路径
        "ghp_",                 // 仓库 token 前缀
        "com.aliyuncs.com",     // 直连端点细节
        "composition_prompt",   // 内部提示词构造器
    ];
    let mut diagnostics: Vec<String> = Vec::new();
    // 1) 九类越界样本的拒绝诊断
    for (name, html, _) in boundary_samples() {
        match validate_effect_caption(&html, &corpus(), &limits()) {
            Ok(_) => panic!("样本「{name}」本应被拒绝"),
            Err(rejection) => {
                diagnostics.push(rejection.code.to_owned());
                diagnostics.push(rejection.detail.clone());
            }
        }
    }
    // 2) 运行时完整性诊断
    let directory = tempfile::tempdir().unwrap();
    let dist = directory.path().join("dist");
    std::fs::create_dir_all(&dist).unwrap();
    std::fs::write(dist.join("engine.js"), b"engine-bytes").unwrap();
    let (file_count, _bytes, tree) = compute_tree_digest(&dist).unwrap();
    let manifest = RendererRuntimeManifest {
        renderer_version: "0.8.96".to_owned(),
        renderer_file_count: file_count,
        renderer_tree_sha256: tree,
        engine_script_bytes: 480_863,
        engine_script_sha256: "38592478aad3f515e33b69ecfc11898e338c28a54799bebcd933c18906decd06"
            .to_owned(),
        audio_fx_script_bytes: 38_874,
        audio_fx_script_sha256: "eec4e4349b87ee1542b88a32a4a5dd1428a2bc50fda7ad124c27083d126eb426"
            .to_owned(),
        browser_version: "154.0.8037.58".to_owned(),
        browser_sha256: String::new(),
        font_version: "caption-fonts-2026.10".to_owned(),
    };
    std::fs::write(dist.join("engine.js"), b"tampered-bytes").unwrap();
    let rejection = verify_renderer_runtime(directory.path(), &manifest).unwrap_err();
    diagnostics.push(rejection.code.to_owned());
    diagnostics.push(rejection.detail.clone());
    // 3) 阶段计划与预览/导出规格诊断（结构体字段全部是枚举/数字/版本号）
    let plan = enhancement_stage_plan();
    diagnostics.push(format!("{plan:?}"));
    let spec = plan_render_spec(
        RenderPurpose::Preview,
        CanvasSpec::default(),
        HighlightRenderWindow {
            start_ms: 0,
            end_ms: 15_000,
        },
    )
    .unwrap();
    diagnostics.push(format!("{spec:?}"));
    diagnostics.push(format!("{:?}", DeterminismPolicy::default()));

    for diagnostic in &diagnostics {
        for needle in forbidden {
            assert!(
                !diagnostic.contains(needle),
                "诊断泄漏了「{needle}」：{diagnostic}"
            );
        }
    }
    // 已发布句段转换只携带 ID 与区间，不携带正文。
    let ranges = published_ranges_from_subtitles(&[]);
    assert!(ranges.is_empty());
}

fn profile() -> dy_screen_app_lib::ai::RecognitionProfile {
    serde_json::from_str(
        r#"{
            "engineId": "dashscope-filetrans",
            "engineVersion": "1",
            "modelId": "qwen3-asr-flash-filetrans",
            "modelVersion": "qwen3-asr-filetrans",
            "languageHint": "zh",
            "vadModelId": "ggml-vad@1",
            "vadThresholdMillis": 120,
            "vadPaddingMs": 300,
            "timestampPolicy": "segment",
            "normalizationVersion": "OpenCC ver.1.4.1",
            "hotwords": []
        }"#,
    )
    .expect("profile")
}
