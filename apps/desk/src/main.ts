import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrent, onOpenUrl } from "@tauri-apps/plugin-deep-link";
import { failure, i18n, isZh, t } from "./i18n";
import { checkSiteDialog, refreshSiteDialogI18n, showSiteDialog } from "./site-dialog";
import { createTrigram, icon } from "../../../packages/ui/icons.js";
import { createTheme } from "../../../packages/ui/theme.js";
import { closePopoverMenu, openPopoverMenu } from "../../../packages/ui/popover.js";
import { openSettingsModal } from "../../../packages/ui/settings.js";
import { createAdminWorkspace } from "../../../packages/ui/admin-view.js";
import {
  effectiveKind as rawEffectiveKind,
  formatEndpoint,
  formatRanges,
  groupByHost,
  meaningfulHostName,
  osLabel,
  resolveHostOs,
  singleIp,
  singlePort,
  singleSpan,
} from "../../../packages/ui/host-card.js";
import "./style.css";

type Span<T> = { start: T; end: T };
type ServiceKind = "tcp" | "http" | "https" | "ssh" | "rdp" | "smb";
type HostOs = "linux" | "windows";
type Share = {
  id: string;
  name: string;
  http?: boolean;
  kind?: ServiceKind;
  host_os?: HostOs;
  rules: { ips: Span<string>[]; ports: Span<number>[] };
};
type Offering = Share & { owner: string; member: string; device?: string };
type Label = { service: string; remark: string; alias: string | null; ip: string | null };
type Mapping = { original: Span<string>; virtual_ip: Span<string> };
type ProbeOutcome = {
  kind: ServiceKind;
  rtt_ms: number;
  http_status?: number;
  host_os?: HostOs;
  detail?: string;
};
type HostPortProbe = {
  port: number;
  open: boolean;
  extra: boolean;
  kind: ServiceKind;
  recommended: boolean;
  rtt_ms?: number;
  http_status?: number;
  detail?: string;
  os_hint?: HostOs;
};
type HostProbe = {
  ip: string;
  host_os?: HostOs;
  ports: HostPortProbe[];
  duration_ms: number;
};
type Status = {
  server: string;
  member: string | null;
  online: boolean;
  running: boolean;
  background: boolean;
  system_routing?: boolean;
  is_admin?: boolean;
  admin_ready?: boolean;
  shares: Share[];
  active: string[];
  active_flows?: number;
  services: Offering[];
  labels: Label[];
  virtual_ips: Mapping[];
  aliases: Record<string, string>;
  error: string | null;
  http_status?: number;
  probe?: ProbeOutcome;
  host_probe?: HostProbe;
  admin_data?: unknown;
};
type Request = Record<string, unknown> & { op: string };
type Reply = { data?: Status; error?: string };
type BrandState = "idle" | "connecting" | "active";
type IconName = Parameters<typeof icon>[0];
type MenuEntry =
  | { sep: true }
  | { icon: IconName; label: string; danger?: boolean; onSelect: () => void };

const effectiveKind = (s: Share): ServiceKind => rawEffectiveKind(s) as ServiceKind;
const mac = /Macintosh/.test(navigator.userAgent);
const desktop = !/Android/i.test(navigator.userAgent);
if (desktop) document.documentElement.classList.add("os-desktop");
if (mac) document.documentElement.classList.add("os-macos");
const theme = createTheme("fesen_theme");

const DEFAULT_WIN = { w: 430, h: 740 };
const WIN_SIZE_KEY = "fesen_window_size";
let winSize = { ...DEFAULT_WIN };
let hasCustomWinSize = false;
let applyingWinSize = false;
let resizeSaveTimer: number | undefined;

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

function refreshWindowSizeRow() {
  const val = settingsDialog.querySelector<HTMLElement>("[data-win-size-val]");
  const btn = settingsDialog.querySelector<HTMLButtonElement>("[data-win-size-reset]");
  if (val) val.textContent = `${winSize.w} × ${winSize.h}`;
  if (btn) btn.disabled = !hasCustomWinSize && isDefaultWinSize(winSize);
}

async function initDesktopWindowSize() {
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
          refreshWindowSizeRow();
        } catch {}
      }, 220);
    });
  } catch {}
}

async function resetDesktopWindowSize() {
  if (!desktop) return;
  applyingWinSize = true;
  window.clearTimeout(resizeSaveTimer);
  winSize = { ...DEFAULT_WIN };
  hasCustomWinSize = false;
  try { localStorage.removeItem(WIN_SIZE_KEY); } catch {}
  refreshWindowSizeRow();
  try {
    const win = getCurrentWindow();
    await win.setSize(new LogicalSize(DEFAULT_WIN.w, DEFAULT_WIN.h));
    await win.center();
  } catch {}
  window.setTimeout(() => { applyingWinSize = false; }, 300);
}

type CloseBehavior = "tray" | "quit";

function getCloseBehavior(): CloseBehavior {
  try {
    const v = localStorage.getItem("fesen_close_behavior");
    if (v === "quit" || v === "tray") return v;
  } catch {}
  return "tray";
}

