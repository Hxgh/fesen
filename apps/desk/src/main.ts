import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { getCurrent, onOpenUrl } from "@tauri-apps/plugin-deep-link";
import { failure, getLocale, setLocale, isZh, t, type Locale } from "./i18n";
import { checkSiteDialog, refreshSiteDialogI18n, showSiteDialog } from "./site-dialog";
import { createTrigram, icon } from "../../../packages/ui/icons.js";
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
type Offering = Share & { owner: string; member: string };
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
};
type Request = Record<string, unknown> & { op: string };
type Reply = { data?: Status; error?: string };
type BrandState = "idle" | "connecting" | "active";

document.documentElement.lang = getLocale();
const mac = /Macintosh/.test(navigator.userAgent);

type Theme = "system" | "light" | "dark";
type CloseBehavior = "tray" | "quit";

function getTheme(): Theme {
  try {
    const v = localStorage.getItem("fesen_theme");
    if (v === "light" || v === "dark" || v === "system") return v;
  } catch {}
  return "system";
}

function applyTheme(theme: Theme = getTheme()) {
  const root = document.documentElement;
  root.setAttribute("data-theme", theme);
  const isDark =
    theme === "dark" ||
    (theme === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  root.classList.toggle("dark", isDark);
}

function setTheme(theme: Theme) {
  try {
    localStorage.setItem("fesen_theme", theme);
  } catch {}
  applyTheme(theme);
}

window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
  if (getTheme() === "system") applyTheme("system");
});

applyTheme(getTheme());

function getCloseBehavior(): CloseBehavior {
  try {
    const v = localStorage.getItem("fesen_close_behavior");
    if (v === "quit" || v === "tray") return v;
  } catch {}
  return "tray";
}

async function syncCloseBehavior(behavior: CloseBehavior): Promise<boolean> {
  try {
    localStorage.setItem("fesen_close_behavior", behavior);
  } catch {}
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
let tab: "group" | "mine" = "group";
let tabOwner = "";
let joining = false;
let busy = false;
let polling = false;
let message = "";
let applied = "";
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
  document.title = name;
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

function tool(name: Parameters<typeof icon>[0], title: string, action: () => void) {
  const b = button("", action, "button icon-button");
  b.append(icon(name));
  b.title = title;
  b.setAttribute("aria-label", title);
  return b;
}

function empty(text: string, action?: HTMLButtonElement) {
  return el("div", "empty", el("p", "", text), ...(action ? [action] : []));
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
  errorLine.hidden = !code;
  errorLine.textContent = code ? failure(code) : "";
  const dialogError = editor.querySelector<HTMLParagraphElement>("[role=alert]");
  if (dialogError) {
    dialogError.hidden = !message;
    dialogError.textContent = message ? failure(message) : "";
  }
};

async function request(request: Request, launch: boolean): Promise<Status> {
  const reply = await invoke<Reply>("core_command", { request, launch });
  if (!reply.data) throw reply.error ?? "internal";
  return reply.data;
}

async function act(body: Request, done?: () => void) {
  if (busy) return;
  busy = true;
  message = "";
  showError();
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
    syncBrand();
    applyJoinLink();
  }
}

const ranges = <T>(spans: Span<T>[]) => spans.map(({ start, end }) =>
  start === end ? String(start) : `${start}–${end}`).join(isZh() ? "、" : ", ");
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

const single = <T>(spans: Span<T>[]) => spans.length === 1 && spans[0]!.start === spans[0]!.end;
const singleIp = (s: Share) => single(s.rules.ips) ? s.rules.ips[0]!.start : null;
const singlePort = (s: Share) => single(s.rules.ports) ? s.rules.ports[0]!.start : null;
const effectiveKind = (s: Share): ServiceKind => s.kind ?? (s.http ? "http" : "tcp");
const kindBadgeText = (kind: ServiceKind) => kind.toUpperCase();
const osLabel = (os?: HostOs | null) => os === "windows" ? "Windows" : os === "linux" ? "Linux" : "";

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

type ListEntry<T extends Share> =
  | { grouped: true; ip: string; items: T[] }
  | { grouped: false; item: T };

