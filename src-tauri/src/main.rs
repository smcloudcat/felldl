// Windows 上不弹出控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // 更新 helper：同一个 exe 的隐藏子命令，必须在 Tauri 初始化之前处理，
    // 否则会拉起整个界面。参数是 argv 独立元素，不经过任何命令行解析。
    if let Some(args) = felldl_lib::update_helper::parse_args(std::env::args()) {
        std::process::exit(felldl_lib::update_helper::run(args));
    }
    felldl_lib::run();
}
