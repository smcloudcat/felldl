# Felldl 🚀

一款从零构建的现代多线程下载加速器，对标 Free Download Manager。
Windows 与 Android 共用**同一套 Rust 引擎 + React 界面**，浏览器扩展可把下载任务自动转交过来。

---

## ✨ 功能

| 功能 | 说明 | 状态 |
|---|---|---|
| 多线程分段下载 | 每个任务最多 32 连接，按字节范围并行拉取 | ✅ |
| 断点续传 | 分片级进度持久化，进程重启后自动续传，ETag 校验 | ✅ |
| 浏览器集成 | MV3 扩展（Chrome/Edge）自动接管下载，带 Token 鉴权 | ✅ |
| 定时下载 | 指定时间自动开始 | ✅ |
| 代理设置 | HTTP/HTTPS/SOCKS 代理 | ✅ |
| 限速 | 全局限速 + 单任务限速（令牌桶） | ✅ |
| 并发控制 | 全局最大同时下载数，超出排队 | ✅ |
| 失败重试 | 断线指数退避重试，从断点继续不重复计数 | ✅ |
| 现代界面 | React 19 + 手写设计系统，浅色/深色主题，移动端适配 | ✅ |
| 检查更新 | 启动自动检查 GitHub Releases，设置页手动检查，横幅提示 + 跳转下载 | ✅ |
| 系统托盘 | 关闭窗口收进托盘、托盘快捷操作、完成通知 | ✅ |
| 单实例 | 重复启动自动聚焦已有窗口 | ✅ (桌面) |
| Android | Tauri 2 Mobile 复用核心，已出 arm64 / x86_64 release 包并实测 | ✅ |

---

## 🏗️ 架构

```
felldl/
├── core/           # Rust 下载引擎（无 GUI 依赖，可独立使用）
│   ├── model.rs    # 任务/设置/事件数据结构
│   ├── engine.rs   # 调度、分片、续传、限速、重试
│   ├── store.rs    # JSON 持久化
│   ├── util.rs     # 令牌桶限速、文件名解析
│   └── api.rs      # 本地 HTTP API（浏览器集成）
├── src-tauri/      # Tauri 2 桌面/移动壳层
│   └── src/        # 命令、托盘、通知、单实例
├── ui/             # React 19 + Vite 界面
├── extension/      # Chrome/Edge MV3 浏览器扩展
├── scripts/        # Android 环境准备 / 构建脚本
└── core/examples/  # CLI 测试工具
```

**设计原则**：核心引擎完全平台无关，Windows/Android 只是两层不同的壳，业务逻辑零重复。

---

## 🔨 构建

### 环境要求
- Node.js 18+、Rust 1.77+（含 `rustup`）
- Windows 桌面：Webview2（Win11 自带）
- Android：Android SDK + NDK（见下方脚本）

### Windows 桌面端

```bash
# 1. 前端依赖
cd ui && npm install && cd ..

# 2. 开发模式（热更新）
cargo install tauri-cli --version "^2" --locked
cargo tauri dev

# 3. 生产构建（生成安装包）
cargo tauri build
```

产物位置：
- 免安装 exe：`target/release/felldl.exe`
- NSIS 安装包：`target/release/bundle/nsis/`
- MSI 安装包：`target/release/bundle/msi/`

### Android 端

**已实测跑通**：arm64 真机包与 x86_64 模拟器包均可安装、启动、下载、断点续传。

环境要求：Android SDK（platform 36 + build-tools）、NDK 27.x、JDK 17+、`cargo-ndk`。

```powershell
# 1. 准备环境（装 cmdline-tools / NDK / SDK 组件 / cargo-ndk，可重复运行）
.\scripts\setup-android.ps1

# 2. 构建 APK
.\scripts\build-android.ps1 -Release
```

手动构建（脚本内部等价步骤）：

