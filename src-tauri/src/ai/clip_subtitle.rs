//! 将工程字幕规划为逐字显示帧，并渲染为受控透明字幕画面序列。

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fontdue::layout::{
    CoordinateSystem, GlyphPosition, HorizontalAlign, Layout, LayoutSettings, TextStyle,
    VerticalAlign, WrapStyle,
};
use fontdue::{Font, FontSettings};
use unicode_segmentation::UnicodeSegmentation;

use super::clip_export::{ClipExportFailure, TextLayerRange, failure};
use super::{
    AiClipSubtitle, AiClipSubtitleFrame, AiTranscriptProjection, ClipExportSource,
    ClipOutputDimensions,
};

static SUBTITLE_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct ClipSubtitleAssets {
    root: PathBuf,
    manifest_path: PathBuf,
}

impl ClipSubtitleAssets {
    pub fn manifest_path(&self) -> &Path {
        &self.manifest_path
    }

    #[cfg(test)]
    fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for ClipSubtitleAssets {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderFrame {
    start_ms: u64,
    end_ms: u64,
    page_text: Option<String>,
    visible_bytes: usize,
}

#[derive(Debug, Clone)]
struct PlannedFrame {
    frame: AiClipSubtitleFrame,
    subtitle_start_ms: u64,
    stable_segment_id: String,
    reveal_index: usize,
}

#[derive(Debug, Clone)]
struct PageUnit {
    text: String,
    line_break_before: bool,
}

#[derive(Debug, Clone)]
struct SubtitlePage {
    units: Vec<PageUnit>,
}

/// 工程已发布字幕后在项目时间轴上的区间（人工编辑结果即输入）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedSubtitleRange {
    pub subtitle_id: i64,
    pub stable_segment_id: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// 特效字幕在某一时刻的可用状态：只有 `Available` 才抑制基础字幕。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectCaptionAvailability {
    Available,
    Removed,
    RenderFailed,
    Rejected,
}

/// 一条特效字幕的时间窗与其可用状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectCaptionWindow {
    pub candidate_id: i64,
    pub window: TextLayerRange,
    pub availability: EffectCaptionAvailability,
}

/// 两层文字的规划结果：同一时间范围只允许出现在其中一侧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextLayerPlan {
    /// 基础字幕被隐藏的区间（这些区间由特效字幕占据）。
    pub suppressed: Vec<TextLayerRange>,
    /// 基础字幕仍然显示的区间。
    pub visible: Vec<TextLayerRange>,
    pub suppressed_subtitle_ids: Vec<i64>,
}

/// 由**工程已发布句段**推导特效字幕时间窗（任务 8.1）。
///
/// 这是抑制与叠加的唯一时间来源：函数签名里根本不存在"模型给出的时间戳"这一输入，
/// 因此模型时间不可能进入抑制计算。未命中的句段 ID 不产生任何影响。
pub fn derive_effect_window(
    published: &[PublishedSubtitleRange],
    segment_ids: &[String],
) -> Option<TextLayerRange> {
    let mut matched: Vec<&PublishedSubtitleRange> = published
        .iter()
        .filter(|range| segment_ids.iter().any(|id| id == &range.stable_segment_id))
        .collect();
    if matched.is_empty() {
        return None;
    }
    matched.sort_by_key(|range| (range.start_ms, range.end_ms));
    Some(TextLayerRange {
        start_ms: matched
            .first()
            .map(|range| range.start_ms)
            .unwrap_or_default(),
        end_ms: matched.last().map(|range| range.end_ms).unwrap_or_default(),
    })
}

fn ranges_overlap(left: TextLayerRange, right: TextLayerRange) -> bool {
    left.start_ms < right.end_ms && right.start_ms < left.end_ms
}

/// 片段级的特效窗口来源：稳定句段 ID 与它在项目时间轴上的区间（片段级前置）。
///
/// 与 `PublishedSubtitleRange` 的区别是它来自**项目转写**而不是合辑已发布字幕，
/// 因而特效窗口可以在建立合辑之前按候选推导。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEffectWindowSource {
    pub stable_segment_id: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// 由项目转写句段构建片段级窗口来源。
///
/// 优先使用项目时间轴坐标（`project_*`）；缺失时退回源内坐标，使单片段工程与
/// 多片段工程的窗口语义一致。
pub fn transcript_effect_window_sources(    projection: &AiTranscriptProjection,
) -> Vec<TranscriptEffectWindowSource> {
    projection
        .inputs
        .iter()
        .flat_map(|input| input.segments.iter())
        .map(|segment| TranscriptEffectWindowSource {
            stable_segment_id: segment.stable_segment_id.clone(),
            start_ms: segment.project_start_ms.unwrap_or(segment.source_start_ms),
            end_ms: segment.project_end_ms.unwrap_or(segment.source_end_ms),
        })
        .collect()
}

/// 由片段级窗口来源推导特效窗口（与 `derive_effect_window` 同构）。
///
/// 不依赖合辑工程：只要候选引用的稳定句段在项目转写中存在即可推导窗口，
/// 因此增强可以在建立合辑之前运行。
pub fn derive_effect_window_from_transcript(
    sources: &[TranscriptEffectWindowSource],
    segment_ids: &[String],
) -> Option<TextLayerRange> {
    let mut matched: Vec<&TranscriptEffectWindowSource> = sources
        .iter()
        .filter(|source| segment_ids.iter().any(|id| id == &source.stable_segment_id))
        .collect();
    if matched.is_empty() {
        return None;
    }
    matched.sort_by_key(|source| (source.start_ms, source.end_ms));
    Some(TextLayerRange {
        start_ms: matched
            .first()
            .map(|source| source.start_ms)
            .unwrap_or_default(),
        end_ms: matched.last().map(|source| source.end_ms).unwrap_or_default(),
    })
}

