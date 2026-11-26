import { invoke } from "@tauri-apps/api/core";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { desktop, mac } from "../model/state";
import type { CloseBehavior } from "../model/types";

export const DEFAULT_WIN = { w: 430, h: 740 };
const WIN_SIZE_KEY = "fesen_window_size";

let winSize = { ...DEFAULT_WIN };
let hasCustomWinSize = false;
let applyingWinSize = false;
let resizeSaveTimer: number | undefined;

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
  const win = getCurrentWindow();
  const saved = readSavedWinSize();
  if (saved) {
    winSize = saved;
    hasCustomWinSize = !isDefaultWinSize(saved);
    if (hasCustomWinSize) {
      applyingWinSize = true;
      try { await win.setSize(new LogicalSize(saved.w, saved.h)); } catch {}
      window.setTimeout(() => { applyingWinSize = false; }, 300);
    }
  } else {
    try {
      const [phys, factor] = await Promise.all([win.innerSize(), win.scaleFactor()]);
      if (phys.width > 0 && phys.height > 0 && factor > 0) {
        winSize = { w: Math.round(phys.width / factor), h: Math.round(phys.height / factor) };
      }
    } catch {}
  }
  try {
    await win.onResized(({ payload }) => {
      if (applyingWinSize || payload.width <= 0 || payload.height <= 0) return;
      window.clearTimeout(resizeSaveTimer);
      resizeSaveTimer = window.setTimeout(async () => {
        try {
          const factor = await win.scaleFactor();
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
    const win = getCurrentWindow();
    await win.setSize(new LogicalSize(DEFAULT_WIN.w, DEFAULT_WIN.h));
    await win.center();
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
