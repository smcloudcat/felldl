package com.felldl.app

import android.content.Context
import android.content.Intent
import android.net.Uri

/**
 * 「打开所在文件夹」的安卓实现。
 *
 * 为什么不在 Rust 里直接 `startActivity(content://com.android.externalstorage.documents/...)`：
 * 那条 SAF 文档 URI 只有在调用方持有该文档授权时才可用，普通应用没有 —— 真机上
 * 直接抛 `SecurityException: UID ... does not have permission to content://...`，
 * 异常穿过 JNI 边界会把进程杀掉（表现为点一下闪退）。所以「能不能用」的判断放在
 * Kotlin 侧用 try/catch 做，异常绝不外泄。
 *
 * 两级策略：
 * 1. ACTION_VIEW 直接定位目录：部分 ROM 会给用 DocumentsUI 打开的机会，成功即最佳体验；
 * 2. 失败则打开系统文件选择器（ACTION_OPEN_DOCUMENT）并用 `EXTRA_INITIAL_URI` 定位到
 *    该目录。选择器由 DocumentsUI 自己访问存储，**不需要调用方持有授权**，因此在所有
 *    设备上都有可见结果 —— 用户看到的就是这个文件夹里的内容。
 *
 * 返回 `OK:view` / `OK:picker` / `ERR:...`，与其它辅助类保持同一套字符串约定。
 */
object FolderOpener {

    @JvmStatic
    fun open(ctx: Context, dir: String): String {
        val rel = dir.trim().trimEnd('/').removePrefix("/storage/emulated/0/")
        val doc = Uri.parse(
            "content://com.android.externalstorage.documents/document/primary%3A" + Uri.encode(rel)
        )

        // 1) 直接定位（有授权的 ROM 会直接显示该目录）
        try {
            val direct = Intent(Intent.ACTION_VIEW)
                .setDataAndType(doc, "vnd.android.document/directory")
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
            ctx.startActivity(direct)
            return "OK:view"
        } catch (_: Exception) {
            // 没有授权就走下一步；这里绝不能把异常抛出去
        }

        // 2) 兜底：系统文件选择器定位到该目录
        return try {
            val pick = Intent(Intent.ACTION_OPEN_DOCUMENT)
                .setType("*/*")
                .putExtra("android.provider.extra.INITIAL_URI", doc)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            ctx.startActivity(pick)
            "OK:picker"
        } catch (e: Exception) {
            "ERR:没有可用的文件管理器：${e.message}"
        }
    }
}
