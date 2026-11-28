package com.felldl.app

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.Settings

/**
 * 「所有文件访问」（MANAGE_EXTERNAL_STORAGE）状态与授权入口。
 *
 * 单独放到 Kotlin 的原因：这个权限是 **Android 11（API 30）才有**的，
 * 而本项目 minSdk 是 24。Rust 侧直接用 JNI 调
 * `Environment.isExternalStorageManager()` 在 API 24~29 上会撞
 * `NoSuchMethodError`（方法根本不存在），线程上挂一个 pending 异常，
 * 返回 Java 时可能直接杀进程；就算不崩，用户拿到的也只是「查询失败」。
 * 版本判断放在这里，`Build.VERSION.SDK_INT` 和 `Environment.isExternalStorageManager()`
 * 都在 Kotlin 里是编译期可见的，不会拼错签名。
 *
 * 三态而不是布尔：API 30 以下没有这个开关，返回 `na`，调用方据此
 * **不显示授权按钮、也不报「缺少权限」**，改用「目录可写性自检」作为唯一判据。
 *
 * 所有方法返回 `OK:...` / `ERR:...` 字符串，异常不跨 JNI 边界（同 ApkInstaller 的约定）。
 */
object StorageAccess {

    /** `OK:granted` / `OK:denied` / `OK:na`（系统没有这个开关） */
    @JvmStatic
    fun status(ctx: Context): String {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return "OK:na"
        return try {
            val granted = Environment.isExternalStorageManager()
            if (granted) "OK:granted" else "OK:denied"
        } catch (e: Exception) {
            "ERR:读取存储权限状态失败：${e.message}"
        }
    }

    /**
     * Android 6~9（API 23~28）的「写外部存储」状态。
     *
     * API 29 起系统不再承认 `WRITE_EXTERNAL_STORAGE`（scoped storage），所以这里
     * 只覆盖到 28；29 及以上一律返回 `na`，交给「所有文件访问」或 SAF。
     * 与 `status()` 分开是因为两者语义不同：`status()` 是「所有文件访问」这个开关，
     * 老系统上它根本不存在；这里是老系统上真实存在的那个危险权限。
     */
    @JvmStatic
    fun legacyWriteState(ctx: Context): String {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.M ||
            Build.VERSION.SDK_INT > Build.VERSION_CODES.P
        ) return "OK:na"
        return try {
            val ok = ctx.checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE) ==
                PackageManager.PERMISSION_GRANTED
            if (ok) "OK:granted" else "OK:denied"
        } catch (e: Exception) {
            "ERR:读取旧版存储权限状态失败：${e.message}"
        }
    }

    /**
     * 打开本应用的「所有文件访问」授权页。
     *
     * API 30+ 用 `ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION`（带 package: URI，
     * 直接落到本应用那一项，省得用户在一长串列表里找）。
     * 个别 ROM 没有这个 Activity，退回全局列表页；再不行退到应用详情页。
     * API 30 以下没有该权限，返回 `OK:na` 让调用方提示「无需授权」而不是弹一个打不开的页面。
     */
    @JvmStatic
    fun openSettings(ctx: Context, pkg: String): String {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) return "OK:na"
        val candidates = listOf(
            Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, Uri.parse("package:$pkg")),
            Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION),
            Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.parse("package:$pkg")),
        )
        for (intent in candidates) {
            try {
                intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                ctx.startActivity(intent)
                return "OK:"
            } catch (e: Exception) {
                // 换下一个候选；全都不行时在下面统一报错
            }
        }
        return "ERR:系统没有可打开该授权页的入口"
    }
}
