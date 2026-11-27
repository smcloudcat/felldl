/**
 * 对话框的无障碍行为，统一一处实现：
 * - Esc 关闭（键盘用户不必去找那个小 × ）；
 * - 打开时把焦点移进对话框，关闭时还给触发它的元素；
 * - Tab / Shift+Tab 在对话框内循环，不会跑到背后的页面上；
 * - 容器补 `tabIndex={-1}`，没有可聚焦子元素时也有落点。
 *
 * `onClose` 用 ref 保存：调用方通常是内联箭头函数，若进依赖数组，
 * 每次渲染都会重新聚焦一次，输入框会一直被打断。
 */
import { useEffect, useRef } from "react";

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

export function useDialogA11y(onClose: () => void, enabled = true) {
  const ref = useRef<HTMLDivElement | null>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;

  useEffect(() => {
    // 常驻组件（例如设置页里的协议弹窗）只在真正打开时才接管焦点与按键
    if (!enabled) return;
    const node = ref.current;
    const prev = document.activeElement as HTMLElement | null;
    // 初始焦点优先给输入框，其次第一个按钮；都没有就落在容器上
    const target =
      node?.querySelector<HTMLElement>("input, select, textarea") ??
      node?.querySelector<HTMLElement>(FOCUSABLE) ??
      node;
    target?.focus();

    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        closeRef.current();
        return;
      }
      if (e.key !== "Tab" || !node) return;
      const items = Array.from(
        node.querySelectorAll<HTMLElement>(FOCUSABLE),
      ).filter(
        (el) => el.offsetParent !== null || el === document.activeElement,
      );
      if (items.length === 0) {
        e.preventDefault();
        return;
      }
      const first = items[0];
      const last = items[items.length - 1];
      const active = document.activeElement;
      if (e.shiftKey && (active === first || active === node)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    };

    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
      // 关闭后把焦点还给触发者；元素已经不在了就留在 body，不要抛错
      if (prev && document.contains(prev)) prev.focus();
    };
  }, [enabled]);

  return ref;
}
