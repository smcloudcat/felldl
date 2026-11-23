# Felldl Android APK 一键构建脚本
#
# 用法：
#   .\scripts\build-android.ps1                # debug APK（含调试符号，体积大，便于排查）
#   .\scripts\build-android.ps1 -Release       # release APK（已签名，体积小，适合分发）
#   .\scripts\build-android.ps1 -Abi armv7     # 构建 armv7（32 位老机型）
#
# 前置：先运行 .\scripts\setup-android.ps1
#
# 实现说明（重要）：
#   Tauri 官方 `cargo tauri android build` 在 Windows 上会用符号链接把 .so 放进 jniLibs，
#   而创建符号链接需要「开发者模式」。本脚本改用 cargo-ndk 的 -o 直接输出 .so，
#   再用 Gradle 打包，并用 `-x rustBuild*` 跳过 Gradle 内的 Rust 重编译任务，
#   因此在未开启开发者模式的 Windows 上也能构建。

param(
    [switch]$Release,
    [ValidateSet("arm64", "armv7")]
    [string]$Abi = "arm64"
)

$ErrorActionPreference = "Stop"
Write-Host "=== Felldl Android 构建 ===" -ForegroundColor Cyan

$projectRoot = Split-Path -Parent $PSScriptRoot
Set-Location $projectRoot

# ── 1. 环境定位 ──────────────────────────────────────────
$sdk = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { Join-Path $env:LOCALAPPDATA "Android\Sdk" }
if (-not (Test-Path $sdk)) { Write-Host "[!] 未找到 Android SDK: $sdk" -ForegroundColor Red; exit 1 }
$env:ANDROID_HOME = $sdk
$env:ANDROID_SDK_ROOT = $sdk

$ndkPath = $env:NDK_HOME
if (-not $ndkPath -or -not (Test-Path $ndkPath)) {
    $cand = Get-ChildItem (Join-Path $sdk "ndk") -Directory -ErrorAction SilentlyContinue |
        Sort-Object Name | Select-Object -Last 1
    if ($cand) { $ndkPath = $cand.FullName }
}
if (-not $ndkPath -or -not (Test-Path $ndkPath)) {
    Write-Host "[!] 未找到 NDK，请先运行 setup-android.ps1" -ForegroundColor Red; exit 1
}
$env:NDK_HOME = $ndkPath
Write-Host "[OK] NDK: $(Split-Path $ndkPath -Leaf)" -ForegroundColor Green

if (-not ($env:JAVA_HOME -and (Test-Path (Join-Path $env:JAVA_HOME "bin\java.exe")))) {
    Write-Host "[!] JAVA_HOME 未设置或无效（Gradle 需要 JDK 17+）" -ForegroundColor Red; exit 1
}
Write-Host "[OK] JDK: $env:JAVA_HOME" -ForegroundColor Green

# cargo 必须在 PATH（cargo-ndk 与 Gradle 都要用）
$cargoBin = Join-Path $env:USERPROFILE ".cargo\bin"
if ((Test-Path $cargoBin) -and ($env:Path -notlike "*$cargoBin*")) {
    $env:Path = "$cargoBin;$env:Path"
}

foreach ($tool in @("cargo", "cargo-ndk")) {
    try { & $tool --version | Out-Null } catch {
        Write-Host "[!] 缺少 $tool" -ForegroundColor Red
        if ($tool -eq "cargo-ndk") { Write-Host "    请运行： cargo install cargo-ndk" -ForegroundColor Yellow }
        exit 1
    }
}

# ── 2. ABI 映射 ──────────────────────────────────────────
$abiMap = @{
    "arm64" = @{ RustTarget = "arm64-v8a"; GradleFlavor = "Arm64"; RustArch = "aarch64" }
    "armv7" = @{ RustTarget = "armeabi-v7a"; GradleFlavor = "Arm"; RustArch = "armv7" }
}
$cfg = $abiMap[$Abi]
Write-Host "[OK] 目标 ABI: $($cfg.RustTarget)" -ForegroundColor Green

