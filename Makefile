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
PROFILE_URL ?= https://www.douyin.com/user/MS4wLjABAAAAdUzdkD-hjRb0rWmS8d02sHzajJrlII40rcefrLK7Oug
ROOM_URLS ?= $(ROOM_URL)
QUALITY ?= HD1
PROTOCOL ?= flv
OUTPUT ?= recordings
SEGMENT_SECONDS ?= 900
PROBE_TIMEOUT_SECONDS ?= 3
JSON ?= 0
ACCESS_FIXTURE_LOG_DIR ?= /private/tmp/dy-screen-access-fixtures
ACCEPT_ROOM_URL ?= https://live.douyin.com/703940802949
ACCEPT_ROOM_URLS ?= https://live.douyin.com/703940802949 https://live.douyin.com/168376497175 https://live.douyin.com/452086788686
ACCEPT_MINUTES ?= 30
APP_LOG_DIR ?= $(HOME)/Library/Logs/com.yino.dyscreen
ASR_SOURCE ?= $(if $(wildcard resources/asr-source/manifest.json),resources/asr-source,)
ASR_STAGE ?= resources/asr-stage
ASR_RESOURCE_ROOT ?= $(if $(wildcard resources/asr-stage/manifest.json),resources/asr-stage,)
ASR_VIDEO ?=
ASR_TEST_VIDEO ?= $(CURDIR)/tests/fixtures/asr/short_zh.mp4
ASR_TRANSCRIBE_VIDEO ?= $(ASR_TEST_VIDEO)
ASR_TRANSCRIBE_OUTPUT ?= /private/tmp/dy-screen-asr-transcript.json
ASR_PERFORMANCE_OUTPUT ?=
ASR_TARGET_EVIDENCE ?=
ASR_APP ?=
ASR_DMG ?=
ASR_RELEASE_EVIDENCE ?=
ASR_BUNDLES ?= app
EXECUTABLE_SUFFIX := $(if $(filter Windows_NT,$(OS)),.exe,)
ASR_BUNDLE_BINARY ?= target/release/asr-bundle$(EXECUTABLE_SUFFIX)
ASR_INSTALLER ?=
ASR_INSTALL_DIR ?=
ASR_INSTALLED_APP_RELATIVE ?= dy-screen-app.exe
ASR_SIGNER_THUMBPRINT ?=
ASR_SMARTSCREEN_EVIDENCE ?=
ASR_QUALITY_DATASET ?=
ASR_QUALITY_RESULTS ?=
ASR_QUALITY_JSON_REPORT ?=
ASR_QUALITY_MARKDOWN_REPORT ?=
ASR_COMPLETION_WINDOWS_TARGET ?=
ASR_COMPLETION_MACOS_RELEASE ?=
ASR_COMPLETION_WINDOWS_RELEASE ?=
ASR_COMPLETION_MACOS_PERFORMANCE ?=
ASR_COMPLETION_WINDOWS_PERFORMANCE ?=
FFMPEG_SOURCE ?= $(if $(wildcard resources/asr-source/sources/ffmpeg-8.1.2.tar.xz),resources/asr-source/sources/ffmpeg-8.1.2.tar.xz,)
FFMPEG_ASR_OUTPUT ?= resources/asr-build/ffmpeg
WHISPER_SOURCE ?= $(if $(wildcard resources/asr-source/sources/whisper.cpp-v1.9.1.tar.gz),resources/asr-source/sources/whisper.cpp-v1.9.1.tar.gz,)
WHISPER_ASR_OUTPUT ?= resources/asr-build/whisper
POWERSHELL ?= powershell.exe
RESOURCE_BASE_URL ?= https://yino-cut.oss-cn-beijing.aliyuncs.com/cut/stable/0.2.0/macos/aarch64/2026.07.3/
RESOURCE_RELEASE_DIR ?= dist/runtime-resources
RESOURCE_CHANNEL ?= stable
RESOURCE_APP_VERSION ?= 0.2.0
# 必须与 resources/asr-source/manifest.json 中的 bundleVersion 一致。
RESOURCE_BUNDLE_VERSION ?= 2026.07.3

BINARY ?= target/release/dy-screen$(EXECUTABLE_SUFFIX)

QUALITY_ARG = $(if $(strip $(QUALITY)),--quality "$(QUALITY)",)
PROTOCOL_ARG = $(if $(strip $(PROTOCOL)),--protocol "$(PROTOCOL)",)
JSON_ARG = $(if $(filter 1 true yes on,$(JSON)),--json,)
ROOM_ARGS = $(foreach room,$(ROOM_URLS),"$(room)")
ASR_RESOURCE_ROOT_ARG = $(if $(strip $(ASR_RESOURCE_ROOT)),--resource-root "$(ASR_RESOURCE_ROOT)",)