async function syncCloseBehavior(behavior: CloseBehavior): Promise<boolean> {
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

let status: Status | undefined;
let tab: "services" | "admin" = "services";
let tabOwner = "";
let joining = false;
let busy = false;
let polling = false;
let message = "";
let applied = "";
let lastViewKey = "";
let pendingJoin: string | undefined;
let appVersion = "";

type Probe = {
  ip: string;
  port: number;
  access: string;
  kind: ServiceKind;
  result?: {
    error?: string;
    httpStatus?: number;
    rttMs?: number;
    hostOs?: HostOs;
    detail?: string;
    visible?: true;
  };
};
const probes = new Map<string, Probe>();
const serviceKey = (service: Offering) => `${service.owner}:${service.id}`;
const content = document.querySelector<HTMLDivElement>("#content")!;
const actions = document.querySelector<HTMLDivElement>("#actions")!;
const navTabs = document.querySelector<HTMLElement>("#nav-tabs")!;
const brandBadge = document.querySelector<HTMLSpanElement>("#brand-badge")!;
const brandMarkHost = document.querySelector<HTMLSpanElement>("#brand-mark")!;
const brandNameEl = document.querySelector<HTMLSpanElement>("#brand-name")!;
const errorLine = document.querySelector<HTMLParagraphElement>("#error")!;
const editor = document.querySelector<HTMLDialogElement>("#editor")!;
const settingsDialog = document.querySelector<HTMLDialogElement>("#settings-dialog")!;
const topMark = createTrigram("idle");
brandMarkHost.replaceWith(topMark.el);
let brandBooted = false;

void getVersion().then((v) => {
  appVersion = v;
  const versionNode = settingsDialog.querySelector<HTMLSpanElement>(".settings-version");
  if (versionNode) versionNode.textContent = `${t("brand")} v${v}`;
}).catch(() => {});

function computeBrandState(): BrandState {
  if (busy && !status?.online) return "connecting";
  if (!status || !status.member) return "idle";
  if (status.running && !status.online) return "connecting";
  if (!status.online) return "idle";
  return "active";
}

function syncBrand(instant = false) {
  const name = t("brand");
  const title = tab === "admin" ? t("admin_title") : name;
  document.title = title;
  void getCurrentWindow().setTitle(title).catch(() => {});
  brandNameEl.textContent = name;
  const state = computeBrandState();
  topMark.setState(state, instant);
  const hostText = status?.member && status.server ? new URL(status.server).host : "";
  const statusTip = state === "active"
    ? (isZh() ? "已连接" : "Connected")
    : state === "connecting"
      ? (isZh() ? "连接中" : "Connecting")
      : (isZh() ? "未连接" : "Disconnected");
  brandBadge.title = [name, statusTip, hostText].filter(Boolean).join(" · ");
}

syncBrand(true);

settingsDialog.addEventListener("click", (event) => {
  if (event.target === settingsDialog) settingsDialog.close();
});
editor.addEventListener("click", (event) => {
  if (event.target === editor && !busy) editor.close();
});

const toast = el("p", "toast");
toast.setAttribute("role", "status");
document.body.append(toast);
let toastTimer: number | undefined;

function notify(text: string, error = false) {
  window.clearTimeout(toastTimer);
  toast.classList.toggle("problem", error);
  toast.textContent = text;
  toastTimer = window.setTimeout(() => { toast.textContent = ""; }, 3200);
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K, classes = "", ...children: (Node | string)[]
): HTMLElementTagNameMap[K] {
  const element = document.createElement(tag);
  element.className = classes;
  element.append(...children);
  return element;
}

function button(text: string, action: () => void, style = "button") {
  const b = el("button", style, text);
  b.type = "button";
  b.addEventListener("click", action);
  return b;
}

function tool(name: IconName, title: string, action: () => void) {
  const b = button("", action, "button icon-button");
  b.append(icon(name));
  b.title = title;
  b.setAttribute("aria-label", title);
  return b;
}

function empty(text: string, ...controls: HTMLElement[]) {
  return el("div", "empty", el("p", "", text), ...(controls.length ? [el("div", "empty-actions", ...controls)] : []));
}

function field(title: string, name: string, value = "", options: Partial<HTMLInputElement> = {}) {
  const input = el("input");
  Object.assign(input, { name, value, maxLength: 256, spellcheck: false }, options);
  return el("label", "", title, input);
}

function form(action: (values: FormData) => void, ...rows: HTMLElement[]) {
  const view = el("form", "stack", ...rows);
  view.addEventListener("submit", (event) => {
    event.preventDefault();
    if (busy) return;
    const data = new FormData(view);
    const password = view.querySelector<HTMLInputElement>('[type="password"]');
    if (password) password.value = "";
    action(data);
  });
  return view;
}

const str = (data: FormData, name: string) => String(data.get(name) ?? "");
const submit = (text: string, tone = "primary") => el("button", `button ${tone}`, text);
const flowErrors = new Set(["offline", "direct_failed", "target_unavailable", "target_timeout", "http_invalid", "protocol_mismatch"]);
const showError = () => {
  const last = status?.error;
  const code = message === "extension_setup" ? "" : message || (status?.online && last && flowErrors.has(last) ? "" : last);
  const inlineError = content.querySelector<HTMLParagraphElement>(".signin [role=alert]");
  if (inlineError) {
    inlineError.hidden = !code;
    inlineError.textContent = code ? failure(code) : "";
    errorLine.hidden = true;
    errorLine.textContent = "";
  } else {
    errorLine.hidden = !code;
    errorLine.textContent = code ? failure(code) : "";
  }
  const dialogError = editor.querySelector<HTMLParagraphElement>("[role=alert]");
  if (dialogError) {
    dialogError.hidden = !message;
    dialogError.textContent = message ? failure(message) : "";
  }
};

function syncSigninSubmit() {
  const btn = content.querySelector<HTMLButtonElement>('.signin button[type="submit"]');
  if (!btn) return;
  btn.disabled = busy;
  if (btn.dataset.loginBtn === "1") btn.textContent = t(busy ? "logging_in" : "login");
}

async function request(body: Request, launch: boolean): Promise<Status> {
  const reply = await invoke<Reply>("core_command", { request: body, launch });
  if (!reply.data) throw reply.error ?? "internal";
  return reply.data;
}

async function act(body: Request, done?: () => void) {
  if (busy) return;
  busy = true;
  message = "";
  showError();
  syncSigninSubmit();
  syncBrand();
  try {
    status = await request(body, true);
    done?.();
    render(true);
  } catch (error) {
    message = body.op === "login" && error === "unauthorized" ? "login_rejected"
      : typeof error === "string" ? error : "internal";
    if (mac && !status) render(true);
    else showError();
  } finally {
    busy = false;
    syncSigninSubmit();
    syncBrand();
    applyJoinLink();
  }
}

const adminWorkspace = createAdminWorkspace({
  t: (k) => t(k as Parameters<typeof t>[0]),
  isZh,
  failure,
  dialog: editor,
  transport: {
    session: async () => {
      const reply = await request({ op: "admin_session" }, false);
      status = reply;
      return reply.admin_data;
    },
    action: async (body) => {
      status = await request({ op: "admin_action", action: body }, false);
    },
  },
  onUnauthorized() {
    if (status) status = { ...status, admin_ready: false };
    if (tab === "admin") tab = "services";
    render(true);
  },
});

adminWorkspace.onUpdate(() => {
  if (tab === "admin") render(true);
});

window.addEventListener("keydown", (event) => {
  if ((event.metaKey || event.ctrlKey) && event.shiftKey && event.key.toLowerCase() === "m") {
    if (!status?.member || !status.admin_ready) return;
    event.preventDefault();
    selectTab(tab === "admin" ? "services" : "admin");
  }
});

function selectTab(next: "services" | "admin") {
  if (next === "admin" && !status?.admin_ready) return;
  if (tab === next) return;
  closePopoverMenu();
  tab = next;
  render(true);
  if (next === "admin") void adminWorkspace.refresh();
}

const ranges = <T>(spans: Span<T>[]) => formatRanges(spans, isZh());
const inputRanges = <T>(spans: Span<T>[]) => spans.map(({ start, end }) =>
  start === end ? String(start) : `${start}-${end}`).join(", ");
const toInt = (ip: string) => ip.split(".").reduce((n, part) => n * 256 + Number(part), 0);
const toIp = (n: number) => [n >>> 24, (n >>> 16) & 255, (n >>> 8) & 255, n & 255].join(".");

function addresses(ips: Span<string>[], maps: Mapping[]) {
  const original: Span<string>[] = [];
  const virtual: Span<string>[] = [];
  for (const ip of ips) {
    let low = toInt(ip.start);
    const high = toInt(ip.end);
    for (const map of maps) {
      const start = Math.max(low, toInt(map.original.start));
      const end = Math.min(high, toInt(map.original.end));
      if (start > end) continue;
      if (low < start) original.push({ start: toIp(low), end: toIp(start - 1) });
      const offset = toInt(map.virtual_ip.start) - toInt(map.original.start);
      virtual.push({ start: toIp(start + offset), end: toIp(end + offset) });
      low = end + 1;
    }
    if (low <= high) original.push({ start: toIp(low), end: toIp(high) });
  }
  return { original, virtual };
}

const accessIp = (ip: string, maps: Mapping[]) =>
  addresses([{ start: ip, end: ip }], maps).virtual[0]?.start ?? ip;

const kindBadgeText = (kind: ServiceKind) => kind.toUpperCase();

function inferDefaultKind(port: number): ServiceKind {
  if (port === 22) return "ssh";
  if (port === 3389) return "rdp";
  if (port === 445) return "smb";
  if (port === 80) return "http";
  if (port === 443) return "https";
  return "tcp";
}

function isUsableHostIp(ip: string): boolean {
  if (!/^\d{1,3}(\.\d{1,3}){3}$/.test(ip)) return false;
  const n = toInt(ip);
  if (toIp(n) !== ip) return false;
  const first = n >>> 24;
  const second = (n >>> 16) & 255;
  if (first === 0 || first === 127 || first >= 224) return false;
  if (first === 169 && second === 254) return false;
  if (first === 198 && (second === 18 || second === 19)) return false;
  return true;
}

function dialogHeader(title: string, onClose: () => void) {
  const heading = el("h2", "", title);
  heading.id = "editor-title";
  const closeBtn = tool("close", t("close"), onClose);
  return el("div", "dialog-header", heading, closeBtn);
}

function showDialog(title: string, bodyNodes: HTMLElement[], footerNodes: HTMLElement[] = [], coreErrors = true) {
  const header = dialogHeader(title, () => { if (!busy) editor.close(); });
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

function edit(title: string, save: HTMLButtonElement, handler: (data: FormData) => void, ...inputs: HTMLElement[]) {
  const header = dialogHeader(title, () => { if (!busy) editor.close(); });
  const warning = el("p", "feedback error");
  warning.setAttribute("role", "alert");
  warning.hidden = true;
  const body = el("div", "dialog-body", ...inputs, warning);
  const footer = el("div", "dialog-footer", button(t("cancel"), () => { if (!busy) editor.close(); }, "button"), save);
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

function customize(service: Offering, label: Label | undefined, routing: boolean) {
  edit(t("access_settings"), submit(t("save")), (data) => {
    const alias = routing ? str(data, "alias").trim() : label?.alias ?? "";
    const ip = routing ? str(data, "ip").trim() : label?.ip ?? null;
    void act({ op: "customize", id: service.id, remark: str(data, "remark").trim(),
      alias, ip: alias ? ip : null }, () => editor.close());
  }, field(t("remark"), "remark", label?.remark ?? "", { maxLength: 64 }),
  ...(routing ? [
    field(t("alias"), "alias", label?.alias ?? "", { placeholder: "app.fesen", maxLength: 72 }),
    field(t("alias_ip"), "ip", label?.ip ?? service.rules.ips[0]?.start ?? "", { maxLength: 15 }),
  ] : []));
}

async function testConnection(service: Offering, ip: string, port: number) {
  const key = serviceKey(service);
  const previous = probes.get(key);
  if (busy || !status?.online || (previous && previous.result === undefined)) return;
  const kind = effectiveKind(service);
  const current: Probe = { ip, port, access: accessIp(ip, status.virtual_ips), kind };
  probes.set(key, current);
  render(true);
  try {
    const reply = await request({ op: "test_connection", id: service.id, ip, port }, false);
    const outcome = reply.probe;
    const httpStatus = outcome?.http_status ?? reply.http_status;
    current.result = kind === "http" && httpStatus === undefined
      ? { error: "http_invalid" }
      : {
        httpStatus,
        rttMs: outcome?.rtt_ms,
        hostOs: outcome?.host_os,
        detail: outcome?.detail,
      };
  } catch (error) {
    current.result = { error: typeof error === "string" ? error : "internal" };
  }
  if (probes.get(key) === current) render(true);
}

function testHostServices(items: Offering[]) {
  if (busy || !status?.online) return;
  const tasks: { service: Offering; ip: string; port: number }[] = [];
  for (const service of items) {
    const ip = singleIp(service);
    const port = singlePort(service);
    if (ip && port) tasks.push({ service, ip, port });
  }
  let cursor = 0;
  const workers = Math.min(4, tasks.length);
  for (let i = 0; i < workers; i++) {
    void (async () => {
      while (cursor < tasks.length) {
        const next = tasks[cursor++]!;
        await testConnection(next.service, next.ip, next.port);
      }
    })();
  }
}

function chooseTarget(service: Share, title: string, action: (ip: string, port: number) => void,
  previous?: Probe, submitLabel = t("test")) {
  const { ips, ports } = service.rules;
  if (singleSpan(ips) && singleSpan(ports)) {
    action(ips[0]!.start, ports[0]!.start);
    return;
  }
  edit(title, submit(submitLabel), (data) => {
    const ip = str(data, "ip").trim();
    const port = Number(str(data, "port"));
    if (!/^\d{1,3}(\.\d{1,3}){3}$/.test(ip) || toIp(toInt(ip)) !== ip
      || !ips.some(r => toInt(r.start) <= toInt(ip) && toInt(ip) <= toInt(r.end))
      || !Number.isInteger(port) || !ports.some(r => r.start <= port && port <= r.end)) {
      message = "invalid_target"; showError(); return;
    }
    message = "";
    editor.close();
    action(ip, port);
  }, field(t("original_ip"), "ip", previous?.ip ?? ips[0]!.start, { required: true, maxLength: 15 }),
  field(t("port"), "port", String(previous?.port ?? ports[0]!.start), { required: true, type: "number", min: "1", max: "65535" }));
}

function runServiceAction(service: Share, owner: string | undefined, kind: "copy" | "command" | "browser", label: string) {
  chooseTarget(service, label, (ip, port) => {
    void invoke<string>("service_action", { id: service.id, owner: owner ?? null, ip, port, action: kind })
      .then(value => { if (kind !== "browser") notify(`${t("address_copied")} · ${value}`); })
      .catch(error => notify(failure(typeof error === "string" ? error
        : kind === "browser" ? "external_browser_failed" : "clipboard_failed"), true));
  }, owner ? probes.get(`${owner}:${service.id}`) : undefined, label);
}

function openWebPreview(service: Share, owner?: string) {
  const title = t(owner ? "web_test" : "local_web_test");
  message = ""; showError();
  chooseTarget(service, title, (ip, port) => {
    const access = owner ? accessIp(ip, status?.virtual_ips ?? []) : ip;
    const session = tabOwner;
    const valid = () => {
      const s = owner ? status?.services.find(s => s.owner === owner && s.id === service.id)
        : status?.shares.find(s => s.id === service.id);
      return !!status?.member && session === JSON.stringify([status.server, status.member])
        && !!s && effectiveKind(s) === "http"
        && (!owner || (status.online && accessIp(ip, status.virtual_ips) === access))
        && s.rules.ips.some(r => toInt(r.start) <= toInt(ip) && toInt(ip) <= toInt(r.end))
        && s.rules.ports.some(r => r.start <= port && port <= r.end);
    };
    showSiteDialog({ id: service.id, owner: owner ?? null, ip, port }, access, valid, (result) => {
      if (!owner) return;
      const key = `${owner}:${service.id}`;
      const prev = probes.get(key)?.result;
      probes.set(key, {
        ip, port, access, kind: "http",
        result: result === "visible"
          ? { visible: true, rttMs: prev?.rttMs, httpStatus: prev?.httpStatus }
          : result,
      });
      render(true);
    });
  });
}

function rowActions(service: Share, name: string, owner: string | undefined, routing: boolean, configure: () => void) {
  const more = tool("more", `${t("more_actions")} · ${name}`, () => {
    const kind = effectiveKind(service);
    const primary: MenuEntry[] = [];
    if (kind === "http" && (!owner || routing)) {
      primary.push({
        icon: "globe",
        label: t(owner ? "web_test" : "local_web_test"),
        onSelect: () => openWebPreview(service, owner),
      });
    }
    if ((kind === "http" || kind === "https") && (!owner || routing)) {
      primary.push({
        icon: "external",
        label: t("open_browser"),
        onSelect: () => runServiceAction(service, owner, "browser", t("open_browser")),
      });
    }
    if (owner && routing && (kind === "ssh" || kind === "rdp" || kind === "smb")) {
      primary.push({
        icon: "terminal",
        label: t("copy_command"),
        onSelect: () => runServiceAction(service, owner, "command", t("copy_command")),
      });
    }
    primary.push({
      icon: "copy",
      label: t("copy_address"),
      onSelect: () => runServiceAction(service, owner, "copy", t("copy_address")),
    });

    const secondary: MenuEntry[] = [
      {
        icon: owner ? "sliders" : "edit",
        label: t(owner ? "access_settings" : "edit_share"),
        onSelect: configure,
      },
    ];
    const hostIp = !owner ? singleIp(service) : null;
    if (hostIp) {
      secondary.push({
        icon: "plus",
        label: t("manage_host_ports"),
        onSelect: () => openAddShare({ ip: hostIp, name: meaningfulHostName(service.name, hostIp, service.host_os ?? null) }),
      });
    }
    openPopoverMenu(more, [...primary, { sep: true }, ...secondary]);
  });
  more.setAttribute("aria-haspopup", "menu");
  more.setAttribute("aria-expanded", "false");
  return more;
}

function endpoint(ips: Span<string>[], ports: Span<number>[]) {
  return formatEndpoint({ ips, ports }, isZh());
}

function probeVisual(service: Offering, data: Status, routing: boolean) {
  const kind = effectiveKind(service);
  const ips = addresses(service.rules.ips, data.virtual_ips);
  const overlapped = ips.virtual.length > 0;
  const one = singleSpan(service.rules.ips) && singleSpan(service.rules.ports);
  const probe = probes.get(serviceKey(service));
  const result = probe?.result;
  const http = result?.httpStatus;
  const pending = !!probe && !result;
  const httpBad = http !== undefined && (http < 200 || http >= 400);
  const errCode = result?.error;

  let caption: string;
  let tone = "";
  let detailTip = t("reprobe");

  if (pending) {
    caption = t("testing");
  } else if (!result) {
    caption = t("unchecked");
  } else if (errCode) {
    caption = failure(errCode);
    tone = errCode === "conflict" ? "warn" : "bad";
    detailTip = `${caption} · ${t("reprobe")}`;
  } else if (httpBad) {
    caption = result.rttMs !== undefined ? `${result.rttMs}ms` : `HTTP ${http}`;
    tone = "bad";
    detailTip = `HTTP ${http}${result.rttMs !== undefined ? ` · ${result.rttMs}ms` : ""} · ${t("reprobe")}`;
  } else {
    caption = result.rttMs !== undefined
      ? `${result.rttMs}ms`
      : result.visible ? t("page_visible") : t("reachable");
    tone = routing ? "good" : "";
    detailTip = [http !== undefined ? `HTTP ${http}` : "", result.rttMs !== undefined ? `${result.rttMs}ms` : "", t("reprobe")]
      .filter(Boolean).join(" · ");
  }

  const badge = button(caption, () => chooseTarget(service, t("test_connection"),
    (ip, port) => void testConnection(service, ip, port), probe), `probe-badge badge ${tone}`);
  badge.title = detailTip;
  badge.setAttribute("aria-label", `${t("refresh_status")}：${caption}`);
  badge.setAttribute("data-pending", String(pending));
  badge.disabled = pending || !data.online;

  let httpTag: HTMLElement | null = null;
  if (http !== undefined && !pending && !errCode) {
    httpTag = el("span", httpBad ? "http-code bad" : "http-code", String(http));
    httpTag.title = `HTTP ${http}`;
  }

  let note: HTMLElement | null = null;
  if (probe && result && !one) {
    const outcome = errCode ? failure(errCode)
      : result.visible ? t("page_visible")
      : http !== undefined ? `HTTP ${http}`
      : kind === "tcp" ? t("reachable") : kindBadgeText(kind);
    note = el("p", errCode || httpBad ? "probe-note problem" : "probe-note", `${probe.access}:${probe.port} · ${outcome}`);
    note.setAttribute("role", "status");
  }
  return { ips, overlapped, badge, httpTag, note, pending, result };
}

function sourceText(service: Offering, currentMember: string | null): { text: string; self: boolean } {
  const self = !!currentMember && service.member === currentMember;
  const dev = service.device?.trim() || "";
  if (self) {
    return { text: dev ? `${t("source_self")} · ${dev}` : t("source_self"), self: true };
  }
  return { text: dev ? `${service.member} · ${dev}` : service.member, self: false };
}

function serviceRow(address: string, metaNodes: (HTMLElement | string)[], tags: HTMLElement[], ...controls: HTMLElement[]) {
  return el("article", "service-row",
    el("div", "service-main",
      el("div", "host-title-line", el("p", "endpoint", address), ...tags),
      ...(metaNodes.length ? [el("div", "service-meta", ...metaNodes)] : [])
    ),
    el("div", "service-actions", ...controls)
  );
}

function groupSingleRow(service: Offering, data: Status, routing: boolean) {
  const kind = effectiveKind(service);
  const label = data.labels.find((l) => l.service === service.id);
  const { ips, overlapped, badge, httpTag, note, result } = probeVisual(service, data, routing);
  const address = endpoint([...ips.original, ...ips.virtual], service.rules.ports);
  const rawName = label?.remark || service.name;
  const ip = singleIp(service);
  const os = resolveHostOs([service], [result?.hostOs]) as HostOs | null;
  const displayName = ip ? meaningfulHostName(rawName, ip, os) : rawName;
  const alias = label?.alias && data.aliases[label.alias] ? label.alias : null;
  const src = sourceText(service, data.member);

  const metaParts: (HTMLElement | string)[] = [];
  if (displayName) metaParts.push(el("span", "service-name", displayName));
  if (metaParts.length) metaParts.push(el("span", "meta-sep", "·"));
  metaParts.push(el("span", src.self ? "source-tag self" : "source-tag", src.text));
  if (alias) {
    metaParts.push(el("span", "meta-sep", "·"));
    metaParts.push(el("span", "mono", alias));
  }

  const tags: HTMLElement[] = [];
  if (os) tags.push(el("span", "host-os-tag", osLabel(os)));
  if (kind !== "tcp") tags.push(el("span", "proto-tag", kindBadgeText(kind)));
  if (httpTag) tags.push(httpTag);
  if (overlapped) {
    const ov = el("span", "badge warn", t("overlap"));
    ov.title = t("local_overlap");
    tags.push(ov);
  }
  const row = serviceRow(
    address, metaParts, tags, badge,
    rowActions(service, rawName, service.owner, routing, () => customize(service, label, routing))
  );
  if (overlapped) row.append(el("p", "probe-note", `${t("original_ip")} ${ranges(service.rules.ips)}`));
  if (note) row.append(note);
  return row;
}

function groupHostCard(ip: string, items: Offering[], data: Status, routing: boolean) {
  const access = accessIp(ip, data.virtual_ips);
  const overlapped = access !== ip;
  const labels = items.map(s => data.labels.find(l => l.service === s.id));
  const rawHostName = labels.find(l => l?.remark)?.remark || items[0]!.name;
  const alias = labels.map(l => l?.alias).find((a): a is string => !!a && !!data.aliases[a]) ?? null;
  const probed = items.map(s => probes.get(serviceKey(s)));
  const anyPending = probed.some(p => p && !p.result);
  const hostOs = resolveHostOs(items, probed.map(p => p?.result?.hostOs)) as HostOs | null;
  const cleanName = meaningfulHostName(rawHostName, ip, hostOs);
  const src = sourceText(items[0]!, data.member);

  const testAll = tool("refresh", t("test_host"), () => testHostServices(items));
  testAll.disabled = anyPending || !data.online || busy;
  if (anyPending) testAll.classList.add("spinning");

  const titleParts: HTMLElement[] = [el("p", "endpoint", access)];
  if (hostOs) titleParts.push(el("span", "host-os-tag", osLabel(hostOs)));
  if (overlapped) {
    const ov = el("span", "badge warn", t("overlap"));
    ov.title = t("local_overlap");
    titleParts.push(ov);
  }

  const metaParts: (HTMLElement | string)[] = [];
  if (cleanName) metaParts.push(el("span", "service-name", cleanName));
  if (metaParts.length) metaParts.push(el("span", "meta-sep", "·"));
  metaParts.push(el("span", src.self ? "source-tag self" : "source-tag", src.text));
  if (alias) {
    metaParts.push(el("span", "meta-sep", "·"));
    metaParts.push(el("span", "mono", alias));
  }
  if (overlapped) {
    metaParts.push(el("span", "meta-sep", "·"));
    metaParts.push(el("span", "mono", `${t("original_ip")} ${ip}`));
  }

  const head = el("div", "host-head",
    el("div", "host-info",
      el("div", "host-title-line", ...titleParts),
      ...(metaParts.length ? [el("div", "service-meta", ...metaParts)] : [])
    ),
    el("div", "host-controls", testAll)
  );

  const portsWrap = el("div", "host-ports", ...items.map((service, idx) => {
    const kind = effectiveKind(service);
    const label = labels[idx];
    const { badge, httpTag, note } = probeVisual(service, data, routing);
    const portText = `:${ranges(service.rules.ports)}`;
    const rowRawName = label?.remark || service.name;
    const subName = rowRawName !== rawHostName ? meaningfulHostName(rowRawName, ip, hostOs) : "";
    const row = el("div", "port-row",
      el("div", "port-main",
        el("span", "port-num", portText),
        el("span", "proto-tag", kindBadgeText(kind)),
        ...(httpTag ? [httpTag] : []),
        ...(subName ? [el("span", "port-subname", subName)] : [])
      ),
      el("div", "port-actions",
        badge,
        rowActions(service, label?.remark || `${ip} ${portText}`, service.owner, routing,
          () => customize(service, label, routing))
      )
    );
    return el("div", "port-row-wrap", row, ...(note ? [note] : []));
  }));

  return el("article", "host-card", head, portsWrap);
}

function groupList(data: Status) {
  const routing = data.system_routing !== false;
  const entries = groupByHost(data.services, (s: Offering) => `${s.owner}:${singleIp(s)}`);
  return el("div", "service-list", ...entries.map((entry) => {
    if (!entry.grouped || entry.items.length === 1) {
      const item = (entry.grouped ? entry.items[0] : entry.item) as Offering;
      return groupSingleRow(item, data, routing);
    }
    return groupHostCard(entry.ip, entry.items as Offering[], data, routing);
  }));
}

function protocolSelect(selected: ServiceKind, cls = "kind-select", onChange?: (k: ServiceKind) => void) {
  const select = el("select", cls);
  select.name = "kind";
  const options: [ServiceKind, Parameters<typeof t>[0]][] = [
    ["tcp", "kind_tcp"],
    ["http", "kind_http"],
    ["https", "kind_https"],
    ["ssh", "kind_ssh"],
    ["rdp", "kind_rdp"],
    ["smb", "kind_smb"],
  ];
  for (const [value, key] of options) {
    const opt = el("option", "", t(key));
    opt.value = value;
    opt.selected = value === selected;
    select.append(opt);
  }
  if (onChange) select.addEventListener("change", () => onChange(select.value as ServiceKind));
  return select;
}

function editShare(share?: Share) {
  const currentKind = share ? effectiveKind(share) : "tcp";
  const controls = edit(t(share ? "edit_share" : "new_share"), submit(t("save")), (data) => {
    const kind = str(data, "kind") as ServiceKind;
    void act({
      op: "save",
      id: share?.id ?? null,
      name: str(data, "name"),
      ips: str(data, "ips"),
      ports: str(data, "ports"),
      http: kind === "http",
      kind,
      host_os: share?.host_os ?? null,
    }, () => editor.close());
  },
  field(t("service_name"), "name", share?.name ?? "", { required: true, maxLength: 64 }),
  field(t("ipv4"), "ips", share ? inputRanges(share.rules.ips) : "", {
    required: true, maxLength: 900, placeholder: "198.51.100.10, 203.0.113.0/29",
  }),
  field(t("port"), "ports", share ? inputRanges(share.rules.ports) : "", {
    required: true, maxLength: 500, placeholder: "80, 8000-8008",
  }),
  el("label", "", t("protocol_type"), protocolSelect(currentKind)));
  if (share) controls.prepend(button(t("remove"), () => {
    edit(t("confirm_remove"), submit(t("remove"), "danger"), () => {
      void act({ op: "remove", id: share.id }, () => editor.close());
    }, el("p", "muted", share.name), el("p", "endpoint", endpoint(share.rules.ips, share.rules.ports)));
  }, "button ghost text-danger leading"));
}

type DraftPort = {
  spec: string;
  port?: number;
  kind: ServiceKind;
  checked: boolean;
  saved: boolean;
  osHint?: HostOs;
  probing?: boolean;
  open?: boolean;
  rttMs?: number;
  httpStatus?: number;
  detail?: string;
};

function parsePortEntries(raw: string): { spec: string; port?: number }[] | null {
  const parts = raw.split(/[,，\s]+/).map(s => s.trim()).filter(Boolean);
  if (!parts.length) return [];
  const out: { spec: string; port?: number }[] = [];
  const seen = new Set<string>();
  for (const part of parts) {
    const m = /^(\d{1,5})(?:-(\d{1,5}))?$/.exec(part);
    if (!m) return null;
    const start = Number(m[1]);
    const end = m[2] !== undefined ? Number(m[2]) : start;
    if (!Number.isInteger(start) || !Number.isInteger(end) || start < 1 || end > 65535 || start > end) return null;
    const spec = start === end ? String(start) : `${start}-${end}`;
    if (!seen.has(spec)) {
      seen.add(spec);
      out.push(start === end ? { spec, port: start } : { spec });
    }
  }
  return out;
}

function openAddShare(prefill?: { ip?: string; name?: string }) {
  message = "";
  let ipVal = prefill?.ip ?? "";
  let portDraft = "";
  let nameVal = prefill?.name ?? "";
  let probingAll = false;
  let localError = "";
  let emptyDetect = false;
  let items: DraftPort[] = [];
  let publishNow = !!status?.online;
  let dialogOpen = true;
  let keepPortFocus = false;
  editor.addEventListener("close", () => { dialogOpen = false; }, { once: true });

  const existingForIp = (ip: string) =>
    (status?.shares ?? []).filter(s => singleIp(s) === ip && singlePort(s) !== null);

  if (ipVal) {
    items = existingForIp(ipVal)
      .map(s => {
        const port = singlePort(s)!;
        const kind = effectiveKind(s);
        const osHint: HostOs | undefined = kind === "rdp" ? "windows" : (kind === "ssh" ? s.host_os : undefined);
        return { spec: String(port), port, kind, checked: true, saved: true, osHint };
      })
      .sort((a, b) => (a.port ?? 0) - (b.port ?? 0));
  }

  function effectiveHostOs(): HostOs | null {
    if (!isUsableHostIp(ipVal.trim())) return null;
    const active = items.filter(i => i.checked && i.port !== undefined);
    if (active.some(i => i.kind === "rdp" || (i.kind === "ssh" && i.osHint === "windows"))) return "windows";
    if (active.some(i => i.kind === "ssh" && i.osHint === "linux")) return "linux";
    return null;
  }

  function sortItems() {
    items.sort((a, b) => {
      const pa = a.port ?? Number(a.spec.split("-")[0] ?? 0);
      const pb = b.port ?? Number(b.spec.split("-")[0] ?? 0);
      return pa - pb || a.spec.localeCompare(b.spec);
    });
  }

  function mergeProbeResults(result: HostProbe, onlySpecific: boolean) {
    const existingSet = new Set(existingForIp(result.ip).map(s => singlePort(s)!));
    const bySpec = new Map(items.map(i => [i.spec, i]));
    let anyOpen = false;
    for (const p of result.ports) {
      if (p.open) anyOpen = true;
      const spec = String(p.port);
      const prev = bySpec.get(spec);
      const hint: HostOs | undefined = p.os_hint ?? (p.kind === "rdp" ? "windows" : undefined);
      if (onlySpecific) {
        if (!prev) continue;
        prev.probing = false;
        prev.open = p.open;
        prev.rttMs = p.rtt_ms;
        prev.httpStatus = p.http_status;
        prev.detail = p.detail;
        if (p.open) { prev.kind = p.kind; prev.osHint = hint; }
      } else if (p.open) {
        if (prev) {
          prev.probing = false;
          prev.open = true;
          prev.kind = p.kind;
          prev.osHint = hint;
          prev.rttMs = p.rtt_ms;
          prev.httpStatus = p.http_status;
          prev.detail = p.detail;
        } else {
          bySpec.set(spec, {
            spec,
            port: p.port,
            kind: p.kind,
            checked: p.recommended && !existingSet.has(p.port),
            saved: existingSet.has(p.port),
            osHint: hint,
            open: true,
            rttMs: p.rtt_ms,
            httpStatus: p.http_status,
            detail: p.detail,
          });
        }
      }
    }
    if (!onlySpecific) {
      emptyDetect = !anyOpen;
      if (![...bySpec.values()].some(i => i.checked)) {
        for (const p of result.ports) {
          if (p.open && p.recommended) {
            const entry = bySpec.get(String(p.port));
            if (entry) entry.checked = true;
          }
        }
      }
    }
    items = [...bySpec.values()];
    sortItems();
  }

  async function detectCommonServices() {
    if (probingAll || busy) return;
    const ip = ipVal.trim();
    if (!isUsableHostIp(ip)) { localError = t("invalid_host_ip"); draw(); return; }
    localError = "";
    message = "";
    emptyDetect = false;
    probingAll = true;
    draw();
    try {
      const extra = items
        .map(i => i.port)
        .filter((p): p is number => p !== undefined && ![22, 3389, 445, 80, 443].includes(p))
        .slice(0, 5);
      const reply = await request({ op: "probe_host", ip, extra_ports: extra }, true);
      if (!reply.host_probe) throw "internal";
      mergeProbeResults(reply.host_probe, false);
    } catch (error) {
      localError = failure(typeof error === "string" ? error : "internal");
    } finally {
      probingAll = false;
      if (dialogOpen && editor.open) draw();
    }
  }

  function commitPortInput(refocus = true) {
    const raw = portDraft.trim();
    if (!raw) return true;
    const parsed = parsePortEntries(raw);
    if (!parsed || !parsed.length) { localError = t("invalid_port_input"); draw(); return false; }
    const ip = ipVal.trim();
    const singleHost = isUsableHostIp(ip);
    const existingSet = new Set(singleHost ? existingForIp(ip).map(s => singlePort(s)!) : []);
    const addedSingle: number[] = [];
    for (const token of parsed) {
      const prev = items.find(i => i.spec === token.spec);
      if (prev) {
        prev.checked = true;
      } else {
        if (items.length >= 10) { localError = t("invalid_port_input"); draw(); return false; }
        const kind = token.port !== undefined ? inferDefaultKind(token.port) : "tcp";
        items.push({
          spec: token.spec,
          port: token.port,
          kind,
          checked: true,
          saved: token.port !== undefined && existingSet.has(token.port),
          osHint: kind === "rdp" ? "windows" : undefined,
          probing: singleHost && token.port !== undefined,
        });
        if (singleHost && token.port !== undefined) addedSingle.push(token.port);
      }
    }
    sortItems();
    portDraft = "";
    localError = "";
    emptyDetect = false;
    keepPortFocus = refocus;
    draw();

    if (addedSingle.length && singleHost) {
      void request({ op: "probe_host", ip, ports: addedSingle }, true)
        .then((reply) => { if (reply.host_probe) mergeProbeResults(reply.host_probe, true); })
        .catch(() => {
          for (const port of addedSingle) {
            const entry = items.find(i => i.port === port);
            if (entry) { entry.probing = false; entry.open = false; }
          }
        })
        .finally(() => { if (dialogOpen && editor.open) draw(); });
    }
    return true;
  }

  function itemMetaText(item: DraftPort): string {
    if (item.probing) return t("port_checking");
    const parts: string[] = [];
    if (item.saved) parts.push(t("already_added"));
    if (item.open === false) parts.push(t("port_offline"));
    else if (item.kind === "rdp" && item.detail === "NLA") parts.push("NLA");
    return parts.join(" · ");
  }

  function draw() {
    const activeName = document.activeElement?.getAttribute("name") ?? "";
    const header = dialogHeader(t("new_share"), () => { if (!busy) editor.close(); });
    const body = el("div", "dialog-body");
    const alertBox = el("p", "feedback error");
    alertBox.setAttribute("role", "alert");

    const osTagHost = el("span");
    function syncHeaderMeta() {
      const os = effectiveHostOs();
      osTagHost.replaceChildren(...(os ? [el("span", "host-os-tag", osLabel(os))] : []));
      detectBtn.disabled = probingAll || busy || !isUsableHostIp(ipVal.trim());
    }

    const ipInput = el("input");
    Object.assign(ipInput, {
      name: "host_ip",
      value: ipVal,
      maxLength: 900,
      placeholder: "198.51.100.35 / 203.0.113.0/29",
      spellcheck: false,
      disabled: probingAll,
    });
    ipInput.addEventListener("input", () => {
      ipVal = ipInput.value;
      if (localError) { localError = ""; alertBox.hidden = true; alertBox.textContent = ""; }
      syncHeaderMeta();
    });
    ipInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        if (isUsableHostIp(ipVal.trim())) void detectCommonServices();
        else portInput.focus();
      }
    });

    const detectBtn = button("", () => void detectCommonServices(),
      probingAll ? "tool-btn icon-only spinning" : "tool-btn icon-only");
    detectBtn.append(icon("refresh"));
    detectBtn.title = t(probingAll ? "detecting_host" : "detect_host");
    detectBtn.setAttribute("aria-label", detectBtn.title);

    const ipHead = el("div", "host-ip-head", el("span", "field-label", t("host_ip")), osTagHost);
    body.append(el("div", "field-group", ipHead, el("div", "host-ip-row", ipInput, detectBtn)));

    const portsBox = el("div", "ports-box");
    if (emptyDetect && !items.length) portsBox.append(el("div", "ports-empty-note", t("host_none_open")));
    for (const item of items) {
      const cb = el("input");
      Object.assign(cb, { type: "checkbox", checked: item.checked });
      const row = el("div", `host-service-item${item.checked ? " checked" : ""}`);
      cb.addEventListener("change", () => {
        item.checked = cb.checked;
        row.classList.toggle("checked", cb.checked);
        syncHeaderMeta();
        updateSubmit();
      });
      const kindSel = protocolSelect(item.kind, "kind-inline", (k) => {
        item.kind = k;
        if (k === "rdp") item.osHint = "windows";
        else if (k !== "ssh") item.osHint = undefined;
        syncHeaderMeta();
      });
      const detail = itemMetaText(item);
      const removeBtn = button("", () => {
        items = items.filter(i => i.spec !== item.spec);
        draw();
      }, "row-remove");
      removeBtn.title = t("remove");
      removeBtn.setAttribute("aria-label", `${t("remove")} :${item.spec}`);
      removeBtn.append(icon("close"));

      const httpCode = item.httpStatus !== undefined
        ? el("span", item.httpStatus >= 400 ? "http-code bad" : "http-code", String(item.httpStatus))
        : null;
      if (httpCode) httpCode.title = `HTTP ${item.httpStatus}`;
      row.append(
        cb,
        el("span", "host-service-port", `:${item.spec}`),
        kindSel,
        ...(httpCode ? [httpCode] : []),
        el("span", "host-service-detail", detail),
        ...(item.rttMs !== undefined ? [el("span", "host-service-rtt", `${item.rttMs}ms`)] : []),
        ...(item.saved ? [] : [removeBtn])
      );
      portsBox.append(row);
    }

    const portInput = el("input", "port-inline-input");
    Object.assign(portInput, {
      name: "add_port",
      value: portDraft,
      maxLength: 120,
      placeholder: items.length ? t("add_more_ports_placeholder") : t("add_ports_placeholder"),
      spellcheck: false,
      disabled: probingAll,
    });
    portInput.addEventListener("input", () => {
      portDraft = portInput.value;
      if (localError) { localError = ""; alertBox.hidden = true; alertBox.textContent = ""; }
      if (/[,，]$/.test(portDraft)) {
        const trimmed = portDraft.replace(/[,，]+$/, "").trim();
        if (trimmed) { portDraft = trimmed; commitPortInput(true); return; }
      }
      updateSubmit();
    });
    portInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") { e.preventDefault(); commitPortInput(true); }
    });
    portsBox.append(portInput);
    body.append(el("div", "field-group", el("span", "field-label", t("add_ports_label")), portsBox));

    if (items.length || portDraft.trim()) {
      const nameInput = el("input");
      Object.assign(nameInput, {
        name: "host_name",
        value: nameVal,
        maxLength: 64,
        placeholder: ipVal.trim() || "198.51.100.35",
        spellcheck: false,
      });
      nameInput.addEventListener("input", () => { nameVal = nameInput.value; });
      body.append(el("label", "", t("host_name"), nameInput));

      if (status?.online && isUsableHostIp(ipVal.trim()) && items.every(i => i.port !== undefined)) {
        const pubCheck = el("input");
        Object.assign(pubCheck, { type: "checkbox", checked: publishNow });
        pubCheck.addEventListener("change", () => { publishNow = pubCheck.checked; updateSubmit(); });
        body.append(el("label", "check-row", pubCheck, t("save_and_publish")));
      }
    }

    const errText = localError || (message ? failure(message) : "");
    alertBox.hidden = !errText;
    alertBox.textContent = errText;
    body.append(alertBox);

    const saveBtn = button("", () => {
      if (portDraft.trim() && !commitPortInput(false)) return;
      const ipsText = ipVal.trim();
      if (!ipsText) { localError = t("invalid_host_ip"); draw(); return; }
      const chosen = items.filter(c => c.checked);
      if (!chosen.length) { localError = t("select_one_service"); draw(); return; }
      localError = "";
      const singleHost = isUsableHostIp(ipsText);
      const allDiscrete = chosen.every(c => c.port !== undefined);
      if (singleHost && allDiscrete) {
        const os = effectiveHostOs();
        void act({
          op: "save_host",
          ip: ipsText,
          name: nameVal.trim() || ipsText,
          host_os: os,
          services: chosen.map(c => ({ port: c.port!, kind: c.kind })),
          publish: !!status?.online && publishNow,
        }, () => editor.close());
      } else {
        const kind = chosen.length === 1 ? chosen[0]!.kind : "tcp";
        void act({
          op: "save",
          id: null,
          name: nameVal.trim() || ipsText,
          ips: ipsText,
          ports: chosen.map(c => c.spec).join(", "),
          http: kind === "http",
          kind,
          host_os: null,
        }, () => editor.close());
      }
    }, "button primary");

    function updateSubmit() {
      const count = items.filter(c => c.checked).length;
      saveBtn.textContent = count > 1 ? `${t("save")} (${count})` : t("save");
      saveBtn.disabled = probingAll || busy || (count === 0 && !portDraft.trim());
    }
    syncHeaderMeta();
    updateSubmit();

    const footer = el("div", "dialog-footer",
      button(t("cancel"), () => { if (!busy) editor.close(); }, "button"),
      saveBtn);

    editor.tabIndex = -1;
    editor.replaceChildren(header, body, footer);
    if (!editor.open) {
      editor.showModal();
      if (ipVal) portInput.focus();
      else ipInput.focus();
    }
    if (keepPortFocus || activeName === "add_port") {
      keepPortFocus = false;
      portInput.focus();
      portInput.setSelectionRange(portInput.value.length, portInput.value.length);
    } else if (activeName === "host_name") {
      const ni = editor.querySelector<HTMLInputElement>('input[name="host_name"]');
      if (ni) { ni.focus(); ni.setSelectionRange(ni.value.length, ni.value.length); }
    } else if (activeName === "host_ip") {
      ipInput.focus();
      ipInput.setSelectionRange(ipInput.value.length, ipInput.value.length);
    }
  }

  draw();
}

