import { invoke } from "@tauri-apps/api/core";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { t } from "../i18n";
import { desktop, mac, win } from "../model/state";
import type { CloseBehavior } from "../model/types";

export const DEFAULT_WIN = { w: 430, h: 740 };
const WIN_SIZE_KEY = "fesen_window_size";
const SVG_NS = "http://www.w3.org/2000/svg";
const MAX_PATH = "M1.5 1.5h7v7h-7z";
const RESTORE_PATH = "M3 3V1.5h5.5V7H7M1.5 3H7v5.5H1.5z";

let winSize = { ...DEFAULT_WIN };
let hasCustomWinSize = false;
let applyingWinSize = false;
let resizeSaveTimer: number | undefined;
let winControlsRefreshI18n: (() => void) | null = null;

function captionSvg(pathData: string, crisp = true): { svg: SVGSVGElement; path: SVGPathElement } {
  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("viewBox", "0 0 10 10");
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "1");
  svg.setAttribute("aria-hidden", "true");
  const path = document.createElementNS(SVG_NS, "path");
  path.setAttribute("d", pathData);
  if (crisp) path.setAttribute("shape-rendering", "crispEdges");
  svg.append(path);
  return { svg, path };
}

export function syncWindowsTheme(mode: string) {
  if (!win) return;
  const next = mode === "light" || mode === "dark" ? mode : null;
  void getCurrentWindow().setTheme(next).catch(() => {});
}

export function syncWindowControlsI18n() {
  winControlsRefreshI18n?.();
}

export function initWindowsChrome(initialTheme: string) {
  if (!win) return;
  const topbar = document.querySelector<HTMLElement>(".topbar");
  if (!topbar || topbar.querySelector(".win-controls")) return;

  const appWin = getCurrentWindow();
  let maximized = false;

  const minBtn = document.createElement("button");
  minBtn.type = "button";
  minBtn.className = "win-btn win-min";
  minBtn.append(captionSvg("M1 5h8").svg);
  minBtn.addEventListener("click", () => { void appWin.minimize().catch(() => {}); });

  const maxBtn = document.createElement("button");
  maxBtn.type = "button";
  maxBtn.className = "win-btn win-max";
  const maxIcon = captionSvg(MAX_PATH);
  maxBtn.append(maxIcon.svg);
  maxBtn.addEventListener("click", () => { void appWin.toggleMaximize().catch(() => {}); });

  const closeBtn = document.createElement("button");
  closeBtn.type = "button";
  closeBtn.className = "win-btn win-close";
  closeBtn.append(captionSvg("M1.5 1.5l7 7M8.5 1.5l-7 7", false).svg);
  closeBtn.addEventListener("click", () => { void appWin.close().catch(() => {}); });

  winControlsRefreshI18n = () => {
    const minTip = t("win_minimize");
    minBtn.title = minTip;
    minBtn.setAttribute("aria-label", minTip);
    const maxTip = t(maximized ? "win_restore" : "win_maximize");
    maxBtn.title = maxTip;
    maxBtn.setAttribute("aria-label", maxTip);
    const closeTip = t("close");
    closeBtn.title = closeTip;
    closeBtn.setAttribute("aria-label", closeTip);
  };
  winControlsRefreshI18n();

  const controls = document.createElement("div");
  controls.className = "win-controls";
  controls.append(minBtn, maxBtn, closeBtn);
  topbar.append(controls);

  let lastSnapKey = "";
  let snapRaf = 0;
  const syncSnapRect = () => {
    snapRaf = 0;
    const r = maxBtn.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    let visible = !document.hidden && r.width > 0 && r.height > 0 && !document.querySelector("dialog[open]");
    if (visible) {
      const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
      visible = !!hit && (hit === maxBtn || maxBtn.contains(hit));
    }
    const x = Math.round(r.left * dpr);
    const y = Math.round(r.top * dpr);
    const width = Math.max(0, Math.round(r.right * dpr) - x);
    const height = Math.max(0, Math.round(r.bottom * dpr) - y);
    const key = `${visible}:${maximized}:${x}:${y}:${width}:${height}`;
    if (key === lastSnapKey) return;
    lastSnapKey = key;
    void invoke("sync_snap_button", { rect: { visible, x, y, width, height } }).catch(() => {});
  };
  const scheduleSnapSync = () => {
    if (!snapRaf) snapRaf = window.requestAnimationFrame(syncSnapRect);
  };

  const refreshMaximized = async () => {
    try {
      const next = await appWin.isMaximized();
      if (next !== maximized) {
        maximized = next;
        maxIcon.path.setAttribute("d", maximized ? RESTORE_PATH : MAX_PATH);
        winControlsRefreshI18n?.();
      }
      scheduleSnapSync();
    } catch {}
  };

  document.documentElement.classList.toggle("is-unfocused", !document.hasFocus());
  void appWin.onFocusChanged(({ payload }) => {
    document.documentElement.classList.toggle("is-unfocused", !payload);
    scheduleSnapSync();
  }).catch(() => {});

  void listen<{ hover: boolean; active: boolean }>("win-max-state", ({ payload }) => {
    maxBtn.classList.toggle("is-hover", !!payload?.hover);
    maxBtn.classList.toggle("is-active", !!payload?.active);
  }).catch(() => {});

  if (typeof ResizeObserver !== "undefined") {
    const ro = new ResizeObserver(() => scheduleSnapSync());
    ro.observe(topbar);
    ro.observe(maxBtn);
  }
  const dialogObserver = new MutationObserver(() => scheduleSnapSync());
  document.querySelectorAll("dialog").forEach((dlg) => {
    dialogObserver.observe(dlg, { attributes: true, attributeFilter: ["open"] });
  });
  const bodyObserver = new MutationObserver(() => scheduleSnapSync());
  bodyObserver.observe(document.body, { childList: true });
  window.addEventListener("resize", scheduleSnapSync);
  document.addEventListener("visibilitychange", scheduleSnapSync);

  void refreshMaximized();
  scheduleSnapSync();
  void appWin.onResized(() => { void refreshMaximized(); }).catch(() => {});
  syncWindowsTheme(initialTheme);
}