.PHONY: help doctor install web-dev typecheck frontend-build app-dev app-build build core-build \
	release fmt fmt-check lint test test-frontend test-core test-app check spec-validate verify \
	preview-doctor test-preview test-preview-integration thumbnail-doctor test-thumbnail test-thumbnail-integration test-profile test-migration \
	test-supervisor-profile test-tags test-tag-migration test-tag-repository test-tag-service test-tag-ui \
	test-ai-scheduler test-ai-repository test-ai-credentials test-ai-workflow test-ai \
	test-browser-access test-access-core test-room-resolution test-tauri-browser test-access-supervisor test-access-ui test-access-fixtures test-app-lifecycle accept-access-fixtures \
	accept-deepseek \
	diagnose-real-room tail-access-log accept-real-room accept-real-multi \
	asr-ffmpeg-macos asr-whisper-macos asr-whisper-windows asr-stage-macos asr-stage-windows asr-build-macos asr-build-windows app-build-resources app-build-resources-windows runtime-resource-verify runtime-resource-publish \
	asr-test-contract asr-test-media asr-test-vad asr-test-whisper asr-test-cli asr-test-stages asr-transcribe \
	asr-check-windows asr-test-windows-target asr-verify-release-macos asr-verify-release-windows asr-quality-collect asr-quality-evaluate asr-performance-macos asr-performance-windows asr-evidence-audit \
	inspect-profile inspect-room resolve record record-multi clean

