param(
    [Parameter(Mandatory = $true)]
    [string]$SourceArchive,

    [Parameter(Mandatory = $true)]
    [string]$OutputRoot
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

# 锁定 whisper.cpp v1.9.1 源码归档。脚本必须在 VS 2022 x64 Developer PowerShell 中运行。
$ExpectedSha256 = "279af4ce60dbf397362868f3bacc75b56a4332ac2541cae155070093f6aaf0e3"
$ExpectedVersion = "1.9.1"
$ExpectedCommit = "f049fff95a089aa9969deb009cdd4892b3e74916"
$CiStage = "preflight"

function ConvertTo-CiAnnotationValue {
    param([string]$Value)
    $sanitized = $Value.Replace([IO.Path]::GetTempPath(), "<temp>\")
    if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_WORKSPACE)) {
        $sanitized = $sanitized.Replace($env:GITHUB_WORKSPACE, "<workspace>")
    }
    if ($sanitized.Length -gt 1800) { $sanitized = $sanitized.Substring(0, 1800) }
    return $sanitized.Replace("%", "%25").Replace("`r", "%0D").Replace("`n", "%0A")
}

function Publish-NativeFailure {
    param(
        [string]$Stage,
        [int]$ExitCode,
        [object[]]$Output
    )
    $diagnostic = @($Output |
        ForEach-Object { [string]$_ } |
        Where-Object { $_ -match "(?i)(error|failed|fatal|undefined|not found|no such|MSB\d+|CMake Error)" } |
        Select-Object -Last 8) -join " | "
    if ([string]::IsNullOrWhiteSpace($diagnostic)) {
        $diagnostic = "native command exit $ExitCode"
    }
    $diagnostic = ConvertTo-CiAnnotationValue $diagnostic
    Write-Host "::error title=Windows Whisper native build failed::stage=$Stage, exitCode=$ExitCode, diagnostic=$diagnostic"
}

function Assert-RegularFile {
    param([string]$Path, [string]$Label)
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ($item.PSIsContainer -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "$Label 必须是普通文件，不能是目录或重解析点。"
    }
}

foreach ($Tool in @("cmake.exe", "tar.exe", "dumpbin.exe")) {
    if (-not (Get-Command $Tool -ErrorAction SilentlyContinue)) {
        throw "缺少构建工具 $Tool；请使用 Visual Studio 2022 x64 Developer PowerShell。"
    }
}

if (-not [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [Runtime.InteropServices.OSPlatform]::Windows
) -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant() -ne "x64") {
    throw "Windows x64 ASR sidecar 只能在 64 位 Windows 构建。"
}
Assert-RegularFile -Path $SourceArchive -Label "whisper.cpp 源码归档"
if (Test-Path -LiteralPath $OutputRoot) {
    throw "输出目录已经存在，请使用一个新的目录。"
}

$ActualSha256 = (Get-FileHash -LiteralPath $SourceArchive -Algorithm SHA256).Hash.ToLowerInvariant()
if ($ActualSha256 -ne $ExpectedSha256) {
    throw "whisper.cpp 源码 SHA-256 与锁定值不一致。"
}

$TemporaryRoot = Join-Path ([IO.Path]::GetTempPath()) ("dy-screen-whisper-" + [Guid]::NewGuid())
$SourceRoot = Join-Path $TemporaryRoot "whisper.cpp-f049fff95a089aa9969deb009cdd4892b3e74916"
$BuildRoot = Join-Path $TemporaryRoot "build"
$Succeeded = $false

