/**
 * 安卓返回键接管。
 *
 * 现状（真机实测）：打开「新建下载」对话框后按返回，应用直接退到桌面 ——
 * 因为 WryActivity 的返回逻辑是「WebView 能后退就后退，否则退出」，而 SPA
 * 本身没有任何历史记录，于是返回键等价于「退出应用」，对话框、多选、设置页
 * 全都被一起丢掉。
 *
 * 做法：把「有东西可关」映射成一条虚拟历史记录。有可关闭层时 pushState 一条，
 * 于是 canGoBack() 变 true，返回键被 WebView 吃掉并触发 popstate，我们按优先级
 * 关掉最上层；层全部关完后把多出的那条历史收回来，下一次返回就正常退出。
 *
 * 为什么不用原生 onBackPressed：`WryActivity.kt` 是 Tauri 生成的文件，下次
 * `tauri android init` 会被覆盖；前端历史方案不动生成代码，也能在浏览器里回归。
 */
import { useEffect, useRef } from "react";

export interface BackLayer {
  /** 这一层当前是否存在（对话框打开 / 处于多选 / 不在下载页…） */
  active: boolean;
  /** 返回键触发时怎么关掉它 */
  close: () => void;
}

const MARK = "felldlBack";

function isAndroid() {
  return document.documentElement.dataset.platform === "android";
}

export function useAndroidBack(layers: BackLayer[]) {
  const layersRef = useRef(layers);
  layersRef.current = layers;
  // 只关心「哪几层是开的」，避免每次渲染都重跑 effect
  const sig = layers.map((l) => (l.active ? "1" : "0")).join("");

  // 有层 → 保证历史里多一条；最后一层被按钮关掉 → 把这条历史收回
  useEffect(() => {
    if (!isAndroid()) return;
    const anyActive = layersRef.current.some((l) => l.active);
    const pushed = history.state?.[MARK] === true;
    if (anyActive && !pushed) {
      history.pushState({ [MARK]: true }, "");
    } else if (!anyActive && pushed) {
      // 用户点了 × / 取消关掉最后一层：不收回的话，返回键会先「空按」一次
      history.back();
    }
  }, [sig]);

  useEffect(() => {
    if (!isAndroid()) return;
    const onPop = () => {
      const cur = layersRef.current;
      const active = cur.filter((l) => l.active);
      // 没有可关闭的层：这是真正的后退（下一次返回会退出应用），不拦
      if (active.length === 0) return;
      active[0].close();
      // 还有层没关（例如多选之上又开了批量结果）：补一条历史，
      // 让下一次返回继续由我们接管，而不是直接退出应用
      if (active.length > 1) history.pushState({ [MARK]: true }, "");
    };
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);
}