/// 按 `insert` 单元把工程字幕映射到导出时间轴（步骤 4 任务 5.4）。
///
/// 每个 `insert` 在左片段的工程结束时间处插入一个单元；该点及其之后的字幕整体顺延
/// 插入时长。因此 **插入区间内不会落到任何来源字幕上**（既显示不出、也烧录不进去），
/// 而插入点之后的右侧字幕按映射后的时间继续显示。
/// 未被插入影响的前缀字幕保持原样。
pub fn shift_subtitles_for_inserts(
    subtitles: &[AiClipSubtitle],
    sources: &[ClipExportSource],
) -> Vec<AiClipSubtitle> {
    let mut insert_points: Vec<(u64, u64)> = Vec::new();
    let mut cursor = 0_u64;
    for source in sources {
        cursor = cursor.saturating_add(
            source
                .segment
                .source_end_ms
                .saturating_sub(source.segment.source_start_ms),
        );
        if let Some(insert) = &source.insert_after {
            insert_points.push((cursor, insert.duration_ms));
        }
    }
    if insert_points.is_empty() {
        return subtitles.to_vec();
    }
    subtitles
        .iter()
        .map(|subtitle| {
            let shift: u64 = insert_points
                .iter()
                .filter(|(point, _)| *point <= subtitle.project_start_ms)
                .map(|(_, duration)| *duration)
                .sum();
            if shift == 0 {
                return subtitle.clone();
            }
            AiClipSubtitle {
                project_start_ms: subtitle.project_start_ms.saturating_add(shift),
                project_end_ms: subtitle.project_end_ms.saturating_add(shift),
                ..subtitle.clone()
            }
        })
        .collect()
}

/// 规划基础字幕与特效字幕（任务 8.1–8.4）。
///
/// - 只有 `Available` 的特效字幕会抑制基础字幕；被移除、渲染失败或被拒绝的特效字幕
///   不抑制任何句段，基础字幕按原样恢复（任务 8.3）。
/// - 输入永远是**当前**的已发布句段，因此用户在被抑制范围内新增或修改字幕时会自然重算（任务 8.2）。
/// - 结果与分辨率无关，同一份计划同时供预览与导出使用（任务 8.4）。
pub fn plan_text_layers(
    published: &[PublishedSubtitleRange],
    effects: &[EffectCaptionWindow],
) -> Result<TextLayerPlan, ClipExportFailure> {
    for range in published {
        if range.end_ms <= range.start_ms {
            return Err(failure(
                "invalid_subtitle_range",
                "已发布字幕的时间范围无效",
            ));
        }
    }
    let available: Vec<TextLayerRange> = effects
        .iter()
        .filter(|effect| effect.availability == EffectCaptionAvailability::Available)
        .map(|effect| effect.window)
        .collect();
    for window in &available {
        if window.end_ms <= window.start_ms {
            return Err(failure("invalid_overlay_range", "特效字幕时间窗无效"));
        }
    }

    let mut suppressed = Vec::new();
    let mut visible = Vec::new();
    let mut suppressed_subtitle_ids = Vec::new();
    for range in published {
        let owned = TextLayerRange {
            start_ms: range.start_ms,
            end_ms: range.end_ms,
        };
        if available
            .iter()
            .any(|window| ranges_overlap(*window, owned))
        {
            suppressed.push(owned);
            suppressed_subtitle_ids.push(range.subtitle_id);
        } else {
            visible.push(owned);
        }
    }
    // 自证：同一时间不允许同时出现在两侧（不出现两层文字）。
    for left in &suppressed {
        for right in &visible {
            if ranges_overlap(*left, *right) {
                return Err(failure(
                    "dual_text_layers",
                    "抑制结果同时包含与显示区间重叠的句段",
                ));
            }
        }
    }
    Ok(TextLayerPlan {
        suppressed,
        visible,
        suppressed_subtitle_ids,
    })
}

/// 生成字幕显示帧，并**剔除**落在抑制区间内的帧（任务 8.3）。
///
/// 被剔除的时间范围由特效字幕层接管；不剔除会在同一时间出现两层文字。
pub fn build_clip_subtitle_frames_with_suppression(
    subtitles: &[AiClipSubtitle],
    dimensions: ClipOutputDimensions,
    total_duration_ms: u64,
    suppressed: &[TextLayerRange],
) -> Vec<AiClipSubtitleFrame> {
    build_clip_subtitle_frames(subtitles, dimensions, total_duration_ms)
        .into_iter()
        .filter(|frame| {
            let frame_range = TextLayerRange {
                start_ms: frame.project_start_ms,
                end_ms: frame.project_end_ms,
            };
            !suppressed
                .iter()
                .any(|range| ranges_overlap(*range, frame_range))
        })
        .collect()
}

