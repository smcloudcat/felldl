package com.felldl.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.IBinder

/**
 * 下载前台服务：只做一件事——在真的有传输时把进程挂成前台，并显示一条常驻通知。
 *
 * 为什么需要它：下载跑在 Rust 引擎里，进程一旦被系统当成「后台缓存进程」，锁屏/待机后
 * CPU 与网络都可能被收紧，任务会以「网络错误」中止。声明 `FOREGROUND_SERVICE` 权限本身
 * 没有任何作用，必须真的有 `startForeground` 在跑。
 *
 * 刻意**不**在这里做下载：任务状态与断点续传都在 Rust 引擎里，服务重启后无法自己接上。
 * 所以 `onStartCommand` 返回 `START_NOT_STICKY`——被系统杀掉就由上层下一轮轮询重新拉起，
 * 而不是让系统重建一个没有引擎状态的空服务。
 *
 * 所有对外入口返回 `OK:...` / `ERR:...` 字符串，异常不跨 JNI 边界（同 ApkInstaller/StorageAccess）。
 */
class DownloadService : Service() {

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val text = intent?.getStringExtra(EXTRA_TEXT) ?: DEFAULT_TEXT
        val active = intent?.getIntExtra(EXTRA_ACTIVE, 1) ?: 1
        ensureChannel()
        return try {
            startForeground(NOTIFY_ID, buildNotification(text, active))
            START_NOT_STICKY
        } catch (e: Exception) {
            // API 31+ 在「应用不可见」时可能抛 ForegroundServiceStartNotAllowedException；
            // 个别 ROM 还有别的限制。这里绝不能崩：下载本身还在跑，只是少了前台保护。
            stopSelf()
            START_NOT_STICKY
        }
    }

    override fun onDestroy() {
        super.onDestroy()
        // stopForeground 在 stopSelf 流程里也会被调用，重复调用无害
        runCatching { stopForeground(STOP_FOREGROUND_REMOVE) }
    }

    private fun buildNotification(text: String, active: Int): Notification {
        val open = Intent(this, MainActivity::class.java).apply {
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        }
        val pending = PendingIntent.getActivity(
            this,
            0,
            open,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        // 「暂停」= 一条广播进投递目录，由引擎决定暂停谁；服务本身不持有任务列表
        val pauseIntent = Intent(this, CommandReceiver::class.java).setAction(CommandReceiver.ACTION_PAUSE_ALL)
        val pausePending = PendingIntent.getBroadcast(
            this,
            1,
            pauseIntent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val builder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            Notification.Builder(this, CHANNEL_ID)
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(this)
        }
        // 只有一条任务时说「暂停」，多条时才说「全部暂停」——文案跟用户看到的内容对齐
        val pauseLabel = if (active <= 1) "暂停" else "全部暂停"
        return builder
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setContentTitle("Felldl")
            .setContentText(text)
            .setContentIntent(pending)
            .addAction(
                Notification.Action.Builder(
                    android.graphics.drawable.Icon.createWithResource(this, android.R.drawable.ic_media_pause),
                    pauseLabel,
                    pausePending,
                ).build(),
            )
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setShowWhen(false)
            .build()
    }

    private fun ensureChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val mgr = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        if (mgr.getNotificationChannel(CHANNEL_ID) != null) return
        val channel = NotificationChannel(CHANNEL_ID, "下载进度", NotificationManager.IMPORTANCE_LOW).apply {
            description = "下载进行中显示当前任务数"
            setShowBadge(false)
            enableVibration(false)
        }
        mgr.createNotificationChannel(channel)
    }

    companion object {
        private const val CHANNEL_ID = "felldl-downloads"
        private const val NOTIFY_ID = 1001
        private const val EXTRA_TEXT = "text"
        private const val EXTRA_ACTIVE = "active"
        private const val DEFAULT_TEXT = "正在下载"

        /**
         * 启动或更新前台服务。`text` 是通知副标题（例如「正在下载 2 个任务」），
         * `active` 用来决定按钮文案（单任务「暂停」/ 多任务「全部暂停」），
         * **不参与状态判断**——暂停谁由 Rust 引擎决定。
         *
         * 通知权限不在这里要：从 JNI 这条路径发起 `requestPermissions` 在真机上没有生效
         * （系统看成后台请求，直接不弹），统一放到 `MainActivity.onCreate` 处理。
         */
        @JvmStatic
        fun start(ctx: Context, text: String, active: Int): String = try {
            val i = Intent(ctx, DownloadService::class.java)
                .putExtra(EXTRA_TEXT, text)
                .putExtra(EXTRA_ACTIVE, active)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                ctx.startForegroundService(i)
            } else {
                @Suppress("DEPRECATION")
                ctx.startService(i)
            }
            "OK:"
        } catch (e: Exception) {
            "ERR:启动下载服务失败：${e.message}"
        }

        /** 结束前台状态；没有人在跑时调用也是安全的 */
        @JvmStatic
        fun stop(ctx: Context): String = try {
            ctx.stopService(Intent(ctx, DownloadService::class.java))
            "OK:"
        } catch (e: Exception) {
            "ERR:停止下载服务失败：${e.message}"
        }
    }
}