# ── 3. 首次初始化 Android 工程 ───────────────────────────
if (-not (Test-Path "src-tauri\gen\android")) {
    Write-Host "[..] 首次初始化 Android 工程（需要 NDK_HOME 已设置）..." -ForegroundColor Yellow
    cargo tauri android init --ci
}

# ── 4. 前端构建 ──────────────────────────────────────────
Write-Host "[..] 构建前端 ..." -ForegroundColor Yellow
Push-Location ui
try {
    if (-not (Test-Path "node_modules")) { npm install }
    npm run build
    if ($LASTEXITCODE -ne 0) { throw "前端构建失败" }
} finally { Pop-Location }

# ── 5. 编译 Rust .so 并输出到 jniLibs ────────────────────
$jniDir = "src-tauri\gen\android\app\src\main\jniLibs"
$profileArgs = if ($Release) { @("--release") } else { @() }
$profileLabel = if ($Release) { "release" } else { "debug" }
Write-Host "[..] 编译 Rust（$profileLabel, $($cfg.RustTarget)）..." -ForegroundColor Yellow
cargo ndk -t $cfg.RustTarget -o $jniDir build @profileArgs -p felldl
if ($LASTEXITCODE -ne 0) { Write-Host "[!] Rust 编译失败" -ForegroundColor Red; exit 1 }

# ── 6. release 签名准备 ──────────────────────────────────
if ($Release) {
    $ksDir = "src-tauri\gen\android\keystore"
    $ks = Join-Path $ksDir "felldl-release.jks"
    if (-not (Test-Path $ks)) {
        Write-Host "[..] 生成自签名密钥（自用分发；上架请换成正式签名）..." -ForegroundColor Yellow
        New-Item -ItemType Directory -Force -Path $ksDir | Out-Null
        & "$env:JAVA_HOME\bin\keytool.exe" -genkeypair -v -keystore $ks -alias felldl `
            -keyalg RSA -keysize 2048 -validity 10000 `
            -storepass felldl123 -keypass felldl123 `
            -dname "CN=Felldl, OU=Dev, O=Felldl, L=Guangzhou, ST=Guangdong, C=CN"
    }
    Write-Host "[OK] 签名密钥: $ks" -ForegroundColor Green
}

# ── 7. Gradle 打包（跳过 Gradle 内的 Rust 重编译）────────
$gradleTask = "assemble$($cfg.GradleFlavor)$(if ($Release) { 'Release' } else { 'Debug' })"
$skipTask = "rustBuild$($cfg.GradleFlavor)$(if ($Release) { 'Release' } else { 'Debug' })"
Write-Host "[..] Gradle 打包：$gradleTask ..." -ForegroundColor Yellow

Push-Location "src-tauri\gen\android"
try {
    .\gradlew.bat $gradleTask `
        "-PabiList=$($cfg.RustTarget)" `
        "-ParchList=$($Abi)" `
        "-PtargetList=$($cfg.RustArch)" `
        "-x" $skipTask `
        --no-daemon --no-configuration-cache
    $gradleOk = ($LASTEXITCODE -eq 0)
} finally { Pop-Location }

if (-not $gradleOk) { Write-Host "[!] Gradle 构建失败，请检查上方日志" -ForegroundColor Red; exit 1 }

# ── 8. 报告产物 ──────────────────────────────────────────
Write-Host ""
Write-Host "=== 构建完成 ===" -ForegroundColor Cyan
$apkRoot = "src-tauri\gen\android\app\build\outputs\apk"
$apks = Get-ChildItem $apkRoot -Recurse -Filter "*.apk" -ErrorAction SilentlyContinue |
    Where-Object { $_.LastWriteTime -gt (Get-Date).AddMinutes(-10) }
if ($apks) {
    foreach ($a in $apks) {
        Write-Host ("  {0}" -f $a.FullName) -ForegroundColor Green
        Write-Host ("  大小: {0} MB" -f [math]::Round($a.Length/1MB, 1)) -ForegroundColor Green
    }
    $first = $apks[0].FullName
    Write-Host ""
    Write-Host "安装到已连接设备：" -ForegroundColor DarkGray
    Write-Host "  adb install -r `"$first`"" -ForegroundColor DarkGray
} else {
    Write-Host "[!] 未找到 APK 产物" -ForegroundColor Red
}