function shareEditAction(s: Share, address: string) {
  return () => {
    if (!status?.active.includes(s.id)) { editShare(s); return; }
    showDialog(
      s.name,
      [el("p", "endpoint dialog-subtitle", address), el("p", "badge good", t("published"))],
      [button(t("stop_edit"), () => void act({ op: "share", id: s.id, enabled: false }, () => editShare(s)), "button primary")],
      false
    );
  };
}

function confirmConnectAndShare(targetLabel: string, shareReq: Request) {
  if (busy) return;
  message = "";
  const cancelBtn = button(t("cancel"), () => { if (!busy) editor.close(); }, "button");
  const confirmBtn = button(t("connect_and_share"), () => void run(), "button primary");

  async function run() {
    if (busy) return;
    busy = true;
    message = "";
    showError();
    syncBrand();
    cancelBtn.disabled = true;
    confirmBtn.disabled = true;
    confirmBtn.textContent = t("connecting_now");
    try {
      let cur = await request({ op: "start" }, true);
      status = cur;
      if (!cur.online) {
        for (let i = 0; i < 25; i++) {
          await new Promise((r) => window.setTimeout(r, 120));
          cur = await request({ op: "status", ui: true }, false);
          status = cur;
          if (cur.online) break;
          if (!cur.running) break;
        }
      }
      if (!cur.online) {
        throw cur.error ?? "not_connected";
      }
      status = await request(shareReq, true);
      editor.close();
      render(true);
    } catch (error) {
      message = typeof error === "string" ? error : "internal";
      showError();
    } finally {
      busy = false;
      cancelBtn.disabled = false;
      confirmBtn.disabled = false;
      confirmBtn.textContent = t("connect_and_share");
      syncBrand();
      applyJoinLink();
    }
  }

  showDialog(
    t("connect_prompt_title"),
    [el("p", "muted", t("connect_prompt_body").replace("{target}", targetLabel))],
    [cancelBtn, confirmBtn],
    true
  );
}

