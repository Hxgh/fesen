import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { state } from "../model/state";
import type { UpdateState } from "../model/types";

type Listener = (next: UpdateState, prev: UpdateState) => void;
const listeners = new Set<Listener>();

let booted = false;
let timer: number | undefined;
let intervalId: number | undefined;

function emit(next: UpdateState) {
  const prev = state.update;
  state.update = next;
  for (const fn of listeners) fn(next, prev);
}

export function onUpdateState(fn: Listener): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

export async function checkForUpdates(silent = false): Promise<UpdateState> {
  if (!silent && !["checking", "downloading", "paused", "ready", "installing"].includes(state.update.status)) {
    emit({ status: "checking" });
  }
  try {
    const next = await invoke<UpdateState>("check_for_update", { silent });
    emit(next);
    return next;
  } catch (err) {
    if (!silent) {
      emit({
        status: "error",
        current: state.appVersion,
        stage: "check",
        message: typeof err === "string" ? err : "update_network",
      });
    }
    return state.update;
  }
}

export async function startUpdateDownload(): Promise<void> {
  try {
    const next = await invoke<UpdateState>("download_update");
    emit(next);
  } catch (err) {
    if (state.update.status !== "error") {
      const cur = state.update;
      emit({
        status: "error",
        current: state.appVersion,
        stage: "download",
        message: typeof err === "string" ? err : "update_network",
        version: "version" in cur ? cur.version : null,
        notes: "notes" in cur ? cur.notes : null,
        pub_date: "pub_date" in cur ? cur.pub_date : null,
        size: "size" in cur ? cur.size : null,
        release_url: "release_url" in cur ? cur.release_url : null,
      });
    }
  }
}

export async function pauseUpdateDownload(): Promise<void> {
  try {
    const next = await invoke<UpdateState>("pause_update_download");
    emit(next);
  } catch {}
}

export async function cancelUpdateDownload(): Promise<void> {
  try {
    const next = await invoke<UpdateState>("cancel_update_download");
    emit(next);
  } catch {}
}

export async function confirmInstallUpdate(): Promise<void> {
  try {
    await invoke("install_update");
  } catch (err) {
    const cur = state.update;
    emit({
      status: "error",
      current: state.appVersion,
      stage: "install",
      message: typeof err === "string" ? err : "update_failed",
      version: "version" in cur ? cur.version : null,
      notes: "notes" in cur ? cur.notes : null,
      pub_date: "pub_date" in cur ? cur.pub_date : null,
      size: "size" in cur ? cur.size : null,
      release_url: "release_url" in cur ? cur.release_url : null,
    });
  }
}

export async function openUpdateReleaseUrl(url: string): Promise<void> {
  try {
    await invoke("open_release_url", { url });
  } catch {}
}

export function startUpdater(): void {
  if (booted) return;
  booted = true;
  void listen<UpdateState>("update://state", ({ payload }) => {
    emit(payload);
  }).catch(() => {});
  void invoke<UpdateState>("update_state")
    .then((snap) => {
      if (snap && snap.status !== "idle") emit(snap);
    })
    .catch(() => {});
  window.clearTimeout(timer);
  timer = window.setTimeout(() => {
    void checkForUpdates(true);
  }, 2000);
  window.clearInterval(intervalId);
  intervalId = window.setInterval(() => {
    void checkForUpdates(true);
  }, 6 * 3600 * 1000);
}
