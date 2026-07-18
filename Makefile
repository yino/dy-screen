.DEFAULT_GOAL := help
SHELL := /bin/sh

CARGO_CANDIDATES := $(wildcard $(HOME)/.cargo/bin/cargo /opt/homebrew/opt/rustup/bin/cargo)
CARGO ?= $(if $(strip $(CARGO_CANDIDATES)),$(firstword $(CARGO_CANDIDATES)),cargo)
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

.PHONY: help doctor build release fmt fmt-check lint test check resolve record record-multi clean

help:
	@printf '%s\n' \
		'dy-screen：抖音多直播间录制 Demo' \
		'' \
		'常用目标：' \
		'  make doctor       检查 Cargo、FFmpeg 和 FFprobe' \
		'  make release      构建发布二进制' \
		'  make resolve      解析单个直播间及可用清晰度' \
		'  make record       录制单个直播间' \
		'  make record-multi 同时录制多个直播间' \
		'  make check        执行格式检查、Clippy 和全部测试' \
		'' \
		'开发目标：' \
		'  make build        构建调试版本' \
		'  make fmt          格式化 Rust 代码' \
		'  make fmt-check    检查 Rust 格式' \
		'  make lint         执行 Clippy 严格检查' \
		'  make test         执行全部 Rust 测试' \
		'  make clean        清理 Cargo 构建产物，不删除录像' \
		'' \
		'常用参数：' \
		'  ROOM_URL=<url>               单个直播间地址' \
		'  ROOM_URLS="<url1> <url2>"   多个直播间地址' \
		'  QUALITY=HD1                  FULL_HD1/HD1/SD1/SD2' \
		'  PROTOCOL=flv                 flv 或 hls' \
		'  OUTPUT=recordings            录像根目录' \
		'  SEGMENT_SECONDS=900          MKV 分片时长（秒）' \
		'  JSON=1                       输出 JSON/JSON-lines' \
		'' \
		'示例：' \
		'  make record ROOM_URL=https://live.douyin.com/452086788686' \
		'  make record-multi ROOM_URLS="https://live.douyin.com/A https://live.douyin.com/B"'

doctor:
	@command -v "$(CARGO)" >/dev/null 2>&1 || { printf '%s\n' '错误：找不到 Cargo，请安装 Rust stable 或通过 CARGO=/path/to/cargo 指定。' >&2; exit 1; }
	@command -v "$(FFMPEG)" >/dev/null 2>&1 || { printf '%s\n' '错误：找不到 FFmpeg，请安装或通过 FFMPEG=/path/to/ffmpeg 指定。' >&2; exit 1; }
	@command -v "$(FFPROBE)" >/dev/null 2>&1 || { printf '%s\n' '错误：找不到 FFprobe，请安装或通过 FFPROBE=/path/to/ffprobe 指定。' >&2; exit 1; }
	@printf '%s\n' '环境检查通过。'

build:
	"$(CARGO)" build

release:
	"$(CARGO)" build --release

fmt:
	"$(CARGO)" fmt

fmt-check:
	"$(CARGO)" fmt --check

lint:
	"$(CARGO)" clippy --all-targets -- -D warnings

test:
	"$(CARGO)" test --all-targets

check: fmt-check lint test

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