function makeToggle(checked: boolean, label: string, onOffline: (() => void) | null, onToggle: () => void, fraction?: string) {
  const sw = button("", () => {
    if (onOffline) { onOffline(); return; }
    onToggle();
  }, "switch");
  if (fraction) sw.append(el("span", "switch-fraction", fraction));
  const track = el("span", "switch-track");
  track.setAttribute("aria-hidden", "true");
  sw.append(track);
  sw.setAttribute("role", "switch");
  sw.setAttribute("aria-label", label);
  sw.setAttribute("aria-checked", String(checked));
  if (fraction) sw.setAttribute("data-partial", "true");
  if (onOffline) {
    sw.setAttribute("data-offline", "true");
    sw.title = t("connect_prompt_title");
  }
  return sw;
}

function mineSingleRow(s: Share, data: Status) {
  const kind = effectiveKind(s);
  const active = data.active.includes(s.id);
  const ip = singleIp(s);
  const targetLabel = ip ? `${ip}:${ranges(s.rules.ports)}` : s.name;
  const publish = makeToggle(
    active,
    `${t("publish")} · ${s.name}`,
    !data.online && !active ? () => confirmConnectAndShare(targetLabel, { op: "share", id: s.id, enabled: true }) : null,
    () => void act({ op: "share", id: s.id, enabled: !active })
  );
  const address = endpoint(s.rules.ips, s.rules.ports);
  const os = resolveHostOs([s]) as HostOs | null;
  const subtitle = ip ? meaningfulHostName(s.name, ip, os) : s.name;
  const tags: HTMLElement[] = [];
  if (os) tags.push(el("span", "host-os-tag", osLabel(os)));
  if (kind !== "tcp") tags.push(el("span", "proto-tag", kindBadgeText(kind)));
  return serviceRow(
    address, subtitle ? [el("span", "service-name", subtitle)] : [], tags, publish,
    rowActions(s, s.name, undefined, true, shareEditAction(s, address))
  );
}

