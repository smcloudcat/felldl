/**
 * 隐私条款 / 用户协议文本（中英双语，随应用内置，不依赖网络）
 *
 * 内容与 Felldl 的实际行为保持一致：纯本地下载工具，不上传用户数据。
 * 修改功能行为时记得同步这里。
 */
import type { Lang } from "./i18n";

export interface LegalDoc {
  title: string;
  updated: string;
  sections: { heading: string; body: string }[];
}

const UPDATED_ZH = "2026-11-25";
const UPDATED_EN = "2026-11-25";

export const PRIVACY: Record<Lang, LegalDoc> = {
  zh: {
    title: "隐私条款",
    updated: UPDATED_ZH,
    sections: [
      {
        heading: "一句话说明",
        body: "Felldl 是一个纯本地运行的下载工具。我们不设账号体系、不做数据埋点，也不会上传你的下载记录、文件内容或浏览信息。",
      },
      {
        heading: "本地保存的数据",
        body:
          "任务列表、下载进度、设置项保存在你电脑的应用数据目录中；下载的文件保存在你自己选择的目录里。" +
          "浏览器集成所用的访问令牌只在本机生成与校验，不会发送到任何服务器。删除这些文件即可完全清除相关数据。",
      },
      {
        heading: "会联网的场景",
        body:
          "只有三种：① 下载你添加的任务（直连目标服务器）；② 检查更新（访问 GitHub Releases 或 version.json 镜像，仅获取版本号与发布说明）；" +
          "③ 下载更新包（访问 GitHub）。这些请求会带上常规的 HTTP 头，可能被对方服务器按其隐私政策记录，这与任何浏览器访问的行为相同。",
      },
      {
        heading: "不会做的事",
        body:
          "不会收集设备标识、位置、剪贴板或通讯录；不会读取与你添加的任务无关的文件；不会把你的数据出售或共享给第三方。",
      },
      {
        heading: "自定义背景图",
        body:
          "你选择的背景图会被复制一份到应用数据目录用于界面显示，原始文件不会被修改。移除背景后，文件会随后续保存操作清理。",
      },
      {
        heading: "联系方式",
        body: "对隐私有疑问可以联系：lwcat.cn（博客留言）。",
      },
    ],
  },
  en: {
    title: "Privacy Policy",
    updated: UPDATED_EN,
    sections: [
      {
        heading: "In one sentence",
        body: "Felldl is a fully local downloader. There are no accounts, no analytics, and your download history, file contents or browsing information are never uploaded.",
      },
      {
        heading: "Data kept on your device",
        body:
          "Task list, progress and settings live in your app data directory; downloaded files go to the folder you choose. " +
          "The browser-integration token is generated and verified locally and is never sent anywhere. Delete those files to erase the data completely.",
      },
      {
        heading: "When it goes online",
        body:
          "Only three cases: (1) downloading the tasks you add (directly to the target server); (2) checking for updates (GitHub Releases or a version.json mirror, fetching only the version and release notes); " +
          "(3) downloading an update package (GitHub). Those requests carry ordinary HTTP headers and may be logged by the other side under its own privacy policy — exactly like any browser visit.",
      },
      {
        heading: "What it never does",
        body:
          "No device identifiers, location, clipboard or contacts. No reading files unrelated to the tasks you added. No selling or sharing your data with third parties.",
      },
      {
        heading: "Custom background image",
        body:
          "The image you pick is copied into the app data directory for display; your original file is never modified. After removing it, the copy is cleaned up on the next save.",
      },
      {
        heading: "Contact",
        body: "Questions about privacy: lwcat.cn (blog comments).",
      },
    ],
  },
};

export const TERMS: Record<Lang, LegalDoc> = {
  zh: {
    title: "用户协议",
    updated: UPDATED_ZH,
    sections: [
      {
        heading: "软件性质",
        body:
          "Felldl 是一款开源的本地多线程下载工具，按「现状」提供，不承诺适用于任何特定用途。你可以免费使用、复制与修改。",
      },
      {
        heading: "你的责任",
        body:
          "你只能下载自己有权获取的内容。请遵守目标网站的服务条款、robots 协议与所在地法律；" +
          "因下载受版权保护的内容、绕过付费墙或违反站点规则所产生的后果，由你自行承担。",
      },
      {
        heading: "使用风险",
        body:
          "下载速度、成功率取决于网络与目标服务器，本软件不对下载结果作保证。" +
          "「下载完成后关机/睡眠」等自动化动作请自行确认不会造成数据丢失。",
      },
      {
        heading: "更新",
        body:
          "应用内更新会从 GitHub 下载新的可执行文件并替换当前版本。更新包与发布页一致，但请自行确认来源网络可信；更新前建议保存正在进行的任务。",
      },
      {
        heading: "免责声明",
        body:
          "在法律允许的范围内，作者不对因使用或无法使用本软件造成的任何直接或间接损失承担责任。",
      },
      {
        heading: "协议变更",
        body: "协议随版本更新调整，最新版本随应用一同发布。继续使用即视为接受更新后的条款。",
      },
    ],
  },
  en: {
    title: "Terms of Service",
    updated: UPDATED_EN,
    sections: [
      {
        heading: "What this software is",
        body:
          "Felldl is an open-source local multi-threaded downloader, provided \"as is\" without warranty of fitness for a particular purpose. You may use, copy and modify it for free.",
      },
      {
        heading: "Your responsibility",
        body:
          "Only download content you have the right to obtain. Respect each site's terms of service, robots policy and your local law; " +
          "you are responsible for the consequences of downloading copyrighted material, bypassing paywalls or breaking site rules.",
      },
      {
        heading: "Risks of use",
        body:
          "Speed and success rate depend on your network and the target server; no guarantee is made about download results. " +
          "Automated actions such as shutting down or sleeping after completion are your own call — make sure nothing unsaved is lost.",
      },
      {
        heading: "Updates",
        body:
          "In-app update downloads a new executable from GitHub and replaces the current version. The package matches the release page, but you should still trust your network; save running tasks before updating.",
      },
      {
        heading: "Disclaimer",
        body:
          "To the extent permitted by law, the author is not liable for any direct or indirect loss arising from the use of, or inability to use, this software.",
      },
      {
        heading: "Changes",
        body:
          "These terms may change between versions; the latest version ships with the app. Continued use means you accept the updated terms.",
      },
    ],
  },
};
