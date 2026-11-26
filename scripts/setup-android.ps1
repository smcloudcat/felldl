# Felldl Android 构建环境一键准备脚本
# 用法：在 PowerShell 中运行  .\scripts\setup-android.ps1
# 如需其他版本： .\scripts\setup-android.ps1 -NdkVersion 30.0.16248370 -Platform android-36
# 幂等：已安装的组件会自动跳过；旧版本组件不会删除，可多个版本并存。

param(
    # 与 app/build.gradle.kts 的 compileSdk 对应
    [string]$Platform = "android-36",
    [string]$BuildTools = "37.0.0",
    [string]$NdkVersion = "30.0.16248370",
    [string]$CMakeVersion = "4.1.2"
)

$ErrorActionPreference = "Stop"
Write-Host "=== Felldl Android 环境准备 ===" -ForegroundColor Cyan

# ── 0. SDK 与 JDK 定位 ───────────────────────────────────
$sdk = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { Join-Path $env:LOCALAPPDATA "Android\Sdk" }
if (-not (Test-Path $sdk)) {
    Write-Host "[!] 未找到 Android SDK：$sdk" -ForegroundColor Yellow
    Write-Host "    请先安装 Android Studio，或设置 ANDROID_HOME 环境变量。" -ForegroundColor Yellow
    exit 1
}
Write-Host "[OK] Android SDK: $sdk" -ForegroundColor Green

$javaExe = Join-Path $env:JAVA_HOME "bin\java.exe"
if (-not $env:JAVA_HOME -or -not (Test-Path $javaExe)) {
    Write-Host "[!] JAVA_HOME 未设置或无效：$env:JAVA_HOME" -ForegroundColor Yellow
    Write-Host "    Android 构建需要 JDK 17+，请设置 JAVA_HOME 指向 JDK 17/21/25 目录。" -ForegroundColor Yellow
} else {
    # JDK 把版本信息写到 stderr；PS 5.1 在 Stop 模式下会把原生命令 stderr 当成异常
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try { $verLine = (& $javaExe -version 2>&1 | Select-Object -First 1) -join "" }
    finally { $ErrorActionPreference = $prevEap }
    Write-Host "[OK] JDK: $verLine" -ForegroundColor Green
    if ($verLine -match '"(1\.)?(\d+)') {
        $major = [int]$Matches[2]
        if ($major -lt 17) {
            Write-Host "[!] 当前 JDK 版本过低（$major），Gradle 需要 17+，构建可能失败。" -ForegroundColor Red
        }
    }
}

# ── 1. 选择命令行工具入口 ────────────────────────────────
# 目录可能是 12.0 / 23.0 这类版本名，也可能是 latest / latest-2 别名。
# 23.0 起 sdkmanager.bat 变成 Android CLI 转接器，对 "ndk;版本" 这类旧式包名不支持，
# 因此安装优先用 23.0 之前的旧管理器；没有旧管理器时才用 android sdk install。
function Get-CmdlineToolsCandidates([string]$sdkRoot) {
    $root = Join-Path $sdkRoot "cmdline-tools"
    if (-not (Test-Path $root)) { return @() }
    $items = foreach ($dir in (Get-ChildItem $root -Directory -ErrorAction SilentlyContinue)) {
        # 真实版本在 source.properties 的 Pkg.Revision；目录名可能只是 latest / latest-2 别名
        $ver = $null
        $props = Join-Path $dir.FullName "source.properties"
        if (Test-Path -LiteralPath $props) {
            $line = Get-Content -LiteralPath $props -ErrorAction SilentlyContinue |
                Where-Object { $_ -match '^Pkg\.Revision\s*=' } | Select-Object -First 1
            if ($line) {
                $raw = ($line -replace '^Pkg\.Revision\s*=', '').Trim()
                if ($raw -match '^\d+(\.\d+)+') { $ver = [version]$Matches[0] }
            }
        }
        if (-not $ver -and $dir.Name -match '^\d+(\.\d+)+$') { $ver = [version]$dir.Name }
        [PSCustomObject]@{
            Name       = $dir.Name
            Version    = if ($ver) { $ver } else { [version]"0.0" }
            SdkManager = Join-Path $dir.FullName "bin\sdkmanager.bat"
            AndroidCli = Join-Path $dir.FullName "bin\android.exe"
        }
    }
    return @($items | Sort-Object Version, Name -Descending)
}

$tools = @(Get-CmdlineToolsCandidates $sdk)
$legacyManager = $tools | Where-Object {
    $_.Version -lt [version]"23.0" -and (Test-Path -LiteralPath $_.SdkManager)
} | Select-Object -First 1
$modernCli = $tools | Where-Object {
    $_.Version -ge [version]"23.0" -and (Test-Path -LiteralPath $_.AndroidCli)
} | Select-Object -First 1
$anyManager = $tools | Where-Object { Test-Path -LiteralPath $_.SdkManager } | Select-Object -First 1