/// 生成前端预览与导出共同使用的权威逐字显示计划。
pub fn build_clip_subtitle_frames(
    subtitles: &[AiClipSubtitle],
    dimensions: ClipOutputDimensions,
    total_duration_ms: u64,
) -> Vec<AiClipSubtitleFrame> {
    let mut candidates = Vec::new();
    for subtitle in subtitles.iter().filter(|subtitle| !subtitle.hidden) {
        candidates.extend(plan_subtitle(subtitle, dimensions, total_duration_ms));
    }
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut boundaries = candidates
        .iter()
        .flat_map(|candidate| {
            [
                candidate.frame.project_start_ms,
                candidate.frame.project_end_ms,
            ]
        })
        .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut resolved: Vec<AiClipSubtitleFrame> = Vec::new();
    for window in boundaries.windows(2) {
        let start_ms = window[0];
        let end_ms = window[1];
        if end_ms <= start_ms {
            continue;
        }
        let Some(selected) = candidates
            .iter()
            .filter(|candidate| {
                candidate.frame.project_start_ms <= start_ms
                    && candidate.frame.project_end_ms > start_ms
            })
            .max_by(|left, right| {
                left.subtitle_start_ms
                    .cmp(&right.subtitle_start_ms)
                    .then(left.stable_segment_id.cmp(&right.stable_segment_id))
                    .then(left.reveal_index.cmp(&right.reveal_index))
            })
        else {
            continue;
        };
        let mut frame = selected.frame.clone();
        frame.project_start_ms = start_ms;
        frame.project_end_ms = end_ms;
        if let Some(previous) = resolved.last_mut()
            && previous.project_end_ms == start_ms
            && previous.subtitle_id == frame.subtitle_id
            && previous.clip_segment_id == frame.clip_segment_id
            && previous.page_text == frame.page_text
            && previous.visible_text == frame.visible_text
            && previous.hidden_text == frame.hidden_text
        {
            previous.project_end_ms = end_ms;
        } else {
            resolved.push(frame);
        }
    }
    resolved
}

fn plan_subtitle(
    subtitle: &AiClipSubtitle,
    dimensions: ClipOutputDimensions,
    total_duration_ms: u64,
) -> Vec<PlannedFrame> {
    let start_ms = subtitle.project_start_ms.min(total_duration_ms);
    let end_ms = subtitle.project_end_ms.min(total_duration_ms);
    if end_ms <= start_ms {
        return Vec::new();
    }
    let display_units = build_display_units(&subtitle.text);
    if display_units.is_empty() {
        return Vec::new();
    }
    let pages = paginate_units(&display_units, dimensions);
    let total_units = pages.iter().map(|page| page.units.len()).sum::<usize>();
    if total_units == 0 {
        return Vec::new();
    }
    let duration_ms = end_ms.saturating_sub(start_ms);
    let reveal_duration_ms = duration_ms.saturating_mul(85) / 100;
    let step_start = |index: usize| {
        if total_units <= 1 {
            start_ms
        } else {
            start_ms.saturating_add(
                reveal_duration_ms.saturating_mul(index as u64) / (total_units as u64 - 1),
            )
        }
    };

    let mut result = Vec::new();
    let mut global_index = 0_usize;
    for page in pages {
        let page_text = compose_page_text(&page.units, 0, page.units.len());
        for visible_count in 1..=page.units.len() {
            let frame_start_ms = step_start(global_index);
            let frame_end_ms = if global_index + 1 < total_units {
                step_start(global_index + 1)
            } else {
                end_ms
            };
            if frame_end_ms > frame_start_ms {
                result.push(PlannedFrame {
                    frame: AiClipSubtitleFrame {
                        subtitle_id: subtitle.id,
                        clip_segment_id: subtitle.clip_segment_id,
                        project_start_ms: frame_start_ms,
                        project_end_ms: frame_end_ms,
                        page_text: page_text.clone(),
                        visible_text: compose_page_text(&page.units, 0, visible_count),
                        hidden_text: compose_page_text(
                            &page.units,
                            visible_count,
                            page.units.len(),
                        ),
                    },
                    subtitle_start_ms: start_ms,
                    stable_segment_id: subtitle.stable_segment_id.clone(),
                    reveal_index: global_index,
                });
            }
            global_index += 1;
        }
    }
    result
}

fn build_display_units(text: &str) -> Vec<String> {
    let mut units: Vec<String> = Vec::new();
    let mut leading = String::new();
    for grapheme in text.graphemes(true) {
        if is_attached_punctuation(grapheme) {
            if let Some(previous) = units.last_mut() {
                previous.push_str(grapheme);
            } else {
                leading.push_str(grapheme);
            }
            continue;
        }
        let mut unit = std::mem::take(&mut leading);
        unit.push_str(grapheme);
        units.push(unit);
    }
    if !leading.is_empty()
        && let Some(previous) = units.last_mut()
    {
        previous.push_str(&leading);
    }
    units
}

fn is_attached_punctuation(grapheme: &str) -> bool {
    const PUNCTUATION: &str = "，。！？；：、,.!?;:()[]{}（）【】《》〈〉“”‘’…—-~·";
    grapheme.chars().all(|character| {
        character.is_whitespace()
            || character.is_ascii_punctuation()
            || PUNCTUATION.contains(character)
    })
}

