import { invoke } from "@tauri-apps/api/core";
import { onBackButtonPress } from "@tauri-apps/api/app";
import { failure, t } from "./i18n";
import { icon, setIconButton } from "../../../packages/ui/icons.js";

const dialog = document.querySelector<HTMLDialogElement>("#site-dialog")!;
const stage = dialog.querySelector<HTMLElement>("#site-stage")!;
const dot = dialog.querySelector<HTMLElement>("#site-dot")!;
const note = dialog.querySelector<HTMLElement>("#site-result")!;
const address = dialog.querySelector<HTMLElement>("#site-url")!;
const close = dialog.querySelector<HTMLButtonElement>("#site-close")!;
const retry = dialog.querySelector<HTMLButtonElement>("#site-retry")!;
const expand = dialog.querySelector<HTMLButtonElement>("#site-expand")!;

let expanded = false;
let attempt = 0, sequence = Date.now();
let invalidate: (() => boolean) | undefined;

function setBtn(btn: HTMLButtonElement, name: Parameters<typeof icon>[0], label: string) {
  setIconButton(btn, name, label);
}

function syncExpandBtn() {
  dialog.classList.toggle("expanded", expanded);
  setBtn(expand, expanded ? "minimize" : "maximize", t(expanded ? "restore_window" : "expand_window"));
}

setBtn(close, "close", t("close"));
setBtn(retry, "refresh", t("retry"));
syncExpandBtn();

close.onclick = () => dialog.close();
expand.onclick = () => {
  expanded = !expanded;
  syncExpandBtn();
  requestAnimationFrame(move);
};

dialog.tabIndex = -1;
dialog.addEventListener("click", (event) => {
  if (event.target === dialog) dialog.close();
});

const bounds = () => {
  const r = stage.getBoundingClientRect(), s = devicePixelRatio;
  return [r.x, r.y, r.width, r.height].map(n => Math.max(0, Math.round(n * s)));
};
const action = (op: string, id: number, extra = {}) =>
  invoke<string>("site_view", { op, attempt: id, ...extra });
const stop = () => {
  const id = attempt; attempt = 0;
  if (id) void action("close", id).catch(() => {});
};
const move = () => { if (attempt) void action("move", attempt, { bounds: bounds() }).catch(() => {}); };
new ResizeObserver(move).observe(stage);
window.addEventListener("resize", move);
window.visualViewport?.addEventListener("resize", move);
dialog.onclose = () => {
  if (!dialog.open) {
    stop();
    invalidate = undefined;
    if (expanded) { expanded = false; syncExpandBtn(); }
  }
};

function setState(state: "loading" | "ok" | "problem", text: string) {
  dot.setAttribute("data-state", state);
  note.className = state === "problem" ? "site-status problem" : "site-status";
  note.textContent = text;
}

export function checkSiteDialog() {
  if (dialog.open && invalidate && !invalidate()) {
    stop();
    stage.textContent = failure("offline");
    setState("problem", failure("offline"));
    retry.disabled = true;
  }
}

type Target = { id: string; owner: string | null; ip: string; port: number };
export function showSiteDialog(target: Target, access: string, valid: () => boolean,
  report: (result: "visible" | { error: string }) => void, accessPort?: number) {
  stop();
  invalidate = valid;
  retry.disabled = false;
  expanded = false;
  syncExpandBtn();
  const original = `http://${access}:${accessPort ?? target.port}/`;
  address.textContent = original;
  address.title = original;
  dialog.showModal();
  dialog.focus();
  if (/Android/i.test(navigator.userAgent)) {
    const handler = onBackButtonPress(() => { if (dialog.open) dialog.close(); });
    dialog.addEventListener("close", () => { void handler.then(h => h.unregister()).catch(() => {}); }, { once: true });
    void handler.catch(() => {});
  }
  async function load() {
    stop();
    const id = ++sequence; attempt = id;
    const current = () => dialog.open && attempt === id && valid();
    setState("loading", t("page_loading"));
    stage.textContent = "";
    retry.classList.add("spinning");
    try {
      const url = await invoke<string>("open_site", { ...target, attempt: id, bounds: bounds() });
      if (!current()) return;
      address.textContent = url;
      address.title = url;
      move();
      const until = Date.now() + 16000;
      while (current() && Date.now() < until) {
        const outcome = await action("inspect", id, { url });
        if (!current()) return;
        if (outcome === "visible") {
          setState("ok", t("page_visible"));
          report("visible");
          return;
        }
        if (outcome !== "loading") throw outcome;
        await new Promise(resolve => setTimeout(resolve, 300));
      }
      if (current()) throw "page_not_rendered";
    } catch (e) {
      if (!current()) return;
      stop();
      if (e === "cancelled" || e === "closed") { dialog.close(); return; }
      const error = typeof e === "string" ? e : "browser_failed";
      stage.textContent = failure(error);
      setState("problem", t("unavailable"));
      report({ error });
    } finally {
      retry.classList.remove("spinning");
    }
  }
  retry.onclick = () => void load();
  void load();
}

export function refreshSiteDialogI18n() {
  setBtn(close, "close", t("close"));
  setBtn(retry, "refresh", t("retry"));
  syncExpandBtn();
}