function groupByHost<T extends Share>(items: T[], keyOf: (item: T) => string | null): ListEntry<T>[] {
  const entries: ListEntry<T>[] = [];
  const buckets = new Map<string, { grouped: true; ip: string; items: T[] }>();
  for (const item of items) {
    const ip = singleIp(item);
    const key = ip ? keyOf(item) : null;
    if (!ip || !key) {
      entries.push({ grouped: false, item });
      continue;
    }
    let bucket = buckets.get(key);
    if (!bucket) {
      bucket = { grouped: true, ip, items: [] };
      buckets.set(key, bucket);
      entries.push(bucket);
    }
    bucket.items.push(item);
  }
  for (const entry of entries) {
    if (entry.grouped && entry.items.length > 1) {
      entry.items.sort((a, b) => (a.rules.ports[0]?.start ?? 0) - (b.rules.ports[0]?.start ?? 0));
    }
  }
  return entries;
}

function resolveHostOs(items: Share[], probedOs: (HostOs | undefined)[] = []): HostOs | null {
  const hasOsCapable = items.some(s => effectiveKind(s) === "ssh" || effectiveKind(s) === "rdp");
  if (!hasOsCapable) return null;
  for (const s of items) if (s.host_os === "windows") return "windows";
  for (const os of probedOs) if (os === "windows") return "windows";
  for (const s of items) if (effectiveKind(s) === "rdp") return "windows";
  for (const s of items) if (s.host_os === "linux") return "linux";
  for (const os of probedOs) if (os === "linux") return "linux";
  return null;
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
  for (const service of items) {
    const ip = singleIp(service);
    const port = singlePort(service);
    if (ip && port) void testConnection(service, ip, port);
  }
}

function chooseTarget(service: Share, title: string, action: (ip: string, port: number) => void,
  previous?: Probe, submitLabel = t("test")) {
  const { ips, ports } = service.rules;
  if (single(ips) && single(ports)) {
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

type IconName = Parameters<typeof icon>[0];
type MenuEntry =
  | { sep: true }
  | { icon: IconName; label: string; danger?: boolean; onSelect: () => void };

let activeMenu: { el: HTMLDivElement; anchor: HTMLButtonElement; cleanup: () => void } | null = null;

function closeMenu() {
  if (!activeMenu) return;
  activeMenu.cleanup();
  activeMenu.anchor.setAttribute("aria-expanded", "false");
  activeMenu.el.remove();
  activeMenu = null;
}

function openMenu(anchor: HTMLButtonElement, entries: MenuEntry[]) {
  if (activeMenu?.anchor === anchor) {
    closeMenu();
    return;
  }
  closeMenu();
  const menu = el("div", "popover-menu");
  menu.setAttribute("role", "menu");
  const buttons: HTMLButtonElement[] = [];
  for (const entry of entries) {
    if ("sep" in entry) {
      const sep = el("div", "menu-sep");
      sep.setAttribute("role", "separator");
      menu.append(sep);
      continue;
    }
    const item = button(entry.label, () => {
      closeMenu();
      entry.onSelect();
    }, `menu-item${entry.danger ? " danger" : ""}`);
    item.setAttribute("role", "menuitem");
    item.prepend(icon(entry.icon));
    buttons.push(item);
    menu.append(item);
  }
  document.body.append(menu);
  const rect = anchor.getBoundingClientRect();
  const mw = menu.offsetWidth;
  const mh = menu.offsetHeight;
  const left = Math.max(12, Math.min(rect.right - mw, window.innerWidth - mw - 12));
  const flipUp = rect.bottom + mh + 12 > window.innerHeight && rect.top - mh - 12 >= 0;
  const top = flipUp ? rect.top - mh - 6 : rect.bottom + 6;
  menu.style.left = `${Math.round(left)}px`;
  menu.style.top = `${Math.round(top)}px`;
  menu.setAttribute("data-origin", flipUp ? "bottom" : "top");
  anchor.setAttribute("aria-expanded", "true");

  const onPointerDown = (event: Event) => {
    const target = event.target as Node | null;
    if (target && (menu.contains(target) || anchor.contains(target))) return;
    closeMenu();
  };
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault();
      closeMenu();
      anchor.focus();
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const cur = buttons.indexOf(document.activeElement as HTMLButtonElement);
      const next = event.key === "ArrowDown"
        ? (cur + 1) % buttons.length
        : (cur - 1 + buttons.length) % buttons.length;
      buttons[next]?.focus();
    }
  };
  const onDismiss = (event: Event) => {
    const target = event.target as Node | null;
    if (event.type === "scroll" && target && menu.contains(target)) return;
    closeMenu();
  };

  window.addEventListener("pointerdown", onPointerDown, true);
  window.addEventListener("keydown", onKeyDown, true);
  window.addEventListener("resize", onDismiss);
  window.addEventListener("scroll", onDismiss, true);
  activeMenu = {
    el: menu,
    anchor,
    cleanup: () => {
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("resize", onDismiss);
      window.removeEventListener("scroll", onDismiss, true);
    },
  };
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
      probes.set(`${owner}:${service.id}`, { ip, port, access, kind: "http",
        result: result === "visible" ? { visible: true } : result });
      render(true);
    });
  });
}