help:
	@printf '%s\n' \
		'直播管家：Tauri 2.0 多主播自动监听与录制客户端' \
		'' \
		'首次使用：' \
		'  make doctor          检查 Node、npm、Cargo、FFmpeg 和 FFprobe' \
		'  make install         安装前端依赖' \
		'  make app-dev         启动 Tauri 桌面客户端（前端热更新，Rust 安全手动重启）' \
		'' \
		'客户端目标：' \
		'  make web-dev         仅预览 React 界面（使用浏览器本地模拟数据）' \
		'  make frontend-build  类型检查并构建前端' \
		'  make app-dev         启动 Tauri 开发客户端（禁用强制结束录制的 Rust watcher）' \
		'  make app-build       构建 macOS .app 安装产物' \
		'  make app-build-resources ASR_SOURCE=... 构建强制携带运行资源的发行包（缺资源直接失败）' \
		'  make app-build-resources-windows ASR_SOURCE=... 构建 Windows x64 强制资源发行包' \
		'  make asr-ffmpeg-macos FFMPEG_SOURCE=/ffmpeg-8.1.2.tar.xz 构建 LGPL 应用运行时 FFmpeg' \
		'  make asr-whisper-macos WHISPER_SOURCE=/whisper.cpp-v1.9.1.tar.gz 构建静态 Metal sidecar' \
		'  make asr-whisper-windows WHISPER_SOURCE=C:/whisper.cpp-v1.9.1.tar.gz 构建 SSE4.2 CPU sidecar' \
		'  make asr-stage-macos ASR_SOURCE=/可信资源目录  准备 macOS ASR 随包资源' \
		'  make asr-build-macos ASR_SOURCE=/可信资源目录  构建含本地 ASR 的 macOS .app' \
		'  本机已准备资源时可直接运行 make asr-build-macos；默认复用 resources/asr-source/' \
		'  make asr-build-macos ASR_BUNDLES=app,dmg  同时生成 DMG（需要可用 Finder 会话）' \
		'  make asr-stage-windows ASR_SOURCE=/可信资源目录 准备 Windows ASR 随包资源' \
		'  make asr-build-windows ASR_SOURCE=/可信资源目录 构建 Windows NSIS 安装包' \
		'  make runtime-resource-verify ASR_STAGE=... 校验 Runtime Resource Pack 清单和逐文件哈希' \
		'  make runtime-resource-publish ASR_STAGE=... RESOURCE_RELEASE_DIR=... 输出自有 HTTPS 静态托管目录' \
		'  make asr-check-windows 交叉检查 Windows x64 根/Tauri crate 与严格 Clippy' \
		'  make asr-test-contract 只测试中立契约、资源、错误、取消和调度边界' \
		'  make asr-test-media 只测试 FFprobe、FFmpeg、临时音频和原视频不变' \
		'  make asr-test-vad ASR_RESOURCE_ROOT=... 使用随包 FFmpeg/VAD 运行真实人声、静音和纯音乐测试' \
		'  make asr-test-whisper ASR_RESOURCE_ROOT=... 运行真实识别、运行中取消和结构化输出测试' \
		'  make asr-test-cli ASR_RESOURCE_ROOT=... [ASR_TEST_VIDEO=...] 验证 probe/audio/vad/asr 并输出完整 ASR JSON' \
		'  make asr-test-stages ASR_RESOURCE_ROOT=... [ASR_TEST_VIDEO=...] 顺序执行全部 ASR 阶段入口' \
		'  make asr-transcribe ASR_RESOURCE_ROOT=... [ASR_TRANSCRIBE_VIDEO=绝对路径] 将单个视频转成带时间戳 JSON' \
		'  make asr-test-windows-target ASR_RESOURCE_ROOT=... ASR_TARGET_EVIDENCE=... 在真实 Windows x64 执行 Unicode/取消/CPU/运行库验收' \
		'  make asr-verify-release-macos ASR_APP=/Applications/直播管家.app ASR_DMG=... ASR_VIDEO=... ASR_RELEASE_EVIDENCE=... 验证签名、公证、离线运行' \
		'  make asr-verify-release-windows ASR_INSTALLER=... ASR_INSTALL_DIR=... ASR_VIDEO=... ASR_SIGNER_THUMBPRINT=... ASR_SMARTSCREEN_EVIDENCE=... ASR_RELEASE_EVIDENCE=... 验证安装发行' \
		'  make asr-quality-collect ASR_QUALITY_DATASET=... ASR_QUALITY_RESULTS=... ASR_RESOURCE_ROOT=... 采集至少 10 个授权样本' \
		'  make asr-quality-evaluate ASR_QUALITY_DATASET=... ASR_QUALITY_RESULTS=... ASR_QUALITY_JSON_REPORT=... ASR_QUALITY_MARKDOWN_REPORT=... 生成质量报告' \
		'  make asr-performance-macos ASR_VIDEO=... ASR_RESOURCE_ROOT=... ASR_PERFORMANCE_OUTPUT=... 在 8GB Mac 采集性能' \
		'  make asr-performance-windows ASR_VIDEO=... ASR_RESOURCE_ROOT=... ASR_PERFORMANCE_OUTPUT=... 在 8GB Windows 采集性能' \
		'  make asr-evidence-audit ASR_COMPLETION_*=... ASR_QUALITY_*=... 汇总六个门禁并输出 readyToComplete' \
		'  make preview-doctor  检查视频预览所需 FFmpeg 编码能力' \
		'  make test-preview    执行预览服务和播放器组件测试' \
		'  make test-preview-integration 使用真实 FFmpeg 样本验证预览转换' \
		'  make thumbnail-doctor 检查视频库封面所需 FFmpeg JPEG 编码能力' \
		'  make test-thumbnail 执行封面缓存、批次服务和视频库卡片测试' \
		'  make test-thumbnail-integration 使用真实 FFmpeg 样本验证横竖屏首帧封面' \
		'  make test-profile    执行个人主页 fixture 与脱敏测试' \
		'  make test-migration  执行三层身份数据库迁移测试' \
		'  make test-supervisor-profile 执行主页/直播间双阶段状态机测试' \
		'  make test-tags       执行主播标签后端、迁移和前端测试' \
		'  make test-ai         执行 AI 调度、SQLite、高光工作流和凭据 fake 测试' \
		'  make accept-deepseek 启动桌面端，使用设置页的系统凭据和“测试连接”进行显式真实验收' \
		'  make test-tag-migration 执行主播标签 SQLite migration 测试' \
		'  make test-tag-repository 执行主播标签 repository 与生命周期测试' \
		'  make test-tag-service 执行主播创建、编辑和标签校验服务测试' \
		'  make test-tag-ui     执行标签表单、展示和浏览器演示测试' \
		'  make test-browser-access 执行浏览器会话解析的全部聚焦测试' \
		'  make test-access-core 执行核心浏览器快照和安全诊断测试' \
		'  make test-room-resolution 执行双通道解析服务状态机测试' \
		'  make test-tauri-browser 执行 Tauri 验证窗口安全策略测试' \
		'  make test-access-supervisor 执行监听、验证等待和续录测试' \
		'  make test-access-ui   执行访问横幅、操作和浏览器降级测试' \
		'  make test-access-fixtures 校验 stderr 与 JSONL 验收输出一致' \
		'  make test-app-lifecycle 执行单实例锁和幂等关闭领取测试' \
		'  make accept-access-fixtures [ACCESS_FIXTURE_LOG_DIR=...] 运行本地双页面验收工具' \
		'  make diagnose-real-room [ACCEPT_ROOM_URL=...] 输出真实房间原生 HTTP 脱敏诊断' \
		'  make tail-access-log [APP_LOG_DIR=...] 持续查看当天访问 JSONL' \
		'  make accept-real-room [ACCEPT_ROOM_URL=...] 启动桌面客户端执行单房 WebView/录制验收' \
		'  make accept-real-multi [ACCEPT_ROOM_URLS="..."] [ACCEPT_MINUTES=30] 启动多房长时验收' \
		'  make check           执行格式、Clippy、测试和前端构建' \
		'  make spec-validate   严格校验当前 OpenSpec 变更' \
		'  make verify          执行 check、OpenSpec 校验和桌面应用构建' \
		'' \
		'原录制核心/CLI：' \
		'  make core-build      构建 Rust CLI 调试版本' \
		'  make release         构建 Rust CLI 发布版本' \
		'  make inspect-profile 只读检查公开个人主页及直播入口' \
		'  make inspect-room    安全诊断直播间响应分类（不输出页面正文）' \
		'  make resolve         解析单个直播间及可用清晰度' \
		'  make record          录制单个直播间' \
		'  make record-multi    同时录制多个直播间' \
		'' \
		'常用录制参数：' \
		'  ROOM_URL=<url>               单个直播间地址' \
		'  PROFILE_URL=<url>            公开个人主页地址' \
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

