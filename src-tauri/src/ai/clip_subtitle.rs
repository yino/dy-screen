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

use super::clip_export::{ClipExportFailure, failure};
use super::{AiClipSubtitle, AiClipSubtitleFrame, ClipOutputDimensions};

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
}