fn paginate_units(units: &[String], dimensions: ClipOutputDimensions) -> Vec<SubtitlePage> {
    let font_size = (dimensions.height as f32 * 0.045).clamp(22.0, 64.0);
    let safe_width = dimensions.width as f32 * 0.86;
    let max_line_em = (safe_width / font_size).max(4.0);
    let mut pages = Vec::new();
    let mut page_units = Vec::new();
    let mut line_index = 0_u8;
    let mut line_width = 0.0_f32;

    for unit in units {
        let width = display_unit_width(unit);
        let needs_new_line = !page_units.is_empty() && line_width + width > max_line_em;
        if needs_new_line && line_index == 1 {
            pages.push(SubtitlePage {
                units: std::mem::take(&mut page_units),
            });
            line_index = 0;
            line_width = 0.0;
        }
        let line_break_before = needs_new_line && !page_units.is_empty();
        if line_break_before {
            line_index = 1;
            line_width = 0.0;
        }
        page_units.push(PageUnit {
            text: unit.clone(),
            line_break_before,
        });
        line_width += width;
    }
    if !page_units.is_empty() {
        pages.push(SubtitlePage { units: page_units });
    }
    pages
}

fn display_unit_width(unit: &str) -> f32 {
    if unit.contains('\u{200d}') {
        return 1.0;
    }
    unit.chars()
        .map(|character| {
            if character.is_whitespace() {
                0.35
            } else if character.is_ascii_alphanumeric() {
                0.6
            } else if character.is_ascii_punctuation() {
                0.45
            } else if character.is_ascii() {
                0.65
            } else {
                1.0
            }
        })
        .sum::<f32>()
        .max(0.25)
}

fn compose_page_text(units: &[PageUnit], start: usize, end: usize) -> String {
    let mut text = String::new();
    for unit in units.iter().take(end).skip(start) {
        if unit.line_break_before {
            text.push('\n');
        }
        text.push_str(&unit.text);
    }
    text
}

pub fn render_clip_subtitle_assets(
    subtitles: &[AiClipSubtitle],
    dimensions: ClipOutputDimensions,
    total_duration_ms: u64,
) -> Result<ClipSubtitleAssets, ClipExportFailure> {
    if total_duration_ms == 0 {
        return Err(failure("invalid_segment", "剪辑工程总时长无效"));
    }
    if subtitles.is_empty() {
        return Err(failure(
            "subtitles_incomplete",
            "剪辑片段缺少 ASR 字幕，请先完成识别或移除该片段",
        ));
    }
    let display_frames = build_clip_subtitle_frames(subtitles, dimensions, total_duration_ms);
    let frames = build_render_frames(&display_frames, total_duration_ms);
    let assets = ClipSubtitleAssets::create()?;
    let font = load_platform_chinese_font()?;
    let canvas_height = ((dimensions.height as f32 * 0.26).round() as u32)
        .clamp(64, 360)
        .min(dimensions.height.max(1));
    let mut manifest = String::from("ffconcat version 1.0\n");
    let mut last_file_name = None;
    for (index, frame) in frames.iter().enumerate() {
        let file_name = format!("frame-{index:06}.png");
        let path = assets.root.join(&file_name);
        let pixels = render_frame(
            &font,
            frame.page_text.as_deref(),
            frame.visible_bytes,
            dimensions.width,
            canvas_height,
            dimensions.height,
        );
        write_rgba_png(&path, dimensions.width, canvas_height, &pixels)?;
        manifest.push_str(&format!(
            "file '{file_name}'\nduration {}\n",
            format_seconds(frame.end_ms.saturating_sub(frame.start_ms))
        ));
        last_file_name = Some(file_name);
    }
    if let Some(last_file_name) = last_file_name {
        // concat demuxer 只有在重复最后一帧时才会采用最后一个 duration。
        manifest.push_str(&format!("file '{last_file_name}'\n"));
    }
    std::fs::write(&assets.manifest_path, manifest)
        .map_err(|_| failure("subtitle_write_failed", "无法写入字幕时间清单"))?;
    Ok(assets)
}

fn build_render_frames(
    display_frames: &[AiClipSubtitleFrame],
    total_duration_ms: u64,
) -> Vec<RenderFrame> {
    let mut boundaries = vec![0, total_duration_ms];
    for frame in display_frames {
        boundaries.push(frame.project_start_ms.min(total_duration_ms));
        boundaries.push(frame.project_end_ms.min(total_duration_ms));
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut frames: Vec<RenderFrame> = Vec::new();
    for window in boundaries.windows(2) {
        let start_ms = window[0];
        let end_ms = window[1];
        if end_ms <= start_ms {
            continue;
        }
        let selected = display_frames
            .iter()
            .find(|frame| frame.project_start_ms <= start_ms && frame.project_end_ms > start_ms);
        let page_text = selected.map(|frame| frame.page_text.clone());
        let visible_bytes = selected.map_or(0, |frame| frame.visible_text.len());
        if let Some(previous) = frames.last_mut()
            && previous.end_ms == start_ms
            && previous.page_text == page_text
            && previous.visible_bytes == visible_bytes
        {
            previous.end_ms = end_ms;
        } else {
            frames.push(RenderFrame {
                start_ms,
                end_ms,
                page_text,
                visible_bytes,
            });
        }
    }
    frames
}

impl ClipSubtitleAssets {
    fn create() -> Result<Self, ClipExportFailure> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = SUBTITLE_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "dy-screen-clip-subtitles-{}-{nanos}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&root)
            .map_err(|_| failure("subtitle_write_failed", "无法创建字幕临时目录"))?;
        Ok(Self {
            manifest_path: root.join("subtitles.ffconcat"),
            root,
        })
    }
}

