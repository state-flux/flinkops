# 把 NSIS（makensis）装到仓库内的 .tools/ 下，供 scripts/dist.py --formats setup 使用。
# 不需要管理员：用 winget 下载官方安装包，再以 /S 静默解到指定目录。
#
#   powershell -ExecutionPolicy Bypass -File scripts\fetch-nsis.ps1
#   $env:MAKENSIS = "$PWD\.tools\nsis\makensis.exe"
#   python scripts/dist.py --formats zip,setup
param(
  [string]$Dest = (Join-Path (Split-Path -Parent $PSScriptRoot) ".tools\nsis"),
  [string]$Cache = (Join-Path (Split-Path -Parent $PSScriptRoot) ".tools\winget")
)

$ErrorActionPreference = "Stop"

$existing = Join-Path $Dest "makensis.exe"
if (Test-Path $existing) {
  Write-Host "makensis 已存在：$existing"
  exit 0
}

if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
  throw "winget 不可用：请改用 choco install nsis -y，或从 https://nsis.sourceforge.io 手动安装后设置 MAKENSIS"
}

New-Item -ItemType Directory -Force -Path $Cache, $Dest | Out-Null
Write-Host "==> winget download NSIS.NSIS"
winget download --id NSIS.NSIS -e --accept-package-agreements --accept-source-agreements --download-directory $Cache | Out-Host

$setup = Get-ChildItem $Cache -Filter "*NSIS*.exe" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $setup) { throw "没找到 NSIS 安装包（$Cache）" }

Write-Host "==> 静默解包：$($setup.Name) -> $Dest"
Start-Process -FilePath $setup.FullName -ArgumentList "/S", "/D=$Dest" -Wait
Start-Sleep -Seconds 2

if (-not (Test-Path $existing)) { throw "makensis.exe 未生成，请手动安装 NSIS" }

Write-Host ""
Write-Host "makensis: $existing"
Write-Host "用法："
Write-Host "  `$env:MAKENSIS = `"$existing`""
Write-Host "  python scripts/dist.py --target x86_64-pc-windows-msvc --formats zip,setup"
