package com.felldl.app

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities

/**
 * 只回答一个问题：当前有没有可用的默认网络。
 *
 * 为什么不注册 `NetworkCallback` 而是让 Rust 每 2s 问一次：回调需要跨 JNI 反向调用，
 * 要在每个 ABI 维护符号名，出错就是无堆栈崩溃；而这个值只用来「别在断网时烧重试次数」，
 * 2s 的采样延迟完全够（第一次退避本来就是 2s）。
 *
 * 注意语义是**有没有网**，不是「目标站能不能通」：连着 5G 但服务器在内网时它照样回 true，
 * 那种情况交给重试次数上限兜底。
 */
object NetworkState {

    @JvmStatic
    fun isOnline(ctx: Context): String = try {
        val cm = ctx.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
        val caps = cm.getNetworkCapabilities(cm.activeNetwork)
        val ok = caps?.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) == true
        if (ok) "OK:1" else "OK:0"
    } catch (e: Exception) {
        // 拿不到就当「有网」：宁可多试几次，也不要因为一次查询异常把自动重试永久挂起
        "ERR:查询网络状态失败：${e.message}"
    }
}
