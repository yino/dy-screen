# 下载并校验 Node 22 LTS 运行时（Windows x64）。
# 用法：powershell -File scripts/build-harness-runtime.ps1
# 产物：resources/harness-source/runtime/win-x64/（含 node.exe、npm 等）
$ErrorActionPreference = "Stop"

$Platform = "win-x64"
$Root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$Lock = Join-Path $Root "resources/harness-source/runtime/node-lock.json"
$Dest = Join-Path $Root "resources/harness-source/runtime/$Platform"

$lock = Get-Content $Lock -Raw | ConvertFrom-Json
$platform = $lock.platforms.$Platform
$Version = $lock.version
$Url = $lock.source + "v" + $Version + "/" + $platform.archive
$Sha256 = $platform.sha256

Write-Host "锁定 Node $Version（$Platform）"
New-Item -ItemType Directory -Force -Path $Dest | Out-Null
$Archive = Join-Path $Dest $platform.archive

if (-not (Test-Path $Archive)) {
    Write-Host "下载 $Url"
    Invoke-WebRequest -Uri $Url -OutFile $Archive
}

Write-Host "校验 SHA256"
$Actual = (Get-FileHash -Algorithm SHA256 $Archive).Hash.ToLower()
if ($Actual -ne $Sha256) {
    Write-Error "哈希不匹配：期望 $Sha256，实际 $Actual"
    exit 1
}

Write-Host "解压"
Expand-Archive -Path $Archive -DestinationPath $Dest -Force

$NodeExe = Join-Path $Dest "node-v$Version-$Platform/node.exe"
if (-not (Test-Path $NodeExe)) { Write-Error "未找到 node.exe"; exit 1 }
Write-Host "验证：$(& $NodeExe --version)"
Write-Host "完成：$Dest"
