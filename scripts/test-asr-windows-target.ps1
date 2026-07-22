# 在真实 Windows x64 目标机上执行 OpenSpec 任务 1.6 的完整 ASR 验收。
# 报告只保存哈希、退出码和布尔结论，不保存本地路径或测试日志原文。
[CmdletBinding()]
param(
    [string]$RepoRoot = (Get-Location).Path,
    [Parameter(Mandatory = $true)]
    [string]$ResourceRoot,
    [string]$Fixture = "tests/fixtures/asr/short_zh.mp4",
    [Parameter(Mandatory = $true)]
    [string]$Output,
    [string]$Cargo = "cargo"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-RegularFile {
    param([string]$Path, [string]$Label)
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ($item.PSIsContainer -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "$Label 必须是普通文件，不能是目录或重解析点。"
    }
}

function Assert-RegularDirectory {
    param([string]$Path, [string]$Label)
    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if (-not $item.PSIsContainer -or (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "$Label 必须是普通目录，不能是重解析点。"
    }
}

function Get-Sha256 {
    param([string]$Path)
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Get-TextSha256 {
    param([string]$Text)
    $bytes = [Text.Encoding]::UTF8.GetBytes($Text)
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        return ([BitConverter]::ToString($sha.ComputeHash($bytes))).Replace("-", "").ToLowerInvariant()
    }
    finally {
        $sha.Dispose()
    }
}

function ConvertTo-WindowsCommandLineArgument {
    param([string]$Value)
    if ($Value.Length -gt 0 -and $Value -notmatch '[\s"]') { return $Value }
    $builder = [Text.StringBuilder]::new()
    [void]$builder.Append('"')
    $backslashes = 0
    foreach ($character in $Value.ToCharArray()) {
        if ($character -eq '\') {
            $backslashes++
            continue
        }
        if ($character -eq '"') {
            [void]$builder.Append(('\' * (($backslashes * 2) + 1)))
            [void]$builder.Append('"')
            $backslashes = 0
            continue
        }
        if ($backslashes -gt 0) {
            [void]$builder.Append(('\' * $backslashes))
            $backslashes = 0
        }
        [void]$builder.Append($character)
    }
    if ($backslashes -gt 0) {
        [void]$builder.Append(('\' * ($backslashes * 2)))
    }
    [void]$builder.Append('"')
    return $builder.ToString()
}

function Invoke-CapturedProcess {
    param(
        [string]$FileName,
        [string[]]$Arguments,
        [string]$WorkingDirectory,
        [hashtable]$Environment = @{}
    )
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $FileName
    $startInfo.Arguments = (($Arguments | ForEach-Object { ConvertTo-WindowsCommandLineArgument $_ }) -join " ")
    $startInfo.WorkingDirectory = $WorkingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.StandardOutputEncoding = [Text.Encoding]::UTF8
    $startInfo.StandardErrorEncoding = [Text.Encoding]::UTF8
    foreach ($entry in $Environment.GetEnumerator()) {
        $startInfo.EnvironmentVariables[[string]$entry.Key] = [string]$entry.Value
    }
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    if (-not $process.Start()) { throw "无法启动目标测试进程。" }
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    $process.WaitForExit()
    $stopwatch.Stop()
    $result = [pscustomobject]@{
        ExitCode = $process.ExitCode
        WallMs = [int64]$stopwatch.ElapsedMilliseconds
        Stdout = $stdoutTask.GetAwaiter().GetResult()
        Stderr = $stderrTask.GetAwaiter().GetResult()
    }
    $process.Dispose()
    return $result
}

function Write-AtomicJson {
    param([string]$Path, [object]$Value)
    if (Test-Path -LiteralPath $Path) {
        throw "目标机验收证据已存在；为保留不可变证据不会覆盖。"
    }
    $absolute = [IO.Path]::GetFullPath($Path)
    $parent = [IO.Path]::GetDirectoryName($absolute)
    [IO.Directory]::CreateDirectory($parent) | Out-Null
    $temporary = [IO.Path]::Combine($parent, "." + [IO.Path]::GetFileName($absolute) + "." + $PID + ".part")
    try {
        $json = $Value | ConvertTo-Json -Depth 8
        $stream = [IO.FileStream]::new(
            $temporary,
            [IO.FileMode]::CreateNew,
            [IO.FileAccess]::Write,
            [IO.FileShare]::None
        )
        try {
            $bytes = [Text.UTF8Encoding]::new($false).GetBytes($json + [Environment]::NewLine)
            $stream.Write($bytes, 0, $bytes.Length)
            $stream.Flush($true)
        }
        finally {
            $stream.Dispose()
        }
        [IO.File]::Move($temporary, $absolute)
    }
    catch {
        if (Test-Path -LiteralPath $temporary) {
            Remove-Item -LiteralPath $temporary -Force
        }
        throw
    }
}

if (-not [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [Runtime.InteropServices.OSPlatform]::Windows
)) {
    throw "该验收脚本只能在 Windows x64 目标机执行。"
}
$architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
if ($architecture -ne "x64") { throw "该验收脚本只能在 Windows x64 目标机执行。" }

$repo = [IO.Path]::GetFullPath($RepoRoot)
$resourceRootAbsolute = [IO.Path]::GetFullPath($ResourceRoot)
$fixtureAbsolute = if ([IO.Path]::IsPathRooted($Fixture)) {
    [IO.Path]::GetFullPath($Fixture)
}
else {
    [IO.Path]::GetFullPath((Join-Path $repo $Fixture))
}
Assert-RegularDirectory -Path $repo -Label "仓库"
Assert-RegularDirectory -Path $resourceRootAbsolute -Label "ASR 资源目录"
Assert-RegularFile -Path $fixtureAbsolute -Label "固定中文视频"
$manifestPath = Join-Path $resourceRootAbsolute "manifest.json"
Assert-RegularFile -Path $manifestPath -Label "资源 manifest"
if (Test-Path -LiteralPath $Output) { throw "目标机验收证据已存在。" }

$manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
$platforms = @($manifest.platforms | Where-Object { $_.os -eq "windows" -and $_.arch -eq "x86_64" })
if ($platforms.Count -ne 1) { throw "资源 manifest 必须且只能包含 Windows x64 平台。" }
$platform = $platforms[0]
$cpuBaselineDeclared = $platform.accelerator -eq "cpu" -and @($platform.minimumCpuFeatures) -contains "sse4.2"
$runtimeDeclared = -not [string]::IsNullOrWhiteSpace([string]$platform.runtime) -and
    -not [string]::IsNullOrWhiteSpace([string]$platform.runtimeFile)
if (-not $cpuBaselineDeclared -or -not $runtimeDeclared) {
    throw "Windows 资源未声明 CPU SSE4.2 基线或 VC++ x64 运行库。"
}
$runtimePath = Join-Path $resourceRootAbsolute ([string]$platform.runtimeFile)
$sidecarPath = Join-Path $resourceRootAbsolute ([string]$platform.sidecar)
Assert-RegularFile -Path $runtimePath -Label "VC++ x64 运行库安装器"
Assert-RegularFile -Path $sidecarPath -Label "Windows CPU whisper.cpp"

$sourceSizeBefore = [int64](Get-Item -LiteralPath $fixtureAbsolute).Length
$sourceShaBefore = Get-Sha256 $fixtureAbsolute
$manifestSha256 = Get-Sha256 $manifestPath
$sidecarSha256 = Get-Sha256 $sidecarPath
$runtimeSha256 = Get-Sha256 $runtimePath
$environment = @{ ASR_RESOURCE_ROOT = $resourceRootAbsolute }

$build = Invoke-CapturedProcess -FileName $Cargo -WorkingDirectory $repo -Arguments @(
    "build", "--offline", "--bin", "dy-screen"
)
$adapter = Invoke-CapturedProcess -FileName $Cargo -WorkingDirectory $repo -Arguments @(
    "test", "--offline", "--test", "asr_whisper_windows_adapter", "--", "--nocapture"
)
$scriptParser = Invoke-CapturedProcess -FileName $Cargo -WorkingDirectory $repo -Arguments @(
    "test", "--offline", "--test", "windows_powershell_scripts", "--", "--nocapture"
)
$integration = Invoke-CapturedProcess -FileName $Cargo -WorkingDirectory $repo -Environment $environment -Arguments @(
    "test", "--offline", "--test", "asr_whisper_integration", "--", "--ignored", "--nocapture"
)
$cliStages = Invoke-CapturedProcess -FileName $Cargo -WorkingDirectory $repo -Environment $environment -Arguments @(
    "test", "--offline", "--test", "asr_cli_stages", "--", "--ignored", "--nocapture"
)

$temporaryRoot = Join-Path ([IO.Path]::GetTempPath()) ("dy-screen-asr-target-" + [Guid]::NewGuid().ToString("N"))
$unicodeDirectory = Join-Path $temporaryRoot "Windows 中文路径"
[IO.Directory]::CreateDirectory($unicodeDirectory) | Out-Null
$unicodeVideo = Join-Path $unicodeDirectory "测试 视频.mp4"
Copy-Item -LiteralPath $fixtureAbsolute -Destination $unicodeVideo
$cliBinary = Join-Path $repo "target\debug\dy-screen.exe"
Assert-RegularFile -Path $cliBinary -Label "Windows ASR CLI"
$cli = Invoke-CapturedProcess -FileName $cliBinary -WorkingDirectory $repo -Arguments @(
    "asr", $unicodeVideo, "--resource-root", $resourceRootAbsolute, "--json"
)

$cliJsonValid = $false
$structuredOutputPassed = $false
try {
    $cliJson = $cli.Stdout | ConvertFrom-Json -ErrorAction Stop
    $text = [string]$cliJson.text
    $structuredOutputPassed = [int64]$cliJson.durationMs -gt 0 -and
        @($cliJson.segments).Count -gt 0 -and
        $text.Contains("直播间") -and ($text.Contains("99") -or $text.Contains("九十九"))
    $cliJsonValid = $true
}
catch {
    $cliJsonValid = $false
}

$sourceSizeAfter = $null
$sourceShaAfter = $null
$sourceUnchanged = $false
try {
    Assert-RegularFile -Path $fixtureAbsolute -Label "固定中文视频"
    $sourceSizeAfter = [int64](Get-Item -LiteralPath $fixtureAbsolute).Length
    $sourceShaAfter = Get-Sha256 $fixtureAbsolute
    $sourceUnchanged = $sourceSizeAfter -eq $sourceSizeBefore -and $sourceShaAfter -eq $sourceShaBefore
}
catch {
    $sourceUnchanged = $false
}

$allPassed = $build.ExitCode -eq 0 -and
    $adapter.ExitCode -eq 0 -and
    $scriptParser.ExitCode -eq 0 -and
    $integration.ExitCode -eq 0 -and
    $cliStages.ExitCode -eq 0 -and
    $cli.ExitCode -eq 0 -and
    $cliJsonValid -and
    $structuredOutputPassed -and
    $sourceUnchanged
$report = [ordered]@{
    schemaVersion = 1
    collectedAtUtc = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
    platform = "windows"
    architecture = "x64"
    osVersion = [Environment]::OSVersion.VersionString
    manifestSha256 = $manifestSha256
    sidecarSha256 = $sidecarSha256
    runtimeSha256 = $runtimeSha256
    cpuBaselineDeclared = $cpuBaselineDeclared
    runtimeDeclared = $runtimeDeclared
    buildExitCode = $build.ExitCode
    buildWallMs = $build.WallMs
    buildStdoutSha256 = Get-TextSha256 $build.Stdout
    buildStderrSha256 = Get-TextSha256 $build.Stderr
    adapterTestsExitCode = $adapter.ExitCode
    adapterTestsWallMs = $adapter.WallMs
    adapterTestsOutputSha256 = Get-TextSha256 ($adapter.Stdout + $adapter.Stderr)
    powershellParserTestsExitCode = $scriptParser.ExitCode
    powershellParserTestsWallMs = $scriptParser.WallMs
    powershellParserTestsOutputSha256 = Get-TextSha256 ($scriptParser.Stdout + $scriptParser.Stderr)
    integrationTestsExitCode = $integration.ExitCode
    integrationTestsWallMs = $integration.WallMs
    integrationTestsOutputSha256 = Get-TextSha256 ($integration.Stdout + $integration.Stderr)
    runningCancellationPassed = $integration.ExitCode -eq 0
    cliStageTestsExitCode = $cliStages.ExitCode
    cliStageTestsWallMs = $cliStages.WallMs
    cliStageTestsOutputSha256 = Get-TextSha256 ($cliStages.Stdout + $cliStages.Stderr)
    unicodeAndSpacePathPassed = $cli.ExitCode -eq 0 -and $cliJsonValid
    structuredOutputPassed = $structuredOutputPassed
    cliExitCode = $cli.ExitCode
    cliWallMs = $cli.WallMs
    cliStdoutSha256 = Get-TextSha256 $cli.Stdout
    cliStderrSha256 = Get-TextSha256 $cli.Stderr
    sourceSizeBytesBefore = $sourceSizeBefore
    sourceSizeBytesAfter = $sourceSizeAfter
    sourceSha256Before = $sourceShaBefore
    sourceSha256After = $sourceShaAfter
    sourceUnchanged = $sourceUnchanged
    allPassed = $allPassed
}
Write-AtomicJson -Path $Output -Value $report
if (Test-Path -LiteralPath $temporaryRoot) {
    Remove-Item -LiteralPath $temporaryRoot -Recurse -Force
}
Write-Host "Windows x64 ASR 目标机验收完成：allPassed=$allPassed"
if (-not $allPassed) { exit 1 }