```powershell
$env:JAVA_HOME    = "G:\java22"                                     # JDK 17+
$env:ANDROID_HOME = "$env:LOCALAPPDATA\Android\Sdk"
$env:NDK_HOME     = "$env:LOCALAPPDATA\Android\Sdk\ndk\27.2.12479018"

# 前端 → 内嵌进 Rust 库
npm --prefix ui run build

# 关键：必须带 --features custom-protocol，否则打进的是 devUrl(localhost:1420)，界面会白屏
cargo ndk -t arm64-v8a -o src-tauri/gen/android/app/src/main/jniLibs build --release -p felldl --features custom-protocol

# 打包（跳过 gradle 里重复的 rust 任务，-x 名与 ABI 对应）
cd src-tauri/gen/android
.\gradlew.bat assembleArm64Release -PabiList=arm64-v8a -ParchList=arm64 -PtargetList=aarch64 `
  -x rustBuildArm64Release --no-daemon --no-configuration-cache
```

产物：
- arm64（真机）：`src-tauri/gen/android/app/build/outputs/apk/arm64/release/app-arm64-release.apk`
- x86_64（模拟器）：`src-tauri/gen/android/app/build/outputs/apk/x86_64/release/app-x86_64-release.apk`

签名信息：`src-tauri/gen/android/keystore/felldl-release.jks`，alias `felldl`，密码 `felldl123`
（自签自用；上架应用商店请替换为正式证书）。

> ⚠️ 两个必须注意的点：
> 1. `src-tauri/capabilities/default.json` 必须存在。Tauri 2 在缺失 capability 时
>    WebView 的 IPC / 事件订阅会被拒绝，表现为界面永远停在「正在启动引擎…」。
> 2. `src-tauri/Cargo.toml` 必须声明 `custom-protocol` feature，且 release 构建时显式启用。

关于安装包构建：`cargo tauri build` 默认会尝试联网下载 NSIS/WiX 制作 `.exe`/`.msi` 安装包，
网络不佳时可能超时。此时可跳过打包，直接使用 `target/release/felldl.exe`（免安装，功能完整）。

### 独立 CLI（用于测试引擎）

```bash
cargo run --release --example cli -- "https://example.com/file.zip" --conn 16 --out ./downloads
```

---

## 🌐 浏览器扩展安装

1. 打开 `chrome://extensions/`（Edge 为 `edge://extensions/`）
2. 开启「开发者模式」
3. 点击「加载已解压的扩展程序」，选择 `felldl/extension` 目录
4. 启动 Felldl 桌面端 → 设置 → 浏览器集成，复制**端口**和 **Token**
5. 点击浏览器工具栏的 Felldl 图标，填入端口和 Token 并保存
6. 之后浏览器的下载会自动转交给 Felldl

> 扩展只监听 `http(s)` 下载，且优先探测 Felldl 是否在线；离线时自动放行原生下载，不打断使用。

---

## 🧪 测试

```bash
# 引擎单元测试 + 集成测试（含本地 Range 服务器、断点续传、限速、重试）
cargo test

# 真实网络端到端（已在开发中验证通过）
cargo run --release --example resume_check -- <url> <expected_sha256> <connections>
```

已验证项目：
- 29 个自动化测试全通过
- 12 线程分段下载 SHA256 与单线程参考完全一致
- 38MB 文件下载到 62% 中断、重启续传后哈希一致
- 真实 exe 启动 → HTTP API → 引擎下载 → 文件哈希一致

### Android 实测记录

在 Android 16（API 36）x86_64 模拟器上逐项验证：

| 项目 | 结果 |
|------|------|
| release APK 安装启动 | ✅ 无崩溃，WebView 正常加载内嵌资源 |
| 列表 / 空状态 / 计数 | ✅ 侧边栏分类计数正确 |
| 新建下载对话框 | ✅ 链接、保存位置、连接数、限速、定时、高级选项齐全 |
| 16 线程真实下载 | ✅ 20MB 与 1MB 文件均完整落盘到 `/storage/emulated/0/Download` |
| 任务实时刷新 | ✅ 提交后无需刷新，`taskUpdated` 事件即时插入列表 |
| 设置页 | ✅ 下载目录、并发数、限速滑块、代理、浏览器集成正常 |
| 移动端布局 | ✅ 侧边栏抽屉、浮动新建按钮（FAB）、安全区适配 |
| 签名 | ✅ v2 方案，证书 `CN=Felldl`，arm64 与 x86_64 同证书 |

> 桌面端与 Android 共用同一套 `ui/` 前端与 `core/` 引擎；桌面端另经
> `HTTP API → 引擎 → taskUpdated 事件 → React 列表` 链路验证实时刷新。

---

## 📄 许可

MIT