try {
    $CiStage = "extract-source"
    New-Item -ItemType Directory -Path $TemporaryRoot | Out-Null
    & tar.exe -xf $SourceArchive -C $TemporaryRoot
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath (Join-Path $SourceRoot "CMakeLists.txt"))) {
        throw "whisper.cpp 源码归档目录结构不符合锁定版本。"
    }

    $CiStage = "patch-msvc-sse42"
    $CpuCmake = Join-Path $SourceRoot "ggml\src\ggml-cpu\CMakeLists.txt"
    Assert-RegularFile -Path $CpuCmake -Label "whisper.cpp GGML CPU CMake 文件"
    $CpuCmakeText = [IO.File]::ReadAllText($CpuCmake)
    $Sse42FlagPattern = [Regex]::new(
        '(?m)^[ \t]*list\(APPEND ARCH_FLAGS /arch:SSE4\.2\)\r?\n'
    )
    if ($Sse42FlagPattern.Matches($CpuCmakeText).Count -ne 1) {
        throw "whisper.cpp 的 MSVC SSE4.2 开关与锁定源码不一致。"
    }
    if ($CpuCmakeText -notmatch 'list\(APPEND ARCH_DEFINITIONS GGML_SSE42\)') {
        throw "whisper.cpp 的 MSVC SSE4.2 配置与锁定源码不一致。"
    }
    # cl.exe x64 没有 /arch:SSE4.2 开关；显式 SSE4.2 intrinsic 可直接编译。
    # 仅移除无效开关，保留 GGML_SSE42 定义和 manifest 声明的最低 CPU 能力。
    $CpuCmakeText = $Sse42FlagPattern.Replace($CpuCmakeText, "", 1)
    if ($CpuCmakeText.Contains("/arch:SSE4.2")) {
        throw "whisper.cpp 的无效 MSVC SSE4.2 开关未完全移除。"
    }
    [IO.File]::WriteAllText($CpuCmake, $CpuCmakeText, [Text.UTF8Encoding]::new($false))

    # 关闭 native/AVX/AVX2/FMA/BMI2/F16C，使二进制最低要求与 manifest 的 SSE4.2 一致。
    # Whisper/GGML 与 MSVC CRT 均静态链接；Windows 安装包仍携带 VC++ 运行库以覆盖主程序
    # 和其他发行组件的兼容要求。
    $CiStage = "cmake-configure"
    $configureOutput = @()
    & cmake.exe -S $SourceRoot -B $BuildRoot -A x64 `
        -DCMAKE_BUILD_TYPE=Release `
        -DCMAKE_POLICY_DEFAULT_CMP0091=NEW `
        -DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded `
        -DBUILD_SHARED_LIBS=OFF `
        -DGGML_STATIC=ON `
        -DGGML_NATIVE=OFF `
        -DGGML_SSE42=ON `
        -DGGML_AVX=OFF `
        -DGGML_AVX2=OFF `
        -DGGML_AVX_VNNI=OFF `
        -DGGML_FMA=OFF `
        -DGGML_F16C=OFF `
        -DGGML_BMI2=OFF `
        -DGGML_OPENMP=OFF `
        -DGGML_METAL=OFF `
        -DWHISPER_BUILD_TESTS=OFF `
        -DWHISPER_BUILD_EXAMPLES=ON `
        -DWHISPER_BUILD_SERVER=OFF `
        -DWHISPER_CURL=OFF 2>&1 | Tee-Object -Variable configureOutput
    $configureExitCode = $LASTEXITCODE
    if ($configureExitCode -ne 0) {
        Publish-NativeFailure -Stage $CiStage -ExitCode $configureExitCode -Output $configureOutput
        throw "whisper.cpp CMake 配置失败。"
    }

    $CiStage = "cmake-build"
    $buildOutput = @()
    & cmake.exe --build $BuildRoot --config Release --parallel `
        --target whisper-cli whisper-vad-speech-segments 2>&1 | Tee-Object -Variable buildOutput
    $buildExitCode = $LASTEXITCODE
    if ($buildExitCode -ne 0) {
        Publish-NativeFailure -Stage $CiStage -ExitCode $buildExitCode -Output $buildOutput
        throw "whisper.cpp Windows x64 构建失败。"
    }

    $CiStage = "locate-sidecars"
    $WhisperCli = Get-ChildItem -LiteralPath $BuildRoot -Recurse -Filter "whisper-cli.exe" |
        Where-Object { $_.FullName -match "[\\/]bin[\\/](Release[\\/])?whisper-cli\.exe$" } |
        Select-Object -First 1
    $VadSidecar = Get-ChildItem -LiteralPath $BuildRoot -Recurse -Filter "whisper-vad-speech-segments.exe" |
        Where-Object { $_.FullName -match "[\\/]bin[\\/](Release[\\/])?whisper-vad-speech-segments\.exe$" } |
        Select-Object -First 1
    if (-not $WhisperCli -or -not $VadSidecar) {
        throw "构建完成但没有找到 Windows x64 sidecar。"
    }

    $BinaryRoot = Join-Path $OutputRoot "bin\windows-x86_64"
    $LicenseRoot = Join-Path $OutputRoot "licenses"
    New-Item -ItemType Directory -Path $BinaryRoot, $LicenseRoot | Out-Null
    Copy-Item -LiteralPath $WhisperCli.FullName -Destination (Join-Path $BinaryRoot "whisper-cli.exe")
    Copy-Item -LiteralPath $VadSidecar.FullName -Destination (Join-Path $BinaryRoot "vad-speech-segments.exe")
    Copy-Item -LiteralPath (Join-Path $SourceRoot "LICENSE") -Destination (Join-Path $LicenseRoot "WhisperCpp-MIT.txt")

    $CiStage = "pe-dependency-validation"
    foreach ($Binary in @(
        (Join-Path $BinaryRoot "whisper-cli.exe"),
        (Join-Path $BinaryRoot "vad-speech-segments.exe")
    )) {
        $Headers = (& dumpbin.exe /headers $Binary | Out-String)
        if ($LASTEXITCODE -ne 0 -or $Headers -notmatch "machine \(x64\)") {
            throw "$Binary 不是 Windows x64 PE 文件。"
        }
        $Dependencies = (& dumpbin.exe /dependents $Binary | Out-String)
        if ($LASTEXITCODE -ne 0) {
            throw "无法读取 $Binary 的 PE 依赖。"
        }
        if ($Dependencies -match "(?i)(libwhisper|libggml|libomp|vcomp|vcruntime|msvcp|ucrtbase)[^\r\n]*\.dll") {
            throw "$Binary 仍依赖未随包声明的 Whisper/GGML/OpenMP/MSVC DLL。"
        }
    }

    $CiStage = "version-validation"
    $VersionOutput = & (Join-Path $BinaryRoot "whisper-cli.exe") --version | Out-String
    if ($LASTEXITCODE -ne 0 -or $VersionOutput -notmatch [Regex]::Escape($ExpectedVersion)) {
        throw "whisper-cli 版本与锁定版本不一致。"
    }
    [IO.File]::WriteAllText(
        (Join-Path $OutputRoot "whisper-version.txt"),
        $VersionOutput,
        [Text.UTF8Encoding]::new($false)
    )

    $CiStage = "build-records"
    $BuildRecord = @(
        "source=whisper.cpp-v1.9.1.tar.gz"
        "source_sha256=$ExpectedSha256"
        "source_commit=$ExpectedCommit"
        "architecture=x86_64"
        "minimum_cpu=sse4.2"
        "msvc_sse42_patch=remove-unsupported-arch-flag"
        "avx=disabled"
        "avx2=disabled"
        "linkage=static-whisper-ggml-msvc-runtime"
        "gpu=disabled"
        "network=disabled"
    ) -join [Environment]::NewLine
    [IO.File]::WriteAllText(
        (Join-Path $OutputRoot "build-record.txt"),
        $BuildRecord + [Environment]::NewLine,
        [Text.UTF8Encoding]::new($false)
    )

    Write-Host "Windows x64 whisper.cpp sidecar 已构建到：$OutputRoot"
    $Succeeded = $true
}
catch {
    $diagnostic = ConvertTo-CiAnnotationValue ([string]$_.Exception.Message)
    Write-Host "::error title=Windows Whisper 构建失败::stage=$CiStage, diagnostic=$diagnostic"
    throw
}
finally {
    if (-not $Succeeded -and (Test-Path -LiteralPath $OutputRoot)) {
        Remove-Item -LiteralPath $OutputRoot -Recurse -Force
    }
    if (Test-Path -LiteralPath $TemporaryRoot) {
        Remove-Item -LiteralPath $TemporaryRoot -Recurse -Force
    }
}