function mineHostCard(ip: string, items: Share[], data: Status) {
  const rawHostName = items[0]!.name;
  const hostOs = resolveHostOs(items) as HostOs | null;
  const cleanName = meaningfulHostName(rawHostName, ip, hostOs);
  const activeCount = items.filter(s => data.active.includes(s.id)).length;
  const allActive = activeCount === items.length;
  const partial = activeCount > 0 && activeCount < items.length;

  const addPortBtn = tool("plus", t("manage_host_ports"), () => openAddShare({ ip, name: cleanName }));
  const master = makeToggle(
    allActive,
    `${t("publish")} · ${cleanName || ip}`,
    !data.online && activeCount === 0
      ? () => confirmConnectAndShare(cleanName ? `${cleanName} (${ip})` : ip, { op: "share_many", ids: items.map(s => s.id), enabled: true })
      : null,
    () => void act({ op: "share_many", ids: items.map(s => s.id), enabled: !allActive }),
    partial ? `${activeCount}/${items.length}` : undefined
  );

  const titleParts: HTMLElement[] = [el("p", "endpoint", ip)];
  if (hostOs) titleParts.push(el("span", "host-os-tag", osLabel(hostOs)));

  const head = el("div", "host-head",
    el("div", "host-info",
      el("div", "host-title-line", ...titleParts),
      ...(cleanName ? [el("div", "service-meta", el("span", "service-name", cleanName))] : [])
    ),
    el("div", "host-controls", master, addPortBtn)
  );

  const portsWrap = el("div", "host-ports", ...items.map((s) => {
    const kind = effectiveKind(s);
    const active = data.active.includes(s.id);
    const portText = `:${ranges(s.rules.ports)}`;
    const publish = makeToggle(
      active,
      `${t("publish")} · ${s.name}`,
      !data.online && !active ? () => confirmConnectAndShare(`${ip}${portText}`, { op: "share", id: s.id, enabled: true }) : null,
      () => void act({ op: "share", id: s.id, enabled: !active })
    );
    const subName = s.name !== rawHostName ? meaningfulHostName(s.name, ip, hostOs) : "";
    const address = endpoint(s.rules.ips, s.rules.ports);
    const row = el("div", "port-row",
      el("div", "port-main",
        el("span", "port-num", portText),
        el("span", "proto-tag", kindBadgeText(kind)),
        ...(subName ? [el("span", "port-subname", subName)] : [])
      ),
      el("div", "port-actions", publish, rowActions(s, s.name, undefined, true, shareEditAction(s, address)))
    );
    return el("div", "port-row-wrap", row);
  }));

  return el("article", "host-card", head, portsWrap);
}

