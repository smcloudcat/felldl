# Felldl Android APK 一键构建脚本
#
# 用法：
#   .\scripts\build-android.ps1                # debug APK（含调试符号，体积大，便于排查）
#   .\scripts\build-android.ps1 -Release       # release APK（已签名，体积小，适合分发）
#   .\scripts\build-android.ps1 -Abi armv7     # 构建 armv7（32 位老机型）
#   .\scripts\build-android.ps1 -Release -Abi x86_64   # 模拟器用 x86_64 包
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
    [ValidateSet("arm64", "armv7", "x86_64")]
    [string]$Abi = "arm64",
    # 留空则按版本号取已安装的最高 NDK；指定时必须是 ndk\<版本> 下已存在的目录名
    [string]$NdkVersion = ""
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

# NDK 选择：显式 -NdkVersion > NDK_HOME > 版本号最高的已安装 NDK
# 不用 Sort-Object Name：字符串排序会把 "9.x" 排到 "30.x" 之后
$ndkRoot = Join-Path $sdk "ndk"
$ndkPath = $null
if ($NdkVersion) {
    $ndkPath = Join-Path $ndkRoot $NdkVersion
    if (-not (Test-Path -LiteralPath $ndkPath)) {
        Write-Host "[!] 指定 NDK 不存在：$ndkPath" -ForegroundColor Red; exit 1
    }
} elseif ($env:NDK_HOME -and (Test-Path -LiteralPath $env:NDK_HOME)) {
    $ndkPath = $env:NDK_HOME
} else {
    $cand = @(Get-ChildItem $ndkRoot -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^\d+(\.\d+)+$' } |
        Sort-Object { [version]$_.Name } -Descending)
    if ($cand.Count -gt 0) { $ndkPath = $cand[0].FullName }
}
if (-not $ndkPath -or -not (Test-Path -LiteralPath $ndkPath)) {
    Write-Host "[!] 未找到 NDK，请先运行 setup-android.ps1" -ForegroundColor Red; exit 1
}
$env:NDK_HOME = $ndkPath
$ndkName = Split-Path $ndkPath -Leaf
Write-Host "[OK] NDK: $ndkName" -ForegroundColor Green
Write-Host "    提示：Gradle 内的 AGP NDK 版本由 app/build.gradle.kts 的 ndkVersion/AGP 默认决定，本脚本只控制 cargo-ndk 使用的 NDK。" -ForegroundColor DarkGray

if (-not ($env:JAVA_HOME -and (Test-Path (Join-Path $env:JAVA_HOME "bin\java.exe")))) {
    Write-Host "[!] JAVA_HOME 未设置或无效（Gradle 需要 JDK 17+）" -ForegroundColor Red; exit 1
}
Write-Host "[OK] JDK: $env:JAVA_HOME" -ForegroundColor Green

# cargo 必须在 PATH；尊重迁移后的 CARGO_HOME，不把旧用户目录抢回首位
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }
$cargoBin = Join-Path $cargoHome "bin"
if (Test-Path $cargoBin) {
    $pathRest = @($env:Path -split ";" | Where-Object {
        $_ -and $_.TrimEnd("\", "/") -ine $cargoBin.TrimEnd("\", "/")
    })
    $env:Path = "$cargoBin;$($pathRest -join ';')"
}

# cargo-ndk 是 cargo 子命令：直接执行 cargo-ndk.exe 会以退出码 1 报「只能通过 cargo ndk 调用」
foreach ($check in @(@("cargo", "cargo --version"), @("cargo-ndk", "cargo ndk --version"))) {
    Invoke-Expression $check[1] *> $null
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[!] 缺少 $($check[0])" -ForegroundColor Red
        if ($check[0] -eq "cargo-ndk") { Write-Host "    请运行： cargo install cargo-ndk" -ForegroundColor Yellow }
        exit 1
    }
}

# ── 2. ABI 映射 ──────────────────────────────────────────
$abiMap = @{
    "arm64" = @{ RustTarget = "arm64-v8a"; GradleFlavor = "Arm64"; RustArch = "aarch64" }
    "armv7" = @{ RustTarget = "armeabi-v7a"; GradleFlavor = "Arm"; RustArch = "armv7" }
    "x86_64" = @{ RustTarget = "x86_64"; GradleFlavor = "X86_64"; RustArch = "x86_64" }
}
$cfg = $abiMap[$Abi]
Write-Host "[OK] 目标 ABI: $($cfg.RustTarget)" -ForegroundColor Green

