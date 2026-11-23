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
| 系统托盘 | 关闭窗口收进托盘、托盘快捷操作、完成通知 | ✅ |
| 单实例 | 重复启动自动聚焦已有窗口 | ✅ (桌面) |
| Android | Tauri 2 Mobile 复用核心，一键构建 APK | ⚙️ 工程已生成，待装 NDK |

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

Android 工程骨架已生成（`src-tauri/gen/android`），Rust 4 个交叉编译目标已安装。
**唯一剩余依赖是 NDK**（约 1GB，需从 Google 下载）。当前开发环境无法访问 Google，
请在能访问的环境里执行：

```powershell
# 1. 一键准备环境（装 cmdline-tools / NDK / SDK 组件 / cargo-ndk，可重复运行）
.\scripts\setup-android.ps1

# 2. 构建 APK
.\scripts\build-android.ps1 -Release
```

> `setup-android.ps1` 会自动下载并配置 Android cmdline-tools，再用它安装 NDK 26.3。
> 若 Android Studio 已装好 NDK，可直接设置 `NDK_HOME` 环境变量后运行 `build-android.ps1`。
>
> Android 权限（网络/通知/唤醒锁/前台服务）已在
> `src-tauri/gen/android/app/src/main/AndroidManifest.xml` 配置好；
> `compileSdk/targetSdk` 已对齐本机 Android 35。

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
- 28 个自动化测试全通过
- 12 线程分段下载 SHA256 与单线程参考完全一致
- 38MB 文件下载到 62% 中断、重启续传后哈希一致
- 真实 exe 启动 → HTTP API → 引擎下载 → 文件哈希一致

---

## 📄 许可

MIT
