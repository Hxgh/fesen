import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { failure, isZh, t } from "../i18n";
import { createTrigram } from "../../../../packages/ui/icons.js";
import { createScale, createTheme } from "../../../../packages/ui/theme.js";
import { mac, persistViewMode, state, toggleCollapsedKey } from "../model/state";
import type { BrandState, Reply, Request, Status, ViewMode } from "../model/types";
import { button, dialogHeader, el, notify } from "../ui/dom";
import { openDiagnostics } from "../dialogs/diag-dialog";
import { syncWindowControlsI18n, syncWindowsTheme } from "../ui/window";

const rawTheme = createTheme("fesen_theme");
export const theme = {
  get: rawTheme.get,
  apply: rawTheme.apply,
  set(next: string) {
    rawTheme.set(next);
    syncWindowsTheme(next);
  },
};
export const scale = createScale("fesen_scale");

export const content = document.querySelector<HTMLDivElement>("#content")!;
export const actions = document.querySelector<HTMLDivElement>("#actions")!;
export const brandBadge = document.querySelector<HTMLSpanElement>("#brand-badge")!;
const brandMarkHost = document.querySelector<HTMLSpanElement>("#brand-mark")!;
export const brandNameEl = document.querySelector<HTMLSpanElement>("#brand-name")!;
export const errorLine = document.querySelector<HTMLParagraphElement>("#error")!;
export const editor = document.querySelector<HTMLDialogElement>("#editor")!;
export const settingsDialog = document.querySelector<HTMLDialogElement>("#settings-dialog")!;

export const topMark = createTrigram("idle");
brandMarkHost.replaceWith(topMark.el);

void getVersion().then((v) => {
  state.appVersion = v;
  const versionNode = settingsDialog.querySelector<HTMLSpanElement>(".settings-version");
  if (versionNode) versionNode.textContent = `${t("brand")} v${v}`;
}).catch(() => {});

settingsDialog.addEventListener("click", (event) => {
  if (event.target === settingsDialog) settingsDialog.close();
});
editor.addEventListener("click", (event) => {
  if (event.target === editor && !state.busy) editor.close();
});

let renderHook: (force?: boolean) => void = () => {};

export function registerRenderer(fn: (force?: boolean) => void) {
  renderHook = fn;
}

export function render(force = false) {
  renderHook(force);
}

export function toggleCollapsed(key: string) {
  toggleCollapsedKey(key);
  render(true);
}

export function setViewMode(next: ViewMode) {
  if (state.viewMode === next) return;
  persistViewMode(next);
  render(true);
}

function computeBrandState(): BrandState {
  if (state.busy && !state.status?.online) return "connecting";
  if (!state.status || !state.status.member) return "idle";
  if (state.status.running && !state.status.online) return "connecting";
  if (!state.status.online) return "idle";
  return "active";
}

let lastWinTitle = "";
let lastBrandName = "";
let lastBrandTip = "";
let cachedServerUrl = "";
let cachedServerHost = "";

export function serverHost(server: string | undefined): string {
  if (!server) return "";
  if (server === cachedServerUrl) return cachedServerHost;
  cachedServerUrl = server;
  try {
    cachedServerHost = new URL(server).host;
  } catch {
    cachedServerHost = "";
  }
  return cachedServerHost;
}

export function syncBrand(instant = false) {
  const name = t("brand");
  const isAdminView = state.tab === "admin" && !!state.status?.member && !!state.status?.admin_ready;
  const title = isAdminView ? t("admin_title") : name;
  if (title !== lastWinTitle) {
    lastWinTitle = title;
    document.title = title;
    void getCurrentWindow().setTitle(title).catch(() => {});
  }
  if (name !== lastBrandName) {
    lastBrandName = name;
    brandNameEl.textContent = name;
  }
  syncWindowControlsI18n();
  const bState = computeBrandState();
  topMark.setState(bState, instant);
  const hostText = state.status?.member ? serverHost(state.status.server) : "";
  const statusTip = bState === "active"
    ? (isZh() ? "已连接" : "Connected")
    : bState === "connecting"
      ? (isZh() ? "连接中" : "Connecting")
      : (isZh() ? "未连接" : "Disconnected");
  const tip = [name, statusTip, hostText].filter(Boolean).join(" · ");
  if (tip !== lastBrandTip) {
    lastBrandTip = tip;
    brandBadge.title = tip;
  }
}

syncBrand(true);

const flowErrors = new Set([
  "offline",
  "direct_failed",
  "target_unavailable",
  "target_timeout",
  "http_invalid",
  "protocol_mismatch",
]);