thumbnail-doctor: doctor
	@"$(FFMPEG)" -hide_banner -encoders 2>/dev/null | grep -Eq 'mjpeg' || { printf '%s\n' '错误：当前 FFmpeg 没有可用的 JPEG 封面编码器。' >&2; exit 1; }
	@printf '%s\n' '视频库封面环境检查通过。'

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
	@printf '%s\n' '普通开发构建：不携带发行运行资源；正式发布请使用 make app-build-resources。'
	"$(NPM)" run tauri:build

app-build-resources: asr-stage-macos
	@test -f "$(ASR_STAGE)/runtime-manifest.json" || { printf '%s\n' '错误：Runtime Resource Pack 清单缺失。' >&2; exit 2; }
	@test -n "$(RESOURCE_BASE_URL)" || { printf '%s\n' '错误：正式资源发行构建必须设置 RESOURCE_BASE_URL。' >&2; exit 2; }
	DY_SCREEN_RESOURCE_BASE_URL="$(RESOURCE_BASE_URL)" "$(NPM)" run tauri:build -- --config src-tauri/tauri.macos.conf.json --bundles "$(ASR_BUNDLES)"

app-build-resources-windows: asr-stage-windows
	@test -f "$(ASR_STAGE)/runtime-manifest.json" || { printf '%s\n' '错误：Runtime Resource Pack 清单缺失。' >&2; exit 2; }
	@test -n "$(RESOURCE_BASE_URL)" || { printf '%s\n' '错误：正式资源发行构建必须设置 RESOURCE_BASE_URL。' >&2; exit 2; }
	DY_SCREEN_RESOURCE_BASE_URL="$(RESOURCE_BASE_URL)" "$(NPM)" run tauri:build -- --config src-tauri/tauri.windows.conf.json

runtime-resource-verify:
	@test -n "$(ASR_STAGE)" || { printf '%s\n' '错误：必须指定 ASR_STAGE。' >&2; exit 2; }
	"$(CARGO)" run --offline --bin asr-bundle -- verify --root "$(ASR_STAGE)" --platform macos-aarch64

runtime-resource-publish: runtime-resource-verify
	@test -n "$(RESOURCE_BASE_URL)" || { printf '%s\n' '错误：必须设置 RESOURCE_BASE_URL（仅用于发布说明，应用地址由构建期注入）。' >&2; exit 2; }
	@mkdir -p "$(RESOURCE_RELEASE_DIR)/$(RESOURCE_CHANNEL)/$(RESOURCE_APP_VERSION)/macos/aarch64/$(RESOURCE_BUNDLE_VERSION)"
	cp -R "$(ASR_STAGE)/." "$(RESOURCE_RELEASE_DIR)/$(RESOURCE_CHANNEL)/$(RESOURCE_APP_VERSION)/macos/aarch64/$(RESOURCE_BUNDLE_VERSION)/"
	@printf '{"channel":"%s","appVersion":"%s","platform":"macos","arch":"aarch64","bundleVersion":"%s","manifest":"runtime-manifest.json"}\n' "$(RESOURCE_CHANNEL)" "$(RESOURCE_APP_VERSION)" "$(RESOURCE_BUNDLE_VERSION)" > "$(RESOURCE_RELEASE_DIR)/$(RESOURCE_CHANNEL)/$(RESOURCE_APP_VERSION)/index.json"
	@printf '%s\n' '资源发布目录已生成。请在上传前使用正式 Ed25519 私钥签署 runtime-manifest.json，并将 RESOURCE_BASE_URL 配置到构建环境。'

asr-ffmpeg-macos:
	@test -n "$(FFMPEG_SOURCE)" || { printf '%s\n' '错误：必须通过 FFMPEG_SOURCE 指定 ffmpeg-8.1.2.tar.xz。' >&2; exit 2; }
	./scripts/build-asr-ffmpeg-macos.sh "$(FFMPEG_SOURCE)" "$(FFMPEG_ASR_OUTPUT)"

asr-whisper-macos:
	@test -n "$(WHISPER_SOURCE)" || { printf '%s\n' '错误：必须通过 WHISPER_SOURCE 指定 whisper.cpp-v1.9.1.tar.gz。' >&2; exit 2; }
	./scripts/build-asr-whisper-macos.sh "$(WHISPER_SOURCE)" "$(WHISPER_ASR_OUTPUT)"

asr-whisper-windows:
	@test -n "$(WHISPER_SOURCE)" || { printf '%s\n' '错误：必须通过 WHISPER_SOURCE 指定 whisper.cpp-v1.9.1.tar.gz。' >&2; exit 2; }
	"$(POWERSHELL)" -NoProfile -File scripts/build-asr-whisper-windows.ps1 -SourceArchive "$(WHISPER_SOURCE)" -OutputRoot "$(WHISPER_ASR_OUTPUT)"

