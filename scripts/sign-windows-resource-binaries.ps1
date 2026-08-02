# 签署或验证 Windows Runtime Resource Pack 中由项目发布的 PE 文件。
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ResourceRoot,
    [Parameter(Mandatory = $true)]
    [string]$CertificateThumbprint,
    [Parameter(Mandatory = $true)]
    [string]$TimestampUrl,
    [switch]$VerifyOnly
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

function Assert-ExpectedSignature {
    param([string]$Path, [string]$ExpectedThumbprint, [string]$Label)
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid -or
        $null -eq $signature.SignerCertificate -or
        $signature.SignerCertificate.Thumbprint -ne $ExpectedThumbprint -or
        $null -eq $signature.TimeStamperCertificate) {
        throw "$Label 缺少预期 Authenticode 签名或 RFC 3161 时间戳。"
    }
}

if (-not [Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
    [Runtime.InteropServices.OSPlatform]::Windows
) -or [Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant() -ne "x64") {
    throw "Windows 资源签名只能在原生 Windows x64 环境执行。"
}
if ($CertificateThumbprint -notmatch '^[0-9A-Fa-f]{40,64}$') { throw "证书指纹格式无效。" }
$thumbprint = $CertificateThumbprint.ToUpperInvariant()
$timestamp = [Uri]$TimestampUrl
if (-not $timestamp.IsAbsoluteUri -or $timestamp.Scheme -ne "https" -or -not [string]::IsNullOrEmpty($timestamp.UserInfo)) {
    throw "时间戳服务必须使用不含凭据的 HTTPS URL。"
}
if (-not (Get-Command "signtool.exe" -ErrorAction SilentlyContinue)) {
    throw "缺少 Windows SDK signtool.exe。"
}

$manifestPath = Join-Path $ResourceRoot "manifest.json"
Assert-RegularFile -Path $manifestPath -Label "资源 manifest"
$manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
$platforms = @($manifest.platforms | Where-Object { $_.os -eq "windows" -and $_.arch -eq "x86_64" })
if ($platforms.Count -ne 1) { throw "资源 manifest 必须且只能包含 Windows x64 平台。" }
$platform = $platforms[0]
$projectFiles = @(
    [string]$platform.sidecar,
    [string]$platform.vadSidecar,
    [string]$platform.ffmpeg,
    [string]$platform.ffprobe
) + @($platform.libraries | ForEach-Object { [string]$_ })

foreach ($relative in $projectFiles) {
    $path = Join-Path $ResourceRoot ($relative.Replace("/", "\"))
    Assert-RegularFile -Path $path -Label "项目原生资源 $relative"
    if (-not $VerifyOnly) {
        & signtool.exe sign /sha1 $thumbprint /fd SHA256 /tr $TimestampUrl /td SHA256 $path | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "项目原生资源签名失败：$relative" }
    }
    Assert-ExpectedSignature -Path $path -ExpectedThumbprint $thumbprint -Label "项目原生资源 $relative"
}

$runtimeRelative = [string]$platform.runtimeFile
$runtimePath = Join-Path $ResourceRoot ($runtimeRelative.Replace("/", "\"))
Assert-RegularFile -Path $runtimePath -Label "Microsoft VC++ 运行库"
$runtimeSignature = Get-AuthenticodeSignature -LiteralPath $runtimePath
if ($runtimeSignature.Status -ne [System.Management.Automation.SignatureStatus]::Valid -or
    $null -eq $runtimeSignature.SignerCertificate -or
    $runtimeSignature.SignerCertificate.Subject -notmatch "Microsoft Corporation") {
    throw "Microsoft VC++ 运行库签名无效。"
}

if (-not $VerifyOnly) {
    $integrity = @()
    foreach ($relative in $projectFiles + @($runtimeRelative)) {
        $path = Join-Path $ResourceRoot ($relative.Replace("/", "\"))
        $integrity += [ordered]@{
            file = $relative
            sizeBytes = [int64](Get-Item -LiteralPath $path).Length
            sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    }
    $platform.resourceIntegrity = $integrity
    $manifest.platforms = @($platform)
    $json = $manifest | ConvertTo-Json -Depth 12
    $temporary = "$manifestPath.part"
    if (Test-Path -LiteralPath $temporary) { throw "存在未清理的 manifest 签名临时文件。" }
    [IO.File]::WriteAllText($temporary, $json + [Environment]::NewLine, [Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $temporary -Destination $manifestPath -Force

    $hashLines = Get-ChildItem -LiteralPath $ResourceRoot -Recurse -File -Force |
        Where-Object { $_.Name -ne "SHA256SUMS" } |
        Sort-Object FullName |
        ForEach-Object {
            $relative = $_.FullName.Substring($ResourceRoot.Length).TrimStart([char[]]"\/").Replace("\", "/")
            "$((Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant())  $relative"
        }
    [IO.File]::WriteAllText(
        (Join-Path $ResourceRoot "SHA256SUMS"),
        (($hashLines -join "`n") + "`n"),
        [Text.UTF8Encoding]::new($false)
    )
}

Write-Host "Windows 项目原生资源 Authenticode 校验通过。"
