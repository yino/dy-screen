# 在 Windows x64 目标设备上采集一次完整 ASR 的可审计性能证据。
# 报告不保存视频/资源绝对路径、命令行或模型 stderr 原文。
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Video,
    [Parameter(Mandatory = $true)]
    [string]$AsrBinary,
    [Parameter(Mandatory = $true)]
    [string]$ResourceRoot,
    [Parameter(Mandatory = $true)]
    [string]$Output,
    [switch]$Require8GB
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

# Windows PowerShell 5.1 没有 ProcessStartInfo.ArgumentList。这里实现微软命令行解析规则，
# 再从字符串数组生成 Arguments，确保中文、空格、引号和尾部反斜杠不会改变参数边界。
function ConvertTo-WindowsCommandLineArgument {
    param([string]$Value)
    if ($Value.Length -gt 0 -and $Value -notmatch '[\s"]') {
        return $Value
    }
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

function Get-ThermalState {
    try {
        $temperatures = @(Get-CimInstance -Namespace root/wmi -ClassName MSAcpi_ThermalZoneTemperature |
            ForEach-Object { ([double]$_.CurrentTemperature / 10.0) - 273.15 })
        if ($temperatures.Count -eq 0) { return "unknown" }
        $maximum = ($temperatures | Measure-Object -Maximum).Maximum
        if ($maximum -ge 90) { return "critical" }
        if ($maximum -ge 80) { return "warning" }
        return "normal"
    }
    catch {
        return "unknown"
    }
}

function Get-ProcessTreeMetrics {
    param([int]$RootProcessId, [hashtable]$Observed)
    $rows = @(Get-CimInstance Win32_Process | Select-Object ProcessId, ParentProcessId)
    $queue = [Collections.Generic.Queue[int]]::new()
    $visited = [Collections.Generic.HashSet[int]]::new()
    $queue.Enqueue($RootProcessId)
    [int64]$workingSet = 0
    [int64]$threads = 0
    [int64]$processes = 0
    while ($queue.Count -gt 0) {
        $processId = $queue.Dequeue()
        if (-not $visited.Add($processId)) { continue }
        foreach ($child in @($rows | Where-Object { [int]$_.ParentProcessId -eq $processId })) {
            $queue.Enqueue([int]$child.ProcessId)
        }
        try {
            $process = Get-Process -Id $processId -ErrorAction Stop
            $workingSet += [int64]$process.WorkingSet64
            $threads += [int64]$process.Threads.Count
            $processes++
            if (-not $Observed.ContainsKey($processId)) {
                $Observed[$processId] = $process.StartTime.ToUniversalTime().Ticks
            }
        }
        catch {
            # 短生命周期 sidecar 可能在进程表快照后退出；下一次采样继续。
        }
    }
    return [pscustomobject]@{
        WorkingSetBytes = $workingSet
        ThreadCount = $threads
        ProcessCount = $processes
    }
}

function Write-AtomicUtf8Json {
    param([string]$Path, [object]$Value)
    if (Test-Path -LiteralPath $Path) {
        throw "性能证据文件已存在；为保留不可变证据不会覆盖。"
    }
    $absolute = [IO.Path]::GetFullPath($Path)
    $parent = [IO.Path]::GetDirectoryName($absolute)
    [IO.Directory]::CreateDirectory($parent) | Out-Null
    $temporary = [IO.Path]::Combine($parent, "." + [IO.Path]::GetFileName($absolute) + "." + $PID + ".part")
    $json = $Value | ConvertTo-Json -Depth 8
    try {
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
    throw "该脚本只接受 Windows x64 目标设备。"
}
$architecture = [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
if ($architecture -ne "x64") {
    throw "该脚本只接受 Windows x64 目标设备。"
}

Assert-RegularFile -Path $Video -Label "视频"
Assert-RegularFile -Path $AsrBinary -Label "ASR 可执行文件"
$manifest = Join-Path $ResourceRoot "manifest.json"
Assert-RegularFile -Path $manifest -Label "资源 manifest"
if (Test-Path -LiteralPath $Output) {
    throw "性能证据文件已存在；为保留不可变证据不会覆盖。"
}

$physicalMemoryBytes = [int64](Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory
$eightGiBMinimum = [int64](7GB)
$eightGiBMaximum = [int64](9GB)
$strictGatePassed = $physicalMemoryBytes -ge $eightGiBMinimum -and $physicalMemoryBytes -le $eightGiBMaximum
if ($Require8GB -and -not $strictGatePassed) {
    throw "严格 8 GB 门禁失败，当前物理内存为 $physicalMemoryBytes 字节。"
}

$inputItemBefore = Get-Item -LiteralPath $Video
$inputSizeBefore = [int64]$inputItemBefore.Length
$inputShaBefore = Get-Sha256 $Video
$asrBinarySha256 = Get-Sha256 $AsrBinary
$resourceManifestSha256 = Get-Sha256 $manifest
$thermalBefore = Get-ThermalState

$arguments = [Collections.Generic.List[string]]::new()
foreach ($argument in @("asr", $Video, "--resource-root", $ResourceRoot, "--json")) {
    $arguments.Add($argument)
}
$startInfo = [Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = [IO.Path]::GetFullPath($AsrBinary)
$startInfo.Arguments = (($arguments | ForEach-Object { ConvertTo-WindowsCommandLineArgument $_ }) -join " ")
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $true
$startInfo.RedirectStandardOutput = $true
$startInfo.RedirectStandardError = $true
$startInfo.StandardOutputEncoding = [Text.Encoding]::UTF8
$startInfo.StandardErrorEncoding = [Text.Encoding]::UTF8

$process = [Diagnostics.Process]::new()
$process.StartInfo = $startInfo
$stopwatch = [Diagnostics.Stopwatch]::StartNew()
if (-not $process.Start()) {
    throw "无法启动 ASR 进程。"
}
$stdoutTask = $process.StandardOutput.ReadToEndAsync()
$stderrTask = $process.StandardError.ReadToEndAsync()
$observed = @{}
[int64]$peakWorkingSetBytes = 0
[int64]$maximumThreadCount = 0
[int64]$maximumProcessCount = 0
while (-not $process.HasExited) {
    $metrics = Get-ProcessTreeMetrics -RootProcessId $process.Id -Observed $observed
    $peakWorkingSetBytes = [Math]::Max($peakWorkingSetBytes, [int64]$metrics.WorkingSetBytes)
    $maximumThreadCount = [Math]::Max($maximumThreadCount, [int64]$metrics.ThreadCount)
    $maximumProcessCount = [Math]::Max($maximumProcessCount, [int64]$metrics.ProcessCount)
    Start-Sleep -Milliseconds 100
}
$process.WaitForExit()
$stopwatch.Stop()
$exitCode = $process.ExitCode
$stdoutText = $stdoutTask.GetAwaiter().GetResult()
$stderrText = $stderrTask.GetAwaiter().GetResult()
$wallMs = [int64]$stopwatch.ElapsedMilliseconds

$stdoutValidJson = $false
$audioDurationMs = $null
try {
    $asrResult = $stdoutText | ConvertFrom-Json -ErrorAction Stop
    $audioDurationMs = [int64]$asrResult.durationMs
    $stdoutValidJson = $true
}
catch {
    # stderr 原文不会写入报告；解析失败通过布尔值和哈希表达。
}
$realtimeFactor = $null
if ($null -ne $audioDurationMs -and $audioDurationMs -gt 0) {
    $realtimeFactor = [Math]::Round([double]$wallMs / [double]$audioDurationMs, 6)
}

$inputSizeAfter = $null
$inputShaAfter = $null
$sourceUnchanged = $false
try {
    Assert-RegularFile -Path $Video -Label "视频"
    $inputSizeAfter = [int64](Get-Item -LiteralPath $Video).Length
    $inputShaAfter = Get-Sha256 $Video
    $sourceUnchanged = $inputSizeAfter -eq $inputSizeBefore -and $inputShaAfter -eq $inputShaBefore
}
catch {
    $sourceUnchanged = $false
}

Start-Sleep -Seconds 2
$lingeringChildProcessCount = 0
foreach ($entry in $observed.GetEnumerator()) {
    if ([int]$entry.Key -eq $process.Id) { continue }
    try {
        $candidate = Get-Process -Id ([int]$entry.Key) -ErrorAction Stop
        if ($candidate.StartTime.ToUniversalTime().Ticks -eq [int64]$entry.Value) {
            $lingeringChildProcessCount++
        }
    }
    catch {
        # 进程已退出即视为资源已释放。
    }
}
$resourcesReleased = $lingeringChildProcessCount -eq 0
$thermalAfter = Get-ThermalState
$stdoutBytes = [Text.Encoding]::UTF8.GetBytes($stdoutText)
$stderrBytes = [Text.Encoding]::UTF8.GetBytes($stderrText)

$report = [ordered]@{
    schemaVersion = 1
    collectedAtUtc = [DateTime]::UtcNow.ToString("yyyy-MM-ddTHH:mm:ssZ")
    platform = "windows"
    architecture = "x64"
    physicalMemoryBytes = $physicalMemoryBytes
    strict8GbDeviceRequired = [bool]$Require8GB
    strict8GbDeviceGatePassed = $strictGatePassed
    inputSizeBytesBefore = $inputSizeBefore
    inputSizeBytesAfter = $inputSizeAfter
    inputSha256Before = $inputShaBefore
    inputSha256After = $inputShaAfter
    sourceUnchanged = $sourceUnchanged
    asrBinarySha256 = $asrBinarySha256
    resourceManifestSha256 = $resourceManifestSha256
    exitCode = $exitCode
    wallMs = $wallMs
    audioDurationMs = $audioDurationMs
    realtimeFactor = $realtimeFactor
    peakWorkingSetBytes = $peakWorkingSetBytes
    maximumThreadCount = $maximumThreadCount
    maximumProcessCount = $maximumProcessCount
    thermalStateBefore = $thermalBefore
    thermalStateAfter = $thermalAfter
    lingeringChildProcessCount = $lingeringChildProcessCount
    resourcesReleased = $resourcesReleased
    stdoutValidJson = $stdoutValidJson
    stdoutSizeBytes = [int64]$stdoutBytes.Length
    stdoutSha256 = Get-TextSha256 $stdoutText
    stderrSizeBytes = [int64]$stderrBytes.Length
    stderrSha256 = Get-TextSha256 $stderrText
}
Write-AtomicUtf8Json -Path $Output -Value $report
Write-Host "Windows ASR 性能证据已生成：exit=$exitCode，RTF=$realtimeFactor，峰值内存=$peakWorkingSetBytes 字节，遗留子进程=$lingeringChildProcessCount"

$process.Dispose()
if ($exitCode -ne 0 -or -not $stdoutValidJson -or -not $sourceUnchanged -or -not $resourcesReleased) {
    exit 1
}
