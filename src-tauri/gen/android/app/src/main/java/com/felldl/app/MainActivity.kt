package com.felldl.app

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    requestNotificationPermissionIfNeeded()
  }

  /**
   * 安卓 13+ 的通知权限。
   *
   * 为什么放在这里：下载期间要显示一条常驻通知（前台服务的一部分），没有这个权限，
   * 通知会被系统直接丢掉（dumpsys 里显示 importance=NONE），用户完全看不到下载在进行。
   * 从服务启动路径通过 JNI 发起 `requestPermissions` 在真机上没有生效，
   * 而 onCreate 是 Activity 主线程、上下文确定，是这里唯一可靠的位置。
   */
  private fun requestNotificationPermissionIfNeeded() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
    val granted = checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) ==
      PackageManager.PERMISSION_GRANTED
    if (granted) return
    requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQ_NOTIFY)
  }

  companion object {
    private const val REQ_NOTIFY = 1002
  }
}
