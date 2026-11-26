fn main() {
    tauri_build::build();
    check_version_single_source();
}

/// 版本号必须只有一处事实来源。
///
/// 同一个版本号有两条互不相干的流向：
/// - `tauri.conf.json` 的 `version` → 桌面安装包与安卓 manifest 的 `versionName`
///   （安卓侧由 `scripts/build-android.ps1` 生成 `app/tauri.properties` 后进 Gradle）；
/// - Cargo 版本 → `env!("CARGO_PKG_VERSION")`，也就是「关于」页显示的当前版本，
///   以及 `check_update` 判断「远端是不是更新」时用的基准。
///
/// 只要两者漂移，用户会看到两种都很怪的景象：装上了 0.4.0、关于页仍写 0.3.9，
/// 于是检查更新反复提示同一个版本；或者反过来，明明没升级却认为已经是最新。
/// `update-cache` 里记 `app_version` 只能挡住缓存过期那一类原因，
/// 挡不住「两个文件写了两个数」。所以直接让构建失败，而不是等用户报障。
///
/// 改版本号时两处一起改：`Cargo.toml` 的 `[workspace.package] version`
/// 与 `src-tauri/tauri.conf.json` 的 `version`（以及 `ui/package.json`、`version.json`）。
fn check_version_single_source() {
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let conf_path = manifest_dir.join("tauri.conf.json");
    println!("cargo:rerun-if-changed={}", conf_path.display());

    let cargo_version = env!("CARGO_PKG_VERSION");
    let text = match std::fs::read_to_string(&conf_path) {
        Ok(t) => t,
        // 没有配置文件时交给 tauri_build 报错，不在这里抢着失败
        Err(_) => return,
    };
    let conf: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => panic!("{} 不是合法 JSON: {e}", conf_path.display()),
    };
    // 缺省时 Tauri 会回落到 CARGO_PKG_VERSION，天然一致
    let conf_version = match conf.get("version").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => return,
    };
    if conf_version == cargo_version {
        return;
    }
    panic!(
        "\n版本号不一致，构建中止：\n\
         \x20 Cargo.toml 的 workspace version = {cargo_version}\n\
         \x20 {} 的 version = {conf_version}\n\
         这两个数分别决定「关于页/更新判断」和「安装包 manifest 的 versionName」。\n\
         不一致时用户会看到安装完成后版本号没变、检查更新反复提示同一版本。\n\
         请把 Cargo.toml（[workspace.package] version）、src-tauri/tauri.conf.json、\n\
         ui/package.json、version.json 一起改成同一个版本。\n",
        conf_path.display()
    );
}