export function currentWinSize() {
  return winSize;
}

export function isWinSizeDefault() {
  return !hasCustomWinSize && isDefaultWinSize(winSize);
}

function readSavedWinSize(): { w: number; h: number } | null {
  try {
    const raw = localStorage.getItem(WIN_SIZE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw);
    const w = Number(parsed?.w);
    const h = Number(parsed?.h);
    if (Number.isFinite(w) && Number.isFinite(h) && w >= 360 && h >= 540 && w <= 3840 && h <= 2400) {
      return { w: Math.round(w), h: Math.round(h) };
    }
  } catch {}
  return null;
}

function isDefaultWinSize(s: { w: number; h: number }): boolean {
  return Math.abs(s.w - DEFAULT_WIN.w) <= 2 && Math.abs(s.h - DEFAULT_WIN.h) <= 2;
}

export function refreshWindowSizeRow(settingsDialog: HTMLDialogElement) {
  const val = settingsDialog.querySelector<HTMLElement>("[data-win-size-val]");
  const btn = settingsDialog.querySelector<HTMLButtonElement>("[data-win-size-reset]");
  if (val) val.textContent = `${winSize.w} × ${winSize.h}`;
  if (btn) btn.disabled = !hasCustomWinSize && isDefaultWinSize(winSize);
}

export async function initDesktopWindowSize(settingsDialog: HTMLDialogElement) {
  if (!desktop) return;
  const appWin = getCurrentWindow();
  const saved = readSavedWinSize();
  if (saved) {
    winSize = saved;
    hasCustomWinSize = !isDefaultWinSize(saved);
    if (hasCustomWinSize) {
      applyingWinSize = true;
      try { await appWin.setSize(new LogicalSize(saved.w, saved.h)); } catch {}
      window.setTimeout(() => { applyingWinSize = false; }, 300);
    }
  } else {
    try {
      const [phys, factor] = await Promise.all([appWin.innerSize(), appWin.scaleFactor()]);
      if (phys.width > 0 && phys.height > 0 && factor > 0) {
        winSize = { w: Math.round(phys.width / factor), h: Math.round(phys.height / factor) };
      }
    } catch {}
  }
  try {
    await appWin.onResized(({ payload }) => {
      if (applyingWinSize || payload.width <= 0 || payload.height <= 0) return;
      window.clearTimeout(resizeSaveTimer);
      resizeSaveTimer = window.setTimeout(async () => {
        try {
          if (await appWin.isMaximized().catch(() => false)) return;
          const factor = await appWin.scaleFactor();
          if (factor <= 0) return;
          const w = Math.round(payload.width / factor);
          const h = Math.round(payload.height / factor);
          if (w < 360 || h < 540) return;
          winSize = { w, h };
          if (isDefaultWinSize(winSize)) {
            hasCustomWinSize = false;
            try { localStorage.removeItem(WIN_SIZE_KEY); } catch {}
          } else {
            hasCustomWinSize = true;
            try { localStorage.setItem(WIN_SIZE_KEY, JSON.stringify(winSize)); } catch {}
          }
          refreshWindowSizeRow(settingsDialog);
        } catch {}
      }, 220);
    });
  } catch {}
}

export async function resetDesktopWindowSize(settingsDialog: HTMLDialogElement) {
  if (!desktop) return;
  applyingWinSize = true;
  window.clearTimeout(resizeSaveTimer);
  winSize = { ...DEFAULT_WIN };
  hasCustomWinSize = false;
  try { localStorage.removeItem(WIN_SIZE_KEY); } catch {}
  refreshWindowSizeRow(settingsDialog);
  try {
    const appWin = getCurrentWindow();
    if (await appWin.isMaximized().catch(() => false)) await appWin.unmaximize();
    await appWin.setSize(new LogicalSize(DEFAULT_WIN.w, DEFAULT_WIN.h));
    await appWin.center();
  } catch {}
  window.setTimeout(() => { applyingWinSize = false; }, 300);
}

export function getCloseBehavior(): CloseBehavior {
  try {
    const v = localStorage.getItem("fesen_close_behavior");
    if (v === "quit" || v === "tray") return v;
  } catch {}
  return "tray";
}

export async function syncCloseBehavior(behavior: CloseBehavior): Promise<boolean> {
  try { localStorage.setItem("fesen_close_behavior", behavior); } catch {}
  if (mac) {
    try {
      const saved = await invoke<boolean>("menu_bar_setting", { enabled: behavior === "tray" });
      return saved === (behavior === "tray");
    } catch {
      return false;
    }
  }
  return true;
}

if (mac) void syncCloseBehavior(getCloseBehavior());
