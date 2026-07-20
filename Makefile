.DEFAULT_GOAL := help
SHELL := /bin/sh

CARGO_CANDIDATES := $(wildcard $(HOME)/.cargo/bin/cargo /opt/homebrew/opt/rustup/bin/cargo)
CARGO ?= $(if $(strip $(CARGO_CANDIDATES)),$(firstword $(CARGO_CANDIDATES)),cargo)
NODE ?= node
NPM ?= npm
FFMPEG ?= ffmpeg
FFPROBE ?= ffprobe

CARGO_BIN_DIR := $(patsubst %/,%,$(dir $(CARGO)))
ifneq ($(CARGO_BIN_DIR),.)
export PATH := $(CARGO_BIN_DIR):$(PATH)
endif

ROOM_URL ?= https://live.douyin.com/452086788686
ROOM_URLS ?= $(ROOM_URL)
QUALITY ?= HD1
PROTOCOL ?= flv
OUTPUT ?= recordings
SEGMENT_SECONDS ?= 900
PROBE_TIMEOUT_SECONDS ?= 3
JSON ?= 0

BINARY ?= target/release/dy-screen

QUALITY_ARG = $(if $(strip $(QUALITY)),--quality "$(QUALITY)",)
PROTOCOL_ARG = $(if $(strip $(PROTOCOL)),--protocol "$(PROTOCOL)",)
JSON_ARG = $(if $(filter 1 true yes on,$(JSON)),--json,)
ROOM_ARGS = $(foreach room,$(ROOM_URLS),"$(room)")

.PHONY: help doctor install web-dev typecheck frontend-build app-dev app-build build core-build \
	release fmt fmt-check lint test test-frontend test-core test-app check spec-validate verify \
	preview-doctor test-preview test-preview-integration resolve record record-multi clean

help:
	@printf '%s\n' \
		'直播管家：Tauri 2.0 多主播自动监听与录制客户端' \
		'' \
		'首次使用：' \
		'  make doctor          检查 Node、npm、Cargo、FFmpeg 和 FFprobe' \
		'  make install         安装前端依赖' \
		'  make app-dev         启动 Tauri 桌面客户端开发模式' \
		'' \
		'客户端目标：' \
		'  make web-dev         仅预览 React 界面（使用浏览器本地模拟数据）' \
		'  make frontend-build  类型检查并构建前端' \
		'  make app-dev         启动 Tauri 开发客户端' \
		'  make app-build       构建 macOS .app 安装产物' \
		'  make preview-doctor  检查视频预览所需 FFmpeg 编码能力' \
		'  make test-preview    执行预览服务和播放器组件测试' \
		'  make test-preview-integration 使用真实 FFmpeg 样本验证预览转换' \
		'  make check           执行格式、Clippy、测试和前端构建' \
		'  make spec-validate   严格校验当前 OpenSpec 变更' \
		'  make verify          执行 check、OpenSpec 校验和桌面应用构建' \
		'' \
		'原录制核心/CLI：' \
		'  make core-build      构建 Rust CLI 调试版本' \
		'  make release         构建 Rust CLI 发布版本' \
		'  make resolve         解析单个直播间及可用清晰度' \
		'  make record          录制单个直播间' \
		'  make record-multi    同时录制多个直播间' \
		'' \
		'常用录制参数：' \
		'  ROOM_URL=<url>               单个直播间地址' \
		'  ROOM_URLS="<url1> <url2>"   多个直播间地址' \
		'  QUALITY=HD1                  FULL_HD1/HD1/SD1/SD2' \
		'  PROTOCOL=flv                 flv 或 hls' \
		'  OUTPUT=recordings            CLI 录像根目录' \
		'  SEGMENT_SECONDS=900          MKV 分片时长（秒）'