function meaningfulHostName(name: string, ip: string, os: HostOs | null): string {
  const trimmed = name.trim();
  if (!trimmed || trimmed === ip) return "";
  const tag = osLabel(os);
  if (tag && trimmed === `${tag} · ${ip}`) return "";
  return trimmed;
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
    openMenu(more, [...primary, { sep: true }, ...secondary]);
  });
  more.setAttribute("aria-haspopup", "menu");
  more.setAttribute("aria-expanded", "false");
  return more;
}

function endpoint(ips: Span<string>[], ports: Span<number>[]) {
  return `${ranges(ips)}${single(ips) && single(ports) ? ":" : " · "}${ranges(ports)}`;
}

function probeVisual(service: Offering, data: Status, routing: boolean) {
  const kind = effectiveKind(service);
  const ips = addresses(service.rules.ips, data.virtual_ips);
  const one = single(service.rules.ips) && single(service.rules.ports);
  const probe = probes.get(serviceKey(service));
  const result = probe?.result;
  const http = result?.httpStatus;
  const pending = !!probe && !result;
  const conflict = ips.virtual.length > 0 || result?.error === "conflict";
  const failed = !!result?.error || (http !== undefined && (http < 200 || http >= 400));
  const state = pending ? "testing" : conflict ? "overlap" : !result ? "unchecked"
    : failed ? "unavailable" : http !== undefined ? "http" : "available";
  const caption = state === "http"
    ? (result?.rttMs !== undefined ? `${http} · ${result.rttMs}ms` : `${http}`)
    : state === "available"
      ? (result?.visible
        ? t("page_visible")
        : result?.rttMs !== undefined ? `${result.rttMs}ms` : t("reachable"))
      : t(state);
  const tone = (state === "available" || state === "http") && routing ? "good"
    : state === "overlap" ? "warn"
    : state === "unavailable" ? "bad" : "";
  const badge = button(caption, () => chooseTarget(service, t("test_connection"),
    (ip, port) => void testConnection(service, ip, port), probe), `probe-badge badge ${tone}`);
  badge.title = ips.virtual.length ? t("local_overlap") : t("reprobe");
  badge.setAttribute("aria-label", `${t("refresh_status")}：${caption}`);
  badge.setAttribute("data-pending", String(pending));
  badge.disabled = pending || !data.online;

  let note: HTMLElement | null = null;
  if (probe && result && (!one || conflict || failed)) {
    const outcome = result.error ? failure(result.error)
      : result.visible ? t("page_visible")
      : http !== undefined ? `HTTP ${http}`
      : kind === "tcp" ? t("reachable") : kindBadgeText(kind);
    note = el("p", failed ? "probe-note problem" : "probe-note", one ? outcome : `${probe.access}:${probe.port} · ${outcome}`);
    note.setAttribute("role", "status");
  }
  return { ips, badge, note, pending, result };
}

function serviceRow(
  address: string,
  subtitle: string,
  tags: HTMLElement[],
  ...controls: HTMLElement[]
) {
  const actionsWrap = el("div", "service-actions", ...controls);
  return el("article", "service-row",
    el("div", "service-main",
      el("div", "host-title-line", el("p", "endpoint", address), ...tags),
      ...(subtitle ? [el("div", "service-meta", el("span", "service-name", subtitle))] : [])
    ),
    actionsWrap
  );
}