function mineList(data: Status) {
  const entries = groupByHost(data.shares, (s: Share) => singleIp(s));
  return el("div", "service-list", ...entries.map((entry) => {
    if (!entry.grouped || entry.items.length === 1) {
      const item = (entry.grouped ? entry.items[0] : entry.item) as Share;
      return mineSingleRow(item, data);
    }
    return mineHostCard(entry.ip, entry.items as Share[], data);
  }));
}

function servicesView(data: Status) {
  const hasRemote = data.services.length > 0;
  const hasLocal = data.shares.length > 0;
  if (!hasRemote && !hasLocal) {
    if (data.running && !data.online) return empty(t("connecting_now"));
    if (!data.online) {
      return empty(
        t("group_disconnected"),
        button(t("connect_now"), () => void act({ op: "start" }), "button primary"),
        button(t("new_share"), () => openAddShare(), "button")
      );
    }
    return empty(t("no_services"), addShare(true));
  }

  const routing = data.system_routing !== false;
  const makeRemoteBlock = () => el("div", "flow-section",
    el("div", "flow-head", el("span", "", t("section_group")), el("span", "count", String(data.services.length))),
    groupList(data),
    ...(routing ? [] : [el("p", "routing-note", t("routing_unavailable"))])
  );
  const makeLocalBlock = () => el("div", "flow-section",
    el("div", "flow-head", el("span", "", t("section_mine")), el("span", "count", String(data.shares.length))),
    mineList(data)
  );

  if (hasRemote && hasLocal) {
    const blocks = routing
      ? [makeRemoteBlock(), makeLocalBlock()]
      : [makeLocalBlock(), makeRemoteBlock()];
    return el("div", "service-flow", ...blocks);
  }
  return el("div", "service-flow", hasRemote ? makeRemoteBlock() : makeLocalBlock());
}