if ($legacyManager) {
    $script:installMode = "sdkmanager"
    $script:sdkmanager = $legacyManager.SdkManager
    Write-Host "[OK] 安装入口：sdkmanager $($legacyManager.Name)（$($legacyManager.Version)）" -ForegroundColor Green
} elseif ($modernCli) {
    $script:installMode = "android-cli"
    $script:androidCli = $modernCli.AndroidCli
    Write-Host "[OK] 安装入口：android sdk install $($modernCli.Name)（$($modernCli.Version)）" -ForegroundColor Green
} elseif ($anyManager) {
    $script:installMode = "sdkmanager"
    $script:sdkmanager = $anyManager.SdkManager
    Write-Host "[OK] 安装入口：sdkmanager $($anyManager.Name)（$($anyManager.Version)）" -ForegroundColor Green
} else {
    $script:installMode = "none"
    Write-Host "[!] 未找到 cmdline-tools（sdkmanager / android）" -ForegroundColor Yellow
    Write-Host "    请下载安装： https://developer.android.com/studio#command-line-tools-only" -ForegroundColor Yellow
    Write-Host "    解压到 $sdk\cmdline-tools\<版本号>，再重跑本脚本。" -ForegroundColor Yellow
}

# 旧式包 ID（platforms;android-36）→ 新 CLI 的 <package>@<version>
function ConvertTo-CliPackageId([string]$id) {
    if ($id -eq "platform-tools") { return "platform-tools" }
    $parts = $id.Split(";")
    if ($parts.Count -eq 2) { return "$($parts[0])@$($parts[1])" }
    return $id
}

function Install-SdkComponent([string]$id, [string]$label) {
    $dest = switch -Wildcard ($id) {
        "platforms;*" { Join-Path $sdk "platforms\$($id.Split(';')[1])" }
        "build-tools;*" { Join-Path $sdk "build-tools\$($id.Split(';')[1])" }
        "ndk;*" { Join-Path $sdk "ndk\$($id.Split(';')[1])" }
        "cmake;*" { Join-Path $sdk "cmake\$($id.Split(';')[1])" }
        "platform-tools" { Join-Path $sdk "platform-tools" }
        default { $null }
    }
    if ($dest -and (Test-Path $dest)) {
        Write-Host "[OK] $label 已安装" -ForegroundColor Green
        return
    }

    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = "Continue"
    try {
        if ($script:installMode -eq "sdkmanager") {
            Write-Host "[..] 安装 $label ($id) ..." -ForegroundColor Yellow
            Write-Host "yes" | & $script:sdkmanager --sdk_root=$sdk $id | Out-Null
            $ok = ($LASTEXITCODE -eq 0)
        } elseif ($script:installMode -eq "android-cli") {
            $cliId = ConvertTo-CliPackageId $id
            Write-Host "[..] 安装 $label ($cliId) ..." -ForegroundColor Yellow
            & $script:androidCli --no-metrics --sdk=$sdk sdk install $cliId | Out-Null
            $ok = ($LASTEXITCODE -eq 0)
        } else {
            $ok = $false
        }
    } finally { $ErrorActionPreference = $prevEap }

    if ($ok) { Write-Host "[OK] $label 安装完成" -ForegroundColor Green }
    else { Write-Host "[!] $label 安装失败，请手动安装：$id" -ForegroundColor Red }
}

Install-SdkComponent "platforms;$Platform" "Android SDK Platform $Platform"
Install-SdkComponent "build-tools;$BuildTools" "Build Tools $BuildTools"
Install-SdkComponent "ndk;$NdkVersion" "NDK $NdkVersion"
Install-SdkComponent "cmake;$CMakeVersion" "CMake $CMakeVersion"
Install-SdkComponent "platform-tools" "Platform Tools"

# NDK 选择：优先显式指定的版本，其次按版本号取最高（不按目录名字符串排序）
$ndkPath = Join-Path $sdk "ndk\$NdkVersion"
if (-not (Test-Path $ndkPath)) {
    $ndkDirs = @(Get-ChildItem (Join-Path $sdk "ndk") -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^\d+(\.\d+)+$' } |
        Sort-Object { [version]$_.Name } -Descending)
    if ($ndkDirs.Count -gt 0) { $ndkPath = $ndkDirs[0].FullName }
}
if (Test-Path $ndkPath) {
    $env:NDK_HOME = $ndkPath
    Write-Host "[OK] NDK_HOME=$(Split-Path $ndkPath -Leaf)（仅当前会话生效）" -ForegroundColor Green
    Write-Host "    如需永久生效： setx NDK_HOME `"$ndkPath`"" -ForegroundColor DarkGray
} else {
    Write-Host "[!] 未找到可用 NDK，build-android.ps1 会失败" -ForegroundColor Red
}

# ── 2. Rust 交叉编译目标 ─────────────────────────────────
Write-Host "[..] 安装 Rust Android targets ..." -ForegroundColor Yellow
$installedTargets = @(rustup target list --installed)
foreach ($t in @("aarch64-linux-android", "armv7-linux-androideabi", "i686-linux-android", "x86_64-linux-android")) {
    if ($installedTargets -contains $t) { Write-Host "[OK] $t" -ForegroundColor Green }
    else {
        rustup target add $t
        Write-Host "[OK] $t 已安装" -ForegroundColor Green
    }
}

# ── 3. cargo-ndk（Tauri Android 构建需要）────────────────
Write-Host "[..] 检查 cargo-ndk ..." -ForegroundColor Yellow
$cargoNdk = (Get-Command cargo-ndk -ErrorAction SilentlyContinue)
if ($cargoNdk) { Write-Host "[OK] cargo-ndk 已安装" -ForegroundColor Green }
else {
    Write-Host "[..] 安装 cargo-ndk ..." -ForegroundColor Yellow
    cargo install cargo-ndk --locked
}

Write-Host ""
Write-Host "=== 环境准备完成 ===" -ForegroundColor Cyan
Write-Host "接下来运行： .\scripts\build-android.ps1" -ForegroundColor Cyan
