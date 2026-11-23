# Felldl Android 构建环境一键准备脚本
# 用法：在 PowerShell 中运行  .\scripts\setup-android.ps1
# 幂等：已安装的组件会自动跳过。

$ErrorActionPreference = "Stop"
Write-Host "=== Felldl Android 环境准备 ===" -ForegroundColor Cyan

$sdk = Join-Path $env:LOCALAPPDATA "Android\Sdk"
if (-not (Test-Path $sdk)) {
    Write-Host "[!] 未找到 Android SDK：$sdk" -ForegroundColor Yellow
    Write-Host "    请先安装 Android Studio，或设置 ANDROID_HOME 环境变量。" -ForegroundColor Yellow
    exit 1
}
Write-Host "[OK] Android SDK: $sdk" -ForegroundColor Green

# 1) 安装 SDK 组件（platform 35 / build-tools / NDK / CMake / platform-tools）
$cmdlineTools = Get-ChildItem (Join-Path $sdk "cmdline-tools") -Directory -ErrorAction SilentlyContinue |
    Sort-Object Name | Select-Object -Last 1
$sdkmanager = $null
if ($cmdlineTools) {
    $cand = Join-Path $cmdlineTools.FullName "bin\sdkmanager.bat"
    if (Test-Path $cand) { $sdkmanager = $cand }
}

function Install-SdkComponent([string]$id, [string]$label) {
    $dest = switch -Wildcard ($id) {
        "platforms;*" { Join-Path $sdk "platforms\$($id.Split(';')[1])" }
        "build-tools;*" { Join-Path $sdk "build-tools\$($id.Split(';')[1])" }
        "ndk;*" { Join-Path $sdk "ndk\$($id.Split(';')[1])" }
        "cmake;*" { Join-Path $sdk "cmake\$($id.Split(';')[1])" }
        default { $null }
    }
    if ($dest -and (Test-Path $dest)) {
        Write-Host "[OK] $label 已安装" -ForegroundColor Green
        return
    }
    if (-not $script:sdkmanager) {
        Write-Host "[!] 找不到 sdkmanager，无法自动安装 $label" -ForegroundColor Yellow
        Write-Host "    请打开 Android Studio → SDK Manager 手动安装：$id" -ForegroundColor Yellow
        return
    }
    Write-Host "[..] 安装 $label ($id) ..." -ForegroundColor Yellow
    Write-Host "yes" | & $script:sdkmanager --sdk_root=$sdk $id | Out-Null
    if ($LASTEXITCODE -eq 0) { Write-Host "[OK] $label 安装完成" -ForegroundColor Green }
    else { Write-Host "[!] $label 安装失败，请手动安装：$id" -ForegroundColor Red }
}

# NDK 版本由 Tauri 2 决定，这里用 26.x（兼容 Tauri 2.x）
Install-SdkComponent "platforms;android-35" "Android 35 SDK"
Install-SdkComponent "build-tools;35.0.0" "Build Tools 35"
Install-SdkComponent "ndk;26.3.11579264" "NDK 26.3"
Install-SdkComponent "cmake;3.22.1" "CMake 3.22"
Install-SdkComponent "platform-tools" "Platform Tools"

# 2) Rust 交叉编译目标
Write-Host "[..] 安装 Rust Android targets ..." -ForegroundColor Yellow
foreach ($t in @("aarch64-linux-android", "armv7-linux-androideabi", "i686-linux-android", "x86_64-linux-android")) {
    $installed = rustup target list --installed
    if ($installed -contains $t) { Write-Host "[OK] $t" -ForegroundColor Green }
    else {
        rustup target add $t
        Write-Host "[OK] $t 已安装" -ForegroundColor Green
    }
}

# 3) cargo-ndk（Tauri Android 构建需要）
Write-Host "[..] 检查 cargo-ndk ..." -ForegroundColor Yellow
$cargoNdk = (Get-Command cargo-ndk -ErrorAction SilentlyContinue)
if ($cargoNdk) { Write-Host "[OK] cargo-ndk 已安装" -ForegroundColor Green }
else {
    Write-Host "[..] 安装 cargo-ndk ..." -ForegroundColor Yellow
    cargo install cargo-ndk
}

Write-Host ""
Write-Host "=== 环境准备完成 ===" -ForegroundColor Cyan
Write-Host "接下来运行： .\scripts\build-android.ps1" -ForegroundColor Cyan
