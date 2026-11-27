/** 线性图标集（stroke 风格，统一 1.9 线宽） */
import type { JSX } from "react";

const S = {
  width: 16,
  height: 16,
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 1.9,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
};

type P = { size?: number; className?: string };

const mk = (children: JSX.Element, extra?: Partial<typeof S>) => {
  const C = ({ size, className }: P) => (
    <svg
      {...S}
      {...extra}
      width={size ?? S.width}
      height={size ?? S.height}
      className={className}
      aria-hidden="true"
    >
      {children}
    </svg>
  );
  return C;
};

export const IconBolt = mk(<path d="M13 2 4.5 13.5H11l-1 8.5L19.5 10.5H13l0-8.5Z" />);
export const IconDownload = mk(
  <>
    <path d="M12 3v12" />
    <path d="m7 11 5 5 5-5" />
    <path d="M4 20h16" />
  </>
);
export const IconPause = mk(
  <>
    <rect x="7" y="4" width="3.5" height="16" rx="1.2" fill="currentColor" stroke="none" />
    <rect x="13.5" y="4" width="3.5" height="16" rx="1.2" fill="currentColor" stroke="none" />
  </>
);
export const IconPlay = mk(
  <path d="M7 4.5v15l12-7.5-12-7.5Z" fill="currentColor" stroke="none" />
);
export const IconTrash = mk(
  <>
    <path d="M4 7h16" />
    <path d="M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2" />
    <path d="M6 7v12a2 2 0 0 0 2 2h8a2 2 0 0 0 2-2V7" />
    <path d="M10 11v6M14 11v6" />
  </>
);
export const IconFolder = mk(
  <path d="M3 7.5A2 2 0 0 1 5 5.5h3.6a2 2 0 0 1 1.5.7l1 1.3H19a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7.5Z" />
);
export const IconSettings = mk(
  <>
    <circle cx="12" cy="12" r="3.2" />
    <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.6 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.6a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9v0a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1Z" />
  </>
);
export const IconPlus = mk(<path d="M12 5v14M5 12h14" />);
export const IconX = mk(<path d="M18 6 6 18M6 6l12 12" />);
export const IconCheck = mk(<path d="m4.5 12.5 5 5L19.5 7" />);
export const IconClock = mk(
  <>
    <circle cx="12" cy="12" r="8.5" />
    <path d="M12 7.5V12l3 2" />
  </>
);
export const IconLayers = mk(
  <>
    <path d="m12 3 8.5 4.5L12 12 3.5 7.5 12 3Z" />
    <path d="m3.5 12.5 8.5 4.5 8.5-4.5" />
    <path d="m3.5 17 8.5 4.5 8.5-4.5" />
  </>
);
export const IconGlobe = mk(
  <>
    <circle cx="12" cy="12" r="8.5" />
    <path d="M3.5 12h17" />
    <path d="M12 3.5a14 14 0 0 1 0 17 14 14 0 0 1 0-17Z" />
  </>
);
export const IconGauge = mk(
  <>
    <path d="M4 18a8.5 8.5 0 1 1 16 0" />
    <path d="m12 14 3.5-3.5" />
    <circle cx="12" cy="14" r="1.6" fill="currentColor" stroke="none" />
  </>
);
export const IconFile = mk(
  <>
    <path d="M13 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V9l-6-6Z" />
    <path d="M13 3v6h6" />
  </>
);
export const IconVideo = mk(
  <>
    <rect x="2.5" y="6" width="13" height="12" rx="2.2" />
    <path d="m16 11 5-3v8l-5-3v-2Z" />
  </>
);
export const IconImage = mk(
  <>
    <rect x="3" y="4.5" width="18" height="15" rx="2.2" />
    <circle cx="8.5" cy="10" r="1.6" />
    <path d="m3.5 17.5 5-4.5 4.5 4 3-2.5 4.5 4" />
  </>
);
export const IconMusic = mk(
  <>
    <path d="M9 18V5.5l11-2V16" />
    <circle cx="6.5" cy="18" r="2.6" />
    <circle cx="17.5" cy="16" r="2.6" />
  </>
);
export const IconArchive = mk(
  <>
    <rect x="3" y="4" width="18" height="5" rx="1.6" />
    <path d="M5 9v9a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V9" />
    <path d="M10.5 13h3" />
  </>
);
export const IconCode = mk(<path d="m8.5 8.5-4 3.5 4 3.5M15.5 8.5l4 3.5-4 3.5M13.5 5l-3 14" />);
export const IconMenu = mk(<path d="M4 7h16M4 12h16M4 17h16" />);
/** 多选入口 / 复选框（未选中时只是一圈圆角方框） */
export const IconCheckSquare = mk(
  <>
    <rect x="3.6" y="3.6" width="16.8" height="16.8" rx="4.4" />
    <path d="m8.3 12.2 2.6 2.6 4.8-5.2" />
  </>
);
export const IconCheckCircle = mk(
  <>
    <circle cx="12" cy="12" r="8.6" />
    <path d="m8.2 12.2 2.6 2.6 4.8-5.2" />
  </>
);
export const IconSun = mk(
  <>
    <circle cx="12" cy="12" r="4" />
    <path d="M12 2.5v2M12 19.5v2M2.5 12h2M19.5 12h2M5.2 5.2l1.4 1.4M17.4 17.4l1.4 1.4M18.8 5.2l-1.4 1.4M6.6 17.4l-1.4 1.4" />
  </>
);
export const IconMoon = mk(
  <path d="M20 14.5A8.5 8.5 0 1 1 9.5 4a6.8 6.8 0 0 0 10.5 10.5Z" />
);
export const IconMonitor = mk(
  <>
    <rect x="2.5" y="4" width="19" height="12.5" rx="2" />
    <path d="M9 20.5h6M12 16.5v4" />
  </>
);
export const IconLink = mk(
  <>
    <path d="M9.5 14.5a4 4 0 0 1 0-5.66l2.83-2.83a4 4 0 1 1 5.66 5.66l-1.4 1.4" />
    <path d="M14.5 9.5a4 4 0 0 1 0 5.66l-2.83 2.83a4 4 0 1 1-5.66-5.66l1.4-1.4" />
  </>
);
export const IconExternal = mk(
  <>
    <path d="M14 4h6v6" />
    <path d="M20 4 10.5 13.5" />
    <path d="M19 14v5a1.5 1.5 0 0 1-1.5 1.5H5.5A1.5 1.5 0 0 1 4 19V6.5A1.5 1.5 0 0 1 5.5 5h5" />
  </>
);
export const IconChevronDown = mk(<path d="m6 9.5 6 6 6-6" />);
export const IconRefresh = mk(
  <>
    <path d="M20.5 12a8.5 8.5 0 1 1-2.6-6.1" />
    <path d="M20.5 4v5h-5" />
  </>
);
export const IconShield = mk(
  <>
    <path d="M12 3 5 6v6c0 4.5 3 7.5 7 9 4-1.5 7-4.5 7-9V6l-7-3Z" />
    <path d="m9 12 2 2 4-4" />
  </>
);
export const IconSliders = mk(
  <>
    <path d="M4 8h10M18 8h2M4 16h4M12 16h8" />
    <circle cx="16" cy="8" r="2.2" />
    <circle cx="10" cy="16" r="2.2" />
  </>
);
export const IconCopy = mk(
  <>
    <rect x="9" y="9" width="11" height="11" rx="2" />
    <path d="M15 9V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h3" />
  </>
);
export const IconZap = mk(<path d="M13 2 4.5 13.5H11l-1 8.5L19.5 10.5H13l0-8.5Z" />);
export const IconArrowLeft = mk(
  <>
    <path d="M19 12H5" />
    <path d="m11 18-6-6 6-6" />
  </>
);
