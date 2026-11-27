package com.felldl.app

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import java.io.File

/**
 * 通知栏按钮的接收端：只负责把命令**投递**给 Rust 引擎，自己不碰任何任务状态。
 *
 * 通道细节见 Rust 侧 `native_cmd.rs`：往 `<cache>/engine-cmd/` 里创建一个空文件，
 * 文件名是 `<毫秒>-<nonce>.<命令>`，引擎每秒扫一次并按名字顺序执行、执行前先删。
 *
 * 为什么不在广播里直接改状态：任务与断点全在 Rust 引擎里，Kotlin 侧没有、也不该有副本；
 * 在这里写一份「已暂停」只会立刻和引擎不一致。
 */
class CommandReceiver : BroadcastReceiver() {

    override fun onReceive(ctx: Context, intent: Intent?) {
        val tag = when (intent?.action) {
            ACTION_PAUSE_ALL -> TAG_PAUSE_ALL
            else -> return
        }
        // 广播里抛异常会直接 ANR/崩进程，这里一律吞掉：按钮没生效比崩溃好
        runCatching { EngineCommands.post(ctx, tag) }
    }

    companion object {
        const val ACTION_PAUSE_ALL = "com.felldl.app.action.PAUSE_ALL"
        private const val TAG_PAUSE_ALL = "pause_all"
    }
}

/**
 * 命令投递目录的写入方。
 *
 * 用 `createNewFile()` 而不是「写临时文件再改名」：命令名就在扩展名里、内容为空，
 * 文件一出现就是完整可读的；改名反而会引入「引擎刚好扫到 .tmp 并把它当脏文件删掉」的竞态。
 */
object EngineCommands {

    fun post(ctx: Context, tag: String): Boolean {
        val dir = File(ctx.cacheDir, "engine-cmd")
        if (!dir.exists() && !dir.mkdirs()) return false
        val name = "${System.currentTimeMillis()}-${Integer.toHexString(System.nanoTime().toInt())}.$tag"
        return File(dir, name).createNewFile()
    }
}