asr-stage-macos:
	@test -n "$(ASR_SOURCE)" || { printf '%s\n' '错误：必须通过 ASR_SOURCE 指定已经准备好的可信资源目录。' >&2; exit 2; }
	"$(CARGO)" run --offline --bin asr-bundle -- stage --source "$(ASR_SOURCE)" --target "$(ASR_STAGE)" --platform macos-aarch64

asr-stage-windows:
	@test -n "$(ASR_SOURCE)" || { printf '%s\n' '错误：必须通过 ASR_SOURCE 指定已经准备好的可信资源目录。' >&2; exit 2; }
	"$(CARGO)" run --offline --bin asr-bundle -- stage --source "$(ASR_SOURCE)" --target "$(ASR_STAGE)" --platform windows-x86-64

asr-build-macos: asr-stage-macos
	"$(NPM)" run tauri:build -- --config src-tauri/tauri.macos.conf.json --bundles "$(ASR_BUNDLES)"

asr-build-windows: asr-stage-windows
	"$(NPM)" run tauri:build -- --config src-tauri/tauri.windows.conf.json

asr-check-windows:
	"$(CARGO)" xwin check --all-targets --target x86_64-pc-windows-msvc
	"$(CARGO)" xwin clippy --all-targets --target x86_64-pc-windows-msvc -- -D warnings
	"$(CARGO)" xwin check --manifest-path src-tauri/Cargo.toml --all-targets --target x86_64-pc-windows-msvc
	"$(CARGO)" xwin clippy --manifest-path src-tauri/Cargo.toml --all-targets --target x86_64-pc-windows-msvc -- -D warnings

asr-test-contract:
	"$(CARGO)" test --offline --test asr_contract --test asr_resources --test asr_whisper_adapter --test asr_scheduler --test asr_spec_traceability -- --nocapture

asr-test-media:
	"$(CARGO)" test --offline --test asr_media -- --nocapture

asr-test-vad:
	@test -n "$(ASR_RESOURCE_ROOT)" || { printf '%s\n' '错误：必须通过 ASR_RESOURCE_ROOT 指定当前平台已经封存的 ASR 资源目录。' >&2; exit 2; }
	ASR_RESOURCE_ROOT="$(ASR_RESOURCE_ROOT)" "$(CARGO)" test --offline --test asr_vad_integration -- --ignored --nocapture

asr-test-whisper:
	@test -n "$(ASR_RESOURCE_ROOT)" || { printf '%s\n' '错误：必须通过 ASR_RESOURCE_ROOT 指定当前平台已经封存的 ASR 资源目录。' >&2; exit 2; }
	ASR_RESOURCE_ROOT="$(ASR_RESOURCE_ROOT)" "$(CARGO)" test --offline --test asr_whisper_integration -- --ignored --nocapture

asr-test-cli:
	@test -n "$(ASR_RESOURCE_ROOT)" || { printf '%s\n' '错误：必须通过 ASR_RESOURCE_ROOT 指定当前平台已经封存的 ASR 资源目录。' >&2; exit 2; }
	@test -f "$(ASR_TEST_VIDEO)" || { printf '%s\n' '错误：ASR_TEST_VIDEO 必须指向一个可读取的本地视频。' >&2; exit 2; }
	ASR_RESOURCE_ROOT="$(ASR_RESOURCE_ROOT)" "$(CARGO)" test --offline --test asr_cli_stages -- --ignored --nocapture
	ASR_RESOURCE_ROOT="$(ASR_RESOURCE_ROOT)" "$(CARGO)" run --offline -- asr "$(ASR_TEST_VIDEO)" --json

asr-test-stages: asr-test-contract asr-test-media asr-test-vad asr-test-whisper asr-test-cli

asr-transcribe:
	@test -n "$(ASR_RESOURCE_ROOT)" || { printf '%s\n' '错误：必须通过 ASR_RESOURCE_ROOT 指定当前平台已经封存的 ASR 资源目录。' >&2; exit 2; }
	@test -f "$(ASR_TRANSCRIBE_VIDEO)" || { printf '%s\n' '错误：ASR_TRANSCRIBE_VIDEO 必须指向一个可读取的本地视频。' >&2; exit 2; }
	@test ! -L "$(ASR_TRANSCRIBE_VIDEO)" || { printf '%s\n' '错误：ASR_TRANSCRIBE_VIDEO 不能是符号链接。' >&2; exit 2; }
	@mkdir -p "$(dir $(ASR_TRANSCRIBE_OUTPUT))"
	@printf '正在识别：%s\n' "$(ASR_TRANSCRIBE_VIDEO)"
	ASR_RESOURCE_ROOT="$(ASR_RESOURCE_ROOT)" "$(CARGO)" run --offline -- asr "$(ASR_TRANSCRIBE_VIDEO)" --json > "$(ASR_TRANSCRIBE_OUTPUT)"
	@printf 'ASR JSON 已输出：%s\n' "$(ASR_TRANSCRIBE_OUTPUT)"

