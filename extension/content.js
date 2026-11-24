/**
 * Felldl 扩展 content script
 *
 * 功能：用户用鼠标选中包含链接的内容后，在选区附近浮现
 * 「使用 Felldl 下载」按钮；点击后把选区内的全部链接一次性
 * 发给本地 Felldl 加入下载队列。
 */

(() => {
  if (window.__felldlExt) return;
  window.__felldlExt = true;

  const BTN_ID = "__felldl-selection-btn";
  const COLLECTION_KEY = "__felldl_links";

  /** 收集选区内的链接（包括锚点、带链接的图片、以及选中文本里出现的 URL） */
  function collectLinks(selection) {
    const found = [];
    const push = (u) => {
      try {
        const url = new URL(u, location.href);
        if (!/^https?:$/.test(url.protocol)) return;
        if (/^(127\.0\.0\.1|localhost)$/.test(url.hostname)) return;
        const s = url.href;
        if (!found.includes(s)) found.push(s);
      } catch {
        /* 非法 URL */
      }
    };

    const rangeText = selection.toString();

    if (selection.rangeCount > 0) {
      const container = selection.getRangeAt(0).commonAncestorContainer;
      const root = container.nodeType === 1 ? container : container.parentElement;
      if (root) {
        // 选区内的锚点 / 带链接元素
        root.querySelectorAll?.("a[href], [data-felldl-link]").forEach((el) => {
          if (selection.containsNode(el, true)) {
            push(el.href || el.dataset.felldlLink);
          }
        });
      }
    }

    // 选中纯文本里的裸 URL
    for (const m of rangeText.matchAll(/https?:\/\/[^\s"'<>)]+/g)) {
      push(m[0]);
    }

    // 没抓到任何链接时，如果点击发生在链接上，回退用最近链接
    if (!found.length && window[COLLECTION_KEY]?.size) {
      for (const u of window[COLLECTION_KEY]) push(u);
    }
    return found;
  }

  /** 浮动按钮 */
  function showButton(x, y, count) {
    hideButton();
    const btn = document.createElement("div");
    btn.id = BTN_ID;
    btn.textContent = `⬇ 使用 Felldl 下载 (${count})`;
    Object.assign(btn.style, {
      position: "fixed",
      left: `${Math.min(x, window.innerWidth - 180)}px`,
      top: `${Math.min(y + 10, window.innerHeight - 40)}px`,
      zIndex: "2147483647",
      padding: "8px 14px",
      borderRadius: "10px",
      background: "linear-gradient(135deg,#0ea5e9,#06b6d4)",
      color: "#fff",
      fontSize: "13px",
      fontWeight: "600",
      fontFamily: "system-ui,sans-serif",
      cursor: "pointer",
      boxShadow: "0 6px 22px -4px rgba(14,165,233,.55)",
      userSelect: "none",
      transition: "transform .12s ease",
    });
    btn.addEventListener("mouseenter", () => (btn.style.transform = "scale(1.05)"));
    btn.addEventListener("mouseleave", () => (btn.style.transform = "scale(1)"));
    btn.addEventListener("mousedown", (e) => e.stopPropagation());
    btn.addEventListener("click", (e) => {
      e.preventDefault();
      e.stopPropagation();
      const sel = window.getSelection();
      const urls = collectLinks(sel);
      hideButton();
      if (!urls.length) return;
      chrome.runtime.sendMessage({ type: "sendLinks", urls });
    });
    document.documentElement.appendChild(btn);
  }

  function hideButton() {
    document.getElementById(BTN_ID)?.remove();
  }

  document.addEventListener("mouseup", (e) => {
    if (e.target?.id === BTN_ID) return;
    // 点击别处先收起
    setTimeout(() => {
      const sel = window.getSelection();
      if (!sel || sel.isCollapsed) {
        hideButton();
        return;
      }
      const text = sel.toString();
      const hasLink = sel.rangeCount > 0 && !!sel.getRangeAt(0).cloneContents().querySelector("a[href]");
      const hasRawUrl = /https?:\/\/[^\s"'<>)]+/.test(text);
      if (!hasLink && !hasRawUrl) {
        hideButton();
        return;
      }
      showButton(e.clientX, e.clientY, sel.toString().length > 40 ? "多个" : "所选");
    }, 10);
  });

  document.addEventListener("mousedown", (e) => {
    if (e.target?.id !== BTN_ID) hideButton();
  });

  // 记录鼠标悬停过的链接（纯文本选中时回退）
  document.addEventListener(
    "mouseover",
    (e) => {
      const a = e.target.closest?.("a[href]");
      window[COLLECTION_KEY] = window[COLLECTION_KEY] || new Set();
      if (a) {
        window[COLLECTION_KEY].clear();
        window[COLLECTION_KEY].add(a.href);
      }
    },
    { passive: true }
  );

  // 右键菜单「批量下载选中的链接」
  chrome.runtime.onMessage.addListener((msg, _s, sendResponse) => {
    if (msg?.type === "getSelectionLinks") {
      sendResponse({ urls: collectLinks(window.getSelection()) });
    }
  });
})();
