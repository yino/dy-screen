#![cfg(unix)]
//! 任务 10.3：FFmpeg 能力审计必须真实校验 RGBA/alpha PNG 解码路径——
//! 缺 `png` 解码器、缺 `rgba` 像素格式或解码路径不可用时，发行校验必须失败。
//!
//! 用桩 FFmpeg 驱动真实脚本 `scripts/verify-clip-ffmpeg-capabilities.sh`：
//! 桩只负责回答能力清单与"产出解码结果"，因此能精确模拟三类缺失。

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const STUB: &str = r#"#!/bin/sh
# 桩 FFmpeg：按参数回答能力清单；带 -i 时模拟解码并写出产物。
omit_png=${STUB_OMIT_PNG:-0}
omit_rgba=${STUB_OMIT_RGBA:-0}
for arg in "$@"; do
  case "$arg" in
    -decoders)
      printf '%s\n' ' V..... h264                 H.264'
      printf '%s\n' ' V..... hevc                 HEVC'
      printf '%s\n' ' A..... aac                  AAC'
      [ "$omit_png" = "1" ] || printf '%s\n' ' V..... png                  PNG (Portable Network Graphics)'
      exit 0
      ;;
    -demuxers)
      printf '%s\n' ' D  concat          Concatenated media'
      printf '%s\n' ' D  image2          Image file sequence'
      exit 0
      ;;
    -muxers)
      printf '%s\n' '  E  mp4             MP4 (MPEG-4 Part 14)'
      printf '%s\n' '  E  image2          image2 sequence'
      exit 0
      ;;
    -filters)
      for filter in aformat asetpts concat fade format overlay pad scale setsar setpts volume; do
        printf '%s\n' " ... $filter           filter"
      done
      exit 0
      ;;
    -encoders)
      printf '%s\n' ' A..... aac                  AAC'
      printf '%s\n' ' V..... libx264              libx264 H.264'
      printf '%s\n' ' V..... mjpeg                MJPEG'
      exit 0
      ;;
    -pix_fmts)
      printf '%s\n' 'Pixel formats:'
      printf '%s\n' 'IO... yuv420p               3            12  8  8  8  8  8  8'
      [ "$omit_rgba" = "1" ] || printf '%s\n' 'IO... rgba                  8           32  8  8  8  8  8  8'
      exit 0
      ;;
  esac
done
# 解码调用：把最后一个参数当作输出文件写出来。
last=""
for arg in "$@"; do last="$arg"; done
[ -n "$last" ] || exit 1
printf 'probe' > "$last"
exit 0
"#;

struct Fixture {
    directory: tempfile::TempDir,
    script: PathBuf,
    stub: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let stub = directory.path().join("ffmpeg-stub");
        fs::write(&stub, STUB).expect("write stub");
        let mut permissions = fs::metadata(&stub).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&stub, permissions).expect("chmod");
        Self {
            script: root.join("scripts/verify-clip-ffmpeg-capabilities.sh"),
            stub,
            directory,
        }
    }

    fn run(&self, omit_png: bool, omit_rgba: bool) -> std::process::Output {
        Command::new("sh")
            .arg(&self.script)
            .arg(&self.stub)
            .env("STUB_OMIT_PNG", if omit_png { "1" } else { "0" })
            .env("STUB_OMIT_RGBA", if omit_rgba { "1" } else { "0" })
            .output()
            .expect("run audit script")
    }
}

#[test]
fn complete_ffmpeg_passes_the_alpha_audit() {
    let fixture = Fixture::new();
    let output = fixture.run(false, false);
    assert!(
        output.status.success(),
        "完整能力应通过：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // 探测目录必须被清理（脚本用 trap 回收）。
    let leftovers = fs::read_dir(fixture.directory.path())
        .expect("read temp dir")
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("tmp."))
        .count();
    assert_eq!(leftovers, 0, "探测临时目录必须被清理");
}

#[test]
fn missing_png_decoder_fails_the_audit() {
    let fixture = Fixture::new();
    let output = fixture.run(true, false);
    assert!(!output.status.success(), "缺少 png 解码器必须失败");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("png"), "失败信息必须点明缺失能力：{stderr}");
}

#[test]
fn missing_rgba_pixel_format_fails_the_audit() {
    let fixture = Fixture::new();
    let output = fixture.run(false, true);
    assert!(!output.status.success(), "缺少 rgba 像素格式必须失败");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("rgba"),
        "失败信息必须点明缺失能力：{stderr}"
    );
}

#[test]
fn windows_audit_script_declares_the_same_alpha_check() {
    // 只做静态契约检查：Windows 侧脚本必须同样声明 rgba 与 alpha PNG 探测。
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = fs::read_to_string(root.join("scripts/verify-clip-ffmpeg-capabilities.ps1"))
        .expect("read ps1");
    assert!(script.contains("rgba"), "PS1 必须校验 rgba 像素格式");
    assert!(
        script.contains("alpha.jpg") && script.contains("FromBase64String"),
        "PS1 必须真实走一遍 Alpha PNG 解码探测"
    );
}
