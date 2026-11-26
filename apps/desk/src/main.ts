import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrent, onOpenUrl } from "@tauri-apps/plugin-deep-link";
import { failure, i18n, isZh, t } from "./i18n";
import { checkSiteDialog, refreshSiteDialogI18n, showSiteDialog } from "./site-dialog";
import { createIconButton, createTrigram, icon, osBadge } from "../../../packages/ui/icons.js";
import { createScale, createTheme } from "../../../packages/ui/theme.js";
import { closePopoverMenu, openPopoverMenu } from "../../../packages/ui/popover.js";
import { openSettingsModal } from "../../../packages/ui/settings.js";
import { createAdminWorkspace } from "../../../packages/ui/admin-view.js";
import {
  deviceOsLabel,
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
type DeviceOs = "macos" | "windows" | "android" | "linux";
type Share = {
  id: string;
  name: string;
  http?: boolean;
  kind?: ServiceKind;
  host_os?: HostOs;
  rules: { ips: Span<string>[]; ports: Span<number>[] };
};
type Offering = Share & { owner: string; member: string; device?: string; device_os?: DeviceOs };
type Label = { service: string; remark: string; alias: string | null; ip: string | null };
type Mapping = { original: Span<string>; virtual_ip: Span<string> };
type LocalMapping = { service: string; target_ip: string; target_port: number; local_port: number };
type LocalForwardStatus = LocalMapping & { active: boolean; error?: string; suggested_port?: number };
type DeviceProfile = {
  machine_id: string;
  name: string;
  os: DeviceOs;
  shares: Share[];
  forwards: LocalMapping[];
  updated_at: number;
};
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
  cancelled?: boolean;
};
type Status = {
  server: string;
  member: string | null;
  machine_id?: string;
  device_os?: DeviceOs;
  device_name?: string;
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
  service_virtual_ips?: Record<string, Mapping[]>;
  aliases: Record<string, string>;
  forwards?: LocalForwardStatus[];
  cloud_profiles?: DeviceProfile[];
  restore_prompt?: DeviceProfile | null;
  error: string | null;
  auto_connect?: boolean;
  auto_share?: boolean;
  cloud_backup?: boolean;
  auto_share_blocked?: boolean;
  suggested_port?: number;
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
  | { icon: IconName; label: string; danger?: boolean; active?: boolean; onSelect: () => void };

const effectiveKind = (s: Share): ServiceKind => rawEffectiveKind(s) as ServiceKind;
const mac = /Macintosh/.test(navigator.userAgent);
const desktop = !/Android/i.test(navigator.userAgent);
if (desktop) document.documentElement.classList.add("os-desktop");
if (mac) document.documentElement.classList.add("os-macos");
const theme = createTheme("fesen_theme");
const scale = createScale("fesen_scale");

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
let lastAutoShareBlocked = false;

type ViewMode = "grouped" | "flat";
const VIEW_MODE_KEY = "fesen_view_mode";
function readViewMode(): ViewMode {
  try {
    const v = localStorage.getItem(VIEW_MODE_KEY);
    if (v === "flat" || v === "grouped") return v;
  } catch {}
  return "grouped";
}
let viewMode: ViewMode = readViewMode();
let searchQuery = "";

const searchInput = document.createElement("input");
searchInput.className = "search-input";
Object.assign(searchInput, {
  type: "text",
  inputMode: "search",
  spellcheck: false,
  maxLength: 120,
  autocomplete: "off",
});
const searchCountEl = document.createElement("span");
searchCountEl.className = "search-count";
const searchClearBtn = document.createElement("button");
searchClearBtn.type = "button";
searchClearBtn.className = "search-clear";
searchClearBtn.append(icon("close"));
const searchInlineBox = document.createElement("div");
searchInlineBox.className = "search-inline";
searchInlineBox.append(icon("search"), searchInput, searchCountEl, searchClearBtn);

const viewSegHost = document.createElement("div");
viewSegHost.className = "view-seg";
viewSegHost.setAttribute("role", "group");

const listToolbar = document.createElement("div");
listToolbar.className = "list-toolbar";
listToolbar.append(searchInlineBox, viewSegHost);

const restoreFlowHost = document.createElement("div");
const mainFlowSection = document.createElement("div");
mainFlowSection.className = "flow-section";
const emptyFlowHost = document.createElement("div");
const serviceFlowRoot = document.createElement("div");
serviceFlowRoot.className = "service-flow";
serviceFlowRoot.append(restoreFlowHost, mainFlowSection, emptyFlowHost);

const servicesPanel = document.createElement("section");
servicesPanel.className = "panel";
servicesPanel.append(listToolbar, serviceFlowRoot);

const COLLAPSE_STORE_KEY = "fesen_collapsed";
let collapsedScope = "";
let collapsed = new Set<string>();

function loadCollapsed(scope: string) {
  if (scope === collapsedScope) return;
  collapsedScope = scope;
  collapsed = new Set<string>();
  if (!scope) return;
  try {
    const raw = localStorage.getItem(COLLAPSE_STORE_KEY);
    if (!raw) return;
    const map = JSON.parse(raw);
    const arr = map && typeof map === "object" ? map[scope] : null;
    if (Array.isArray(arr)) {
      for (const item of arr) {
        if (typeof item === "string") collapsed.add(item);
      }
    }
  } catch {}
}

function saveCollapsed() {
  if (!collapsedScope) return;
  try {
    const raw = localStorage.getItem(COLLAPSE_STORE_KEY);
    const map: Record<string, string[]> = raw ? (JSON.parse(raw) || {}) : {};
    if (collapsed.size) {
      map[collapsedScope] = [...collapsed].slice(-200);
    } else {
      delete map[collapsedScope];
    }
    const keys = Object.keys(map);
    if (keys.length > 16) {
      for (const oldKey of keys.slice(0, keys.length - 16)) delete map[oldKey];
    }
    localStorage.setItem(COLLAPSE_STORE_KEY, JSON.stringify(map));
  } catch {}
}

function isCollapsed(key: string): boolean {
  return collapsed.has(key);
}

function toggleCollapsed(key: string) {
  if (collapsed.has(key)) collapsed.delete(key);
  else collapsed.add(key);
  saveCollapsed();
  render(true);
}

function collapseToggle(key: string, expanded: boolean, cls: string, ...children: (Node | string)[]) {
  const chev = icon("chevron");
  chev.classList.add("collapse-chevron");
  const b = button("", () => toggleCollapsed(key), `collapse-toggle ${cls}`.trim());
  b.dataset.collapseKey = key;
  b.setAttribute("aria-expanded", String(expanded));
  b.append(chev, ...children);
  return b;
}

function setViewMode(next: ViewMode) {
  if (viewMode === next) return;
  viewMode = next;
  try { localStorage.setItem(VIEW_MODE_KEY, next); } catch {}
  render(true);
}

function parseSearchTokens(q: string): string[] {
  return q.trim().toLowerCase().split(/\s+/).filter(Boolean);
}

function matchesTokens(haystack: string, tokens: string[]): boolean {
  return tokens.every((tok) => haystack.includes(tok));
}

function remoteSearchHaystack(s: Offering, data: Status): string {
  const label = data.labels.find((l) => l.service === s.id);
  const kind = effectiveKind(s);
  const os = resolveHostOs([s]);
  const devOs = deviceOsLabel(s.device_os);
  const ips = addresses(s.rules.ips, mapsForService(s.id, data));
  const endpointOrig = endpoint(s.rules.ips, s.rules.ports);
  const endpointAccess = endpoint([...ips.original, ...ips.virtual], s.rules.ports);
  const fwds = forwardsOfService(s.id, data);
  return [
    s.name,
    label?.remark ?? "",
    label?.alias ?? "",
    s.member,
    s.member === data.member ? `${t("source_self")} 本账号 my account` : "",
    s.device ?? "",
    devOs,
    osLabel(os),
    kind,
    endpointOrig,
    endpointAccess,
    inputRanges(s.rules.ips),
    inputRanges(s.rules.ports),
    ...fwds.map((f) => `127.0.0.1:${f.local_port} ${f.local_port} ${f.target_ip}:${f.target_port}`),
  ].join(" ").toLowerCase();
}

function localSearchHaystack(s: Share, data: Status): string {
  const kind = effectiveKind(s);
  const os = resolveHostOs([s]);
  const active = data.active.includes(s.id);
  return [
    s.name,
    data.device_name ?? "",
    `${t("badge_local")} ${t("source_self")} 本机 本账号 local my account`,
    osLabel(os),
    kind,
    endpoint(s.rules.ips, s.rules.ports),
    inputRanges(s.rules.ips),
    inputRanges(s.rules.ports),
    active ? `${t("published")} 共享中 sharing` : "",
  ].join(" ").toLowerCase();
}

function syncSearchBar(_data?: Status, matchCount?: number, totalCount?: number) {
  const hasQuery = !!searchQuery.trim();
  searchInput.placeholder = t("search_services");
  searchInput.setAttribute("aria-label", t("search_services"));
  searchClearBtn.title = t("clear_search");
  searchClearBtn.setAttribute("aria-label", t("clear_search"));
  searchClearBtn.hidden = !searchQuery;
  searchInlineBox.classList.toggle("has-query", hasQuery);
  if (searchInput.value !== searchQuery) searchInput.value = searchQuery;
  if (hasQuery && matchCount !== undefined && totalCount !== undefined) {
    searchCountEl.textContent = `${matchCount}/${totalCount}`;
    searchCountEl.hidden = false;
  } else {
    searchCountEl.textContent = "";
    searchCountEl.hidden = true;
  }
}

function refreshServicesResults() {
  if (!status?.member || tab !== "services") return;
  updateServicesFlow(status);
}

function openSearch() {
  if (!status?.member || tab !== "services") return;
  if (!status.services.length && !status.shares.length) return;
  searchInput.focus();
  searchInput.select();
}

function closeSearch() {
  searchQuery = "";
  searchInput.value = "";
  searchInput.blur();
  if (status && tab === "services") {
    syncSearchBar(status);
    updateServicesFlow(status);
  }
}

searchInput.addEventListener("input", () => {
  searchQuery = searchInput.value;
  refreshServicesResults();
});
searchClearBtn.addEventListener("click", () => {
  if (searchQuery) {
    searchQuery = "";
    searchInput.value = "";
    refreshServicesResults();
    searchInput.focus();
  } else {
    closeSearch();
  }
});

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
  return createIconButton({
    icon: name,
    title,
    variant: "ghost",
    size: "md",
    onClick: action,
  });
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
    return;
  }
  if ((event.metaKey || event.ctrlKey) && !event.shiftKey && !event.altKey && event.key.toLowerCase() === "f") {
    if (!status?.member || tab !== "services" || editor.open || settingsDialog.open) return;
    if (!status.services.length && !status.shares.length) return;
    event.preventDefault();
    openSearch();
    return;
  }
  if (event.key === "Escape" && (document.activeElement === searchInput || !!searchQuery) && tab === "services" && !editor.open && !settingsDialog.open) {
    event.preventDefault();
    closeSearch();
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

const mapsForService = (serviceId: string, data?: Status): Mapping[] => {
  const per = data?.service_virtual_ips?.[serviceId];
  return per && per.length ? per : (data?.virtual_ips ?? []);
};

const accessIpFor = (serviceId: string, ip: string, data?: Status) =>
  accessIp(ip, mapsForService(serviceId, data));

const forwardFor = (serviceId: string, ip: string, port: number, data?: Status): LocalForwardStatus | undefined =>
  data?.forwards?.find(f => f.service === serviceId && f.target_ip === ip && f.target_port === port);

const forwardsOfService = (serviceId: string, data?: Status): LocalForwardStatus[] =>
  data?.forwards?.filter(f => f.service === serviceId) ?? [];

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
  editor.classList.remove("dialog-wide");
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
  editor.classList.remove("dialog-wide");
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

async function switchForwardPort(service: string, target_ip: string, target_port: number, local_port: number) {
  if (busy) return;
  busy = true;
  message = "";
  showError();
  try {
    status = await request({ op: "set_local_forward", service, target_ip, target_port, local_port }, false);
    render(true);
    if (local_port > 0) notify(`127.0.0.1:${local_port}`);
  } catch (error) {
    message = typeof error === "string" ? error : "internal";
    showError();
  } finally {
    busy = false;
  }
}

function customize(
  service: Offering,
  label: Label | undefined,
  routing: boolean,
  focusPort = false,
  onDone?: () => void
) {
  const existingFwds = forwardsOfService(service.id, status);
  const firstFwd = existingFwds[0];
  const single = singleSpan(service.rules.ips) && singleSpan(service.rules.ports);
  let remarkVal = label?.remark ?? "";
  let targetIpVal = firstFwd?.target_ip ?? service.rules.ips[0]?.start ?? "";
  let targetPortVal = String(firstFwd?.target_port ?? service.rules.ports[0]?.start ?? "");
  let localPortVal = firstFwd ? String(firstFwd.local_port) : "";
  let aliasVal = label?.alias ?? "";
  let aliasIpVal = label?.ip ?? service.rules.ips[0]?.start ?? "";
  let conflictPort: number | null = firstFwd?.error === "conflict" ? firstFwd.local_port : null;
  let suggestedPort: number | null = firstFwd?.suggested_port ?? null;
  let localError = "";
  let dialogOpen = true;
  editor.addEventListener("close", () => { dialogOpen = false; }, { once: true });

  const clearPortErr = () => {
    localError = "";
    conflictPort = null;
  };

  async function suggestFree(preferred?: number) {
    try {
      const reply = await request({ op: "suggest_local_port", preferred }, false);
      if (reply.suggested_port) {
        localPortVal = String(reply.suggested_port);
        suggestedPort = reply.suggested_port;
        clearPortErr();
        if (dialogOpen && editor.open) draw();
      }
    } catch {}
  }

  function draw() {
    const header = dialogHeader(t("access_settings"), () => { if (!busy) editor.close(); });
    const body = el("div", "dialog-body");
    if (focusPort && !routing) {
      body.append(el("p", "muted", t("need_local_forward")));
    }

    const remarkInput = el("input");
    Object.assign(remarkInput, { name: "remark", value: remarkVal, maxLength: 64, spellcheck: false });
    remarkInput.addEventListener("input", () => { remarkVal = remarkInput.value; });
    body.append(el("label", "", t("remark"), remarkInput));

    let targetIpInput: HTMLInputElement | null = null;
    let targetPortInput: HTMLInputElement | null = null;
    if (!single && existingFwds.length > 0) {
      const chipList = el("div", "fwd-chip-list", ...existingFwds.map((f) => {
        const rm = createIconButton({
          icon: "close",
          title: t("local_port_clear"),
          label: `${t("local_port_clear")} 127.0.0.1:${f.local_port}`,
          variant: "ghost",
          size: "sm",
          onClick: () => {
            void switchForwardPort(f.service, f.target_ip, f.target_port, 0).then(() => {
              if (dialogOpen && editor.open) customize(service, status?.labels.find(l => l.service === service.id), routing, focusPort, onDone);
            });
          },
        });
        return el("div", "fwd-chip",
          el("span", "mono", `127.0.0.1:${f.local_port} ← ${f.target_ip}:${f.target_port}`),
          rm
        );
      }));
      body.append(el("div", "field-group", el("span", "field-label", t("active_forwards")), chipList));
    }

    if (!single) {
      targetIpInput = el("input");
      Object.assign(targetIpInput, { name: "target_ip", value: targetIpVal, maxLength: 15, spellcheck: false });
      targetIpInput.addEventListener("input", () => { targetIpVal = targetIpInput!.value; clearPortErr(); });
      targetPortInput = el("input");
      Object.assign(targetPortInput, { name: "target_port", value: targetPortVal, type: "number", min: "1", max: "65535" });
      targetPortInput.addEventListener("input", () => { targetPortVal = targetPortInput!.value; clearPortErr(); });
      body.append(el("div", "precheck-inputs",
        el("label", "", t("local_target_ip"), targetIpInput),
        el("label", "", t("local_target_port"), targetPortInput)
      ));
    }

    const localPortInput = el("input");
    Object.assign(localPortInput, {
      name: "local_port",
      value: localPortVal,
      type: "number",
      min: "1",
      max: "65535",
      placeholder: t("local_port_placeholder"),
    });
    localPortInput.addEventListener("input", () => {
      localPortVal = localPortInput.value;
      clearPortErr();
    });
    const suggestBtn = button(t("local_port_suggest"), () => {
      const p = Number(localPortVal) || Number(targetPortVal) || undefined;
      void suggestFree(p);
    }, "button");
    const portRow = el("div", "host-ip-row", localPortInput, suggestBtn);
    if (firstFwd) {
      const clearBtn = button(t("local_port_clear"), () => {
        localPortVal = "";
        clearPortErr();
        void saveAll(true);
      }, "button");
      portRow.append(clearBtn);
    }
    body.append(el("div", "field-group", el("span", "field-label", t("local_port_label")), portRow));

    let aliasInput: HTMLInputElement | null = null;
    let aliasIpInput: HTMLInputElement | null = null;
    if (routing) {
      aliasInput = el("input");
      Object.assign(aliasInput, { name: "alias", value: aliasVal, placeholder: "app.fesen", maxLength: 72, spellcheck: false });
      aliasInput.addEventListener("input", () => { aliasVal = aliasInput!.value; });
      aliasIpInput = el("input");
      Object.assign(aliasIpInput, { name: "ip", value: aliasIpVal, maxLength: 15, spellcheck: false });
      aliasIpInput.addEventListener("input", () => { aliasIpVal = aliasIpInput!.value; });
      body.append(
        el("label", "", t("alias"), aliasInput),
        el("label", "", t("alias_ip"), aliasIpInput)
      );
    }

    const errText = localError || (conflictPort ? t("local_port_occupied").replace("{port}", String(conflictPort)) : "");
    if (errText) {
      const box = el("div", "feedback error inline-port-alert", el("span", "", errText));
      box.setAttribute("role", "alert");
      if (suggestedPort && suggestedPort !== conflictPort) {
        const useBtn = button(t("switch_to_port").replace("{port}", String(suggestedPort)), () => {
          localPortVal = String(suggestedPort);
          clearPortErr();
          void saveAll();
        }, "button primary");
        box.append(useBtn);
      }
      body.append(box);
    }

    const saveBtn = button(t("save"), () => void saveAll(), "button primary");
    saveBtn.disabled = busy;
    const cancelBtn = button(t("cancel"), () => { if (!busy) editor.close(); }, "button");
    cancelBtn.disabled = busy;
    const footer = el("div", "dialog-footer", cancelBtn, saveBtn);

    const formEl = el("form", "", header, body, footer);
    formEl.className = "";
    formEl.addEventListener("submit", (e) => {
      e.preventDefault();
      void saveAll();
    });

    editor.tabIndex = -1;
    editor.replaceChildren(formEl);
    if (!editor.open) {
      editor.showModal();
      (focusPort ? localPortInput : remarkInput).focus();
    }
  }

  async function saveAll(forceClearForward = false) {
    if (busy) return;
    localError = "";
    const tIp = targetIpVal.trim();
    const tPort = Number(targetPortVal);
    const lPortText = forceClearForward ? "" : localPortVal.trim();
    const lPort = lPortText === "" ? 0 : Number(lPortText);

    if (lPortText !== "" && (!Number.isInteger(lPort) || lPort < 1 || lPort > 65535)) {
      localError = t("local_port_invalid");
      draw();
      return;
    }
    if (lPort > 0 || forceClearForward || firstFwd) {
      if (!/^\d{1,3}(\.\d{1,3}){3}$/.test(tIp) || toIp(toInt(tIp)) !== tIp
        || !service.rules.ips.some(r => toInt(r.start) <= toInt(tIp) && toInt(tIp) <= toInt(r.end))
        || !Number.isInteger(tPort) || !service.rules.ports.some(r => r.start <= tPort && tPort <= r.end)) {
        localError = t("invalid_target");
        draw();
        return;
      }
    }

    busy = true;
    draw();
    try {
      const needForwardChange = forceClearForward
        || (lPort > 0 && (!firstFwd || firstFwd.local_port !== lPort || firstFwd.target_ip !== tIp || firstFwd.target_port !== tPort))
        || (lPort === 0 && !!firstFwd);

      if (needForwardChange) {
        try {
          if (firstFwd && (firstFwd.target_ip !== tIp || firstFwd.target_port !== tPort)) {
            status = await request({
              op: "set_local_forward",
              service: service.id,
              target_ip: firstFwd.target_ip,
              target_port: firstFwd.target_port,
              local_port: 0,
            }, false);
          }
          status = await request({
            op: "set_local_forward",
            service: service.id,
            target_ip: tIp,
            target_port: tPort,
            local_port: lPort,
          }, false);
        } catch (err) {
          if (err === "conflict" && lPort > 0) {
            conflictPort = lPort;
            try {
              const s = await request({ op: "suggest_local_port", preferred: lPort }, false);
              suggestedPort = s.suggested_port ?? null;
            } catch {}
            return;
          }
          localError = failure(typeof err === "string" ? err : "internal");
          return;
        }
      }

      const alias = routing ? aliasVal.trim() : (label?.alias ?? "");
      const ip = routing ? aliasIpVal.trim() : (label?.ip ?? null);
      status = await request({
        op: "customize",
        id: service.id,
        remark: remarkVal.trim(),
        alias,
        ip: alias ? ip : null,
      }, false);

      editor.close();
      render(true);
      onDone?.();
    } catch (err) {
      localError = failure(typeof err === "string" ? err : "internal");
    } finally {
      busy = false;
      if (dialogOpen && editor.open) draw();
    }
  }

  draw();
  if (focusPort && !localPortVal) {
    void suggestFree(Number(targetPortVal) || undefined);
  }
}

async function testConnection(service: Offering, ip: string, port: number) {
  const key = serviceKey(service);
  const previous = probes.get(key);
  if (busy || !status?.online || (previous && previous.result === undefined)) return;
  const kind = effectiveKind(service);
  const current: Probe = { ip, port, access: accessIpFor(service.id, ip, status), kind };
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
    if (owner && status?.system_routing === false) {
      const fwd = forwardFor(service.id, ip, port, status);
      if (!fwd) {
        const offering = service as Offering;
        const lbl = status.labels.find(l => l.service === service.id);
        customize(offering, lbl, false, true, () => runServiceAction(service, owner, kind, label));
        return;
      }
      if (!fwd.active) {
        notify(t("local_port_conflict"), true);
        return;
      }
    }
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
    const fwd = owner ? forwardFor(service.id, ip, port, status) : undefined;
    if (owner && status?.system_routing === false && !fwd) {
      const lbl = status.labels.find(l => l.service === service.id);
      customize(service as Offering, lbl, false, true, () => openWebPreview(service, owner));
      return;
    }
    if (fwd && !fwd.active) {
      notify(t("local_port_conflict"), true);
      return;
    }
    const access = owner ? (fwd ? "127.0.0.1" : accessIpFor(service.id, ip, status)) : ip;
    const accessPort = fwd ? fwd.local_port : port;
    const session = tabOwner;
    const valid = () => {
      const s = owner ? status?.services.find(s => s.owner === owner && s.id === service.id)
        : status?.shares.find(s => s.id === service.id);
      if (!status?.member || session !== JSON.stringify([status.server, status.member]) || !s || effectiveKind(s) !== "http") {
        return false;
      }
      if (owner) {
        if (!status.online) return false;
        const curFwd = forwardFor(service.id, ip, port, status);
        if (fwd) {
          if (!curFwd?.active || curFwd.local_port !== accessPort) return false;
        } else if (accessIpFor(service.id, ip, status) !== access) {
          return false;
        }
      }
      return s.rules.ips.some(r => toInt(r.start) <= toInt(ip) && toInt(ip) <= toInt(r.end))
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
    }, accessPort);
  });
}