asr-test-windows-target:
	@test -n "$(ASR_RESOURCE_ROOT)" || { printf '%s\n' '错误：必须设置 ASR_RESOURCE_ROOT。' >&2; exit 2; }
	@test -n "$(ASR_TARGET_EVIDENCE)" || { printf '%s\n' '错误：必须设置 ASR_TARGET_EVIDENCE。' >&2; exit 2; }
	"$(POWERSHELL)" -NoProfile -File scripts/test-asr-windows-target.ps1 \
		-RepoRoot "$(CURDIR)" -ResourceRoot "$(ASR_RESOURCE_ROOT)" \
		-Output "$(ASR_TARGET_EVIDENCE)" -Cargo "$(CARGO)"

asr-verify-release-macos:
	@test -n "$(ASR_APP)" || { printf '%s\n' '错误：必须设置 ASR_APP。' >&2; exit 2; }
	@test -n "$(ASR_DMG)" || { printf '%s\n' '错误：必须设置 ASR_DMG。' >&2; exit 2; }
	@test -n "$(ASR_VIDEO)" || { printf '%s\n' '错误：必须设置 ASR_VIDEO。' >&2; exit 2; }
	@test -n "$(ASR_RELEASE_EVIDENCE)" || { printf '%s\n' '错误：必须设置 ASR_RELEASE_EVIDENCE。' >&2; exit 2; }
	"$(CARGO)" build --offline --release --bin dy-screen --bin asr-bundle
	./scripts/verify-asr-release-macos.sh \
		--app "$(ASR_APP)" --dmg "$(ASR_DMG)" \
		--asr-cli "$(BINARY)" --asr-bundle "$(ASR_BUNDLE_BINARY)" \
		--video "$(ASR_VIDEO)" --output "$(ASR_RELEASE_EVIDENCE)" --launch-app

asr-verify-release-windows:
	@test -n "$(ASR_INSTALLER)" || { printf '%s\n' '错误：必须设置 ASR_INSTALLER。' >&2; exit 2; }
	@test -n "$(ASR_INSTALL_DIR)" || { printf '%s\n' '错误：必须设置 ASR_INSTALL_DIR。' >&2; exit 2; }
	@test -n "$(ASR_VIDEO)" || { printf '%s\n' '错误：必须设置 ASR_VIDEO。' >&2; exit 2; }
	@test -n "$(ASR_SIGNER_THUMBPRINT)" || { printf '%s\n' '错误：必须设置 ASR_SIGNER_THUMBPRINT。' >&2; exit 2; }
	@test -n "$(ASR_SMARTSCREEN_EVIDENCE)" || { printf '%s\n' '错误：必须设置 ASR_SMARTSCREEN_EVIDENCE。' >&2; exit 2; }
	@test -n "$(ASR_RELEASE_EVIDENCE)" || { printf '%s\n' '错误：必须设置 ASR_RELEASE_EVIDENCE。' >&2; exit 2; }
	"$(CARGO)" build --offline --release --bin dy-screen --bin asr-bundle
	"$(POWERSHELL)" -NoProfile -File scripts/verify-asr-release-windows.ps1 \
		-Installer "$(ASR_INSTALLER)" -InstallDirectory "$(ASR_INSTALL_DIR)" \
		-InstalledAppRelative "$(ASR_INSTALLED_APP_RELATIVE)" \
		-AsrCli "$(BINARY)" -AsrBundle "$(ASR_BUNDLE_BINARY)" -Video "$(ASR_VIDEO)" \
		-ExpectedSignerThumbprint "$(ASR_SIGNER_THUMBPRINT)" \
		-SmartScreenEvidenceId "$(ASR_SMARTSCREEN_EVIDENCE)" -Output "$(ASR_RELEASE_EVIDENCE)"

asr-quality-collect: release
	@test -n "$(ASR_QUALITY_DATASET)" || { printf '%s\n' '错误：必须设置 ASR_QUALITY_DATASET。' >&2; exit 2; }
	@test -n "$(ASR_QUALITY_RESULTS)" || { printf '%s\n' '错误：必须设置 ASR_QUALITY_RESULTS。' >&2; exit 2; }
	"$(CARGO)" run --offline --release --bin asr-quality -- collect \
		--dataset "$(ASR_QUALITY_DATASET)" \
		--results "$(ASR_QUALITY_RESULTS)" \
		--asr-binary "$(BINARY)" $(ASR_RESOURCE_ROOT_ARG) \
		--minimum-samples 10

asr-quality-evaluate:
	@test -n "$(ASR_QUALITY_DATASET)" || { printf '%s\n' '错误：必须设置 ASR_QUALITY_DATASET。' >&2; exit 2; }
	@test -n "$(ASR_QUALITY_RESULTS)" || { printf '%s\n' '错误：必须设置 ASR_QUALITY_RESULTS。' >&2; exit 2; }
	@test -n "$(ASR_QUALITY_JSON_REPORT)" || { printf '%s\n' '错误：必须设置 ASR_QUALITY_JSON_REPORT。' >&2; exit 2; }
	@test -n "$(ASR_QUALITY_MARKDOWN_REPORT)" || { printf '%s\n' '错误：必须设置 ASR_QUALITY_MARKDOWN_REPORT。' >&2; exit 2; }
	"$(CARGO)" run --offline --release --bin asr-quality -- evaluate \
		--dataset "$(ASR_QUALITY_DATASET)" \
		--results "$(ASR_QUALITY_RESULTS)" \
		--json-report "$(ASR_QUALITY_JSON_REPORT)" \
		--markdown-report "$(ASR_QUALITY_MARKDOWN_REPORT)" \
		--minimum-samples 10

