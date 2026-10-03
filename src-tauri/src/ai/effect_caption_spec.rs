//! 特效字幕的预览与导出规格（步骤 3 任务组 6）。
//!
//! 两条硬约束：
//!
//! 1. **预览只渲染高光范围**：规格里的时长只来自高光窗口，与工程总时长无关，
//!    因此不可能因为预览而渲染整段工程。
//! 2. **预览与导出共用同一组合、文本、字体与相对布局**：两者的差异只允许是像素分辨率与帧率；
//!    组合版本、组合指纹、字体版本、时长必须一致，否则拒绝复用预览的帧。

use serde::{Deserialize, Serialize};

use super::effect_caption_render::{EffectCaptionRuntimeError, RenderVersionSet};

/// 画布规格：随包冻结的导出分辨率与帧率。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasSpec {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl Default for CanvasSpec {
    fn default() -> Self {
        Self {
            width: 1080,
            height: 1920,
            // 决策四：30 fps 为上限，60 fps 明确不支持。
            fps: 30,
        }
    }
}

/// 高光在工程里的时间窗口（毫秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HighlightRenderWindow {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl HighlightRenderWindow {
    pub fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderPurpose {
    Preview,
    Export,
}

/// 预览相对导出的缩放分母：预览为导出分辨率的 1/2。
pub const PREVIEW_SCALE_DIVISOR: u32 = 2;

/// 预览帧率上限。
pub const PREVIEW_FPS_CAP: u32 = 15;

/// 渲染规格帧率上限（决策四）。
pub const MAX_RENDER_FPS: u32 = 30;

/// 一次预览或导出的渲染规格。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderSpec {
    pub purpose: RenderPurpose,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub duration_ms: u64,
    pub frame_count: u64,
}

impl RenderSpec {
    /// 把本次规格与版本信息组装成缓存键输入。
    ///
    /// 预览与导出会因为宽高/帧率不同而产生不同的键，但组合版本、字体等字段必须一致，
    /// 这正是"差异仅在像素分辨率"的落点。
    pub fn version_set(
        &self,
        composition_version: &str,
        composition_fingerprint: &str,
        font_version: &str,
        browser_version: &str,
        renderer_version: &str,
        engine_script_version: &str,
    ) -> RenderVersionSet {
        RenderVersionSet {
            composition_version: composition_version.to_owned(),
            composition_fingerprint: composition_fingerprint.to_owned(),
            font_version: font_version.to_owned(),
            browser_version: browser_version.to_owned(),
            renderer_version: renderer_version.to_owned(),
            engine_script_version: engine_script_version.to_owned(),
            width: self.width,
            height: self.height,
            fps: self.fps,
        }
    }
}

/// 规划渲染规格（任务 6.1 / 6.2）。
///
/// - 预览：分辨率 = 画布 / [`PREVIEW_SCALE_DIVISOR`]，帧率 = min(画布帧率, [`PREVIEW_FPS_CAP`])；
/// - 导出：分辨率与帧率取自冻结画布；
/// - 两者时长都**只**来自高光窗口。
pub fn plan_render_spec(
    purpose: RenderPurpose,
    canvas: CanvasSpec,
    window: HighlightRenderWindow,
) -> Result<RenderSpec, EffectCaptionRuntimeError> {
    if canvas.width == 0 || canvas.height == 0 {
        return Err(EffectCaptionRuntimeError::new(
            "render_spec_invalid_canvas",
            "画布尺寸必须为正",
        ));
    }
    if canvas.fps == 0 || canvas.fps > MAX_RENDER_FPS {
        return Err(EffectCaptionRuntimeError::new(
            "render_fps_unsupported",
            format!("帧率必须在 1..={MAX_RENDER_FPS} 之间"),
        ));
    }
    if window.end_ms <= window.start_ms {
        return Err(EffectCaptionRuntimeError::new(
            "render_window_invalid",
            "高光时间窗口必须满足 end_ms > start_ms",
        ));
    }
    let duration_ms = window.duration_ms();
    let (width, height, fps) = match purpose {
        RenderPurpose::Preview => (
            even_dimension(canvas.width / PREVIEW_SCALE_DIVISOR),
            even_dimension(canvas.height / PREVIEW_SCALE_DIVISOR),
            canvas.fps.min(PREVIEW_FPS_CAP),
        ),
        RenderPurpose::Export => (
            even_dimension(canvas.width),
            even_dimension(canvas.height),
            canvas.fps,
        ),
    };
    if width == 0 || height == 0 {
        return Err(EffectCaptionRuntimeError::new(
            "render_spec_invalid_canvas",
            "目标分辨率过小",
        ));
    }
    // 帧数只由高光窗口与帧率决定：预览不会因为工程更长而渲染更多帧。
    let frame_count = duration_ms.saturating_mul(u64::from(fps)).div_ceil(1_000);
    Ok(RenderSpec {
        purpose,
        width,
        height,
        fps,
        duration_ms,
        frame_count,
    })
}

