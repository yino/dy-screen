//! 将只读 ASR 句段渲染为受控透明字幕画面序列。

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

use super::clip_export::{ClipExportFailure, failure};
use super::{AiClipSubtitle, ClipOutputDimensions};

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
struct SubtitleFrame {
    start_ms: u64,
    end_ms: u64,
    text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PixelRectangle {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
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
    let assets = ClipSubtitleAssets::create()?;
    let font = load_platform_chinese_font()?;
    let frames = build_subtitle_frames(subtitles, total_duration_ms);
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
            frame.text.as_deref(),
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

fn build_subtitle_frames(
    subtitles: &[AiClipSubtitle],
    total_duration_ms: u64,
) -> Vec<SubtitleFrame> {
    let mut boundaries = vec![0, total_duration_ms];
    for subtitle in subtitles {
        boundaries.push(subtitle.project_start_ms.min(total_duration_ms));
        boundaries.push(subtitle.project_end_ms.min(total_duration_ms));
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    let mut frames: Vec<SubtitleFrame> = Vec::new();
    for window in boundaries.windows(2) {
        let start_ms = window[0];
        let end_ms = window[1];
        if end_ms <= start_ms {
            continue;
        }
        let text = subtitles
            .iter()
            .filter(|subtitle| {
                subtitle.project_start_ms <= start_ms && subtitle.project_end_ms > start_ms
            })
            .max_by(|left, right| {
                left.project_start_ms
                    .cmp(&right.project_start_ms)
                    .then(left.stable_segment_id.cmp(&right.stable_segment_id))
            })
            .map(|subtitle| subtitle.normalized_text.clone());
        if let Some(previous) = frames.last_mut()
            && previous.text == text
            && previous.end_ms == start_ms
        {
            previous.end_ms = end_ms;
        } else {
            frames.push(SubtitleFrame {
                start_ms,
                end_ms,
                text,
            });
        }
    }
    frames
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
    text: Option<&str>,
    width: u32,
    height: u32,
    video_height: u32,
) -> Vec<u8> {
    let mut pixels = vec![0_u8; width as usize * height as usize * 4];
    let Some(text) = text.map(normalize_text).filter(|text| !text.is_empty()) else {
        return pixels;
    };
    let horizontal_padding = (width as f32 * 0.07).round().max(12.0);
    let vertical_padding = (height as f32 * 0.12).round().max(8.0);
    let max_width = (width as f32 - horizontal_padding * 2.0).max(1.0);
    let max_height = (height as f32 - vertical_padding * 2.0).max(1.0);
    let preferred_size = (video_height as f32 * 0.045).clamp(22.0, 64.0);
    let (glyphs, _) = fit_text(
        font,
        &text,
        preferred_size,
        horizontal_padding,
        vertical_padding,
        max_width,
        max_height,
    );
    let Some((min_x, min_y, max_x, max_y)) = glyph_bounds(&glyphs, width, height) else {
        return pixels;
    };
    let box_padding_x = (preferred_size * 0.45).round() as i32;
    let box_padding_y = (preferred_size * 0.25).round() as i32;
    fill_rectangle(
        &mut pixels,
        width,
        height,
        PixelRectangle {
            left: min_x - box_padding_x,
            top: min_y - box_padding_y,
            right: max_x + box_padding_x,
            bottom: max_y + box_padding_y,
        },
        [0, 0, 0, 168],
    );
    for glyph in glyphs {
        let (_, bitmap) = font.rasterize_config(glyph.key);
        draw_glyph(&mut pixels, width, height, &glyph, &bitmap);
    }
    pixels
}

fn normalize_text(text: &str) -> String {
    let mut normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() > 180 {
        normalized = normalized.chars().take(179).collect::<String>();
        normalized.push('…');
    }
    normalized
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

fn glyph_bounds(glyphs: &[GlyphPosition], width: u32, height: u32) -> Option<(i32, i32, i32, i32)> {
    let visible = glyphs
        .iter()
        .filter(|glyph| glyph.width > 0 && glyph.height > 0)
        .collect::<Vec<_>>();
    let min_x = visible.iter().map(|glyph| glyph.x.floor() as i32).min()?;
    let min_y = visible.iter().map(|glyph| glyph.y.floor() as i32).min()?;
    let max_x = visible
        .iter()
        .map(|glyph| glyph.x.ceil() as i32 + glyph.width as i32)
        .max()?
        .min(width as i32);
    let max_y = visible
        .iter()
        .map(|glyph| glyph.y.ceil() as i32 + glyph.height as i32)
        .max()?
        .min(height as i32);
    Some((min_x, min_y, max_x, max_y))
}

fn fill_rectangle(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    rectangle: PixelRectangle,
    color: [u8; 4],
) {
    for y in rectangle.top.max(0)..rectangle.bottom.min(height as i32) {
        for x in rectangle.left.max(0)..rectangle.right.min(width as i32) {
            let offset = (y as usize * width as usize + x as usize) * 4;
            pixels[offset..offset + 4].copy_from_slice(&color);
        }
    }
}

fn draw_glyph(pixels: &mut [u8], width: u32, height: u32, glyph: &GlyphPosition, bitmap: &[u8]) {
    let origin_x = glyph.x.round() as i32;
    let origin_y = glyph.y.round() as i32;
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
            let offset = (y as usize * width as usize + x as usize) * 4;
            let background_alpha = pixels[offset + 3] as f32 / 255.0;
            let foreground_alpha = coverage as f32 / 255.0;
            let output_alpha = foreground_alpha + background_alpha * (1.0 - foreground_alpha);
            let foreground_weight = foreground_alpha / output_alpha.max(f32::EPSILON);
            let background_weight = 1.0 - foreground_weight;
            for channel in 0..3 {
                pixels[offset + channel] = (255.0 * foreground_weight
                    + pixels[offset + channel] as f32 * background_weight)
                    .round() as u8;
            }
            pixels[offset + 3] = (output_alpha * 255.0).round() as u8;
        }
    }
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
    use super::*;

    fn subtitle(id: &str, start_ms: u64, end_ms: u64, text: &str) -> AiClipSubtitle {
        AiClipSubtitle {
            stable_segment_id: id.to_owned(),
            clip_segment_id: 1,
            input_id: 1,
            normalized_text: text.to_owned(),
            source_start_ms: start_ms,
            source_end_ms: end_ms,
            project_start_ms: start_ms,
            project_end_ms: end_ms,
        }
    }

    #[test]
    fn subtitle_frames_include_blank_ranges_and_latest_overlap_wins() {
        let frames = build_subtitle_frames(
            &[
                subtitle("a", 200, 1_200, "第一句"),
                subtitle("b", 800, 1_500, "第二句"),
            ],
            2_000,
        );
        assert_eq!(
            frames,
            vec![
                SubtitleFrame {
                    start_ms: 0,
                    end_ms: 200,
                    text: None
                },
                SubtitleFrame {
                    start_ms: 200,
                    end_ms: 800,
                    text: Some("第一句".to_owned())
                },
                SubtitleFrame {
                    start_ms: 800,
                    end_ms: 1_500,
                    text: Some("第二句".to_owned())
                },
                SubtitleFrame {
                    start_ms: 1_500,
                    end_ms: 2_000,
                    text: None
                },
            ]
        );
    }

    #[test]
    fn rendered_assets_use_only_numbered_relative_files_and_are_cleaned_on_drop() {
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
        assert!(root.join("frame-000000.png").is_file());
        drop(assets);
        assert!(!root.exists());
    }
}
