package com.felldl.app

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.provider.Settings
import java.security.MessageDigest

/**
 * 更新包安装辅助：读 APK/已安装包的包名、versionCode、签名摘要，检查「安装未知应用」权限，
 * 并用 FileProvider 的 content:// URI 拉起系统安装器。
 *
 * Rust 侧通过 JNI 调用这里的 @JvmStatic 方法。所有方法都返回
 * "OK:..." / "ERR:..." 字符串而不是抛异常：异常跨 JNI 边界容易变成
 * 未处理的 PendingException，排查起来非常痛苦。
 *
 * 注意：判断「这个包能不能装」的逻辑不在这里，在 Rust 的 `apk_update.rs`（可单测）。
 */
object ApkInstaller {

    /** 已安装版本：`OK:<versionCode>|<签名SHA256>` */
    @JvmStatic
    fun installedInfo(ctx: Context, pkg: String): String = try {
        val info = ctx.packageManager.getPackageInfo(pkg, signFlags())
        "OK:${versionCodeOf(info)}|${signerSha256(info)}"
    } catch (e: Exception) {
        "ERR:读取已安装版本失败：${e.message}"
    }

    /** APK 文件：`OK:<包名>|<versionCode>|<签名SHA256>` */
    @JvmStatic
    fun apkInfo(ctx: Context, path: String): String = try {
        val info = ctx.packageManager.getPackageArchiveInfo(path, signFlags())
        if (info == null) {
            "ERR:无法解析安装包（文件损坏或不是 APK）"
        } else {
            val pkg = info.packageName
            if (pkg.isNullOrEmpty()) {
                "ERR:安装包缺少包名"
            } else {
                "OK:$pkg|${versionCodeOf(info)}|${signerSha256(info)}"
            }
        }
    } catch (e: Exception) {
        "ERR:解析安装包失败：${e.message}"
    }

    /** 是否已允许「安装未知应用」；API 26 以下没有这个开关 */
    @JvmStatic
    fun canInstall(ctx: Context): Boolean = try {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            ctx.packageManager.canRequestPackageInstalls()
        } else {
            true
        }
    } catch (e: Exception) {
        false
    }

    /** 打开本应用的「安装未知应用」授权页 */
    @JvmStatic
    fun openInstallSettings(ctx: Context, pkg: String): String = try {
        val intent = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:$pkg"))
        } else {
            Intent(Settings.ACTION_SECURITY_SETTINGS)
        }
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        ctx.startActivity(intent)
        "OK:"
    } catch (e: Exception) {
        "ERR:无法打开安装权限设置：${e.message}"
    }

    /**
     * 用 content:// URI 拉起系统安装器（必须带读权限，否则安装器打不开文件）。
     *
     * 错误按类型加前缀，便于上层给不同提示：
     * - `install.no_handler`：系统没有能处理 apk 的安装器；
     * - `install.uri_denied`：URI 授权/权限被拒（FileProvider 配置或 grant flag 有问题）；
     * - `install.failed`：其它未知失败。
     *
     * 不用 `resolveActivity` 预检：target 30+ 的包可见性会让它误报 null，
     * 直接 startActivity 并捕获 ActivityNotFoundException 更准。
     */
    @JvmStatic
    fun install(ctx: Context, uri: String, mime: String): String = try {
        val intent = Intent(Intent.ACTION_VIEW)
        intent.setDataAndType(Uri.parse(uri), mime)
        intent.addFlags(
            Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK
        )
        ctx.startActivity(intent)
        "OK:"
    } catch (e: ActivityNotFoundException) {
        "ERR:install.no_handler: 系统没有可用的安装器"
    } catch (e: SecurityException) {
        "ERR:install.uri_denied: 系统拒绝了安装器读取更新包的权限"
    } catch (e: Exception) {
        "ERR:install.failed: ${e.message}"
    }

    private fun signFlags(): Int =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            PackageManager.GET_SIGNING_CERTIFICATES
        } else {
            @Suppress("DEPRECATION")
            PackageManager.GET_SIGNATURES
        }

    private fun versionCodeOf(info: PackageInfo): Long =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            info.longVersionCode
        } else {
            @Suppress("DEPRECATION")
            info.versionCode.toLong()
        }

    /** 当前签名证书的 SHA-256（小写十六进制）。多签名时取第一个。 */
    private fun signerSha256(info: PackageInfo): String {
        val certs = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            val si = info.signingInfo ?: return ""
            if (si.hasMultipleSigners()) si.apkContentsSigners else si.signingCertificateHistory
        } else {
            @Suppress("DEPRECATION")
            info.signatures ?: return ""
        }
        val cert = certs.firstOrNull() ?: return ""
        val digest = MessageDigest.getInstance("SHA-256").digest(cert.toByteArray())
        return digest.joinToString("") { "%02x".format(it) }
    }
}
