# Felldl Android APK 一键构建脚本
# 用法： .\scripts\build-android.ps1 [-Release]
# 前置：先运行 .\scripts\setup-android.ps1

param([switch]$Release)

$ErrorActionPreference = "Stop"
Write-Host "=== Felldl Android 构建 ===" -ForegroundColor Cyan

$sdk = Join-Path $env:LOCALAPPDATA "Android\Sdk"
$env:ANDROID_HOME = $sdk
$env:ANDROID_SDK_ROOT = $sdk
$ndk = Get-ChildItem (Join-Path $sdk "ndk") -Directory -ErrorAction SilentlyContinue | Sort-Object Name | Select-Object -Last 1
if (-not $ndk) {
    Write-Host "[!] 未找到 NDK，请先运行 setup-android.ps1" -ForegroundColor Red
    exit 1
}
$env:NDK_HOME = $ndk.FullName
Write-Host "[OK] NDK: $($ndk.Name)" -ForegroundColor Green

# 确认 Tauri CLI
$tauriOk = $false
try { cargo tauri --version | Out-Null; $tauriOk = $true } catch {}
if (-not $tauriOk) {
    Write-Host "[!] 未安装 Tauri CLI，请先运行： cargo install tauri-cli --version '^2' --locked" -ForegroundColor Red
    exit 1
}

# 首次初始化 Android 工程（生成 src-tauri/gen/android）
if (-not (Test-Path "src-tauri\gen\android")) {
    Write-Host "[..] 首次初始化 Android 工程 ..." -ForegroundColor Yellow
    cargo tauri android init
}

# 构建 APK
Write-Host "[..] 编译 Android APK ..." -ForegroundColor Yellow
$profile = if ($Release) { @("--release") } else { @() }
cargo tauri android build @profile --apk --target aarch64

Write-Host ""
Write-Host "=== 构建完成 ===" -ForegroundColor Cyan
$apk = Get-ChildItem "src-tauri\gen\android\app\build\outputs\apk" -Recurse -Filter "*.apk" -ErrorAction SilentlyContinue | Select-Object -First 1
if ($apk) {
    Write-Host "APK: $($apk.FullName)" -ForegroundColor Green
    Write-Host "大小: $([math]::Round($apk.Length/1MB, 1)) MB" -ForegroundColor Green
}
