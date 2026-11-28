import { i18n } from "../i18n";
import type { Label, LocalForwardStatus, Offering, Probe, Status, ViewMode } from "./types";

export const mac = /Macintosh/.test(navigator.userAgent);
export const desktop = !/Android/i.test(navigator.userAgent);
export const win = desktop && /Windows/i.test(navigator.userAgent);
if (desktop) document.documentElement.classList.add("os-desktop");
if (mac) document.documentElement.classList.add("os-macos");
if (win) document.documentElement.classList.add("os-windows");

const VIEW_MODE_KEY = "fesen_view_mode";
const COLLAPSE_STORE_KEY = "fesen_collapsed";

function readViewMode(): ViewMode {
  try {
    const v = localStorage.getItem(VIEW_MODE_KEY);
    if (v === "flat" || v === "grouped") return v;
  } catch {}
  return "grouped";
}

export const state = {
  status: undefined as Status | undefined,
  tab: "services" as "services" | "admin",
  tabOwner: "",
  joining: false,
  busy: false,
  polling: false,
  message: "",
  applied: "",
  lastViewKey: "",
  pendingJoin: undefined as string | undefined,
  appVersion: "",
  lastAutoShareBlocked: false,
  brandBooted: false,
  viewMode: readViewMode() as ViewMode,
  searchQuery: "",
};

export const probes = new Map<string, Probe>();
export const serviceKey = (service: Offering) => `${service.owner}:${service.id}`;

export function persistViewMode(next: ViewMode) {
  state.viewMode = next;
  try { localStorage.setItem(VIEW_MODE_KEY, next); } catch {}
}

let collapsedScope = "";
let collapsed = new Set<string>();

export function loadCollapsed(scope: string) {
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

export function saveCollapsed() {
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

export function isCollapsed(key: string): boolean {
  return collapsed.has(key);
}

export function setCollapsedKeys(keys: string[], shouldCollapse: boolean) {
  for (const k of keys) {
    if (shouldCollapse) collapsed.add(k);
    else collapsed.delete(k);
  }
  saveCollapsed();
}

export function toggleCollapsedKey(key: string) {
  if (collapsed.has(key)) collapsed.delete(key);
  else collapsed.add(key);
  saveCollapsed();
}

export type StatusIndex = {
  labels: Map<string, Label>;
  forwards: Map<string, LocalForwardStatus[]>;
  active: Set<string>;
  remoteHaystack: Map<string, string>;
  localHaystack: Map<string, string>;
  locale: string;
};

export const EMPTY_FORWARDS: LocalForwardStatus[] = [];
const statusIndexCache = new WeakMap<Status, StatusIndex>();

export function indexOf(data: Status): StatusIndex {
  const curLocale = i18n.getLocale();
  let idx = statusIndexCache.get(data);
  if (idx) {
    if (idx.locale !== curLocale) {
      idx.remoteHaystack.clear();
      idx.localHaystack.clear();
      idx.locale = curLocale;
    }
    return idx;
  }
  const labels = new Map<string, Label>();
  for (const l of data.labels) labels.set(l.service, l);
  const forwards = new Map<string, LocalForwardStatus[]>();
  if (data.forwards) {
    for (const f of data.forwards) {
      const arr = forwards.get(f.service);
      if (arr) arr.push(f);
      else forwards.set(f.service, [f]);
    }
  }
  idx = {
    labels,
    forwards,
    active: new Set(data.active),
    remoteHaystack: new Map(),
    localHaystack: new Map(),
    locale: curLocale,
  };
  statusIndexCache.set(data, idx);
  return idx;
}

export const labelFor = (serviceId: string, data?: Status): Label | undefined =>
  data ? indexOf(data).labels.get(serviceId) : undefined;

export const isActiveShare = (shareId: string, data?: Status): boolean =>
  data ? indexOf(data).active.has(shareId) : false;

export const forwardsOfService = (serviceId: string, data?: Status): LocalForwardStatus[] =>
  data ? (indexOf(data).forwards.get(serviceId) ?? EMPTY_FORWARDS) : EMPTY_FORWARDS;

export const forwardFor = (
  serviceId: string,
  ip: string,
  port: number,
  data?: Status,
): LocalForwardStatus | undefined =>
  forwardsOfService(serviceId, data).find((f) => f.target_ip === ip && f.target_port === port);