function groupSingleRow(service: Offering, data: Status, routing: boolean) {
  const kind = effectiveKind(service);
  const label = data.labels.find((l) => l.service === service.id);
  const { ips, badge, note, result } = probeVisual(service, data, routing);
  const address = endpoint([...ips.original, ...ips.virtual], service.rules.ports);
  const rawName = label?.remark || service.name;
  const ip = singleIp(service);
  const os = resolveHostOs([service], [result?.hostOs]);
  const displayName = ip ? meaningfulHostName(rawName, ip, os) : rawName;
  const alias = label?.alias && data.aliases[label.alias] ? label.alias : null;
  const subtitle = [displayName, alias].filter(Boolean).join(" · ");
  const tags: HTMLElement[] = [];
  if (os) tags.push(el("span", "host-os-tag", osLabel(os)));
  if (kind !== "tcp") tags.push(el("span", "proto-tag", kindBadgeText(kind)));
  const row = serviceRow(
    address,
    subtitle,
    tags,
    badge,
    rowActions(service, rawName, service.owner, routing, () => customize(service, label, routing))
  );
  if (ips.virtual.length) {
    row.append(el("p", "probe-note", `${t("original_ip")} ${ranges(service.rules.ips)}`));
  }
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
  const hostOs = resolveHostOs(items, probed.map(p => p?.result?.hostOs));
  const cleanName = meaningfulHostName(rawHostName, ip, hostOs);

  const testAll = tool("refresh", t("test_host"), () => testHostServices(items));
  testAll.disabled = anyPending || !data.online || busy;
  if (anyPending) testAll.classList.add("spinning");

  const titleParts: HTMLElement[] = [el("p", "endpoint", access)];
  if (hostOs) titleParts.push(el("span", "host-os-tag", osLabel(hostOs)));
  if (overlapped) titleParts.push(el("span", "badge warn", t("overlap")));

  const metaParts: (HTMLElement | string)[] = [];
  if (cleanName) metaParts.push(el("span", "service-name", cleanName));
  if (alias) {
    if (metaParts.length) metaParts.push(el("span", "meta-sep", "·"));
    metaParts.push(el("span", "mono", alias));
  }
  if (overlapped) {
    if (metaParts.length) metaParts.push(el("span", "meta-sep", "·"));
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
    const { badge, note } = probeVisual(service, data, routing);
    const portText = `:${ranges(service.rules.ports)}`;
    const rowRawName = label?.remark || service.name;
    const subName = rowRawName !== rawHostName ? meaningfulHostName(rowRawName, ip, hostOs) : "";
    const row = el("div", "port-row",
      el("div", "port-main",
        el("span", "port-num", portText),
        el("span", "proto-tag", kindBadgeText(kind)),
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

function groupView(data: Status) {
  if (!data.services.length) return empty(t(data.online ? "no_services" : "group_disconnected"));
  const routing = data.system_routing !== false;
  const entries = groupByHost(data.services, (s) => `${s.owner}:${singleIp(s)}`);
  return el("div", "service-list", ...entries.map((entry) => {
    if (!entry.grouped || entry.items.length === 1) {
      const item = entry.grouped ? entry.items[0]! : entry.item;
      return groupSingleRow(item, data, routing);
    }
    return groupHostCard(entry.ip, entry.items, data, routing);
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
  if (onChange) {
    select.addEventListener("change", () => onChange(select.value as ServiceKind));
  }
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

  const existingForIp = (ip: string) => {
    const out: Share[] = [];
    for (const s of status?.shares ?? []) {
      if (singleIp(s) === ip && singlePort(s) !== null) out.push(s);
    }
    return out;
  };

  if (ipVal) {
    const existing = existingForIp(ipVal);
    items = existing
      .map(s => {
        const port = singlePort(s)!;
        const kind = effectiveKind(s);
        const osHint = kind === "rdp" ? "windows" : (kind === "ssh" ? s.host_os : undefined);
        return {
          spec: String(port),
          port,
          kind,
          checked: true,
          saved: true,
          osHint,
        };
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
      const hint = p.os_hint ?? (p.kind === "rdp" ? "windows" : undefined);
      if (onlySpecific) {
        if (!prev) continue;
        prev.probing = false;
        prev.open = p.open;
        prev.rttMs = p.rtt_ms;
        prev.httpStatus = p.http_status;
        prev.detail = p.detail;
        if (p.open) {
          prev.kind = p.kind;
          prev.osHint = hint;
        }
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
    if (!isUsableHostIp(ip)) {
      localError = t("invalid_host_ip");
      draw();
      return;
    }
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
    if (!parsed || !parsed.length) {
      localError = t("invalid_port_input");
      draw();
      return false;
    }
    const ip = ipVal.trim();
    const singleHost = isUsableHostIp(ip);
    const existingSet = new Set(singleHost ? existingForIp(ip).map(s => singlePort(s)!) : []);
    const addedSingle: number[] = [];
    for (const token of parsed) {
      const prev = items.find(i => i.spec === token.spec);
      if (prev) {
        prev.checked = true;
      } else {
        if (items.length >= 10) {
          localError = t("invalid_port_input");
          draw();
          return false;
        }
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
        .then((reply) => {
          if (reply.host_probe) mergeProbeResults(reply.host_probe, true);
        })
        .catch(() => {
          for (const port of addedSingle) {
            const entry = items.find(i => i.port === port);
            if (entry) { entry.probing = false; entry.open = false; }
          }
        })
        .finally(() => {
          if (dialogOpen && editor.open) draw();
        });
    }
    return true;
  }

  function itemMetaText(item: DraftPort): string {
    if (item.probing) return t("port_checking");
    const parts: string[] = [];
    if (item.saved) parts.push(t("already_added"));
    if (item.open === false) parts.push(t("port_offline"));
    else if (item.httpStatus) parts.push(String(item.httpStatus));
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
    if (emptyDetect && !items.length) {
      portsBox.append(el("div", "ports-empty-note", t("host_none_open")));
    }
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

      row.append(
        cb,
        el("span", "host-service-port", `:${item.spec}`),
        kindSel,
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
        if (trimmed) {
          portDraft = trimmed;
          commitPortInput(true);
          return;
        }
      }
      updateSubmit();
    });
    portInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        commitPortInput(true);
      }
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
        pubCheck.addEventListener("change", () => {
          publishNow = pubCheck.checked;
          updateSubmit();
        });
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

function makeToggle(checked: boolean, label: string, disabled: boolean, onToggle: () => void, fraction?: string) {
  const sw = button("", onToggle, "switch");
  if (fraction) sw.append(el("span", "switch-fraction", fraction));
  const track = el("span", "switch-track");
  track.setAttribute("aria-hidden", "true");
  sw.append(track);
  sw.setAttribute("role", "switch");
  sw.setAttribute("aria-label", label);
  sw.setAttribute("aria-checked", String(checked));
  if (fraction) sw.setAttribute("data-partial", "true");
  sw.disabled = disabled;
  return sw;
}

function mineSingleRow(s: Share, data: Status) {
  const kind = effectiveKind(s);
  const active = data.active.includes(s.id);
  const publish = makeToggle(active, `${t("publish")} · ${s.name}`, !data.online && !active,
    () => void act({ op: "share", id: s.id, enabled: !active }));
  const address = endpoint(s.rules.ips, s.rules.ports);
  const ip = singleIp(s);
  const os = resolveHostOs([s]);
  const subtitle = ip ? meaningfulHostName(s.name, ip, os) : s.name;
  const tags: HTMLElement[] = [];
  if (os) tags.push(el("span", "host-os-tag", osLabel(os)));
  if (kind !== "tcp") tags.push(el("span", "proto-tag", kindBadgeText(kind)));
  return serviceRow(
    address,
    subtitle,
    tags,
    publish,
    rowActions(s, s.name, undefined, true, shareEditAction(s, address))
  );
}

function mineHostCard(ip: string, items: Share[], data: Status) {
  const rawHostName = items[0]!.name;
  const hostOs = resolveHostOs(items);
  const cleanName = meaningfulHostName(rawHostName, ip, hostOs);
  const activeCount = items.filter(s => data.active.includes(s.id)).length;
  const allActive = activeCount === items.length;
  const partial = activeCount > 0 && activeCount < items.length;

  const addPortBtn = tool("plus", t("manage_host_ports"), () => openAddShare({ ip, name: cleanName }));
  const master = makeToggle(
    allActive,
    `${t("publish")} · ${cleanName || ip}`,
    !data.online && activeCount === 0,
    () => void act({ op: "share_many", ids: items.map(s => s.id), enabled: !allActive }),
    partial ? `${activeCount}/${items.length}` : undefined
  );

  const titleParts: HTMLElement[] = [el("p", "endpoint", ip)];
  if (hostOs) titleParts.push(el("span", "host-os-tag", osLabel(hostOs)));

  // Exact column alignment: [master switch] aligns directly over port switches, [+] aligns over [⋯].
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
    const publish = makeToggle(active, `${t("publish")} · ${s.name}`, !data.online && !active,
      () => void act({ op: "share", id: s.id, enabled: !active }));

    const portText = `:${ranges(s.rules.ports)}`;
    const subName = s.name !== rawHostName ? meaningfulHostName(s.name, ip, hostOs) : "";
    const address = endpoint(s.rules.ips, s.rules.ports);
    const row = el("div", "port-row",
      el("div", "port-main",
        el("span", "port-num", portText),
        el("span", "proto-tag", kindBadgeText(kind)),
        ...(subName ? [el("span", "port-subname", subName)] : [])
      ),
      el("div", "port-actions",
        publish,
        rowActions(s, s.name, undefined, true, shareEditAction(s, address))
      )
    );
    return el("div", "port-row-wrap", row);
  }));

  return el("article", "host-card", head, portsWrap);
}

function mineView(data: Status) {
  if (!data.shares.length) return empty(t("no_presets"), addShare(true));
  const entries = groupByHost(data.shares, (s) => singleIp(s));
  return el("div", "service-list", ...entries.map((entry) => {
    if (!entry.grouped || entry.items.length === 1) {
      const item = entry.grouped ? entry.items[0]! : entry.item;
      return mineSingleRow(item, data);
    }
    return mineHostCard(entry.ip, entry.items, data);
  }));
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

  function renderSettings() {
    settingsDialog.replaceChildren();

    const heading = el("h2", "settings-title", t("settings"));
    heading.id = "settings-heading";
    const closeBtn = tool("close", t("close"), () => settingsDialog.close());
    const header = el("div", "settings-header", heading, closeBtn);

    const body = el("div", "settings-body");

    const themeSeg = el("div", "segmented");
    for (const [id, key] of [["system", "theme_system"], ["light", "theme_light"], ["dark", "theme_dark"]] as const) {
      const btn = button(t(key), () => {
        setTheme(id);
        renderSettings();
      }, id === getTheme() ? "active" : "");
      btn.setAttribute("aria-pressed", String(id === getTheme()));
      themeSeg.append(btn);
    }
    body.append(el("div", "settings-row", el("span", "settings-label", t("appearance")), themeSeg));

    const langSeg = el("div", "segmented");
    for (const [id, key] of [["zh-CN", "lang_zh"], ["en", "lang_en"]] as const) {
      const btn = button(t(key), () => {
        setLocale(id);
        refreshSiteDialogI18n();
        syncBrand();
        renderSettings();
        render(true);
      }, id === getLocale() ? "active" : "");
      btn.setAttribute("aria-pressed", String(id === getLocale()));
      langSeg.append(btn);
    }
    body.append(el("div", "settings-row", el("span", "settings-label", t("language")), langSeg));

    if (mac) {
      const closeSeg = el("div", "segmented");
      for (const [id, key] of [["tray", "close_to_tray"], ["quit", "close_quit"]] as const) {
        const btn = button(t(key), () => {
          if (currentBehavior === id) return;
          currentBehavior = id;
          saveError = false;
          void syncCloseBehavior(id).then((ok) => {
            saveError = !ok;
            if (settingsDialog.open) renderSettings();
          });
          renderSettings();
        }, id === currentBehavior ? "active" : "");
        btn.setAttribute("aria-pressed", String(id === currentBehavior));
        closeSeg.append(btn);
      }
      body.append(el("div", "settings-row", el("span", "settings-label", t("on_window_close")), closeSeg));
      if (saveError) {
        const fb = el("p", "feedback error", t("menu_save_failed"));
        fb.setAttribute("role", "alert");
        body.append(fb);
      }
    }

    if (status?.member && status.server) {
      const acctInfo = el("span", "settings-account-info", `${status.member} · ${new URL(status.server).host}`);
      const logoutBtn = button(t("logout"), () => {
        settingsDialog.close();
        void act({ op: "logout" });
      }, "button danger");
      body.append(
        el("div", "settings-divider"),
        el("div", "settings-account", acctInfo, logoutBtn),
        ...(status.system_routing === false ? [el("p", "settings-note", t("routing_unavailable"))] : [])
      );
    }

    const footer = el("div", "settings-footer",
      el("span", "settings-version", appVersion ? `${t("brand")} v${appVersion}` : t("brand")));

    settingsDialog.append(header, body, footer);
  }

  settingsDialog.tabIndex = -1;
  renderSettings();
  settingsDialog.showModal();
  settingsDialog.focus();
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
  b.append(icon(symbol));
  b.title = tip;
  b.setAttribute("aria-label", tip);
  b.setAttribute("aria-pressed", String(running));
  return b;
}

function render(force = false) {
  checkSiteDialog();
  showError();
  syncBrand(!brandBooted);
  const data = status;
  const owner = data?.member ? JSON.stringify([data.server, data.member]) : "";
  if (owner !== tabOwner) { tabOwner = owner; tab = data?.system_routing === false ? "mine" : "group"; }
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
    ...data, error: null, background: null, active_flows: null, probe: null, host_probe: null,
  }, tab, joining]);
  if (!force && applied === next) return;
  if (!force && !data?.member && content.contains(document.activeElement)) return;
  applied = next;
  closeMenu();
  actions.replaceChildren();
  const settingsBtn = button("", () => openSettings(), "tool-btn icon-only");
  settingsBtn.append(icon("settings"));
  settingsBtn.title = t("settings");
  settingsBtn.setAttribute("aria-label", t("settings"));
  settingsBtn.setAttribute("aria-haspopup", "dialog");
  actions.append(settingsBtn);
  const loggedIn = !!data?.member;
  brandNameEl.hidden = loggedIn;
  navTabs.hidden = !loggedIn;
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
    if (!data.server || joining) {
      content.replaceChildren(el("section", "signin", form((values) => {
        void act({ op: "join", address: str(values, "address") }, () => { joining = false; });
      }, el("h2", "form-title", t("join_group")),
      field(t("join_link"), "address", data.server, { required: true, maxLength: 1024, placeholder: "fesen://join…" }),
      data.server
        ? el("div", "signin-actions",
            button(t("cancel"), () => { joining = false; render(true); }, "button"),
            submit(t("save")))
        : submit(t("save")))));
    } else {
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
        submit(t("login")))));
    }
    return;
  }
  actions.prepend(
    ...(tab === "mine" && data.shares.length ? [addShare()] : []),
    buildConnectionControl(data)
  );
  navTabs.setAttribute("aria-label", t("services"));
  navTabs.replaceChildren(...(["group", "mine"] as const).map((id) => {
    const b = button(t(id), () => {
      if (tab === id) return;
      tab = id;
      render(true);
      navTabs.querySelector<HTMLButtonElement>('[aria-current="page"]')?.focus();
    }, "");
    b.append(el("span", "count", String(id === "group" ? data.services.length : data.shares.length)));
    b.setAttribute("aria-current", id === tab ? "page" : "false");
    return b;
  }));
  const panel = el("section", "panel", tab === "group" ? groupView(data) : mineView(data));
  if (tab === "group" && data.services.length && data.system_routing === false) {
    panel.append(el("p", "routing-note", t("routing_unavailable")));
  }
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
  try { status = await request({ op: "attach" }, !mac); }
  catch (error) { message = typeof error === "string" ? error : "not_connected"; }
  syncBrand(true);
  brandBooted = true;
  render(true);
  void getCurrent().then(deepLink).catch(() => {});
  void onOpenUrl(deepLink).catch(() => {});
  window.setInterval(async () => {
    if (busy || polling || (mac && !status) || document.visibilityState === "hidden") return;
    polling = true;
    try {
      const next = await request({ op: "status", ui: true }, false);
      if (!busy) {
        status = next;
        render();
      }
    } catch {
      if (status?.online || status?.running) {
        status = { ...status, running: false, online: false, active: [], active_flows: 0, services: [], virtual_ips: [], aliases: {} };
        render();
      }
    } finally { polling = false; }
  }, 1800);
}

void boot();