function addShare(emptyState = false) {
  if (emptyState) return button(t("new_share"), () => openAddShare(), "button primary");
  const b = button("", () => openAddShare(), "tool-btn icon-only");
  b.append(icon("plus"));
  b.title = t("new_share");
  b.setAttribute("aria-label", t("new_share"));
  return b;
}

function openSettings() {
  let currentBehavior = getCloseBehavior();
  let saveError = false;

  openSettingsModal(settingsDialog, {
    t: (k) => t(k as Parameters<typeof t>[0]),
    theme,
    i18n,
    onLocaleChange: () => {
      refreshSiteDialogI18n();
      syncBrand();
      render(true);
    },
    extraBody(redraw) {
      const nodes: HTMLElement[] = [];
      if (mac) {
        const closeSeg = el("div", "segmented");
        for (const [id, key] of [["tray", "close_to_tray"], ["quit", "close_quit"]] as const) {
          const btn = button(t(key), () => {
            if (currentBehavior === id) return;
            currentBehavior = id;
            saveError = false;
            void syncCloseBehavior(id).then((ok) => {
              saveError = !ok;
              if (settingsDialog.open) redraw();
            });
            redraw();
          }, id === currentBehavior ? "active" : "");
          btn.setAttribute("aria-pressed", String(id === currentBehavior));
          closeSeg.append(btn);
        }
        nodes.push(el("div", "settings-row", el("span", "settings-label", t("on_window_close")), closeSeg));
        if (saveError) {
          const fb = el("p", "feedback error", t("menu_save_failed"));
          fb.setAttribute("role", "alert");
          nodes.push(fb);
        }
      }

      if (desktop) {
        const sizeVal = el("span", "settings-window-val", `${winSize.w} × ${winSize.h}`);
        sizeVal.setAttribute("data-win-size-val", "1");
        const resetBtn = button(t("reset_window"), () => void resetDesktopWindowSize(), "button");
        resetBtn.setAttribute("data-win-size-reset", "1");
        resetBtn.disabled = !hasCustomWinSize && isDefaultWinSize(winSize);
        nodes.push(el("div", "settings-row",
          el("span", "settings-label", t("window_size")),
          el("div", "settings-window", sizeVal, resetBtn)
        ));
      }

      if (status?.member && status.server) {
        nodes.push(el("div", "settings-divider"));

        const acctInfo = el("span", "settings-account-info", `${status.member} · ${new URL(status.server).host}`);
        const logoutBtn = button(t("logout"), () => {
          settingsDialog.close();
          tab = "services";
          void act({ op: "logout" });
        }, "button danger");

        nodes.push(el("div", "settings-account-block",
          el("div", "settings-account-top", el("span", "settings-label", t("account")), acctInfo),
          el("div", "settings-account-btns", el("div", "settings-btn-pair", logoutBtn))
        ));
      }
      return nodes;
    },
    footerText: () => (appVersion ? `${t("brand")} v${appVersion}` : t("brand")),
  });
}