function rowActions(service: Share, name: string, owner: string | undefined, routing: boolean, configure: () => void) {
  const more = tool("more", `${t("more_actions")} · ${name}`, () => {
    const kind = effectiveKind(service);
    const primary: MenuEntry[] = [];
    const conflictFwd = owner ? forwardsOfService(service.id, status).find(f => f.error === "conflict") : undefined;
    if (conflictFwd?.suggested_port) {
      primary.push({
        icon: "alert",
        label: t("switch_to_port").replace("{port}", String(conflictFwd.suggested_port)),
        onSelect: () => void switchForwardPort(
          conflictFwd.service, conflictFwd.target_ip, conflictFwd.target_port, conflictFwd.suggested_port!
        ),
      });
    }
    if (kind === "http") {
      primary.push({
        icon: "globe",
        label: t(owner ? "web_test" : "local_web_test"),
        onSelect: () => openWebPreview(service, owner),
      });
    }
    if (kind === "http" || kind === "https") {
      primary.push({
        icon: "external",
        label: t("open_browser"),
        onSelect: () => runServiceAction(service, owner, "browser", t("open_browser")),
      });
    }
    if (owner && (kind === "ssh" || kind === "rdp" || kind === "smb")) {
      primary.push({
        icon: "terminal",
        label: t("copy_command"),
        onSelect: () => runServiceAction(service, owner, "command", t("copy_command")),
      });
    }
    if (!owner || routing || forwardsOfService(service.id, status).some(f => f.active)) {
      primary.push({
        icon: "copy",
        label: t("copy_address"),
        onSelect: () => runServiceAction(service, owner, "copy", t("copy_address")),
      });
    }

    const secondary: MenuEntry[] = [
      {
        icon: owner ? "sliders" : "edit",
        label: t(owner ? "access_settings" : "edit_share"),
        onSelect: configure,
      },
    ];
    if (!owner) {
      const hostIp = singleIp(service);
      if (hostIp) {
        secondary.push({
          icon: "plus",
          label: t("manage_host_ports"),
          onSelect: () => openAddShare({ ip: hostIp, name: meaningfulHostName(service.name, hostIp, service.host_os ?? null) }),
        });
      }
      secondary.push({
        icon: "close",
        label: t("remove"),
        danger: true,
        onSelect: () => confirmRemoveShare(service),
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
  const ips = addresses(service.rules.ips, mapsForService(service.id, data));
  const overlapped = ips.virtual.length > 0;
  const one = singleSpan(service.rules.ips) && singleSpan(service.rules.ports);
  const fwds = forwardsOfService(service.id, data);
  const singleFwd = one ? fwds.find(f => f.target_ip === service.rules.ips[0]!.start && f.target_port === service.rules.ports[0]!.start) : undefined;
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
    tone = (routing || !!singleFwd?.active) ? "good" : "";
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
  return { ips, overlapped, fwds, singleFwd, badge, httpTag, note, pending, result };
}

type SourceInfo = {
  text: string;
  self: boolean;
  osType?: DeviceOs;
  memberKey: string;
  memberLabel: string;
  deviceLabel: string;
};

function sourceText(service: Offering, currentMember: string | null, includeMember = true): SourceInfo {
  const self = !!currentMember && service.member === currentMember;
  const dev = service.device?.trim() || "";
  const memberLabel = self ? t("source_self") : service.member;
  const deviceLabel = dev || memberLabel;
  const text = includeMember
    ? (dev ? `${memberLabel} · ${dev}` : memberLabel)
    : deviceLabel;
  return { text, self, osType: service.device_os, memberKey: service.member, memberLabel, deviceLabel };
}

function serviceRow(address: string, metaNodes: (HTMLElement | string)[], tags: HTMLElement[], leadIcon: HTMLElement | null, ...controls: HTMLElement[]) {
  return el("article", "service-row",
    el("div", "service-main",
      el("div", "host-title-line", ...(leadIcon ? [leadIcon] : []), el("p", "endpoint", address), ...tags),
      ...(metaNodes.length ? [el("div", "service-meta", ...metaNodes)] : [])
    ),
    el("div", "service-actions", ...controls)
  );
}

function portConflictPill(fwd: LocalForwardStatus): HTMLElement {
  const label = t("local_port_occupied").replace("{port}", String(fwd.local_port));
  if (fwd.suggested_port) {
    const b = button(`${label} · ${t("switch_to_port").replace("{port}", String(fwd.suggested_port))}`, () => {
      void switchForwardPort(fwd.service, fwd.target_ip, fwd.target_port, fwd.suggested_port!);
    }, "badge warn");
    return b;
  }
  return el("span", "badge warn", label);
}

function groupSingleRow(service: Offering, data: Status, routing: boolean, showSource = false, includeMember = true) {
  const kind = effectiveKind(service);
  const label = data.labels.find((l) => l.service === service.id);
  const { ips, overlapped, fwds, singleFwd, badge, httpTag, note, result } = probeVisual(service, data, routing);
  const origEndpoint = endpoint(service.rules.ips, service.rules.ports);
  const address = singleFwd ? `127.0.0.1:${singleFwd.local_port}` : endpoint([...ips.original, ...ips.virtual], service.rules.ports);
  const rawName = label?.remark || service.name;
  const ip = singleIp(service);
  const os = resolveHostOs([service], [result?.hostOs]) as HostOs | null;
  const displayName = ip ? meaningfulHostName(rawName, ip, os) : rawName;
  const alias = label?.alias && data.aliases[label.alias] ? label.alias : null;

  const metaParts: (HTMLElement | string)[] = [];
  if (displayName) metaParts.push(el("span", "service-name", displayName));
  let leadOs: HostOs | DeviceOs | null = os;
  if (showSource) {
    const src = sourceText(service, data.member, includeMember);
    if (!leadOs && src.osType) leadOs = src.osType;
    if (metaParts.length) metaParts.push(el("span", "meta-sep", "·"));
    metaParts.push(el("span", src.self && includeMember ? "source-tag self" : "source-tag", src.text));
    if (src.osType && os && src.osType !== os) {
      const devOsEl = osBadge(src.osType);
      if (devOsEl) metaParts.push(devOsEl);
    }
  }
  if (singleFwd) {
    if (metaParts.length) metaParts.push(el("span", "meta-sep", "·"));
    metaParts.push(el("span", "mono", `← ${origEndpoint}`));
  }
  if (alias && routing) {
    if (metaParts.length) metaParts.push(el("span", "meta-sep", "·"));
    metaParts.push(el("span", "mono", alias));
  }

  const tags: HTMLElement[] = [];
  if (kind !== "tcp") tags.push(el("span", "proto-tag", kindBadgeText(kind)));
  if (httpTag) tags.push(httpTag);
  if (singleFwd) {
    if (singleFwd.error === "conflict") {
      tags.push(portConflictPill(singleFwd));
    } else {
      tags.push(el("span", "proto-tag", t("mapped_local")));
    }
  } else if (overlapped) {
    const ov = el("span", "badge warn", t("overlap"));
    ov.title = t("local_overlap");
    tags.push(ov);
  }
  const row = serviceRow(
    address, metaParts, tags, osBadge(leadOs, "server"), badge,
    rowActions(service, rawName, service.owner, routing, () => customize(service, label, routing))
  );
  if (!singleFwd && overlapped) row.append(el("p", "probe-note", `${t("original_ip")} ${ranges(service.rules.ips)}`));
  if (!singleFwd && fwds.length) {
    for (const f of fwds) {
      if (f.error === "conflict") row.append(el("div", "probe-note", portConflictPill(f)));
      else row.append(el("p", "probe-note mono", `127.0.0.1:${f.local_port} ← ${f.target_ip}:${f.target_port}`));
    }
  }
  if (note) row.append(note);
  return row;
}

function buildRemotePortRows(ip: string, items: Offering[], data: Status, routing: boolean, firstAccess: string, hostOs: HostOs | null, rawHostName: string, labels: (Label | undefined)[]) {
  return items.map((service, idx) => {
    const kind = effectiveKind(service);
    const label = labels[idx];
    const { singleFwd, badge, httpTag, note } = probeVisual(service, data, routing);
    const portText = `:${ranges(service.rules.ports)}`;
    const svcAccess = accessIpFor(service.id, ip, data);
    const rowRawName = label?.remark || service.name;
    const subName = rowRawName !== rawHostName ? meaningfulHostName(rowRawName, ip, hostOs) : "";
    const row = el("div", "port-row",
      el("div", "port-main",
        el("span", "port-num", singleFwd ? `127.0.0.1:${singleFwd.local_port}` : portText),
        el("span", "proto-tag", kindBadgeText(kind)),
        ...(httpTag ? [httpTag] : []),
        ...(singleFwd?.error === "conflict"
          ? [portConflictPill(singleFwd)]
          : singleFwd
            ? [el("span", "port-subname mono", `← ${portText}`)]
            : svcAccess !== firstAccess
              ? [el("span", "port-subname mono", svcAccess)]
              : subName
                ? [el("span", "port-subname", subName)]
                : [])
      ),
      el("div", "port-actions",
        badge,
        rowActions(service, label?.remark || `${ip} ${portText}`, service.owner, routing,
          () => customize(service, label, routing))
      )
    );
    return el("div", "port-row-wrap", row, ...(note ? [note] : []));
  });
}

function groupHostCard(
  collapseKey: string,
  ip: string,
  items: Offering[],
  data: Status,
  routing: boolean,
  showSource: boolean,
  includeMember: boolean,
  forceExpand = false
) {
  const expanded = forceExpand || !isCollapsed(collapseKey);
  const firstAccess = accessIpFor(items[0]!.id, ip, data);
  const overlapped = items.some(s => accessIpFor(s.id, ip, data) !== ip);
  const labels = items.map(s => data.labels.find(l => l.service === s.id));
  const rawHostName = labels.find(l => l?.remark)?.remark || items[0]!.name;
  const alias = labels.map(l => l?.alias).find((a): a is string => !!a && !!data.aliases[a]) ?? null;
  const probed = items.map(s => probes.get(serviceKey(s)));
  const anyPending = probed.some(p => p && !p.result);
  const hostOs = resolveHostOs(items, probed.map(p => p?.result?.hostOs)) as HostOs | null;
  const cleanName = meaningfulHostName(rawHostName, ip, hostOs);

  const testAll = tool("refresh", t("test_host"), () => testHostServices(items));
  testAll.disabled = anyPending || !data.online || busy;
  if (anyPending) testAll.classList.add("spinning");

  const src = showSource ? sourceText(items[0]!, data.member, includeMember) : null;
  const leadOsEl = osBadge(hostOs ?? src?.osType, "server");
  const titleParts: HTMLElement[] = [
    ...(leadOsEl ? [leadOsEl] : []),
    el("p", "endpoint", firstAccess),
    el("span", "count", String(items.length)),
  ];
  if (overlapped) {
    const ov = el("span", "badge warn", t("overlap"));
    ov.title = t("local_overlap");
    titleParts.push(ov);
  }

  const metaParts: (HTMLElement | string)[] = [];
  if (cleanName) metaParts.push(el("span", "service-name", cleanName));
  if (src) {
    if (metaParts.length) metaParts.push(el("span", "meta-sep", "·"));
    metaParts.push(el("span", src.self && includeMember ? "source-tag self" : "source-tag", src.text));
    if (src.osType && hostOs && src.osType !== hostOs) {
      const devOsEl = osBadge(src.osType);
      if (devOsEl) metaParts.push(devOsEl);
    }
  }
  if (alias && routing) {
    if (metaParts.length) metaParts.push(el("span", "meta-sep", "·"));
    metaParts.push(el("span", "mono", alias));
  }
  if (overlapped) {
    if (metaParts.length) metaParts.push(el("span", "meta-sep", "·"));
    metaParts.push(el("span", "mono", `${t("original_ip")} ${ip}`));
  }

  const info = el("div", "host-info",
    el("div", "host-title-line", ...titleParts),
    ...(metaParts.length ? [el("div", "service-meta", ...metaParts)] : [])
  );
  const head = el("div", "host-head",
    collapseToggle(collapseKey, expanded, "", info),
    el("div", "host-controls", testAll)
  );

  const portsWrap = el("div", "host-ports", ...buildRemotePortRows(ip, items, data, routing, firstAccess, hostOs, rawHostName, labels));
  portsWrap.dataset.ports = String(items.length);
  if (items.length >= 9) portsWrap.dataset.many = "1";
  portsWrap.hidden = !expanded;

  return el("article", expanded ? "host-card collapsible" : "host-card collapsible is-collapsed", head, portsWrap);
}

function groupSubSingleRow(service: Offering, data: Status, routing: boolean, _deviceOs?: DeviceOs) {
  const kind = effectiveKind(service);
  const label = data.labels.find((l) => l.service === service.id);
  const { ips, overlapped, fwds, singleFwd, badge, httpTag, note, result } = probeVisual(service, data, routing);
  const origEndpoint = endpoint(service.rules.ips, service.rules.ports);
  const address = singleFwd ? `127.0.0.1:${singleFwd.local_port}` : endpoint([...ips.original, ...ips.virtual], service.rules.ports);
  const rawName = label?.remark || service.name;
  const ip = singleIp(service);
  const os = resolveHostOs([service], [result?.hostOs]) as HostOs | null;
  const displayName = ip ? meaningfulHostName(rawName, ip, os) : rawName;
  const alias = label?.alias && data.aliases[label.alias] ? label.alias : null;

  const metaParts: (HTMLElement | string)[] = [];
  if (displayName) metaParts.push(el("span", "service-name", displayName));
  if (singleFwd) {
    if (metaParts.length) metaParts.push(el("span", "meta-sep", "·"));
    metaParts.push(el("span", "mono", `← ${origEndpoint}`));
  }
  if (alias && routing) {
    if (metaParts.length) metaParts.push(el("span", "meta-sep", "·"));
    metaParts.push(el("span", "mono", alias));
  }

  const leadOsEl = osBadge(os, "server");
  const tags: HTMLElement[] = [];
  if (kind !== "tcp") tags.push(el("span", "proto-tag", kindBadgeText(kind)));
  if (httpTag) tags.push(httpTag);
  if (singleFwd) {
    tags.push(singleFwd.error === "conflict" ? portConflictPill(singleFwd) : el("span", "proto-tag", t("mapped_local")));
  } else if (overlapped) {
    const ov = el("span", "badge warn", t("overlap"));
    ov.title = t("local_overlap");
    tags.push(ov);
  }

  const row = el("div", "device-sub-row",
    el("div", "service-main",
      el("div", "host-title-line", ...(leadOsEl ? [leadOsEl] : []), el("p", "endpoint", address), ...tags),
      ...(metaParts.length ? [el("div", "service-meta", ...metaParts)] : [])
    ),
    el("div", "service-actions", badge,
      rowActions(service, rawName, service.owner, routing, () => customize(service, label, routing)))
  );
  const notes: HTMLElement[] = [];
  if (!singleFwd && overlapped) notes.push(el("p", "probe-note", `${t("original_ip")} ${ranges(service.rules.ips)}`));
  if (!singleFwd && fwds.length) {
    for (const f of fwds) {
      if (f.error === "conflict") notes.push(el("div", "probe-note", portConflictPill(f)));
      else notes.push(el("p", "probe-note mono", `127.0.0.1:${f.local_port} ← ${f.target_ip}:${f.target_port}`));
    }
  }
  if (note) notes.push(note);
  return el("div", "device-sub-wrap", row, ...notes);
}

function groupSubHostBlock(owner: string, ip: string, items: Offering[], data: Status, routing: boolean, forceExpand = false) {
  const key = `rhost:${owner}:${ip}`;
  const expanded = forceExpand || !isCollapsed(key);
  const firstAccess = accessIpFor(items[0]!.id, ip, data);
  const overlapped = items.some(s => accessIpFor(s.id, ip, data) !== ip);
  const labels = items.map(s => data.labels.find(l => l.service === s.id));
  const rawHostName = labels.find(l => l?.remark)?.remark || items[0]!.name;
  const alias = labels.map(l => l?.alias).find((a): a is string => !!a && !!data.aliases[a]) ?? null;
  const probed = items.map(s => probes.get(serviceKey(s)));
  const anyPending = probed.some(p => p && !p.result);
  const hostOs = resolveHostOs(items, probed.map(p => p?.result?.hostOs)) as HostOs | null;
  const cleanName = meaningfulHostName(rawHostName, ip, hostOs);

  const testSub = tool("refresh", t("test_host"), () => testHostServices(items));
  testSub.disabled = anyPending || !data.online || busy;
  if (anyPending) testSub.classList.add("spinning");

  const hostOsEl = osBadge(hostOs, "server");
  const titleParts: HTMLElement[] = [
    ...(hostOsEl ? [hostOsEl] : []),
    el("p", "endpoint", firstAccess),
    el("span", "count", String(items.length)),
  ];
  if (overlapped) {
    const ov = el("span", "badge warn", t("overlap"));
    ov.title = t("local_overlap");
    titleParts.push(ov);
  }
  const metaParts: (HTMLElement | string)[] = [];
  if (cleanName) metaParts.push(el("span", "service-name", cleanName));
  if (alias && routing) {
    if (metaParts.length) metaParts.push(el("span", "meta-sep", "·"));
    metaParts.push(el("span", "mono", alias));
  }
  if (overlapped) {
    if (metaParts.length) metaParts.push(el("span", "meta-sep", "·"));
    metaParts.push(el("span", "mono", `${t("original_ip")} ${ip}`));
  }

  const info = el("div", "host-info",
    el("div", "host-title-line", ...titleParts),
    ...(metaParts.length ? [el("div", "service-meta", ...metaParts)] : [])
  );
  const head = el("div", "host-sub-head",
    collapseToggle(key, expanded, "", info),
    el("div", "host-controls", testSub)
  );
  const portsWrap = el("div", "host-sub-ports", ...buildRemotePortRows(ip, items, data, routing, firstAccess, hostOs, rawHostName, labels));
  portsWrap.dataset.ports = String(items.length);
  if (items.length >= 9) portsWrap.dataset.many = "1";
  portsWrap.hidden = !expanded;
  return el("div", "host-sub", head, portsWrap);
}

type HostEntry =
  | { grouped: true; ip: string; items: Offering[] }
  | { grouped: false; item: Offering };

type DeviceBucket = {
  owner: string;
  src: SourceInfo;
  items: Offering[];
  entries: HostEntry[];
};

type AccountGroup = {
  memberKey: string;
  memberLabel: string;
  self: boolean;
  buckets: DeviceBucket[];
  totalServices: number;
};

function buildRemoteAccounts(services: Offering[], data: Status): AccountGroup[] {
  const byOwner = new Map<string, Offering[]>();
  for (const s of services) {
    const arr = byOwner.get(s.owner);
    if (arr) arr.push(s);
    else byOwner.set(s.owner, [s]);
  }
  const byMember = new Map<string, DeviceBucket[]>();
  for (const [owner, items] of byOwner) {
    const src = sourceText(items[0]!, data.member, true);
    const entries = (groupByHost(items, (s: Offering) => singleIp(s)) as HostEntry[]).sort((a, b) => {
      if (a.grouped !== b.grouped) return a.grouped ? -1 : 1;
      if (a.grouped && b.grouped) return toInt(a.ip) - toInt(b.ip);
      const ia = !a.grouped ? toInt(a.item.rules.ips[0]?.start ?? "0.0.0.0") : 0;
      const ib = !b.grouped ? toInt(b.item.rules.ips[0]?.start ?? "0.0.0.0") : 0;
      return ia - ib;
    });
    const bucket: DeviceBucket = { owner, src, items, entries };
    const arr = byMember.get(src.memberKey);
    if (arr) arr.push(bucket);
    else byMember.set(src.memberKey, [bucket]);
  }
  const accounts: AccountGroup[] = [];
  for (const [memberKey, buckets] of byMember) {
    buckets.sort((a, b) => a.src.deviceLabel.localeCompare(b.src.deviceLabel) || a.owner.localeCompare(b.owner));
    accounts.push({
      memberKey,
      memberLabel: buckets[0]!.src.memberLabel,
      self: buckets[0]!.src.self,
      buckets,
      totalServices: buckets.reduce((sum, b) => sum + b.items.length, 0),
    });
  }
  accounts.sort((a, b) => {
    if (a.self !== b.self) return a.self ? -1 : 1;
    return a.memberKey.localeCompare(b.memberKey);
  });
  return accounts;
}

function renderDeviceBucket(bucket: DeviceBucket, data: Status, routing: boolean, includeMember: boolean, forceExpand = false): HTMLElement {
  if (bucket.items.length === 1) {
    return groupSingleRow(bucket.items[0]!, data, routing, true, includeMember);
  }
  const firstEntry = bucket.entries[0];
  if (bucket.entries.length === 1 && firstEntry?.grouped && firstEntry.items.length >= 2) {
    return groupHostCard(`dev:${bucket.owner}`, firstEntry.ip, firstEntry.items, data, routing, true, includeMember, forceExpand);
  }

  const key = `dev:${bucket.owner}`;
  const expanded = forceExpand || !isCollapsed(key);
  const anyPending = bucket.items.some((s) => {
    const p = probes.get(serviceKey(s));
    return !!p && !p.result;
  });
  const testDevice = tool("refresh", t("test_host"), () => testHostServices(bucket.items));
  testDevice.disabled = anyPending || !data.online || busy;
  if (anyPending) testDevice.classList.add("spinning");

  const label = includeMember ? bucket.src.text : bucket.src.deviceLabel;
  const devOsEl = osBadge(bucket.src.osType, "device");
  const titleLine = el("div", "host-title-line",
    ...(devOsEl ? [devOsEl] : []),
    el("span", bucket.src.self && includeMember ? "device-card-title self" : "device-card-title", label),
    el("span", "count", String(bucket.items.length))
  );
  const head = el("div", "host-head",
    collapseToggle(key, expanded, "", el("div", "host-info", titleLine)),
    el("div", "host-controls", testDevice)
  );
  const hostsWrap = el("div", "device-hosts", ...bucket.entries.map((entry) => {
    if (!entry.grouped || entry.items.length === 1) {
      const item = entry.grouped ? entry.items[0]! : entry.item;
      return groupSubSingleRow(item, data, routing, bucket.src.osType);
    }
    return groupSubHostBlock(bucket.owner, entry.ip, entry.items, data, routing, forceExpand);
  }));
  hostsWrap.hidden = !expanded;
  return el("article", expanded ? "host-card device-card collapsible" : "host-card device-card collapsible is-collapsed", head, hostsWrap);
}

function flatRemoteRow(service: Offering, data: Status, routing: boolean, remoteIpOs?: Map<string, HostOs | null>): HTMLElement {
  const kind = effectiveKind(service);
  const label = data.labels.find((l) => l.service === service.id);
  const { ips, overlapped, fwds, singleFwd, badge, httpTag, note, result } = probeVisual(service, data, routing);
  const origEndpoint = endpoint(service.rules.ips, service.rules.ports);
  const address = singleFwd ? `127.0.0.1:${singleFwd.local_port}` : endpoint([...ips.original, ...ips.virtual], service.rules.ports);
  const rawName = label?.remark || service.name;
  const ip = singleIp(service);
  const os = ((ip ? remoteIpOs?.get(`${service.owner}:${ip}`) : null) ?? resolveHostOs([service], [result?.hostOs])) as HostOs | null;
  const displayName = ip ? meaningfulHostName(rawName, ip, os) : rawName;
  const src = sourceText(service, data.member, true);
  const srcLabel = src.self ? src.deviceLabel : src.text;

  const subParts: (HTMLElement | string)[] = [];
  if (displayName) subParts.push(el("span", "flat-sub-name", displayName));
  if (subParts.length) subParts.push(el("span", "meta-sep", "·"));
  subParts.push(el("span", src.self ? "source-tag self flat-sub-src" : "flat-sub-src", srcLabel));
  if (singleFwd) {
    subParts.push(el("span", "meta-sep", "·"));
    subParts.push(el("span", "mono flat-sub-text", `← ${origEndpoint}`));
  }

  const tags: HTMLElement[] = [];
  if (kind !== "tcp") tags.push(el("span", "proto-tag", kindBadgeText(kind)));
  if (httpTag) tags.push(httpTag);
  if (singleFwd?.error === "conflict") {
    tags.push(el("span", "badge warn", t("conflict")));
  } else if (!singleFwd && overlapped) {
    const ov = el("span", "badge warn", t("overlap"));
    ov.title = t("local_overlap");
    tags.push(ov);
  }

  const leadOsEl = osBadge(os ?? service.device_os, "server");
  const row = el("div", "flat-row",
    el("div", "flat-main",
      ...(leadOsEl ? [leadOsEl] : []),
      el("span", "flat-addr", address),
      ...tags,
      ...(subParts.length ? [el("span", "flat-sub", ...subParts)] : [])
    ),
    el("div", "service-actions", badge,
      rowActions(service, rawName, service.owner, routing, () => customize(service, label, routing)))
  );

  const notes: HTMLElement[] = [];
  if (singleFwd?.error === "conflict") notes.push(el("div", "probe-note", portConflictPill(singleFwd)));
  else if (!singleFwd && overlapped) notes.push(el("p", "probe-note", `${t("original_ip")} ${ranges(service.rules.ips)}`));
  if (!singleFwd && fwds.length) {
    for (const f of fwds) {
      if (f.error === "conflict") notes.push(el("div", "probe-note", portConflictPill(f)));
      else notes.push(el("p", "probe-note mono", `127.0.0.1:${f.local_port} ← ${f.target_ip}:${f.target_port}`));
    }
  }
  if (note) notes.push(note);
  return el("div", "flat-row-wrap", row, ...notes);
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

function confirmRemoveShare(share: Share) {
  message = "";
  edit(t("confirm_remove"), submit(t("remove"), "danger"), () => {
    void act({ op: "remove", id: share.id }, () => editor.close());
  }, el("p", "muted", share.name), el("p", "endpoint", endpoint(share.rules.ips, share.rules.ports)));
}

function confirmRemoveHost(ip: string, hostLabel: string, items: Share[]) {
  message = "";
  edit(
    t("confirm_remove_host"),
    submit(t("remove_host"), "danger"),
    () => {
      void act({ op: "remove_many", ids: items.map((s) => s.id) }, () => editor.close());
    },
    el("p", "muted", t("confirm_remove_host_body").replace("{host}", hostLabel).replace("{n}", String(items.length))),
    el("p", "endpoint", `${ip} · ${items.map((s) => `:${ranges(s.rules.ports)}`).join(", ")}`)
  );
}

function editShare(share?: Share) {
  const currentKind = share ? effectiveKind(share) : "tcp";
  const wasActive = !!share && !!status?.active.includes(share.id);
  let republishChecked = wasActive && !!status?.online;
  const rows: HTMLElement[] = [
    field(t("service_name"), "name", share?.name ?? "", { required: true, maxLength: 64 }),
    field(t("ipv4"), "ips", share ? inputRanges(share.rules.ips) : "", {
      required: true, maxLength: 900, placeholder: "198.51.100.10, 203.0.113.0/29",
    }),
    field(t("port"), "ports", share ? inputRanges(share.rules.ports) : "", {
      required: true, maxLength: 500, placeholder: "80, 8000-8008",
    }),
    el("label", "", t("protocol_type"), protocolSelect(currentKind)),
  ];
  if (wasActive && status?.online) {
    const repubCheck = el("input");
    Object.assign(repubCheck, { type: "checkbox", checked: republishChecked });
    repubCheck.addEventListener("change", () => { republishChecked = repubCheck.checked; });
    rows.push(el("label", "check-row", repubCheck, t("republish_on_save")));
  }
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
      republish: wasActive ? republishChecked : false,
    }, () => editor.close());
  }, ...rows);
  if (share) controls.prepend(button(t("remove"), () => confirmRemoveShare(share), "button ghost text-danger leading"));
}

type DraftPort = {
  spec: string;
  port?: number;
  kind: ServiceKind;
  checked: boolean;
  saved: boolean;
  active?: boolean;
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
  let probingDeep = false;
  let probeSeq = 0;
  let localError = "";
  let emptyDetect = false;
  let items: DraftPort[] = [];
  let publishNow = !!status?.online;
  let dialogOpen = true;
  let keepPortFocus = false;

  function cancelHostProbe(redraw = true) {
    probeSeq += 1;
    const wasProbing = probingAll;
    probingAll = false;
    probingDeep = false;
    if (wasProbing) {
      void request({ op: "cancel_probe_host" }, false).catch(() => {});
      if (redraw && dialogOpen && editor.open) draw();
    }
  }

  editor.addEventListener("close", () => {
    dialogOpen = false;
    editor.classList.remove("dialog-wide");
    cancelHostProbe(false);
  }, { once: true });

  const existingForIp = (ip: string) =>
    (status?.shares ?? []).filter(s => singleIp(s) === ip && singlePort(s) !== null);

  const initialExisting = ipVal ? existingForIp(ipVal) : [];
  const manageMode = initialExisting.length > 0;
  const oldHostName = manageMode ? (initialExisting[0]!.name || ipVal) : "";

  if (manageMode) {
    items = initialExisting
      .map(s => {
        const port = singlePort(s)!;
        const kind = effectiveKind(s);
        const active = !!status?.active.includes(s.id);
        const osHint: HostOs | undefined = kind === "rdp" ? "windows" : (kind === "ssh" ? s.host_os : undefined);
        return { spec: String(port), port, kind, checked: true, saved: true, active, osHint };
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

  async function detectCommonServices(deep = false) {
    if (probingAll || busy) return;
    const ip = ipVal.trim();
    if (!isUsableHostIp(ip)) { localError = t("invalid_host_ip"); draw(); return; }
    const seq = ++probeSeq;
    localError = "";
    message = "";
    emptyDetect = false;
    probingAll = true;
    probingDeep = deep;
    draw();
    try {
      const extra = items
        .map(i => i.port)
        .filter((p): p is number => p !== undefined && ![22, 3389, 445, 80, 443].includes(p))
        .slice(0, 5);
      const reply = await request({ op: "probe_host", ip, extra_ports: extra, deep }, true);
      if (seq !== probeSeq) return;
      if (!reply.host_probe) throw "internal";
      if (reply.host_probe.cancelled) return;
      mergeProbeResults(reply.host_probe, false);
    } catch (error) {
      if (seq !== probeSeq) return;
      localError = failure(typeof error === "string" ? error : "internal");
    } finally {
      if (seq === probeSeq) {
        probingAll = false;
        probingDeep = false;
        if (dialogOpen && editor.open) draw();
      }
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
        if (items.length >= 24) { localError = t("invalid_port_input"); draw(); return false; }
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
    if (item.saved && !item.checked) return t("will_remove");
    const parts: string[] = [];
    if (item.saved) parts.push(t(item.active ? "published" : "already_added"));
    if (item.open === false) parts.push(t("port_offline"));
    else if (item.detail && item.kind !== "ssh") parts.push(item.detail);
    return parts.join(" · ");
  }

  function draw() {
    const activeName = document.activeElement?.getAttribute("name") ?? "";
    const header = dialogHeader(t(manageMode ? "manage_host" : "new_share"), () => { if (!busy) editor.close(); });
    const body = el("div", "dialog-body");
    const alertBox = el("p", "feedback error");
    alertBox.setAttribute("role", "alert");

    const osTagHost = el("span");
    const deepBtn = probingAll
      ? button(t("cancel_scan"), () => cancelHostProbe(true), "deep-scan-btn cancel")
      : button(t("deep_scan"), () => void detectCommonServices(true), "deep-scan-btn");
    function syncHeaderMeta() {
      const osEl = osBadge(effectiveHostOs(), "server");
      osTagHost.replaceChildren(...(osEl ? [osEl] : []));
      const validIp = isUsableHostIp(ipVal.trim());
      detectBtn.disabled = busy || (!probingAll && !validIp);
      deepBtn.disabled = busy || (!probingAll && !validIp);
    }

    const ipInput = el("input");
    Object.assign(ipInput, {
      name: "host_ip",
      value: ipVal,
      maxLength: 900,
      placeholder: "198.51.100.35 / 203.0.113.0/29",
      spellcheck: false,
      disabled: manageMode || probingAll,
    });
    ipInput.addEventListener("input", () => {
      ipVal = ipInput.value;
      if (localError) { localError = ""; alertBox.hidden = true; alertBox.textContent = ""; }
      syncHeaderMeta();
    });
    ipInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        if (isUsableHostIp(ipVal.trim())) void detectCommonServices(false);
        else portInput.focus();
      }
    });

    const detectBtn = createIconButton({
      icon: probingAll ? "close" : "refresh",
      title: t(probingAll ? "cancel_scan" : "detect_host"),
      variant: "solid",
      size: "lg",
      onClick: () => {
        if (probingAll) cancelHostProbe(true);
        else void detectCommonServices(false);
      },
    });

    editor.classList.toggle("dialog-wide", manageMode || items.length > 0 || !!portDraft.trim());
    const ipHead = el("div", "host-ip-head", el("span", "field-label", t("host_ip")), osTagHost);
    const ipGroup = el("div", "field-group", ipHead, el("div", "host-ip-row", ipInput, detectBtn));

    const showName = manageMode || items.length > 0 || !!portDraft.trim();
    if (showName) {
      const nameInput = el("input");
      Object.assign(nameInput, {
        name: "host_name",
        value: nameVal,
        maxLength: 64,
        placeholder: ipVal.trim() || "198.51.100.35",
        spellcheck: false,
      });
      nameInput.addEventListener("input", () => { nameVal = nameInput.value; });
      const nameGroup = el("div", "field-group",
        el("span", "field-label", t("host_name")),
        nameInput
      );
      body.append(el("div", "dialog-pair", ipGroup, nameGroup));
    } else {
      body.append(ipGroup);
    }

    const portsBox = el("div", "ports-box");
    if (items.length >= 6) portsBox.dataset.multi = "1";
    if (emptyDetect && !items.length) portsBox.append(el("div", "ports-empty-note", t("host_none_open")));
    for (const item of items) {
      const cb = el("input");
      Object.assign(cb, { type: "checkbox", checked: item.checked });
      const rowCls = `host-service-item${item.checked ? " checked" : item.saved ? " removing" : ""}`;
      const row = el("div", rowCls);
      const detailEl = el("span", "host-service-detail", itemMetaText(item));
      cb.addEventListener("change", () => {
        item.checked = cb.checked;
        row.classList.toggle("checked", cb.checked);
        row.classList.toggle("removing", item.saved && !cb.checked);
        detailEl.textContent = itemMetaText(item);
        syncHeaderMeta();
        updateSubmit();
      });
      const kindSel = protocolSelect(item.kind, "kind-inline", (k) => {
        item.kind = k;
        if (k === "rdp") item.osHint = "windows";
        else if (k !== "ssh") item.osHint = undefined;
        syncHeaderMeta();
      });
      const removeBtn = createIconButton({
        icon: item.saved && !item.checked ? "refresh" : "close",
        title: t(item.saved && !item.checked ? "save" : "remove"),
        label: `${t("remove")} :${item.spec}`,
        variant: "ghost",
        size: "sm",
        onClick: () => {
          if (item.saved) {
            item.checked = !item.checked;
            draw();
          } else {
            items = items.filter(i => i.spec !== item.spec);
            draw();
          }
        },
      });

      const httpCode = item.httpStatus !== undefined
        ? el("span", item.httpStatus >= 400 ? "http-code bad" : "http-code", String(item.httpStatus))
        : null;
      if (httpCode) httpCode.title = `HTTP ${item.httpStatus}`;
      row.append(
        cb,
        el("span", "host-service-port", `:${item.spec}`),
        kindSel,
        ...(httpCode ? [httpCode] : []),
        detailEl,
        ...(item.rttMs !== undefined ? [el("span", "host-service-rtt", `${item.rttMs}ms`)] : []),
        removeBtn
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
    const portsHead = el("div", "host-ip-head",
      el("span", "field-label", probingAll ? t(probingDeep ? "deep_scanning" : "detecting_host") : t("add_ports_label")),
      deepBtn
    );
    body.append(el("div", "field-group", portsHead, portsBox));

    if (items.length || portDraft.trim()) {
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
      if (!chosen.length) { localError = t(manageMode ? "host_select_or_remove" : "select_one_service"); draw(); return; }
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
          replace: manageMode,
          old_host_name: manageMode ? oldHostName : null,
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
      const removeCount = manageMode ? items.filter(c => c.saved && !c.checked).length : 0;
      if (manageMode) {
        saveBtn.textContent = removeCount > 0 ? `${t("save")} (-${removeCount})` : count > 1 ? `${t("save")} (${count})` : t("save");
      } else {
        saveBtn.textContent = count > 1 ? `${t("save")} (${count})` : t("save");
      }
      saveBtn.disabled = probingAll || busy || (count === 0 && !portDraft.trim());
    }
    syncHeaderMeta();
    updateSubmit();

    const footer = el("div", "dialog-footer",
      ...(manageMode ? [button(t("remove_host"), () => confirmRemoveHost(ipVal.trim(), nameVal.trim() || ipVal.trim(), initialExisting), "button ghost text-danger leading")] : []),
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

function shareEditAction(s: Share) {
  return () => editShare(s);
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

function mineSingleRow(s: Share, data: Status, showDevice = false, includeMember = true) {
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
  if (kind !== "tcp") tags.push(el("span", "proto-tag", kindBadgeText(kind)));
  const metaNodes: (HTMLElement | string)[] = [];
  if (subtitle) metaNodes.push(el("span", "service-name", subtitle));
  if (showDevice) {
    const dev = data.device_name?.trim() || "";
    const devText = dev || t("badge_local");
    if (metaNodes.length) metaNodes.push(el("span", "meta-sep", "·"));
    metaNodes.push(el("span", includeMember ? "source-tag self" : "source-tag", devText));
    if (data.device_os && os && data.device_os !== os) {
      const devOsEl = osBadge(data.device_os);
      if (devOsEl) metaNodes.push(devOsEl);
    }
  }
  const localPrefix = includeMember ? `${t("badge_local")} · ${t("source_self")}` : t("badge_local");
  const leadOsEl = osBadge(os ?? (showDevice ? data.device_os : null), {
    fallback: "server",
    local: true,
    prefix: localPrefix,
  });
  return serviceRow(
    address, metaNodes, tags, leadOsEl, publish,
    rowActions(s, s.name, undefined, true, shareEditAction(s))
  );
}

function buildLocalHostMore(ip: string, cleanName: string, items: Share[]) {
  const hostMore = tool("more", `${t("more_actions")} · ${cleanName || ip}`, () => {
    openPopoverMenu(hostMore, [
      {
        icon: "sliders",
        label: t("manage_host"),
        onSelect: () => openAddShare({ ip, name: cleanName }),
      },
      { sep: true },
      {
        icon: "close",
        label: t("remove_host"),
        danger: true,
        onSelect: () => confirmRemoveHost(ip, cleanName ? `${cleanName} (${ip})` : ip, items),
      },
    ]);
  });
  hostMore.setAttribute("aria-haspopup", "menu");
  hostMore.setAttribute("aria-expanded", "false");
  return hostMore;
}

function buildLocalHostPorts(ip: string, items: Share[], data: Status, rawHostName: string, hostOs: HostOs | null) {
  return items.map((s) => {
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
    const row = el("div", "port-row",
      el("div", "port-main",
        el("span", "port-num", portText),
        el("span", "proto-tag", kindBadgeText(kind)),
        ...(subName ? [el("span", "port-subname", subName)] : [])
      ),
      el("div", "port-actions", publish, rowActions(s, s.name, undefined, true, shareEditAction(s)))
    );
    return el("div", "port-row-wrap", row);
  });
}

function mineHostCard(
  collapseKey: string,
  ip: string,
  items: Share[],
  data: Status,
  forceExpand = false,
  showDevice = false,
  includeMember = true
) {
  const expanded = forceExpand || !isCollapsed(collapseKey);
  const rawHostName = items[0]!.name;
  const hostOs = resolveHostOs(items) as HostOs | null;
  const cleanName = meaningfulHostName(rawHostName, ip, hostOs);
  const activeCount = items.filter(s => data.active.includes(s.id)).length;
  const allActive = activeCount === items.length;
  const partial = activeCount > 0 && activeCount < items.length;

  const hostMore = buildLocalHostMore(ip, cleanName, items);
  const master = makeToggle(
    allActive,
    `${t("publish")} · ${cleanName || ip}`,
    !data.online && activeCount === 0
      ? () => confirmConnectAndShare(cleanName ? `${cleanName} (${ip})` : ip, { op: "share_many", ids: items.map(s => s.id), enabled: true })
      : null,
    () => void act({ op: "share_many", ids: items.map(s => s.id), enabled: !allActive }),
    partial ? `${activeCount}/${items.length}` : undefined
  );

  const localPrefix = includeMember ? `${t("badge_local")} · ${t("source_self")}` : t("badge_local");
  const hostOsEl = osBadge(hostOs ?? (showDevice ? data.device_os : null), {
    fallback: "server",
    local: true,
    prefix: localPrefix,
  });
  const titleParts: HTMLElement[] = [
    ...(hostOsEl ? [hostOsEl] : []),
    el("p", "endpoint", ip),
    el("span", "count", String(items.length)),
  ];
  const metaParts: (HTMLElement | string)[] = [];
  if (cleanName) metaParts.push(el("span", "service-name", cleanName));
  if (showDevice) {
    const dev = data.device_name?.trim() || "";
    const devText = dev || t("badge_local");
    if (metaParts.length) metaParts.push(el("span", "meta-sep", "·"));
    metaParts.push(el("span", includeMember ? "source-tag self" : "source-tag", devText));
    if (data.device_os && hostOs && data.device_os !== hostOs) {
      const devOsEl = osBadge(data.device_os);
      if (devOsEl) metaParts.push(devOsEl);
    }
  }

  const info = el("div", "host-info",
    el("div", "host-title-line", ...titleParts),
    ...(metaParts.length ? [el("div", "service-meta", ...metaParts)] : [])
  );
  const head = el("div", "host-head",
    collapseToggle(collapseKey, expanded, "", info),
    el("div", "host-controls", master, hostMore)
  );

  const portsWrap = el("div", "host-ports", ...buildLocalHostPorts(ip, items, data, rawHostName, hostOs));
  portsWrap.dataset.ports = String(items.length);
  if (items.length >= 9) portsWrap.dataset.many = "1";
  portsWrap.hidden = !expanded;

  return el("article", expanded ? "host-card collapsible" : "host-card collapsible is-collapsed", head, portsWrap);
}

function mineSubSingleRow(s: Share, data: Status) {
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
  const leadOsEl = osBadge(os, "server");
  const tags: HTMLElement[] = [];
  if (kind !== "tcp") tags.push(el("span", "proto-tag", kindBadgeText(kind)));
  const row = el("div", "device-sub-row",
    el("div", "service-main",
      el("div", "host-title-line", ...(leadOsEl ? [leadOsEl] : []), el("p", "endpoint", address), ...tags),
      ...(subtitle ? [el("div", "service-meta", el("span", "service-name", subtitle))] : [])
    ),
    el("div", "service-actions", publish, rowActions(s, s.name, undefined, true, shareEditAction(s)))
  );
  return el("div", "device-sub-wrap", row);
}

function mineSubHostBlock(ip: string, items: Share[], data: Status, forceExpand = false) {
  const key = `lhost:${ip}`;
  const expanded = forceExpand || !isCollapsed(key);
  const rawHostName = items[0]!.name;
  const hostOs = resolveHostOs(items) as HostOs | null;
  const cleanName = meaningfulHostName(rawHostName, ip, hostOs);
  const activeCount = items.filter(s => data.active.includes(s.id)).length;
  const allActive = activeCount === items.length;
  const partial = activeCount > 0 && activeCount < items.length;

  const hostMore = buildLocalHostMore(ip, cleanName, items);
  const master = makeToggle(
    allActive,
    `${t("publish")} · ${cleanName || ip}`,
    !data.online && activeCount === 0
      ? () => confirmConnectAndShare(cleanName ? `${cleanName} (${ip})` : ip, { op: "share_many", ids: items.map(s => s.id), enabled: true })
      : null,
    () => void act({ op: "share_many", ids: items.map(s => s.id), enabled: !allActive }),
    partial ? `${activeCount}/${items.length}` : undefined
  );
  const hostOsEl = osBadge(hostOs, "server");
  const titleParts: HTMLElement[] = [
    ...(hostOsEl ? [hostOsEl] : []),
    el("p", "endpoint", ip),
    el("span", "count", String(items.length)),
  ];
  const info = el("div", "host-info",
    el("div", "host-title-line", ...titleParts),
    ...(cleanName ? [el("div", "service-meta", el("span", "service-name", cleanName))] : [])
  );
  const head = el("div", "host-sub-head",
    collapseToggle(key, expanded, "", info),
    el("div", "host-controls", master, hostMore)
  );
  const portsWrap = el("div", "host-sub-ports", ...buildLocalHostPorts(ip, items, data, rawHostName, hostOs));
  portsWrap.dataset.ports = String(items.length);
  if (items.length >= 9) portsWrap.dataset.many = "1";
  portsWrap.hidden = !expanded;
  return el("div", "host-sub", head, portsWrap);
}

type LocalEntry =
  | { grouped: true; ip: string; items: Share[] }
  | { grouped: false; item: Share };

function renderLocalBucket(
  shares: Share[],
  entries: LocalEntry[],
  data: Status,
  includeMember: boolean,
  forceExpand = false
): HTMLElement {
  if (shares.length === 1) {
    return mineSingleRow(shares[0]!, data, true, includeMember);
  }
  const firstEntry = entries[0];
  if (entries.length === 1 && firstEntry?.grouped && firstEntry.items.length >= 2) {
    return mineHostCard("dev:local", firstEntry.ip, firstEntry.items, data, forceExpand, true, includeMember);
  }

  const key = "dev:local";
  const expanded = forceExpand || !isCollapsed(key);
  const devName = data.device_name?.trim() || "";
  const label = devName || t("badge_local");
  const localTip = includeMember ? `${t("badge_local")} · ${t("source_self")}` : t("badge_local");
  const devOsEl = osBadge(data.device_os, { fallback: "device", local: true, prefix: localTip });
  const activeCount = shares.filter((s) => data.active.includes(s.id)).length;
  const allActive = activeCount === shares.length;
  const partial = activeCount > 0 && activeCount < shares.length;
  const master = makeToggle(
    allActive,
    `${t("publish")} · ${label}`,
    !data.online && activeCount === 0
      ? () => confirmConnectAndShare(label, { op: "share_many", ids: shares.map((s) => s.id), enabled: true })
      : null,
    () => void act({ op: "share_many", ids: shares.map((s) => s.id), enabled: !allActive }),
    partial ? `${activeCount}/${shares.length}` : undefined
  );
  const addLocalBtn = tool("plus", t("new_share"), () => openAddShare());
  const titleLine = el("div", "host-title-line",
    ...(devOsEl ? [devOsEl] : []),
    el("span", includeMember ? "device-card-title self" : "device-card-title", label),
    el("span", "count", String(shares.length))
  );
  const head = el("div", "host-head",
    collapseToggle(key, expanded, "", el("div", "host-info", titleLine)),
    el("div", "host-controls", master, addLocalBtn)
  );
  const hostsWrap = el("div", "device-hosts", ...entries.map((entry) => {
    if (!entry.grouped || entry.items.length === 1) {
      const item = (entry.grouped ? entry.items[0] : entry.item) as Share;
      return mineSubSingleRow(item, data);
    }
    return mineSubHostBlock(entry.ip, entry.items, data, forceExpand);
  }));
  hostsWrap.hidden = !expanded;
  return el("article", expanded ? "host-card device-card collapsible" : "host-card device-card collapsible is-collapsed", head, hostsWrap);
}

function flatMineRow(s: Share, data: Status, localIpOs?: Map<string, HostOs | null>): HTMLElement {
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
  const os = ((ip ? localIpOs?.get(ip) : null) ?? resolveHostOs([s])) as HostOs | null;
  const subtitle = ip ? meaningfulHostName(s.name, ip, os) : s.name;
  const osEl = osBadge(os, { fallback: "server", local: true, prefix: t("badge_local") });
  const row = el("div", "flat-row",
    el("div", "flat-main",
      ...(osEl ? [osEl] : []),
      el("span", "flat-addr", address),
      ...(kind !== "tcp" ? [el("span", "proto-tag", kindBadgeText(kind))] : []),
      ...(subtitle ? [el("span", "flat-sub", el("span", "flat-sub-text", subtitle))] : [])
    ),
    el("div", "service-actions", publish, rowActions(s, s.name, undefined, true, shareEditAction(s)))
  );
  return el("div", "flat-row-wrap", row);
}

function sortLocalShares(shares: Share[]): Share[] {
  return [...shares].sort((a, b) => {
    const ipCmp = toInt(a.rules.ips[0]?.start ?? "0.0.0.0") - toInt(b.rules.ips[0]?.start ?? "0.0.0.0");
    if (ipCmp !== 0) return ipCmp;
    const portCmp = (a.rules.ports[0]?.start ?? 0) - (b.rules.ports[0]?.start ?? 0);
    if (portCmp !== 0) return portCmp;
    return a.name.localeCompare(b.name) || a.id.localeCompare(b.id);
  });
}

function sortRemoteServices(services: Offering[], data: Status): Offering[] {
  return [...services].sort((a, b) => {
    const aFwd = forwardsOfService(a.id, data).length > 0 ? 1 : 0;
    const bFwd = forwardsOfService(b.id, data).length > 0 ? 1 : 0;
    if (aFwd !== bFwd) return bFwd - aFwd;
    const aSelf = data.member && a.member === data.member ? 1 : 0;
    const bSelf = data.member && b.member === data.member ? 1 : 0;
    if (aSelf !== bSelf) return bSelf - aSelf;
    const mCmp = a.member.localeCompare(b.member);
    if (mCmp !== 0) return mCmp;
    const dCmp = (a.device || "").localeCompare(b.device || "");
    if (dCmp !== 0) return dCmp;
    const ipCmp = toInt(a.rules.ips[0]?.start ?? "0.0.0.0") - toInt(b.rules.ips[0]?.start ?? "0.0.0.0");
    if (ipCmp !== 0) return ipCmp;
    const portCmp = (a.rules.ports[0]?.start ?? 0) - (b.rules.ports[0]?.start ?? 0);
    if (portCmp !== 0) return portCmp;
    return a.id.localeCompare(b.id);
  });
}

function flatUnifiedList(localShares: Share[], remoteServices: Offering[], data: Status, routing: boolean): HTMLElement {
  const localByIp = new Map<string, Share[]>();
  for (const s of data.shares) {
    const ip = singleIp(s);
    if (!ip) continue;
    const arr = localByIp.get(ip);
    if (arr) arr.push(s);
    else localByIp.set(ip, [s]);
  }
  const localIpOs = new Map<string, HostOs | null>();
  for (const [ip, items] of localByIp) localIpOs.set(ip, resolveHostOs(items) as HostOs | null);

  const remoteByOwnerIp = new Map<string, Offering[]>();
  for (const s of data.services) {
    const ip = singleIp(s);
    if (!ip) continue;
    const k = `${s.owner}:${ip}`;
    const arr = remoteByOwnerIp.get(k);
    if (arr) arr.push(s);
    else remoteByOwnerIp.set(k, [s]);
  }
  const remoteIpOs = new Map<string, HostOs | null>();
  for (const [k, items] of remoteByOwnerIp) {
    remoteIpOs.set(k, resolveHostOs(items, items.map(x => probes.get(serviceKey(x))?.result?.hostOs)) as HostOs | null);
  }

  const rows: HTMLElement[] = [
    ...sortLocalShares(localShares).map((s) => flatMineRow(s, data, localIpOs)),
    ...sortRemoteServices(remoteServices, data).map((s) => flatRemoteRow(s, data, routing, remoteIpOs)),
  ];
  return el("div", "list-card flat-list", ...rows);
}

function restorePromptCard(profile: DeviceProfile) {
  const bodyText = t("restore_banner_body")
    .replace("{shares}", String(profile.shares.length))
    .replace("{forwards}", String(profile.forwards.length));
  const osEl = osBadge(profile.os, "device");
  return el("div", "restore-card",
    el("div", "restore-info",
      el("div", "host-title-line",
        ...(osEl ? [osEl] : []),
        el("span", "restore-title", t("restore_banner_title")),
        el("span", "mono muted", `#${profile.machine_id.slice(0, 6)}`)
      ),
      el("p", "muted", bodyText)
    ),
    el("div", "restore-actions",
      button(t("dismiss"), () => void act({ op: "dismiss_restore" }), "button"),
      button(t("restore_now"), () => void act({
        op: "import_profile",
        machine_id: profile.machine_id,
        import_shares: true,
        import_forwards: true,
      }, () => notify(t("profile_imported"))), "button primary")
    )
  );
}

function updateServicesFlow(data: Status) {
  const hasRemoteAll = data.services.length > 0;
  const hasLocalAll = data.shares.length > 0;
  const prompt = data.restore_prompt ?? null;
  if (prompt) {
    restoreFlowHost.hidden = false;
    restoreFlowHost.replaceChildren(restorePromptCard(prompt));
  } else {
    restoreFlowHost.hidden = true;
    restoreFlowHost.replaceChildren();
  }

  if (!hasRemoteAll && !hasLocalAll) {
    listToolbar.hidden = true;
    syncSearchBar(data);
    mainFlowSection.hidden = true;
    mainFlowSection.replaceChildren();
    emptyFlowHost.hidden = false;
    const emptyEl = !data.online
      ? (data.running
        ? empty(t("connecting_now"))
        : empty(t("group_disconnected"), button(t("connect_now"), () => void act({ op: "start" }), "button primary"), button(t("new_share"), () => openAddShare(), "button")))
      : empty(t("no_services"), addShare(true));
    emptyFlowHost.replaceChildren(emptyEl);
    return;
  }

  listToolbar.hidden = false;
  emptyFlowHost.hidden = true;
  emptyFlowHost.replaceChildren();
  mainFlowSection.hidden = false;

  const tokens = parseSearchTokens(searchQuery);
  const searching = tokens.length > 0;
  const remoteServices = searching
    ? data.services.filter((s) => matchesTokens(remoteSearchHaystack(s, data), tokens))
    : data.services;
  const localShares = searching
    ? data.shares.filter((s) => matchesTokens(localSearchHaystack(s, data), tokens))
    : data.shares;

  const totalAll = data.services.length + data.shares.length;
  const totalMatched = remoteServices.length + localShares.length;
  syncSearchBar(data, totalMatched, totalAll);

  const hasRemote = remoteServices.length > 0;
  const hasLocal = localShares.length > 0;

  const routing = data.system_routing !== false;
  const remoteAccounts = hasRemote ? buildRemoteAccounts(remoteServices, data) : [];
  const localEntries = hasLocal ? (groupByHost(localShares, (s: Share) => singleIp(s)) as LocalEntry[]) : [];

  // Build collapse targets (device & host cards; keep account headers expanded during batch collapse)
  const itemKeys: string[] = [];
  if (localShares.length >= 2) {
    itemKeys.push("dev:local");
    const singleLocalIp = localEntries.length === 1 && localEntries[0]!.grouped && localEntries[0]!.items.length >= 2;
    if (!singleLocalIp) {
      for (const entry of localEntries) {
        if (entry.grouped && entry.items.length >= 2) itemKeys.push(`lhost:${entry.ip}`);
      }
    }
  }
  for (const acct of remoteAccounts) {
    for (const bucket of acct.buckets) {
      if (bucket.items.length >= 2) {
        itemKeys.push(`dev:${bucket.owner}`);
        const isSingleIpCard = bucket.entries.length === 1 && bucket.entries[0]!.grouped && bucket.entries[0]!.items.length >= 2;
        if (!isSingleIpCard) {
          for (const entry of bucket.entries) {
            if (entry.grouped && entry.items.length >= 2) itemKeys.push(`rhost:${bucket.owner}:${entry.ip}`);
          }
        }
      }
    }
  }

  const allCollapsedActive = viewMode === "grouped" && !searching && itemKeys.length > 0
    && itemKeys.every((k) => isCollapsed(k));
  const activeDensity: "normal" | "collapsed" | "compact" =
    viewMode === "flat" ? "compact" : allCollapsedActive ? "collapsed" : "normal";

  function viewIconBtn(ico: IconName, label: string, mode: "normal" | "compact" | "collapsed", onClick: () => void) {
    const b = el("button", "", icon(ico));
    b.type = "button";
    b.title = label;
    b.setAttribute("aria-label", label);
    b.dataset.viewMode = mode;
    b.setAttribute("aria-pressed", String(activeDensity === mode));
    b.addEventListener("click", onClick);
    return b;
  }

  const normalBtn = viewIconBtn("cards", t("view_grouped"), "normal", () => {
    viewMode = "grouped";
    try { localStorage.setItem(VIEW_MODE_KEY, "grouped"); } catch {}
    if (itemKeys.length > 0 && itemKeys.every((k) => isCollapsed(k))) {
      for (const k of itemKeys) collapsed.delete(k);
      saveCollapsed();
    }
    render(true);
  });

  const flatBtn = viewIconBtn("list", t("view_flat"), "compact", () => setViewMode("flat"));

  const collapsedBtn = viewIconBtn("collapse", t("view_collapsed"), "collapsed", () => {
    if (searchQuery) {
      searchQuery = "";
      searchInput.value = "";
    }
    viewMode = "grouped";
    try { localStorage.setItem(VIEW_MODE_KEY, "grouped"); } catch {}
    for (const k of itemKeys) collapsed.add(k);
    saveCollapsed();
    render(true);
  });
  collapsedBtn.disabled = itemKeys.length === 0;

  viewSegHost.setAttribute("aria-label", t("view_mode"));
  viewSegHost.replaceChildren(normalBtn, flatBtn, collapsedBtn);

  if (!hasRemote && !hasLocal) {
    mainFlowSection.hidden = true;
    mainFlowSection.replaceChildren();
    emptyFlowHost.hidden = false;
    emptyFlowHost.replaceChildren(empty(t("search_no_results"), button(t("clear_search"), () => {
      searchQuery = "";
      searchInput.value = "";
      refreshServicesResults();
      searchInput.focus();
    }, "button")));
    return;
  }

  if (viewMode === "flat") {
    const flatEl = flatUnifiedList(localShares, remoteServices, data, routing);
    mainFlowSection.replaceChildren(
      flatEl,
      ...(!routing && hasRemote ? [el("p", "routing-note", t("routing_unavailable"))] : [])
    );
    return;
  }

  // Unified grouped hierarchy: 本账号 (本机 + 本账号其他设备) followed by 组内其他成员
  const selfAcct = remoteAccounts.find((a) => a.self) ?? null;
  const otherAccts = remoteAccounts.filter((a) => !a.self);
  const selfRemoteBuckets = selfAcct?.buckets ?? [];
  const myDeviceCount = (hasLocal ? 1 : 0) + selfRemoteBuckets.length;

  const listNodes: HTMLElement[] = [];
  if (myDeviceCount === 1) {
    if (hasLocal) {
      listNodes.push(renderLocalBucket(localShares, localEntries, data, true, searching));
    } else if (selfRemoteBuckets[0]) {
      listNodes.push(renderDeviceBucket(selfRemoteBuckets[0], data, routing, true, searching));
    }
  } else if (myDeviceCount >= 2) {
    const memberKey = data.member || "_self";
    const acctKey = `acct:${memberKey}`;
    const acctExpanded = searching || !isCollapsed(acctKey);
    const totalSelfServices = localShares.length + selfRemoteBuckets.reduce((sum, b) => sum + b.items.length, 0);
    const head = el("div", "account-head",
      collapseToggle(acctKey, acctExpanded, "account-toggle",
        el("span", "account-name self", t("source_self")),
        el("span", "account-sub", t("devices_count").replace("{n}", String(myDeviceCount))),
        el("span", "count", String(totalSelfServices))
      )
    );
    const bodyChildren: HTMLElement[] = [
      ...(hasLocal ? [renderLocalBucket(localShares, localEntries, data, false, searching)] : []),
      ...selfRemoteBuckets.map((bucket) => renderDeviceBucket(bucket, data, routing, false, searching)),
    ];
    const body = el("div", "account-body", ...bodyChildren);
    body.hidden = !acctExpanded;
    listNodes.push(el("div", "account-group", head, body));
  }

  for (const acct of otherAccts) {
    if (acct.buckets.length === 1) {
      listNodes.push(renderDeviceBucket(acct.buckets[0]!, data, routing, true, searching));
      continue;
    }
    const acctKey = `acct:${acct.memberKey}`;
    const acctExpanded = searching || !isCollapsed(acctKey);
    const head = el("div", "account-head",
      collapseToggle(acctKey, acctExpanded, "account-toggle",
        el("span", "account-name", acct.memberLabel),
        el("span", "account-sub", t("devices_count").replace("{n}", String(acct.buckets.length))),
        el("span", "count", String(acct.totalServices))
      )
    );
    const body = el("div", "account-body", ...acct.buckets.map((bucket) => renderDeviceBucket(bucket, data, routing, false, searching)));
    body.hidden = !acctExpanded;
    listNodes.push(el("div", "account-group", head, body));
  }

  mainFlowSection.replaceChildren(
    el("div", "service-list", ...listNodes),
    ...(!routing && hasRemote ? [el("p", "routing-note", t("routing_unavailable"))] : [])
  );
}

function addShare(emptyState = false) {
  if (emptyState) return button(t("new_share"), () => openAddShare(), "button primary");
  return createIconButton({
    icon: "plus",
    title: t("new_share"),
    variant: "solid",
    size: "lg",
    onClick: () => openAddShare(),
  });
}

function openRenameDevice(currentName: string) {
  settingsDialog.close();
  edit(t("current_device"), submit(t("save")), (values) => {
    void act({ op: "rename_device", label: str(values, "label").trim() }, () => editor.close());
  }, field(t("device"), "label", currentName, { required: true, maxLength: 64 }));
}

function openSettings() {
  let currentBehavior = getCloseBehavior();
  let saveError = false;
  let prefAutoConnect = status?.auto_connect ?? true;
  let prefAutoShare = status?.auto_share ?? true;
  let prefCloudBackup = status?.cloud_backup ?? true;

  openSettingsModal(settingsDialog, {
    t: (k) => t(k as Parameters<typeof t>[0]),
    theme,
    scale,
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

      if (status) {
        nodes.push(el("div", "settings-divider"));

        const connSeg = el("div", "segmented");
        for (const [val, key] of [[true, "connect_auto"], [false, "connect_manual"]] as const) {
          const btn = button(t(key), () => {
            if (prefAutoConnect === val) return;
            const prev = prefAutoConnect;
            prefAutoConnect = val;
            redraw();
            void request({ op: "set_preferences", auto_connect: val }, false)
              .then((next) => { status = next; render(); })
              .catch(() => {
                prefAutoConnect = prev;
                if (settingsDialog.open) redraw();
              });
          }, val === prefAutoConnect ? "active" : "");
          btn.setAttribute("aria-pressed", String(val === prefAutoConnect));
          connSeg.append(btn);
        }
        nodes.push(el("div", "settings-row", el("span", "settings-label", t("on_launch_connect")), connSeg));

        const shareSeg = el("div", "segmented");
        for (const [val, key] of [[true, "restore_same_net"], [false, "restore_off"]] as const) {
          const btn = button(t(key), () => {
            if (prefAutoShare === val) return;
            const prev = prefAutoShare;
            prefAutoShare = val;
            redraw();
            void request({ op: "set_preferences", auto_share: val }, false)
              .then((next) => { status = next; render(); })
              .catch(() => {
                prefAutoShare = prev;
                if (settingsDialog.open) redraw();
              });
          }, val === prefAutoShare ? "active" : "");
          btn.setAttribute("aria-pressed", String(val === prefAutoShare));
          shareSeg.append(btn);
        }
        nodes.push(el("div", "settings-row", el("span", "settings-label", t("restore_shares")), shareSeg));

        const backupSeg = el("div", "segmented");
        for (const [val, key] of [[true, "backup_on"], [false, "backup_off"]] as const) {
          const btn = button(t(key), () => {
            if (prefCloudBackup === val) return;
            const prev = prefCloudBackup;
            prefCloudBackup = val;
            redraw();
            void request({ op: "set_preferences", cloud_backup: val }, false)
              .then((next) => { status = next; render(); })
              .catch(() => {
                prefCloudBackup = prev;
                if (settingsDialog.open) redraw();
              });
          }, val === prefCloudBackup ? "active" : "");
          btn.setAttribute("aria-pressed", String(val === prefCloudBackup));
          backupSeg.append(btn);
        }
        nodes.push(el("div", "settings-row", el("span", "settings-label", t("cloud_backup")), backupSeg));
      }

      if (status?.device_name) {
        nodes.push(el("div", "settings-divider"));
        const devOsEl = osBadge(status.device_os, { fallback: "device", local: true, prefix: t("badge_local") });
        const shortId = status.machine_id ? `#${status.machine_id.slice(0, 6)}` : "";
        const devVal = el("div", "settings-window",
          el("span", "settings-device-val",
            ...(devOsEl ? [devOsEl] : []),
            el("span", "settings-device-name", status.device_name),
            ...(shortId ? [el("span", "mono muted", shortId)] : [])
          ),
          button(t("rename_device"), () => openRenameDevice(status?.device_name ?? ""), "button")
        );
        nodes.push(el("div", "settings-row", el("span", "settings-label", t("current_device")), devVal));
      }

      const profiles = (status?.cloud_profiles ?? []).filter(p => p.shares.length > 0 || p.forwards.length > 0);
      if (status?.member && profiles.length) {
        const list = el("div", "settings-profile-list", ...profiles.map((p) => {
          const osEl = osBadge(p.os, "device");
          const counts = `${p.shares.length}/${p.forwards.length}`;
          const impBtn = button(t("import_profile"), () => {
            void request({
              op: "import_profile",
              machine_id: p.machine_id,
              import_shares: true,
              import_forwards: true,
            }, false).then((next) => {
              status = next;
              render(true);
              notify(t("profile_imported"));
              if (settingsDialog.open) redraw();
            }).catch((err) => notify(failure(err), true));
          }, "button");
          const delBtn = tool("close", t("delete_backup"), () => {
            void request({ op: "delete_profile", machine_id: p.machine_id }, false)
              .then((next) => {
                status = next;
                render(true);
                if (settingsDialog.open) redraw();
              })
              .catch((err) => notify(failure(err), true));
          });
          return el("div", "settings-profile-row",
            el("div", "settings-profile-meta",
              ...(osEl ? [osEl] : []),
              el("span", "settings-device-name", p.name),
              el("span", "mono muted", `#${p.machine_id.slice(0, 6)} · ${counts}`)
            ),
            el("div", "settings-profile-btns", impBtn, delBtn)
          );
        }));
        nodes.push(el("div", "field-group", el("span", "settings-label", t("cloud_profiles")), list));
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
  const b = createIconButton({
    icon: symbol,
    title: tip,
    variant: "solid",
    size: "lg",
    tone: online ? "accent" : "muted",
    spinning: connecting,
    onClick: () => void act({ op: running ? "stop" : "start" }),
  });
  b.setAttribute("data-connect-btn", "1");
  b.setAttribute("aria-pressed", String(running));
  return b;
}

function render(force = false) {
  checkSiteDialog();
  showError();
  const data = status;
  const blocked = !!data?.auto_share_blocked;
  if (blocked && !lastAutoShareBlocked) {
    notify(t("auto_share_network_changed"));
  }
  lastAutoShareBlocked = blocked;
  if (!data?.admin_ready && tab === "admin") tab = "services";
  syncBrand(!brandBooted);
  const owner = data?.member ? JSON.stringify([data.server, data.member]) : "";
  if (owner !== tabOwner) {
    tabOwner = owner;
    tab = "services";
    searchQuery = "";
    searchInput.value = "";
  }
  loadCollapsed(owner);
  if (!data?.online) probes.clear();
  else if (probes.size) {
    const listed = new Map(data.services.map(s => [serviceKey(s), s]));
    for (const [key, probe] of probes) {
      const s = listed.get(key);
      const ip = toInt(probe.ip);
      if (!s || effectiveKind(s) !== probe.kind || accessIpFor(s.id, probe.ip, data) !== probe.access
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
  const settingsBtn = createIconButton({
    icon: "settings",
    title: t("settings"),
    variant: "solid",
    size: "lg",
    onClick: () => openSettings(),
  });
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
      const devOsEl = osBadge(data.device_os, { fallback: "device", local: true, prefix: t("badge_local") });
      const shortId = data.machine_id ? `#${data.machine_id.slice(0, 6)}` : "";
      const devChip = data.device_name ? el("div", "signin-device",
        ...(devOsEl ? [devOsEl] : []),
        el("span", "signin-device-name", data.device_name),
        ...(shortId ? [el("span", "mono muted", shortId)] : [])
      ) : null;
      content.replaceChildren(el("section", "signin",
        form((values) => void act({ op: "login", username: str(values, "username").trim(),
          password: str(values, "password"), label: data.device_name ?? "" }),
        el("h2", "form-title", t("login")),
        el("div", "signin-server",
          el("span", "mono signin-server-host", new URL(data.server).host),
          button(t("change"), () => { joining = true; render(true); }, "button ghost")),
        field(t("username"), "username", "", { required: true, autocomplete: "username", maxLength: 64 }),
        field(t("password"), "password", "", { required: true, type: "password", autocomplete: "current-password" }),
        ...(devChip ? [devChip] : []),
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

  const activeEl = document.activeElement as HTMLElement | null;
  const kbFocusVisible = !!activeEl && content.contains(activeEl) && (() => {
    try { return activeEl.matches(":focus-visible"); } catch { return false; }
  })();
  const focusedCollapseKey = kbFocusVisible ? activeEl?.closest<HTMLElement>("[data-collapse-key]")?.dataset.collapseKey : undefined;
  const focusedViewMode = kbFocusVisible ? activeEl?.closest<HTMLElement>("[data-view-mode]")?.dataset.viewMode : undefined;

  const viewKey = "client:services";
  const animateEnter = viewKey !== lastViewKey;
  lastViewKey = viewKey;
  servicesPanel.className = animateEnter ? "panel panel-enter" : "panel";
  if (servicesPanel.parentElement !== content) {
    content.replaceChildren(servicesPanel);
  }
  updateServicesFlow(data);

  if (focusedCollapseKey) {
    const target = [...content.querySelectorAll<HTMLElement>("[data-collapse-key]")]
      .find((n) => n.dataset.collapseKey === focusedCollapseKey);
    target?.focus({ preventScroll: true });
  } else if (focusedViewMode) {
    const target = [...content.querySelectorAll<HTMLElement>("[data-view-mode]")]
      .find((n) => n.dataset.viewMode === focusedViewMode);
    target?.focus({ preventScroll: true });
  }
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
  if (status?.member && status.auto_connect !== false && !status.running && !status.online && !pendingJoin) {
    void act({ op: "start" });
  }
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