export function showError() {
  const last = state.status?.error;
  const code = state.message === "extension_setup"
    ? ""
    : state.message || (state.status?.online && last && flowErrors.has(last) ? "" : last);
  const inlineError = content.querySelector<HTMLParagraphElement>(".signin [role=alert]");
  if (inlineError) {
    inlineError.hidden = !code;
    inlineError.textContent = code ? failure(code) : "";
    errorLine.hidden = true;
    errorLine.replaceChildren();
  } else {
    errorLine.hidden = !code;
    if (code) {
      const msg = el("span", "error-line-msg", failure(code));
      const logBtn = button(t("diag_open_logs_btn"), () => openDiagnostics("logs"), "button ghost error-log-btn");
      errorLine.replaceChildren(msg, logBtn);
    } else {
      errorLine.replaceChildren();
    }
  }
  const dialogError = editor.querySelector<HTMLParagraphElement>("[role=alert]");
  if (dialogError) {
    dialogError.hidden = !state.message;
    dialogError.textContent = state.message ? failure(state.message) : "";
  }
}

export function syncSigninSubmit() {
  const btn = content.querySelector<HTMLButtonElement>('.signin button[type="submit"]');
  if (!btn) return;
  btn.disabled = state.busy;
  if (btn.dataset.loginBtn === "1") btn.textContent = t(state.busy ? "logging_in" : "login");
}

export function form(action: (values: FormData) => void, ...rows: HTMLElement[]) {
  const view = el("form", "stack", ...rows);
  view.addEventListener("submit", (event) => {
    event.preventDefault();
    if (state.busy) return;
    const data = new FormData(view);
    const password = view.querySelector<HTMLInputElement>('[type="password"]');
    if (password) password.value = "";
    action(data);
  });
  return view;
}

export function showDialog(
  title: string,
  bodyNodes: HTMLElement[],
  footerNodes: HTMLElement[] = [],
  coreErrors = true,
) {
  editor.classList.remove("dialog-wide");
  const header = dialogHeader(title, () => { if (!state.busy) editor.close(); });
  const body = el("div", "dialog-body", ...bodyNodes);
  if (coreErrors) {
    const warning = el("p", "feedback error");
    warning.setAttribute("role", "alert");
    warning.hidden = true;
    body.append(warning);
  }
  editor.tabIndex = -1;
  editor.replaceChildren(header, body, ...(footerNodes.length ? [el("div", "dialog-footer", ...footerNodes)] : []));
  if (!editor.open) {
    editor.showModal();
    const firstInput = body.querySelector<HTMLElement>("input:not(:disabled), select:not(:disabled)");
    if (firstInput) firstInput.focus();
    else editor.focus();
  }
}

export function edit(
  title: string,
  save: HTMLButtonElement,
  handler: (data: FormData) => void,
  ...inputs: HTMLElement[]
) {
  editor.classList.remove("dialog-wide");
  const header = dialogHeader(title, () => { if (!state.busy) editor.close(); });
  const warning = el("p", "feedback error");
  warning.setAttribute("role", "alert");
  warning.hidden = true;
  const body = el("div", "dialog-body", ...inputs, warning);
  const footer = el("div", "dialog-footer", button(t("cancel"), () => { if (!state.busy) editor.close(); }, "button"), save);
  const view = form(handler, header, body, footer);
  view.className = "";
  editor.tabIndex = -1;
  editor.replaceChildren(view);
  if (!editor.open) {
    editor.showModal();
    const firstInput = body.querySelector<HTMLElement>("input:not(:disabled), select:not(:disabled)");
    if (firstInput) firstInput.focus();
    else editor.focus();
  }
  return footer;
}

export async function request(body: Request, launch: boolean): Promise<Status> {
  const reply = await invoke<Reply>("core_command", { request: body, launch });
  if (!reply.data) throw reply.error ?? "internal";
  return reply.data;
}

export function applyJoinLink() {
  if (!state.status || state.busy || !state.pendingJoin) return;
  const address = state.pendingJoin;
  state.pendingJoin = undefined;
  void act({ op: "join", address }, () => { state.joining = false; });
}

export async function act(body: Request, done?: () => void) {
  if (state.busy) return;
  state.busy = true;
  state.message = "";
  showError();
  syncSigninSubmit();
  syncBrand();
  try {
    state.status = await request(body, true);
    done?.();
    render(true);
  } catch (error) {
    state.message = body.op === "login" && error === "unauthorized"
      ? "login_rejected"
      : typeof error === "string" ? error : "internal";
    if (mac && !state.status) render(true);
    else showError();
  } finally {
    state.busy = false;
    syncSigninSubmit();
    syncBrand();
    applyJoinLink();
  }
}

export async function switchForwardPort(
  service: string,
  target_ip: string,
  target_port: number,
  local_port: number,
) {
  if (state.busy) return;
  state.busy = true;
  state.message = "";
  showError();
  try {
    state.status = await request({ op: "set_local_forward", service, target_ip, target_port, local_port }, false);
    render(true);
    if (local_port > 0) notify(`127.0.0.1:${local_port}`);
  } catch (error) {
    state.message = typeof error === "string" ? error : "internal";
    showError();
  } finally {
    state.busy = false;
  }
}
