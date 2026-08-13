use std::fs;
use std::path::Path;

use dy_screen::asr::AsrBundleManifest;

fn read(relative: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)).unwrap()
}

#[test]
fn makefile_maps_macos_arch_to_rust_resource_and_bundle_targets() {
    let makefile = read("Makefile");
    for required in [
        "MACOS_HOST_ARCH_arm64 := aarch64",
        "MACOS_HOST_ARCH_x86_64 := x86_64",
        "MACOS_ARCH ?= $(MACOS_HOST_ARCH)",
        "MACOS_RUST_TARGET_aarch64 := aarch64-apple-darwin",
        "MACOS_RUST_TARGET_x86_64 := x86_64-apple-darwin",
        "MACOS_RESOURCE_PLATFORM := macos-$(subst _,-,$(MACOS_ARCH))",
        "MACOS_RESOURCE_DIR := macos-$(MACOS_ARCH)",
        "ASR_STAGE ?= resources/asr-stage$(if $(filter x86_64,$(MACOS_ARCH)),-x86_64,)",
        "--target \"$(MACOS_RUST_TARGET)\"",
        "--platform $(MACOS_RESOURCE_PLATFORM)",
        "$(RESOURCE_APP_VERSION)_$(MACOS_ARCH).dmg",
        "build-mac-intel-release:",
        "MACOS_ARCH=x86_64",
        "macos-build-doctor:",
        "command -v rustup",
        "https://*/$(RESOURCE_CHANNEL)/$(RESOURCE_APP_VERSION)/macos/$(MACOS_ARCH)/$(RESOURCE_BUNDLE_VERSION)/",
        "$(RESOURCE_APP_VERSION)/macos/$(MACOS_ARCH)/index.json",
        "\"resourceBaseUrl\":\"%s\"",
    ] {
        assert!(makefile.contains(required), "Makefile 缺少 {required}");
    }
    assert!(makefile.contains("不支持的 MACOS_ARCH"));
}

#[test]
fn macos_native_resource_scripts_accept_and_validate_both_architectures() {
    for relative in [
        "scripts/build-asr-ffmpeg-macos.sh",
        "scripts/build-asr-whisper-macos.sh",
    ] {
        let script = read(relative);
        for required in [
            "aarch64|x86_64",
            "MACOS_ARCH=$3",
            "MACOS_RESOURCE_DIR=macos-$MACOS_ARCH",
            "lipo -archs",
            "assert_macho_architecture",
            "assert_macos_deployment_target",
        ] {
            assert!(script.contains(required), "{relative} 缺少 {required}");
        }
    }

    let ffmpeg = read("scripts/build-asr-ffmpeg-macos.sh");
    assert!(ffmpeg.contains("command -v nasm"));
    assert!(ffmpeg.contains("--arch=\"$APPLE_ARCH\""));
    assert!(ffmpeg.contains("--extra-cflags=\"-arch $APPLE_ARCH -mmacosx-version-min=12.0\""));
    assert!(ffmpeg.contains("--extra-ldflags=\"-arch $APPLE_ARCH -mmacosx-version-min=12.0\""));

    let whisper = read("scripts/build-asr-whisper-macos.sh");
    for required in [
        "-DCMAKE_OSX_ARCHITECTURES=$APPLE_ARCH",
        "-DGGML_NATIVE=OFF",
        "-DGGML_BLAS=OFF",
        "-DGGML_METAL=$GGML_METAL",
        "-DGGML_METAL_EMBED_LIBRARY=$GGML_METAL_EMBED_LIBRARY",
    ] {
        assert!(
            whisper.contains(required),
            "Whisper 构建脚本缺少 {required}"
        );
    }
}

#[test]
fn macos_resource_assembler_creates_architecture_specific_trusted_source() {
    let script = read("scripts/prepare-asr-resources-macos.sh");
    for required in [
        "<aarch64|x86_64>",
        "MACOS_RESOURCE_DIR=macos-$MACOS_ARCH",
        "resources/asr/manifest.json",
        "ffmpeg",
        "whisper-cli",
        "lipo -archs",
        "resource-source-record.txt",
    ] {
        assert!(
            script.contains(required),
            "macOS 资源组装脚本缺少 {required}"
        );
    }
    let verifier = read("scripts/verify-macos-bundle-architecture.sh");
    assert!(verifier.contains("LC_BUILD_VERSION"));
    assert!(verifier.contains("version + 0 <= 12.0"));
}

#[test]
fn manifest_declares_intel_macos_cpu_resources() {
    let manifest = AsrBundleManifest::from_json(&read("resources/asr/manifest.json")).unwrap();
    let intel = manifest.platform("macos", "x86_64").unwrap();
    assert_eq!(intel.accelerator, "cpu");
    assert_eq!(intel.sidecar, "bin/macos-x86_64/whisper-cli");
    assert_eq!(intel.vad_sidecar, "bin/macos-x86_64/vad-speech-segments");
    assert_eq!(intel.ffmpeg, "bin/macos-x86_64/ffmpeg");
    assert_eq!(intel.ffprobe, "bin/macos-x86_64/ffprobe");
    assert_eq!(intel.libraries.len(), 7);
    assert!(
        intel
            .libraries
            .iter()
            .all(|path| path.starts_with("lib/macos-x86_64/"))
    );
}

#[test]
fn asr_bundle_cli_accepts_the_intel_macos_platform() {
    let source = read("src/bin/asr-bundle.rs");
    assert!(source.contains("MacosX86_64"));
    assert!(source.contains("Self::MacosX86_64 => (\"macos\", \"x86_64\")"));
    assert!(source.contains("Self::MacosX86_64 => \"macos-x86-64\""));
}