fn load_platform_chinese_font() -> Result<Font, ClipExportFailure> {
    for path in platform_font_candidates() {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        for collection_index in 0..16 {
            let settings = FontSettings {
                collection_index,
                ..FontSettings::default()
            };
            let Ok(font) = Font::from_bytes(bytes.clone(), settings) else {
                break;
            };
            if font.lookup_glyph_index('中') != 0 && font.lookup_glyph_index('A') != 0 {
                return Ok(font);
            }
        }
    }
    Err(failure(
        "subtitle_font_unavailable",
        "系统缺少可用的中文字体，无法生成烧录字幕",
    ))
}

#[cfg(target_os = "macos")]
fn platform_font_candidates() -> Vec<PathBuf> {
    [
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/System/Library/Fonts/STHeiti Medium.ttc",
        "/System/Library/Fonts/PingFang.ttc",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(target_os = "windows")]
fn platform_font_candidates() -> Vec<PathBuf> {
    let root = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    ["msyh.ttc", "msyhbd.ttc", "simhei.ttf"]
        .into_iter()
        .map(|name| root.join("Fonts").join(name))
        .collect()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_font_candidates() -> Vec<PathBuf> {
    [
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

fn render_frame(
    font: &Font,
    page_text: Option<&str>,
    visible_bytes: usize,
    width: u32,
    height: u32,
    video_height: u32,
) -> Vec<u8> {
    let mut pixels = vec![0_u8; width as usize * height as usize * 4];
    let Some(page_text) = page_text.filter(|text| !text.is_empty() && visible_bytes > 0) else {
        return pixels;
    };
    let horizontal_padding = (width as f32 * 0.07).round().max(12.0);
    let vertical_padding = (height as f32 * 0.12).round().max(8.0);
    let max_width = (width as f32 - horizontal_padding * 2.0).max(1.0);
    let max_height = (height as f32 - vertical_padding * 2.0).max(1.0);
    let preferred_size = (video_height as f32 * 0.045).clamp(22.0, 64.0);
    let (glyphs, size) = fit_text(
        font,
        page_text,
        preferred_size,
        horizontal_padding,
        vertical_padding,
        max_width,
        max_height,
    );
    let outline_radius = (size * 0.055).round().clamp(1.0, 4.0) as i32;
    let shadow_offset = outline_radius + 1;
    for glyph in glyphs
        .into_iter()
        .filter(|glyph| glyph.byte_offset < visible_bytes)
    {
        let (_, bitmap) = font.rasterize_config(glyph.key);
        draw_glyph_layer(
            &mut pixels,
            width,
            height,
            &glyph,
            &bitmap,
            shadow_offset,
            shadow_offset,
            [0, 0, 0, 115],
        );
        for offset_y in -outline_radius..=outline_radius {
            for offset_x in -outline_radius..=outline_radius {
                if offset_x * offset_x + offset_y * offset_y > outline_radius * outline_radius {
                    continue;
                }
                draw_glyph_layer(
                    &mut pixels,
                    width,
                    height,
                    &glyph,
                    &bitmap,
                    offset_x,
                    offset_y,
                    [0, 0, 0, 255],
                );
            }
        }
        draw_glyph_layer(
            &mut pixels,
            width,
            height,
            &glyph,
            &bitmap,
            0,
            0,
            [255, 255, 255, 255],
        );
    }
    pixels
}

fn fit_text(
    font: &Font,
    text: &str,
    preferred_size: f32,
    x: f32,
    y: f32,
    max_width: f32,
    max_height: f32,
) -> (Vec<GlyphPosition>, f32) {
    let mut size = preferred_size;
    loop {
        let glyphs = layout_text(font, text, size, x, y, max_width, max_height);
        let fits = glyphs.iter().all(|glyph| {
            glyph.x >= 0.0
                && glyph.y >= 0.0
                && glyph.x + glyph.width as f32 <= x + max_width + 1.0
                && glyph.y + glyph.height as f32 <= y + max_height + 1.0
        });
        if fits || size <= 16.0 {
            return (glyphs, size);
        }
        size -= 2.0;
    }
}

fn layout_text(
    font: &Font,
    text: &str,
    size: f32,
    x: f32,
    y: f32,
    max_width: f32,
    max_height: f32,
) -> Vec<GlyphPosition> {
    let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
    layout.reset(&LayoutSettings {
        x,
        y,
        max_width: Some(max_width),
        max_height: Some(max_height),
        horizontal_align: HorizontalAlign::Center,
        vertical_align: VerticalAlign::Middle,
        line_height: 1.12,
        wrap_style: WrapStyle::Letter,
        wrap_hard_breaks: true,
    });
    layout.append(&[font], &TextStyle::new(text, size, 0));
    layout.glyphs().to_vec()
}

#[allow(clippy::too_many_arguments)]
fn draw_glyph_layer(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    glyph: &GlyphPosition,
    bitmap: &[u8],
    offset_x: i32,
    offset_y: i32,
    color: [u8; 4],
) {
    let origin_x = glyph.x.round() as i32 + offset_x;
    let origin_y = glyph.y.round() as i32 + offset_y;
    for row in 0..glyph.height {
        for column in 0..glyph.width {
            let x = origin_x + column as i32;
            let y = origin_y + row as i32;
            if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
                continue;
            }
            let coverage = bitmap[row * glyph.width + column];
            if coverage == 0 {
                continue;
            }
            let alpha = ((u16::from(coverage) * u16::from(color[3])) / 255) as u8;
            blend_pixel(
                &mut pixels[(y as usize * width as usize + x as usize) * 4..][..4],
                [color[0], color[1], color[2], alpha],
            );
        }
    }
}

fn blend_pixel(destination: &mut [u8], source: [u8; 4]) {
    let source_alpha = source[3] as f32 / 255.0;
    let destination_alpha = destination[3] as f32 / 255.0;
    let output_alpha = source_alpha + destination_alpha * (1.0 - source_alpha);
    if output_alpha <= f32::EPSILON {
        return;
    }
    for channel in 0..3 {
        destination[channel] = ((source[channel] as f32 * source_alpha
            + destination[channel] as f32 * destination_alpha * (1.0 - source_alpha))
            / output_alpha)
            .round() as u8;
    }
    destination[3] = (output_alpha * 255.0).round() as u8;
}

fn write_rgba_png(
    path: &Path,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<(), ClipExportFailure> {
    let file =
        File::create(path).map_err(|_| failure("subtitle_write_failed", "无法创建字幕画面文件"))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|_| failure("subtitle_write_failed", "无法编码字幕画面"))?;
    writer
        .write_image_data(pixels)
        .map_err(|_| failure("subtitle_write_failed", "无法写入字幕画面"))?;
    Ok(())
}

fn format_seconds(milliseconds: u64) -> String {
    format!("{:.3}", milliseconds as f64 / 1_000.0)
}

#[cfg(test)]
mod tests {
    use std::io::BufReader;

    use super::*;
    use crate::ai::{
        AiClipEffect, AiClipSegment, ClipExportInsert, EffectCaptionOverlay, SourceFingerprint,
        validate_single_text_layer,
    };

    fn canvas() -> ClipOutputDimensions {
        ClipOutputDimensions {
            width: 1080,
            height: 1920,
        }
    }

    fn subtitle(id: &str, start_ms: u64, end_ms: u64, text: &str) -> AiClipSubtitle {
        AiClipSubtitle {
            id: 1,
            clip_project_id: 1,
            stable_segment_id: id.to_owned(),
            clip_segment_id: 1,
            input_id: 1,
            original_text: text.to_owned(),
            text: text.to_owned(),
            hidden: false,
            source_start_ms: start_ms,
            source_end_ms: end_ms,
            project_start_ms: start_ms,
            project_end_ms: end_ms,
        }
    }

    #[test]
    fn render_frames_include_transparent_gaps() {
        let display = build_clip_subtitle_frames(
            &[subtitle("a", 200, 800, "第一句")],
            ClipOutputDimensions {
                width: 320,
                height: 240,
            },
            1_000,
        );
        let frames = build_render_frames(&display, 1_000);
        assert_eq!(frames.first().unwrap().page_text, None);
        assert_eq!(frames.first().unwrap().start_ms, 0);
        assert_eq!(frames.first().unwrap().end_ms, 200);
        assert_eq!(frames.last().unwrap().page_text, None);
        assert_eq!(frames.last().unwrap().start_ms, 800);
        assert_eq!(frames.last().unwrap().end_ms, 1_000);
    }

    #[test]
    fn rendered_assets_have_transparent_background_and_are_cleaned_on_drop() {
        let assets = render_clip_subtitle_assets(
            &[subtitle("a", 0, 900, "中文 ASR 字幕，包含标点。")],
            ClipOutputDimensions {
                width: 320,
                height: 240,
            },
            1_000,
        )
        .unwrap();
        let root = assets.root().to_path_buf();
        let manifest = std::fs::read_to_string(assets.manifest_path()).unwrap();
        assert!(manifest.starts_with("ffconcat version 1.0\n"));
        assert!(manifest.contains("file 'frame-000000.png'"));
        assert!(!manifest.contains("中文"));
        assert!(!manifest.contains(root.to_string_lossy().as_ref()));
        let decoder = png::Decoder::new(BufReader::new(
            File::open(root.join("frame-000000.png")).unwrap(),
        ));
        let mut reader = decoder.read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        let pixels = &pixels[..info.buffer_size()];
        assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] > 0));
        assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] == 0));
        assert!(
            !pixels.chunks_exact(4).any(|pixel| {
                pixel[3] == 168 && pixel[0] == 0 && pixel[1] == 0 && pixel[2] == 0
            })
        );
        drop(assets);
        assert!(!root.exists());
    }

    // ---- 步骤 3 任务组 8：基础字幕抑制 ----

    fn range(id: &str, subtitle_id: i64, start_ms: u64, end_ms: u64) -> PublishedSubtitleRange {
        PublishedSubtitleRange {
            subtitle_id,
            stable_segment_id: id.to_owned(),
            start_ms,
            end_ms,
        }
    }

    fn available(candidate_id: i64, start_ms: u64, end_ms: u64) -> EffectCaptionWindow {
        EffectCaptionWindow {
            candidate_id,
            window: TextLayerRange { start_ms, end_ms },
            availability: EffectCaptionAvailability::Available,
        }
    }

    #[test]
    fn effect_window_is_derived_only_from_published_segments() {
        let published = vec![
            range("seg-1", 1, 0, 1_000),
            range("seg-2", 2, 1_000, 2_000),
            range("seg-3", 3, 3_000, 4_000),
        ];
        // 只按句段 ID 推导：时间来自已发布句段，调用方无从传入模型时间。
        assert_eq!(
            derive_effect_window(&published, &["seg-2".to_owned()]),
            Some(TextLayerRange {
                start_ms: 1_000,
                end_ms: 2_000
            })
        );
        assert_eq!(
            derive_effect_window(&published, &["seg-1".to_owned(), "seg-3".to_owned()]),
            Some(TextLayerRange {
                start_ms: 0,
                end_ms: 4_000
            })
        );
        assert_eq!(
            derive_effect_window(&published, &["invented".to_owned()]),
            None,
            "未命中的句段不得产生时间窗"
        );
    }

    #[test]
    fn suppression_recomputes_after_manual_edit() {
        let published = vec![range("seg-1", 1, 0, 1_000), range("seg-2", 2, 1_000, 2_000)];
        let effects = vec![available(7, 0, 1_000)];
        let plan = plan_text_layers(&published, &effects).unwrap();
        assert_eq!(
            plan.suppressed,
            vec![TextLayerRange {
                start_ms: 0,
                end_ms: 1_000
            }]
        );
        assert_eq!(
            plan.visible,
            vec![TextLayerRange {
                start_ms: 1_000,
                end_ms: 2_000
            }]
        );
        assert_eq!(plan.suppressed_subtitle_ids, vec![1]);

        // 用户在抑制范围内修改：把 seg-1 移出该窗口 → 基础字幕恢复显示。
        let edited = vec![
            range("seg-1", 1, 2_000, 3_000),
            range("seg-2", 2, 1_000, 2_000),
        ];
        let plan = plan_text_layers(&edited, &effects).unwrap();
        assert!(plan.suppressed.is_empty(), "移出窗口后不应再被抑制");
        assert_eq!(plan.visible.len(), 2);

        // 用户在抑制范围内新增字幕 → 新句段被抑制。
        let extended = vec![
            range("seg-2", 2, 1_000, 2_000),
            range("seg-new", 9, 500, 900),
        ];
        let plan = plan_text_layers(&extended, &effects).unwrap();
        assert_eq!(
            plan.suppressed,
            vec![TextLayerRange {
                start_ms: 500,
                end_ms: 900
            }]
        );
        assert_eq!(plan.suppressed_subtitle_ids, vec![9]);
        assert_eq!(
            plan.visible,
            vec![TextLayerRange {
                start_ms: 1_000,
                end_ms: 2_000
            }]
        );
    }

    #[test]
    fn removed_or_failed_effect_restores_base_subtitles() {
        let published = vec![range("seg-1", 1, 0, 1_000), range("seg-2", 2, 1_000, 2_000)];
        for availability in [
            EffectCaptionAvailability::Removed,
            EffectCaptionAvailability::RenderFailed,
            EffectCaptionAvailability::Rejected,
        ] {
            let effects = vec![EffectCaptionWindow {
                candidate_id: 7,
                window: TextLayerRange {
                    start_ms: 0,
                    end_ms: 1_000,
                },
                availability,
            }];
            let plan = plan_text_layers(&published, &effects).unwrap();
            assert!(
                plan.suppressed.is_empty(),
                "{availability:?} 不得抑制基础字幕"
            );
            assert_eq!(plan.visible.len(), 2, "基础字幕必须完整恢复");
            let subtitles = [
                subtitle("seg-1", 0, 1_000, "第一句"),
                subtitle("seg-2", 1_000, 2_000, "第二句"),
            ];
            let frames = build_clip_subtitle_frames_with_suppression(
                &subtitles,
                canvas(),
                2_000,
                &plan.suppressed,
            );
            assert!(
                frames.iter().any(|frame| frame.project_start_ms < 1_000),
                "恢复后第一句必须可见"
            );
        }
    }

    #[test]
    fn available_effect_suppresses_base_text_and_never_double_layers() {
        let published = vec![range("seg-1", 1, 0, 1_000), range("seg-2", 2, 1_000, 2_000)];
        let effects = vec![available(7, 0, 1_000)];
        let plan = plan_text_layers(&published, &effects).unwrap();
        // 与导出计划交叉校验：可见区间与特效层不得重叠（不出现两层文字）。
        let overlays = vec![EffectCaptionOverlay {
            manifest_path: std::path::PathBuf::from("/tmp/effect.ffconcat"),
            frames_dir: std::path::PathBuf::from("/tmp/effect-frames"),
            start_ms: 0,
            end_ms: 1_000,
        }];
        validate_single_text_layer(&plan.visible, &overlays).unwrap();

        let subtitles = [
            subtitle("seg-1", 0, 1_000, "第一句"),
            subtitle("seg-2", 1_000, 2_000, "第二句"),
        ];
        let frames = build_clip_subtitle_frames_with_suppression(
            &subtitles,
            canvas(),
            2_000,
            &plan.suppressed,
        );
        assert!(
            !frames.iter().any(|frame| frame.project_start_ms < 1_000),
            "被抑制的第一句不得产生字幕帧，否则会与特效层叠加"
        );
        assert!(
            frames.iter().any(|frame| frame.project_start_ms >= 1_000),
            "未被抑制的第二句必须保留"
        );
    }

    #[test]
    fn same_suppression_plan_drives_preview_and_export() {
        let published = vec![range("seg-1", 1, 0, 1_000), range("seg-2", 2, 1_000, 2_000)];
        let effects = vec![available(7, 0, 1_000)];
        let first = plan_text_layers(&published, &effects).unwrap();
        let second = plan_text_layers(&published, &effects).unwrap();
        assert_eq!(first, second, "同一输入必须得到同一抑制结果");

        // 抑制结果与分辨率无关：预览与导出规格分别构建帧，抑制集合一致。
        let subtitles = [
            subtitle("seg-1", 0, 1_000, "第一句"),
            subtitle("seg-2", 1_000, 2_000, "第二句"),
        ];
        let preview_dimensions = ClipOutputDimensions {
            width: 540,
            height: 960,
        };
        for specs in [canvas(), preview_dimensions] {
            let frames = build_clip_subtitle_frames_with_suppression(
                &subtitles,
                specs,
                2_000,
                &first.suppressed,
            );
            assert!(
                !frames.iter().any(|frame| frame.project_start_ms < 1_000),
                "预览与导出必须使用同一抑制结果"
            );
            assert!(frames.iter().any(|frame| frame.project_start_ms >= 1_000));
        }
    }

    #[test]
    fn invalid_published_range_is_rejected() {
        let published = vec![range("seg-bad", 1, 500, 500)];
        assert_eq!(
            plan_text_layers(&published, &[]).unwrap_err().code,
            "invalid_subtitle_range"
        );
    }

    #[test]
    fn transcript_window_covers_referenced_segments_without_clip_project() {
        let sources = vec![
            TranscriptEffectWindowSource {
                stable_segment_id: "a".to_owned(),
                start_ms: 1_000,
                end_ms: 3_000,
            },
            TranscriptEffectWindowSource {
                stable_segment_id: "b".to_owned(),
                start_ms: 3_000,
                end_ms: 6_000,
            },
        ];
        // 引用 a+b：窗口覆盖两段，无需任何合辑工程。
        assert_eq!(
            derive_effect_window_from_transcript(&sources, &["a".to_owned(), "b".to_owned()]),
            Some(TextLayerRange {
                start_ms: 1_000,
                end_ms: 6_000
            })
        );
        // 只引用 b。
        assert_eq!(
            derive_effect_window_from_transcript(&sources, &["b".to_owned()]),
            Some(TextLayerRange {
                start_ms: 3_000,
                end_ms: 6_000
            })
        );
        // 引用不存在的句段：不伪造窗口。
        assert_eq!(
            derive_effect_window_from_transcript(&sources, &["missing".to_owned()]),
            None
        );
    }

    fn export_source_with_insert(duration_ms: u64, insert_ms: Option<u64>) -> ClipExportSource {
        ClipExportSource {
            segment: AiClipSegment {
                id: 1,
                clip_project_id: 1,
                candidate_id: 1,
                input_id: 1,
                position: 0,
                title: "片段".to_owned(),
                source_start_ms: 0,
                source_end_ms: duration_ms,
                volume_percent: 100,
                effect: AiClipEffect::None,
            },
            source_path: "/safe/input.mp4".to_owned(),
            source_fingerprint: SourceFingerprint {
                normalized_path: "/safe/input.mp4".to_owned(),
                size_bytes: 1,
                modified_at_ms: 1,
                video_id: None,
            },
            subtitles: Vec::new(),
            bridge_after: None,
            replace: None,
            insert_after: insert_ms.map(|duration_ms| ClipExportInsert {
                asset_path: "/safe/animation.mp4".to_owned(),
                duration_ms,
                left_segment_id: 1,
                right_segment_id: 2,
                boundary_id: 7,
            }),
        }
    }

    #[test]
    fn insert_shifts_right_subtitles_and_keeps_the_insert_range_free_of_source_text() {
        // 左片段 0–2000ms（工程时间）后插入 1500ms，右片段 2000–4000ms。
        let sources = vec![
            export_source_with_insert(2_000, Some(1_500)),
            export_source_with_insert(2_000, None),
        ];
        let subtitles = vec![
            subtitle("left", 0, 800, "左侧字幕"),
            subtitle("right", 2_100, 2_900, "右侧字幕"),
        ];
        let shifted = shift_subtitles_for_inserts(&subtitles, &sources);
        // 插入点之前的字幕原样保留。
        assert_eq!(shifted[0].project_start_ms, 0);
        assert_eq!(shifted[0].project_end_ms, 800);
        // 插入点之后的右侧字幕整体顺延 1500ms，继续按映射时间显示。
        assert_eq!(shifted[1].project_start_ms, 3_600);
        assert_eq!(shifted[1].project_end_ms, 4_400);
        // 插入区间 [2000, 3500] 内不落任何来源字幕：既显示不出也烧录不进去。
        assert!(shifted
            .iter()
            .all(|item| item.project_end_ms <= 2_000 || item.project_start_ms >= 3_500));
    }

    #[test]
    fn subtitles_are_untouched_without_inserts() {
        let sources = vec![export_source_with_insert(2_000, None)];
        let subtitles = vec![subtitle("left", 0, 800, "左侧字幕")];
        let shifted = shift_subtitles_for_inserts(&subtitles, &sources);
        assert_eq!(shifted.len(), 1);
        assert_eq!(shifted[0].project_start_ms, 0);
        assert_eq!(shifted[0].project_end_ms, 800);
        assert_eq!(shifted[0].text, "左侧字幕");
    }
}