# ── 3. 首次初始化 Android 工程 ───────────────────────────
if (-not (Test-Path "src-tauri\gen\android")) {
    Write-Host "[..] 首次初始化 Android 工程（需要 NDK_HOME 已设置）..." -ForegroundColor Yellow
    cargo tauri android init --ci
}

# 直接 cargo-ndk 构建也需传入官方 Android 环境，build.rs 才会再生依赖路径。
# 不直接修改 tauri.settings.gradle；同时传包名，避免 Tauri 生成 proguard 时缺参。
$androidProject = Join-Path $projectRoot "src-tauri\gen\android"
$androidPackage = (Get-Content -LiteralPath (Join-Path $projectRoot "src-tauri\tauri.conf.json") -Raw -Encoding UTF8 | ConvertFrom-Json).identifier
$generatedKotlin = Join-Path $androidProject ("app\src\main\java\" + $androidPackage.Replace(".", "\") + "\generated")
if (-not (Test-Path -LiteralPath $generatedKotlin)) {
    Write-Host "[!] Android generated 目录缺失，请先重新初始化 Android 工程。" -ForegroundColor Red
    exit 1
}
$env:TAURI_ANDROID_PROJECT_PATH = $androidProject
$env:TAURI_ANDROID_PACKAGE_UNESCAPED = $androidPackage
$env:WRY_ANDROID_PACKAGE = $androidPackage
$env:WRY_ANDROID_LIBRARY = "felldl_lib"
$env:WRY_ANDROID_KOTLIN_FILES_OUT_DIR = $generatedKotlin

# ── 4. 前端构建 ──────────────────────────────────────────
Write-Host "[..] 构建前端 ..." -ForegroundColor Yellow
Push-Location ui
$prevEap = $ErrorActionPreference
$ErrorActionPreference = "Continue"   # npm 把 notice 写到 stderr，Stop 模式会误判为异常
try {
    if (-not (Test-Path "node_modules")) {
        npm install
        if ($LASTEXITCODE -ne 0) { throw "npm install 失败" }
    }
    npm run build
    $npmExit = $LASTEXITCODE
} finally { $ErrorActionPreference = $prevEap; Pop-Location }
if ($npmExit -ne 0) { Write-Host "[!] 前端构建失败" -ForegroundColor Red; exit 1 }

# ── 5. 编译 Rust .so 并输出到 jniLibs ────────────────────
$jniDir = "src-tauri\gen\android\app\src\main\jniLibs"
$profileLabel = if ($Release) { "release" } else { "debug" }
Write-Host "[..] 编译 Rust（$profileLabel, $($cfg.RustTarget)）..." -ForegroundColor Yellow
# --platform 24：NDK r26+ 默认 minSdk 更高，不显式指定会出现 getifaddrs 未定义
# 参数用数组一次性拼好再 splat：散着写 @var 在 PowerShell 5.1 下会被拆坏
$rustArgs = @("ndk", "--platform", "24", "-t", $cfg.RustTarget, "-o", $jniDir, "build")
if ($Release) { $rustArgs += @("--release", "--features", "custom-protocol") }
$rustArgs += @("-p", "felldl")
# PowerShell 5.1 会把原生命令写到 stderr 的正常日志当成 NativeCommandError；
# 这里临时放宽，失败仍以 $LASTEXITCODE 判定
$prevEap = $ErrorActionPreference
$ErrorActionPreference = "Continue"
try {
    & cargo @rustArgs
    $rustExit = $LASTEXITCODE
} finally { $ErrorActionPreference = $prevEap }
if ($rustExit -ne 0) { Write-Host "[!] Rust 编译失败" -ForegroundColor Red; exit 1 }

# CARGO_HOME 切换后，旧 Cargo build 输出可能仍引用旧 registry。
# 先检查官方生成文件，避免表面使用新缓存，Gradle 实际仍编译旧盘源码。
$settingsFile = Join-Path $androidProject "tauri.settings.gradle"
$settingsText = Get-Content -LiteralPath $settingsFile -Raw -Encoding UTF8
$dependencyPaths = [regex]::Matches($settingsText, 'projectDir\s*=\s*new File\(("(?:\\.|[^"\\])*")\)')
if ($dependencyPaths.Count -eq 0) {
    Write-Host "[!] 未找到 Tauri Android 依赖路径，停止打包。" -ForegroundColor Red
    exit 1
}
$currentRegistry = [IO.Path]::GetFullPath((Join-Path $cargoHome "registry\src")).TrimEnd("\", "/") + [IO.Path]::DirectorySeparatorChar
foreach ($match in $dependencyPaths) {
    $dependencyPath = [IO.Path]::GetFullPath(($match.Groups[1].Value | ConvertFrom-Json))
    if ($dependencyPath -match '[\\/]registry[\\/]src[\\/]' -and
        -not $dependencyPath.StartsWith($currentRegistry, [StringComparison]::OrdinalIgnoreCase)) {
        Write-Host "[!] Tauri Android 依赖仍指向其他 Cargo 缓存: $dependencyPath" -ForegroundColor Red
        Write-Host "    请为 CARGO_TARGET_DIR 指定新的项目 target 子目录后重跑，触发官方重新生成；不要手改 tauri.settings.gradle。" -ForegroundColor Yellow
        exit 1
    }
}

# ── 6. release 签名检查 ──────────────────────────────────
# 签名材料不入库：从 gen/android/keystore.properties 或环境变量读取，由 build.gradle.kts 消费。
# 这里只做前置校验，避免“以为签了其实签了空包”。
if ($Release) {
    $ksProps = "src-tauri\gen\android\keystore.properties"
    $signingReady = $false
    if (Test-Path -LiteralPath $ksProps) {
        $props = @{}
        foreach ($line in (Get-Content -LiteralPath $ksProps -Encoding UTF8)) {
            if ($line -match '^\s*([^#=]+?)\s*=\s*(.*)$') { $props[$Matches[1]] = $Matches[2].Trim() }
        }
        $required = @("storeFile", "storePassword", "keyAlias", "keyPassword")
        $missing = @($required | Where-Object { -not $props[$_] })
        if ($missing.Count -eq 0) {
            $ksPath = Join-Path "src-tauri\gen\android" $props["storeFile"]
            if (Test-Path -LiteralPath $ksPath) { $signingReady = $true }
            else { Write-Host "[!] keystore.properties 指向的密钥库不存在：$ksPath" -ForegroundColor Red }
        } else {
            Write-Host "[!] keystore.properties 缺少字段：$($missing -join ', ')" -ForegroundColor Red
        }
    }
    if (-not $signingReady -and $env:FELLDL_KEYSTORE_PASSWORD -and $env:FELLDL_KEY_ALIAS -and $env:FELLDL_KEYSTORE_FILE) {
        $signingReady = $true
    }
    if (-not $signingReady) {
        Write-Host "[!] 未找到可用的 release 签名配置，已中止（避免产出未签名 release 包）" -ForegroundColor Red
        Write-Host "    参考 src-tauri/gen/android/keystore.properties.example 创建 keystore.properties，" -ForegroundColor Yellow
        Write-Host "    或设置 FELLDL_KEYSTORE_FILE / FELLDL_KEYSTORE_PASSWORD / FELLDL_KEY_ALIAS / FELLDL_KEY_PASSWORD。" -ForegroundColor Yellow
        Write-Host "    密钥与口令不要提交到 Git。" -ForegroundColor Yellow
        exit 1
    }
    Write-Host "[OK] release 签名配置已就绪（密钥路径与口令不在此处回显）" -ForegroundColor Green
}

# ── 7. Gradle 打包（跳过 Gradle 内的 Rust 重编译）────────
$gradleTask = "assemble$($cfg.GradleFlavor)$(if ($Release) { 'Release' } else { 'Debug' })"
$skipTask = "rustBuild$($cfg.GradleFlavor)$(if ($Release) { 'Release' } else { 'Debug' })"
Write-Host "[..] Gradle 打包：$gradleTask ..." -ForegroundColor Yellow

Push-Location "src-tauri\gen\android"
$prevEap = $ErrorActionPreference
$ErrorActionPreference = "Continue"
try {
    .\gradlew.bat $gradleTask `
        "-PabiList=$($cfg.RustTarget)" `
        "-ParchList=$($Abi)" `
        "-PtargetList=$($cfg.RustArch)" `
        "-x" $skipTask `
        --no-daemon --no-configuration-cache
    $gradleOk = ($LASTEXITCODE -eq 0)
} finally { $ErrorActionPreference = $prevEap; Pop-Location }

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