function buildConnectionControl(data: Status): HTMLElement {
  const running = data.running;
  const online = data.online;
  const connecting = running && !online;
  const symbol: IconName = online ? "link" : connecting ? "refresh" : "unlink";
  const tip = t(online ? "online" : connecting ? "connecting" : "disconnected");
  const cls = online
    ? "tool-btn icon-only active-link"
    : connecting
      ? "tool-btn icon-only muted-icon spinning"
      : "tool-btn icon-only muted-icon";
  const b = button("", () => void act({ op: running ? "stop" : "start" }), cls);
  b.setAttribute("data-connect-btn", "1");
  b.append(icon(symbol));
  b.title = tip;
  b.setAttribute("aria-label", tip);
  b.setAttribute("aria-pressed", String(running));
  return b;
}

function render(force = false) {
  checkSiteDialog();
  showError();
  const data = status;
  if (!data?.admin_ready && tab === "admin") tab = "services";
  syncBrand(!brandBooted);
  const owner = data?.member ? JSON.stringify([data.server, data.member]) : "";
  if (owner !== tabOwner) {
    tabOwner = owner;
    tab = "services";
  }
  if (!data?.online) probes.clear();
  else if (probes.size) {
    const listed = new Map(data.services.map(s => [serviceKey(s), s]));
    for (const [key, probe] of probes) {
      const s = listed.get(key);
      const ip = toInt(probe.ip);
      if (!s || effectiveKind(s) !== probe.kind || accessIp(probe.ip, data.virtual_ips) !== probe.access
        || !s.rules.ips.some(r => toInt(r.start) <= ip && ip <= toInt(r.end))
        || !s.rules.ports.some(r => r.start <= probe.port && probe.port <= r.end)) probes.delete(key);
    }
  }
  const next = JSON.stringify([data && {
    ...data, error: null, background: null, active_flows: null, probe: null, host_probe: null, admin_data: null,
  }, tab, joining]);
  if (!force && applied === next) return;
  if (!force && !data?.member && content.contains(document.activeElement)) return;
  applied = next;
  closePopoverMenu();
  actions.replaceChildren();
  const settingsBtn = button("", () => openSettings(), "tool-btn icon-only");
  settingsBtn.append(icon("settings"));
  settingsBtn.title = t("settings");
  settingsBtn.setAttribute("aria-label", t("settings"));
  settingsBtn.setAttribute("aria-haspopup", "dialog");
  actions.append(settingsBtn);

  const loggedIn = !!data?.member;
  const showNav = loggedIn && !!data?.admin_ready;
  brandNameEl.hidden = showNav;
  navTabs.hidden = !showNav;
  if (!data) {
    navTabs.replaceChildren();
    const attach = (caption: string) => button(caption, () => void act({ op: "attach" }), "button primary");
    if (mac && message === "extension_permission") {
      content.replaceChildren(el("div", "flex flex-wrap gap-2", button(t("system_settings"), () => {
        void invoke("open_extension_settings").catch(() => { message = "extension_failed"; showError(); });
      }), attach(t("continue"))));
    } else if (mac && ["extension_unsigned", "extension_restart", "extension_location"].includes(message)) {
      content.replaceChildren();
    } else {
      const caption = message === "extension_setup" ? t("enable_extension") : message === "extension_failed" ? t("retry") : t("open");
      content.replaceChildren(attach(caption));
    }
    return;
  }
  if (!data.member) {
    navTabs.replaceChildren();
    const inlineAlert = el("p", "feedback error");
    inlineAlert.setAttribute("role", "alert");
    inlineAlert.hidden = true;
    if (!data.server || joining) {
      content.replaceChildren(el("section", "signin", form((values) => {
        void act({ op: "join", address: str(values, "address").trim() }, () => { joining = false; });
      }, el("h2", "form-title", t("join_group")),
      field(t("join_link"), "address", data.server, { required: true, maxLength: 1024, placeholder: "fesen://join…" }),
      inlineAlert,
      data.server
        ? el("div", "signin-actions",
            button(t("cancel"), () => { joining = false; render(true); }, "button"),
            submit(t("save")))
        : submit(t("save")))));
    } else {
      const loginBtn = submit(t("login"));
      loginBtn.dataset.loginBtn = "1";
      content.replaceChildren(el("section", "signin",
        form((values) => void act({ op: "login", username: str(values, "username").trim(),
          password: str(values, "password"), label: str(values, "label").trim() }),
        el("h2", "form-title", t("login")),
        el("div", "signin-server",
          el("span", "mono signin-server-host", new URL(data.server).host),
          button(t("change"), () => { joining = true; render(true); }, "button ghost")),
        field(t("username"), "username", "", { required: true, autocomplete: "username", maxLength: 64 }),
        field(t("password"), "password", "", { required: true, type: "password", autocomplete: "current-password" }),
        field(t("device"), "label", /Android/i.test(navigator.userAgent) ? "Android" : /Windows/i.test(navigator.userAgent) ? "Windows" : "Mac",
          { required: true, maxLength: 64 }),
        inlineAlert,
        loginBtn)));
    }
    showError();
    syncSigninSubmit();
    return;
  }

  actions.prepend(
    ...(tab === "services" ? [addShare()] : []),
    buildConnectionControl(data)
  );
  if (data.admin_ready) {
    const totalServices = data.services.length + data.shares.length;
    navTabs.setAttribute("aria-label", t("services"));
    const tabDefs: { id: "services" | "admin"; label: string; count?: number }[] = [
      { id: "services", label: t("services"), count: totalServices },
      { id: "admin", label: t("admin_tab") },
    ];
    navTabs.replaceChildren(...tabDefs.map((item) => {
      const b = button(item.label, () => {
        if (tab === item.id) return;
        selectTab(item.id);
        navTabs.querySelector<HTMLButtonElement>('[aria-current="page"]')?.focus();
      }, "");
      if (item.count !== undefined && item.count > 0) b.append(el("span", "count", String(item.count)));
      b.setAttribute("aria-current", item.id === tab ? "page" : "false");
      return b;
    }));
  } else {
    navTabs.replaceChildren();
  }

  if (tab === "admin" && data.admin_ready) {
    const viewKey = `admin:${adminWorkspace.getTabIndex()}`;
    const panel = adminWorkspace.renderPanel();
    panel.prepend(adminWorkspace.renderSubbar());
    if (viewKey !== lastViewKey) {
      panel.classList.add("panel-enter");
      lastViewKey = viewKey;
    }
    content.replaceChildren(panel);
    return;
  }

  const viewKey = "client:services";
  const animateEnter = viewKey !== lastViewKey;
  lastViewKey = viewKey;
  const panel = el("section", animateEnter ? "panel panel-enter" : "panel", servicesView(data));
  content.replaceChildren(panel);
}

function applyJoinLink() {
  if (!status || busy || !pendingJoin) return;
  const address = pendingJoin;
  pendingJoin = undefined;
  void act({ op: "join", address }, () => { joining = false; });
}

function deepLink(urls: string[] | null) {
  const link = urls?.find((url) => url.startsWith("fesen://join?"));
  if (!link) return;
  pendingJoin = link;
  applyJoinLink();
}

async function boot() {
  syncBrand(true);
  void initDesktopWindowSize();
  i18n.onChange(() => {
    refreshSiteDialogI18n();
    syncBrand();
    render(true);
  });
  try { status = await request({ op: "attach" }, !mac); }
  catch (error) { message = typeof error === "string" ? error : "not_connected"; }
  syncBrand(true);
  brandBooted = true;
  render(true);
  void getCurrent().then(deepLink).catch(() => {});
  void onOpenUrl(deepLink).catch(() => {});
  const pollOnce = async () => {
    if (busy || polling || (mac && !status) || (!desktop && document.visibilityState === "hidden")) return;
    polling = true;
    try {
      const next = await request({ op: "status", ui: true }, false);
      if (!busy) {
        status = next;
        render();
      }
    } catch {
      if (mac && status) {
        try {
          const reattached = await request({ op: "attach" }, false);
          if (!busy) {
            status = reattached;
            render();
            return;
          }
        } catch {}
      }
      if (status?.online || status?.running) {
        status = { ...status, running: false, online: false, active: [], active_flows: 0, services: [], virtual_ips: [], aliases: {} };
        render();
      }
    } finally { polling = false; }
  };
  window.addEventListener("focus", () => { void pollOnce(); });
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") void pollOnce();
  });
  window.setInterval(() => { void pollOnce(); }, 1800);
}

void boot();