fn even_dimension(value: u32) -> u32 {
    match value {
        0 | 1 => 0,
        value => value - (value % 2),
    }
}

/// 校验预览与导出的布局一致性（任务 6.2 / 6.3）。
///
/// 允许的差异：分辨率与帧率。不允许：组合版本、组合指纹、字体版本、时长、宽高比。
pub fn assert_preview_export_consistent(
    preview: &RenderSpec,
    export: &RenderSpec,
    preview_versions: &RenderVersionSet,
    export_versions: &RenderVersionSet,
) -> Result<(), EffectCaptionRuntimeError> {
    if preview.purpose != RenderPurpose::Preview || export.purpose != RenderPurpose::Export {
        return Err(EffectCaptionRuntimeError::new(
            "render_spec_purpose_mismatch",
            "必须按预览与导出的顺序比较规格",
        ));
    }
    for (name, left, right) in [
        (
            "组合版本",
            preview_versions.composition_version.as_str(),
            export_versions.composition_version.as_str(),
        ),
        (
            "组合指纹",
            preview_versions.composition_fingerprint.as_str(),
            export_versions.composition_fingerprint.as_str(),
        ),
        (
            "字体版本",
            preview_versions.font_version.as_str(),
            export_versions.font_version.as_str(),
        ),
        (
            "渲染器版本",
            preview_versions.renderer_version.as_str(),
            export_versions.renderer_version.as_str(),
        ),
        (
            "引擎脚本版本",
            preview_versions.engine_script_version.as_str(),
            export_versions.engine_script_version.as_str(),
        ),
    ] {
        if left != right {
            return Err(EffectCaptionRuntimeError::new(
                "render_spec_version_mismatch",
                format!("预览与导出的{name}不一致，不能复用预览的帧"),
            ));
        }
    }
    if preview.duration_ms != export.duration_ms {
        return Err(EffectCaptionRuntimeError::new(
            "render_spec_duration_mismatch",
            "预览与导出的时长不一致",
        ));
    }
    // 相对布局一致的判定：宽高比必须相同（差异只在像素分辨率）。
    let preview_ratio = f64::from(preview.width) / f64::from(preview.height);
    let export_ratio = f64::from(export.width) / f64::from(export.height);
    if (preview_ratio - export_ratio).abs() > 1e-6 {
        return Err(EffectCaptionRuntimeError::new(
            "render_spec_layout_mismatch",
            "预览与导出的宽高比不一致，相对布局会变化",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window() -> HighlightRenderWindow {
        HighlightRenderWindow {
            start_ms: 12_000,
            end_ms: 27_000,
        }
    }

    #[test]
    fn preview_renders_only_the_highlight_window_at_low_spec() {
        // 工程总时长 10 分钟，高光只有 15 秒：预览规格必须只覆盖高光。
        let project_duration_ms = 600_000_u64;
        let spec = plan_render_spec(RenderPurpose::Preview, CanvasSpec::default(), window())
            .expect("预览规格");
        assert_eq!(spec.width, 540);
        assert_eq!(spec.height, 960);
        assert_eq!(spec.fps, PREVIEW_FPS_CAP);
        assert_eq!(spec.duration_ms, 15_000);
        assert!(
            spec.duration_ms < project_duration_ms,
            "预览时长不得等于工程总时长"
        );
        assert_eq!(spec.frame_count, 225, "15s × 15fps");
    }

    #[test]
    fn export_uses_canvas_dimensions_and_frozen_fps() {
        let spec = plan_render_spec(RenderPurpose::Export, CanvasSpec::default(), window())
            .expect("导出规格");
        assert_eq!(spec.width, 1080);
        assert_eq!(spec.height, 1920);
        assert_eq!(spec.fps, 30);
        assert_eq!(spec.duration_ms, 15_000);
        assert_eq!(spec.frame_count, 450);
    }

    #[test]
    fn spec_rejects_unsupported_fps_invalid_window_and_tiny_canvas() {
        let slow = CanvasSpec {
            fps: 60,
            ..CanvasSpec::default()
        };
        assert_eq!(
            plan_render_spec(RenderPurpose::Export, slow, window())
                .unwrap_err()
                .code,
            "render_fps_unsupported"
        );
        let empty_window = HighlightRenderWindow {
            start_ms: 1_000,
            end_ms: 1_000,
        };
        assert_eq!(
            plan_render_spec(RenderPurpose::Preview, CanvasSpec::default(), empty_window)
                .unwrap_err()
                .code,
            "render_window_invalid"
        );
        let tiny = CanvasSpec {
            width: 2,
            height: 2,
            fps: 30,
        };
        assert_eq!(
            plan_render_spec(RenderPurpose::Preview, tiny, window())
                .unwrap_err()
                .code,
            "render_spec_invalid_canvas"
        );
    }

    fn versions(spec: &RenderSpec, composition: &str, font: &str) -> RenderVersionSet {
        spec.version_set(
            composition,
            "composition-fingerprint",
            font,
            "154.0.8037.58",
            "0.8.96",
            "38592478aad3f515",
        )
    }

    #[test]
    fn preview_and_export_differ_only_in_resolution_and_fps() {
        let preview = plan_render_spec(RenderPurpose::Preview, CanvasSpec::default(), window())
            .expect("preview");
        let export = plan_render_spec(RenderPurpose::Export, CanvasSpec::default(), window())
            .expect("export");
        assert_preview_export_consistent(
            &preview,
            &export,
            &versions(&preview, "composition-v1", "caption-fonts-2026.10"),
            &versions(&export, "composition-v1", "caption-fonts-2026.10"),
        )
        .expect("布局必须一致");

        // 分辨率与帧率不同 → 缓存键不同（不会错误复用预览的帧）。
        let preview_key = super::super::effect_caption_render::RenderCacheKey::new(&versions(
            &preview,
            "composition-v1",
            "caption-fonts-2026.10",
        ));
        let export_key = super::super::effect_caption_render::RenderCacheKey::new(&versions(
            &export,
            "composition-v1",
            "caption-fonts-2026.10",
        ));
        assert_ne!(preview_key, export_key);
    }

    #[test]
    fn changed_composition_or_font_after_preview_blocks_reuse() {
        let preview = plan_render_spec(RenderPurpose::Preview, CanvasSpec::default(), window())
            .expect("preview");
        let export = plan_render_spec(RenderPurpose::Export, CanvasSpec::default(), window())
            .expect("export");

        // 组合版本变化
        assert_eq!(
            assert_preview_export_consistent(
                &preview,
                &export,
                &versions(&preview, "composition-v1", "caption-fonts-2026.10"),
                &versions(&export, "composition-v2", "caption-fonts-2026.10"),
            )
            .unwrap_err()
            .code,
            "render_spec_version_mismatch"
        );

        // 字体版本变化
        assert_eq!(
            assert_preview_export_consistent(
                &preview,
                &export,
                &versions(&preview, "composition-v1", "caption-fonts-2026.10"),
                &versions(&export, "composition-v1", "caption-fonts-2026.11"),
            )
            .unwrap_err()
            .code,
            "render_spec_version_mismatch"
        );

        // 时长变化
        let longer = plan_render_spec(
            RenderPurpose::Export,
            CanvasSpec::default(),
            HighlightRenderWindow {
                start_ms: 12_000,
                end_ms: 30_000,
            },
        )
        .expect("export");
        assert_eq!(
            assert_preview_export_consistent(
                &preview,
                &longer,
                &versions(&preview, "composition-v1", "caption-fonts-2026.10"),
                &versions(&longer, "composition-v1", "caption-fonts-2026.10"),
            )
            .unwrap_err()
            .code,
            "render_spec_duration_mismatch"
        );
    }

    #[test]
    fn different_aspect_ratio_is_rejected_as_layout_change() {
        let preview = plan_render_spec(RenderPurpose::Preview, CanvasSpec::default(), window())
            .expect("preview");
        let square = plan_render_spec(
            RenderPurpose::Export,
            CanvasSpec {
                width: 1080,
                height: 1080,
                fps: 30,
            },
            window(),
        )
        .expect("export");
        assert_eq!(
            assert_preview_export_consistent(
                &preview,
                &square,
                &versions(&preview, "composition-v1", "caption-fonts-2026.10"),
                &versions(&square, "composition-v1", "caption-fonts-2026.10"),
            )
            .unwrap_err()
            .code,
            "render_spec_layout_mismatch"
        );
    }

    #[test]
    fn spec_purpose_order_is_enforced() {
        let preview = plan_render_spec(RenderPurpose::Preview, CanvasSpec::default(), window())
            .expect("preview");
        let export = plan_render_spec(RenderPurpose::Export, CanvasSpec::default(), window())
            .expect("export");
        assert_eq!(
            assert_preview_export_consistent(
                &export,
                &preview,
                &versions(&preview, "composition-v1", "caption-fonts-2026.10"),
                &versions(&export, "composition-v1", "caption-fonts-2026.10"),
            )
            .unwrap_err()
            .code,
            "render_spec_purpose_mismatch"
        );
    }
}