asr-performance-macos: release
	@test -n "$(ASR_VIDEO)" || { printf '%s\n' '错误：必须设置 ASR_VIDEO。' >&2; exit 2; }
	@test -n "$(ASR_RESOURCE_ROOT)" || { printf '%s\n' '错误：必须设置 ASR_RESOURCE_ROOT。' >&2; exit 2; }
	@test -n "$(ASR_PERFORMANCE_OUTPUT)" || { printf '%s\n' '错误：必须设置 ASR_PERFORMANCE_OUTPUT。' >&2; exit 2; }
	./scripts/collect-asr-performance-macos.sh \
		--video "$(ASR_VIDEO)" --asr-binary "$(BINARY)" \
		--resource-root "$(ASR_RESOURCE_ROOT)" --output "$(ASR_PERFORMANCE_OUTPUT)" --require-8gb

asr-performance-windows: release
	@test -n "$(ASR_VIDEO)" || { printf '%s\n' '错误：必须设置 ASR_VIDEO。' >&2; exit 2; }
	@test -n "$(ASR_RESOURCE_ROOT)" || { printf '%s\n' '错误：必须设置 ASR_RESOURCE_ROOT。' >&2; exit 2; }
	@test -n "$(ASR_PERFORMANCE_OUTPUT)" || { printf '%s\n' '错误：必须设置 ASR_PERFORMANCE_OUTPUT。' >&2; exit 2; }
	"$(POWERSHELL)" -NoProfile -File scripts/collect-asr-performance-windows.ps1 \
		-Video "$(ASR_VIDEO)" -AsrBinary "$(BINARY)" \
		-ResourceRoot "$(ASR_RESOURCE_ROOT)" -Output "$(ASR_PERFORMANCE_OUTPUT)" -Require8GB

asr-evidence-audit:
	@test -n "$(ASR_COMPLETION_WINDOWS_TARGET)" || { printf '%s\n' '错误：必须设置 ASR_COMPLETION_WINDOWS_TARGET。' >&2; exit 2; }
	@test -n "$(ASR_COMPLETION_MACOS_RELEASE)" || { printf '%s\n' '错误：必须设置 ASR_COMPLETION_MACOS_RELEASE。' >&2; exit 2; }
	@test -n "$(ASR_COMPLETION_WINDOWS_RELEASE)" || { printf '%s\n' '错误：必须设置 ASR_COMPLETION_WINDOWS_RELEASE。' >&2; exit 2; }
	@test -n "$(ASR_COMPLETION_MACOS_PERFORMANCE)" || { printf '%s\n' '错误：必须设置 ASR_COMPLETION_MACOS_PERFORMANCE。' >&2; exit 2; }
	@test -n "$(ASR_COMPLETION_WINDOWS_PERFORMANCE)" || { printf '%s\n' '错误：必须设置 ASR_COMPLETION_WINDOWS_PERFORMANCE。' >&2; exit 2; }
	@test -n "$(ASR_QUALITY_DATASET)" || { printf '%s\n' '错误：必须设置 ASR_QUALITY_DATASET。' >&2; exit 2; }
	@test -n "$(ASR_QUALITY_RESULTS)" || { printf '%s\n' '错误：必须设置 ASR_QUALITY_RESULTS。' >&2; exit 2; }
	@test -n "$(ASR_QUALITY_JSON_REPORT)" || { printf '%s\n' '错误：必须设置 ASR_QUALITY_JSON_REPORT。' >&2; exit 2; }
	@test -n "$(ASR_QUALITY_MARKDOWN_REPORT)" || { printf '%s\n' '错误：必须设置 ASR_QUALITY_MARKDOWN_REPORT。' >&2; exit 2; }
	"$(CARGO)" run --offline --bin asr-evidence -- \
		--windows-target "$(ASR_COMPLETION_WINDOWS_TARGET)" \
		--macos-release "$(ASR_COMPLETION_MACOS_RELEASE)" \
		--windows-release "$(ASR_COMPLETION_WINDOWS_RELEASE)" \
		--macos-performance "$(ASR_COMPLETION_MACOS_PERFORMANCE)" \
		--windows-performance "$(ASR_COMPLETION_WINDOWS_PERFORMANCE)" \
		--quality-dataset "$(ASR_QUALITY_DATASET)" --quality-results "$(ASR_QUALITY_RESULTS)" \
		--quality-json "$(ASR_QUALITY_JSON_REPORT)" --quality-markdown "$(ASR_QUALITY_MARKDOWN_REPORT)"

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

test-thumbnail:
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --test thumbnail
	"$(NPM)" test -- --run ui/src/App.test.tsx ui/src/api.test.ts