doctor:
	@command -v "$(NODE)" >/dev/null 2>&1 || { printf '%s\n' '错误：找不到 Node.js。' >&2; exit 1; }
	@command -v "$(NPM)" >/dev/null 2>&1 || { printf '%s\n' '错误：找不到 npm。' >&2; exit 1; }
	@command -v "$(CARGO)" >/dev/null 2>&1 || { printf '%s\n' '错误：找不到 Cargo，请安装 Rust stable 或通过 CARGO=/path/to/cargo 指定。' >&2; exit 1; }
	@command -v "$(FFMPEG)" >/dev/null 2>&1 || { printf '%s\n' '错误：找不到 FFmpeg，请安装或通过 FFMPEG=/path/to/ffmpeg 指定。' >&2; exit 1; }
	@command -v "$(FFPROBE)" >/dev/null 2>&1 || { printf '%s\n' '错误：找不到 FFprobe，请安装或通过 FFPROBE=/path/to/ffprobe 指定。' >&2; exit 1; }
	@printf '%s\n' '环境检查通过。'

preview-doctor: doctor
	@"$(FFMPEG)" -hide_banner -encoders 2>/dev/null | grep -Eq 'h264_videotoolbox|libx264|h264_mf' || { printf '%s\n' '错误：当前 FFmpeg 没有可用的 H.264 预览编码器。' >&2; exit 1; }
	@printf '%s\n' '视频预览环境检查通过。'

install:
	"$(NPM)" install

web-dev:
	"$(NPM)" run dev

typecheck:
	"$(NPM)" run typecheck

frontend-build:
	"$(NPM)" run build

app-dev:
	"$(NPM)" run tauri:dev

app-build:
	"$(NPM)" run tauri:build

build: core-build frontend-build

core-build:
	"$(CARGO)" build

release:
	"$(CARGO)" build --release

fmt:
	"$(CARGO)" fmt --all
	"$(CARGO)" fmt --manifest-path src-tauri/Cargo.toml --all

fmt-check:
	"$(CARGO)" fmt --all -- --check
	"$(CARGO)" fmt --manifest-path src-tauri/Cargo.toml --all -- --check

lint:
	"$(CARGO)" clippy --all-targets -- -D warnings
	"$(CARGO)" clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings

test-frontend:
	"$(NPM)" test

test-core:
	"$(CARGO)" test --all-targets

test-app:
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --all-targets

test-preview:
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --test preview
	"$(NPM)" test -- --run ui/src/App.test.tsx

test-preview-integration: preview-doctor
	FFMPEG="$(FFMPEG)" FFPROBE="$(FFPROBE)" "$(CARGO)" test --manifest-path src-tauri/Cargo.toml \
		--test preview real_ffmpeg_handles_remux_transcode_and_video_without_audio -- --ignored --nocapture

test: test-frontend test-core test-app

check: fmt-check lint test frontend-build

spec-validate:
	openspec validate --all --strict

verify: check spec-validate app-build

resolve: release
	"$(BINARY)" resolve "$(ROOM_URL)" $(QUALITY_ARG) $(PROTOCOL_ARG) $(JSON_ARG)

record: release
	"$(BINARY)" record "$(ROOM_URL)" $(QUALITY_ARG) $(PROTOCOL_ARG) \
		--output "$(OUTPUT)" \
		--segment-seconds "$(SEGMENT_SECONDS)" \
		--ffmpeg "$(FFMPEG)" \
		--ffprobe "$(FFPROBE)" \
		--probe-timeout-seconds "$(PROBE_TIMEOUT_SECONDS)" $(JSON_ARG)

record-multi: release
	@set -- $(ROOM_ARGS); \
	if [ "$$#" -lt 2 ]; then \
		printf '%s\n' '错误：record-multi 至少需要两个地址，请设置 ROOM_URLS="url1 url2"。' >&2; \
		exit 2; \
	fi; \
	"$(BINARY)" record "$$@" $(QUALITY_ARG) $(PROTOCOL_ARG) \
		--output "$(OUTPUT)" \
		--segment-seconds "$(SEGMENT_SECONDS)" \
		--ffmpeg "$(FFMPEG)" \
		--ffprobe "$(FFPROBE)" \
		--probe-timeout-seconds "$(PROBE_TIMEOUT_SECONDS)" $(JSON_ARG)

clean:
	"$(CARGO)" clean
	"$(CARGO)" clean --manifest-path src-tauri/Cargo.toml