test-thumbnail-integration: thumbnail-doctor
	FFMPEG="$(FFMPEG)" FFPROBE="$(FFPROBE)" "$(CARGO)" test --manifest-path src-tauri/Cargo.toml \
		--test thumbnail real_ffmpeg_generates_landscape_portrait_and_video_without_audio -- --ignored --nocapture

test-profile:
	"$(CARGO)" test --test profile_resolver

test-migration:
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --test repository legacy_migration
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --test repository migrated_identity_indexes_are_partial_and_room_id_is_not_unique -- --exact

test-supervisor-profile:
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --test supervisor_profile

test-tag-migration:
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --test streamer_tag_repository \
		v2_migration_preserves_existing_data_and_adds_empty_tag_collections -- --exact

test-tag-repository:
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --test streamer_tag_repository

test-tag-service:
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --test streamer_tags
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --test streamer_service

test-tag-ui:
	"$(NPM)" test -- --run ui/src/App.test.tsx ui/src/api.test.ts

test-tags: test-tag-migration test-tag-repository test-tag-service test-tag-ui

test-ai-scheduler:
	"$(CARGO)" test --offline --test asr_scheduler -- --nocapture

test-ai-repository:
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --offline --test ai_repository -- --nocapture

test-ai-credentials:
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --offline --test ai_llm -- --nocapture

test-ai-workflow:
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --offline ai::highlight::tests -- --nocapture

test-ai: test-ai-scheduler test-ai-repository test-ai-credentials test-ai-workflow

accept-deepseek: doctor
	@printf '%s\n' \
		'真实 DeepSeek 验收只通过桌面端设置页执行，不把 Key 放入命令行、日志或 SQLite。' \
		'启动后进入“设置 → 高光分析 Provider”，保存模型和系统凭据，再点击“测试连接”。' \
		'连接诊断只发送固定提示；确认成功后再在已完成 ASR 项目中显式点击“开始高光分析”。'
	"$(NPM)" run tauri:dev

test-access-core:
	"$(CARGO)" test --test browser_snapshot

test-room-resolution:
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --test room_resolution

test-tauri-browser:
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml tauri_browser::tests

test-access-supervisor:
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --test supervisor
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --test supervisor_profile

test-access-ui:
	"$(NPM)" test -- --run ui/src/App.test.tsx ui/src/api.test.ts

test-access-fixtures:
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --test access_fixture_acceptance

test-app-lifecycle:
	"$(CARGO)" test --manifest-path src-tauri/Cargo.toml --test app_lifecycle

test-browser-access: test-access-core test-room-resolution test-tauri-browser test-access-supervisor test-access-ui test-access-fixtures test-app-lifecycle

accept-access-fixtures:
	@mkdir -p "$(ACCESS_FIXTURE_LOG_DIR)"
	"$(CARGO)" run --manifest-path src-tauri/Cargo.toml --bin access_fixture -- supported "$(ACCESS_FIXTURE_LOG_DIR)"
	"$(CARGO)" run --manifest-path src-tauri/Cargo.toml --bin access_fixture -- challenge "$(ACCESS_FIXTURE_LOG_DIR)"
	@printf '验收 JSONL：%s/dy-screen-%s.jsonl\n' "$(ACCESS_FIXTURE_LOG_DIR)" "$$(date -u +%Y-%m-%d)"

diagnose-real-room: release
	"$(BINARY)" inspect-room "$(ACCEPT_ROOM_URL)" --json

tail-access-log:
	@mkdir -p "$(APP_LOG_DIR)"
	@log="$(APP_LOG_DIR)/dy-screen-$$(date -u +%Y-%m-%d).jsonl"; \
	printf '持续查看：%s\n' "$$log"; \
	touch "$$log"; \
	tail -f "$$log"

accept-real-room: doctor
	@printf '%s\n' \
		'真实网络验收不会自动绕过访问验证。' \
		'启动后请在客户端添加或立即检查：$(ACCEPT_ROOM_URL)' \
		'观察控制台、访问状态横幅、FFmpeg 启动和录像目录；必要时点击“立即验证”。'
	"$(NPM)" run tauri:dev

accept-real-multi: doctor
	@printf '%s\n' \
		'多房长时验收地址：$(ACCEPT_ROOM_URLS)' \
		'要求连续观察至少 $(ACCEPT_MINUTES) 分钟，并确认共享会话串行解析、并发录制和一次同会话续录。' \
		'真实网络验收不会自动进入普通 test/check/CI。'
	"$(NPM)" run tauri:dev

test: test-frontend test-core test-app

check: fmt-check lint test frontend-build

spec-validate:
	openspec validate --all --strict

verify: check spec-validate app-build

resolve: release
	"$(BINARY)" resolve "$(ROOM_URL)" $(QUALITY_ARG) $(PROTOCOL_ARG) $(JSON_ARG)

inspect-profile: release
	"$(BINARY)" inspect-profile "$(PROFILE_URL)" $(JSON_ARG)

inspect-room: release
	"$(BINARY)" inspect-room "$(ROOM_URL)" $(JSON_ARG)

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
