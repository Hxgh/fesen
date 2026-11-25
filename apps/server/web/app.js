import { createTrigram, icon } from "./icons.js";

const byId = (id) => document.getElementById(id);

const copy = {
  brand: ["飞弦", "Fesen"],
  admin_badge: ["管理台", "Admin"],
  title: ["飞弦 · 管理台", "Fesen · Admin"],
  workspace: ["组管理", "Group management"],
  loading: ["加载中…", "Loading…"],
  login_title: ["管理员登录", "Administrator sign-in"],
  account: ["账号", "Username"],
  server: ["服务器", "Server"],
  settings: ["设置", "Settings"],
  appearance: ["外观", "Appearance"],
  theme_system: ["跟随系统", "System"],
  theme_light: ["浅色", "Light"],
  theme_dark: ["深色", "Dark"],
  language: ["语言", "Language"],
  lang_zh: ["简体中文", "简体中文"],
  lang_en: ["English", "English"],
  configure: ["操作", "Actions"],
  members: ["成员", "Members"],
  devices: ["设备", "Devices"],
  services: ["服务", "Services"],
  conflicts: ["冲突", "Conflicts"],
  devices_empty: ["暂无设备接入", "No devices yet"],
  services_empty: ["暂无广播服务", "No active services"],
  password: ["密码", "Password"],
  new_password: ["新密码", "New password"],
  login: ["登录", "Sign in"],
  logout: ["退出登录", "Sign out"],
  join: ["接入链接", "Join link"],
  copy_link: ["复制链接", "Copy link"],
  copied: ["已复制", "Copied"],
  open_join: ["打开接入页", "Open join page"],
  close: ["关闭", "Close"],
  create: ["新建成员", "Add member"],
  reset: ["重置密码", "Reset password"],
  refresh: ["刷新", "Refresh"],
  cancel: ["取消", "Cancel"],
  admin: ["管理员", "Admin"],
  disabled: ["已停用", "Disabled"],
  enabled: ["正常", "Active"],
  enable: ["启用", "Enable"],
  disable: ["停用", "Disable"],
  revoked: ["已撤销", "Revoked"],
  online: ["在线", "Online"],
  offline: ["离线", "Offline"],
  broadcasting: ["广播中", "Live"],
  revoke: ["撤销设备", "Revoke device"],
  confirm_disable: ["停用后，该账号下所有设备将立即断开。", "All devices on this account will disconnect."],
  confirm_revoke: ["撤销后，该设备将断开且无法继续接入。", "This device will lose access."],
  confirm_reset: ["重置后，该账号下所有设备须重新登录。", "All devices on this account will need to sign in again."],
  unauthorized: ["未授权或账号密码错误", "Unauthorized or incorrect credentials"],
  conflict: ["账号已存在", "Account already exists"],
  invalid: ["输入无效", "Invalid input"],
  busy: ["请求过多，请稍后再试", "Too many requests"],
  network: ["无法连接后台", "Cannot reach server"],
  internal: ["操作失败", "Action failed"],
  audit_ok_title: ["规则无冲突", "No rule conflicts"],
  audit_conflict_title: ["检测到规则冲突", "Rule conflicts detected"],
  audited_services: ["已审计 ", "Audited "],
  precheck_title: ["冲突预检", "Conflict pre-check"],
  precheck_run: ["预检", "Check"],
  precheck_safe: ["无冲突", "No conflicts"],
  precheck_conflict: ["与以下服务冲突：", "Overlaps with:"],
  invalid_rule_input: ["请输入有效的 IPv4/CIDR 与端口", "Enter valid IPv4/CIDR and port"],
  ipv4: ["IPv4 地址", "IPv4 address"],
  port: ["端口", "Port"],
};

// Reusable Bagua brand mark: Kun ☷ (idle) -> orderly single-yao evolution -> Gen ☶ (active)
const topMark = createTrigram("connecting");
byId("brand-mark").replaceWith(topMark.el);

function getInitialLocale() {
  try {
    const saved = localStorage.getItem("fesen_admin_locale");
    if (saved === "zh-CN" || saved === "en") return saved;
  } catch {}
  const language = navigator.languages.find((value) => /^(zh|en)(-|$)/i.test(value));
  return /^zh(-|$)/i.test(language ?? "") ? "zh-CN" : "en";
}

let activeLocale = getInitialLocale();
const isZh = () => activeLocale === "zh-CN";
const t = (key) => (Object.hasOwn(copy, key) ? copy[key] : copy.internal)[isZh() ? 0 : 1];

function getTheme() {
  try {
    const saved = localStorage.getItem("fesen_admin_theme");
    if (saved === "light" || saved === "dark" || saved === "system") return saved;
  } catch {}
  return "system";
}

function applyTheme(theme = getTheme()) {
  document.documentElement.setAttribute("data-theme", theme);
  const isDark = theme === "dark" || (theme === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.classList.toggle("dark", isDark);
}

function setTheme(theme) {
  try { localStorage.setItem("fesen_admin_theme", theme); } catch {}
  applyTheme(theme);
}

function setLocale(loc) {
  activeLocale = loc;
  try { localStorage.setItem("fesen_admin_locale", loc); } catch {}
  updateAllCopy();
}

window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
  if (getTheme() === "system") applyTheme("system");
});

applyTheme(getTheme());
document.querySelectorAll("[data-icon]").forEach((el) => el.replaceWith(icon(el.dataset.icon)));

let currentSessionData = null;
let currentHost = "";
let activeTabIndex = 0;

function updateAllCopy() {
  document.documentElement.lang = activeLocale;
  document.title = t("title");
  document.querySelectorAll("[data-copy]").forEach((el) => {
    el.textContent = t(el.dataset.copy);
  });
  document.querySelectorAll("[data-label]").forEach((el) => {
    el.setAttribute("aria-label", t(el.dataset.label));
    if (el.tagName === "BUTTON") el.title = t(el.dataset.label);
  });
  syncBrandTip();
  if (currentSessionData) show(currentSessionData, true);
}

function syncBrandTip() {
  const parts = [t("brand"), t("admin_badge"), currentHost].filter(Boolean);
  byId("brand-badge").title = parts.join(" · ");
}

updateAllCopy();

async function api(path, body) {
  const response = await fetch(path, body ? {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Fesen-Admin": "1" },
    body: JSON.stringify(body),
  } : undefined).catch(() => { throw new Error(t("network")); });
  if (!response.ok) {
    const data = await response.json().catch(() => ({}));
    throw Object.assign(new Error(t(data.error)), { status: response.status });
  }
  return response.status === 204 ? null : response.json();
}

function element(tag, cls = "", text = "") {
  const el = document.createElement(tag);
  if (cls) el.className = cls;
  if (text) el.textContent = text;
  return el;
}

// Anchored popover menu (icon + text)
let activeAdminMenu = null;
function closeAdminMenu() {
  if (!activeAdminMenu) return;
  activeAdminMenu.cleanup();
  activeAdminMenu.anchor.setAttribute("aria-expanded", "false");
  activeAdminMenu.el.remove();
  activeAdminMenu = null;
}

function openAdminMenu(anchor, entries) {
  if (activeAdminMenu?.anchor === anchor) { closeAdminMenu(); return; }
  closeAdminMenu();
  const menu = element("div", "popover-menu");
  menu.setAttribute("role", "menu");
  for (const entry of entries) {
    const item = element("button", `menu-item${entry.danger ? " danger" : ""}`, t(entry.label));
    item.type = "button";
    item.setAttribute("role", "menuitem");
    item.prepend(icon(entry.icon));
    item.addEventListener("click", () => { closeAdminMenu(); entry.action(); });
    menu.append(item);
  }
  document.body.append(menu);
  const rect = anchor.getBoundingClientRect();
  const mw = menu.offsetWidth, mh = menu.offsetHeight;
  const left = Math.max(12, Math.min(rect.right - mw, window.innerWidth - mw - 12));
  const flipUp = rect.bottom + mh + 12 > window.innerHeight && rect.top - mh - 12 >= 0;
  menu.style.left = `${Math.round(left)}px`;
  menu.style.top = `${Math.round(flipUp ? rect.top - mh - 6 : rect.bottom + 6)}px`;
  menu.setAttribute("data-origin", flipUp ? "bottom" : "top");
  anchor.setAttribute("aria-expanded", "true");
  const onDown = (e) => { if (!menu.contains(e.target) && !anchor.contains(e.target)) closeAdminMenu(); };
  const onKey = (e) => { if (e.key === "Escape") { closeAdminMenu(); anchor.focus(); } };
  const onDismiss = (e) => { if (e.type === "scroll" && menu.contains(e.target)) return; closeAdminMenu(); };
  window.addEventListener("pointerdown", onDown, true);
  window.addEventListener("keydown", onKey, true);
  window.addEventListener("resize", onDismiss);
  window.addEventListener("scroll", onDismiss, true);
  activeAdminMenu = {
    el: menu,
    anchor,
    cleanup: () => {
      window.removeEventListener("pointerdown", onDown, true);
      window.removeEventListener("keydown", onKey, true);
      window.removeEventListener("resize", onDismiss);
      window.removeEventListener("scroll", onDismiss, true);
    },
  };
}

function option(label, action, danger = false, symbol = "sliders") {
  return { label, action, danger, icon: symbol };
}

function entityRow(name, status, tone, actions, meta) {
  const row = element("div", "entity-row");
  const main = element("div", "entity-main");
  main.append(element("span", "entity-name", name));
  if (meta) main.append(element("span", "entity-meta", meta));

  const right = element("div", "entity-actions");
  right.append(element("span", `state-dot${tone ? ` ${tone}` : ""}`, t(status)));
  if (actions.length) {
    const more = element("button", "button icon-button");
    more.type = "button";
    more.title = `${t("configure")} · ${name}`;
    more.setAttribute("aria-label", more.title);
    more.setAttribute("aria-haspopup", "menu");
    more.setAttribute("aria-expanded", "false");
    more.append(icon("more"));
    more.addEventListener("click", () => openAdminMenu(more, actions));
    right.append(more);
  } else {
    right.append(element("span", "entity-slot"));
  }
  row.append(main, right);
  return row;
}

// Service aggregation (mirrors client single-IP host card grouping)
const singleSpan = (arr) => Array.isArray(arr) && arr.length === 1 && arr[0].start === arr[0].end;
const serviceSingleIp = (s) => singleSpan(s.rules?.ips) ? s.rules.ips[0].start : null;
const serviceKind = (s) => s.kind || (s.http ? "http" : "tcp");
const formatRanges = (spans) => (spans || []).map(r => r.start === r.end ? String(r.start) : `${r.start}–${r.end}`).join(", ");
const formatEndpoint = (rules) => {
  if (!rules) return "-";
  const ips = formatRanges(rules.ips);
  const ports = formatRanges(rules.ports);
  return `${ips || "*"}${singleSpan(rules.ips) && singleSpan(rules.ports) ? ":" : " · "}${ports || "*"}`;
};
const osTag = (os) => os === "windows" ? "Windows" : os === "linux" ? "Linux" : "";

function resolveGroupOs(items) {
  const hasOsCapable = items.some(s => serviceKind(s) === "ssh" || serviceKind(s) === "rdp");
  if (!hasOsCapable) return "";
  if (items.some(s => s.host_os === "windows" || serviceKind(s) === "rdp")) return "Windows";
  if (items.some(s => s.host_os === "linux")) return "Linux";
  return "";
}

function renderServices(services) {
  const host = byId("services-list");
  host.replaceChildren();
  const entries = [];
  const buckets = new Map();
  for (const s of services) {
    const ip = serviceSingleIp(s);
    const key = ip ? `${s.owner || s.member}:${ip}` : null;
    if (!ip || !key) {
      entries.push({ grouped: false, item: s });
      continue;
    }
    let bucket = buckets.get(key);
    if (!bucket) {
      bucket = { grouped: true, ip, items: [] };
      buckets.set(key, bucket);
      entries.push(bucket);
    }
    bucket.items.push(s);
  }
  for (const entry of entries) {
    if (!entry.grouped || entry.items.length === 1) {
      const s = entry.grouped ? entry.items[0] : entry.item;
      const kind = serviceKind(s);
      const os = resolveGroupOs([s]);
      const card = element("article", "service-row");
      const main = element("div", "service-main");
      const top = element("div", "host-title-line");
      top.append(element("p", "endpoint", formatEndpoint(s.rules)));
      if (os) top.append(element("span", "host-os-tag", os));
      if (kind !== "tcp") top.append(element("span", "proto-tag", kind.toUpperCase()));
      main.append(top);
      const sub = [s.name && s.name !== serviceSingleIp(s) ? s.name : "", s.member].filter(Boolean).join(" · ");
      if (sub) main.append(element("span", "entity-meta", sub));
      card.append(main, element("span", "state-dot good", t("broadcasting")));
      host.append(card);
      continue;
    }
    entry.items.sort((a, b) => (a.rules?.ports?.[0]?.start ?? 0) - (b.rules?.ports?.[0]?.start ?? 0));
    const os = resolveGroupOs(entry.items);
    const first = entry.items[0];
    const card = element("article", "host-card");
    const head = element("div", "host-head");
    const info = element("div", "host-info");
    const title = element("div", "host-title-line");
    title.append(element("p", "endpoint", entry.ip));
    if (os) title.append(element("span", "host-os-tag", os));
    info.append(title);
    const label = first.name && first.name !== entry.ip && first.name !== `${os} · ${entry.ip}` ? first.name : "";
    const meta = [label, first.member].filter(Boolean).join(" · ");
    if (meta) info.append(element("span", "entity-meta", meta));
    head.append(info, element("span", "state-dot good", t("broadcasting")));

    const portsWrap = element("div", "host-ports");
    for (const s of entry.items) {
      const kind = serviceKind(s);
      const row = element("div", "port-row");
      const pm = element("div", "port-main");
      pm.append(element("span", "port-num", `:${formatRanges(s.rules?.ports)}`));
      pm.append(element("span", "proto-tag", kind.toUpperCase()));
      if (s.name && s.name !== first.name && s.name !== entry.ip) {
        pm.append(element("span", "port-sub", s.name));
      }
      row.append(pm);
      portsWrap.append(row);
    }
    card.append(head, portsWrap);
    host.append(card);
  }
}

// Conflict math
function parseIpv4(s) {
  const parts = s.trim().split(".");
  if (parts.length !== 4) return null;
  let num = 0;
  for (const p of parts) {
    if (!/^\d+$/.test(p)) return null;
    const n = Number(p);
    if (n < 0 || n > 255) return null;
    num = (num * 256) + n;
  }
  return num >>> 0;
}

function parseIpRange(s) {
  s = s.trim();
  if (s.includes("/")) {
    const [ipStr, prefixStr] = s.split("/");
    const ip = parseIpv4(ipStr);
    const prefix = Number(prefixStr);
    if (ip === null || isNaN(prefix) || prefix < 0 || prefix > 32) return null;
    const mask = prefix === 0 ? 0 : (~0 << (32 - prefix)) >>> 0;
    const start = (ip & mask) >>> 0;
    const end = (start | (~mask >>> 0)) >>> 0;
    return [{ start, end }];
  }
  if (s.includes("-")) {
    const [a, b] = s.split("-");
    const start = parseIpv4(a), end = parseIpv4(b);
    if (start === null || end === null || start > end) return null;
    return [{ start, end }];
  }
  const ip = parseIpv4(s);
  return ip === null ? null : [{ start: ip, end: ip }];
}

function parsePortRange(s) {
  s = s.trim();
  if (s.includes("-")) {
    const [a, b] = s.split("-");
    const start = Number(a), end = Number(b);
    if (isNaN(start) || isNaN(end) || start < 1 || end > 65535 || start > end) return null;
    return [{ start, end }];
  }
  const p = Number(s);
  return (isNaN(p) || p < 1 || p > 65535) ? null : [{ start: p, end: p }];
}

function parseMultiple(str, fn) {
  const out = [];
  for (const part of str.split(/[,，]/)) {
    if (!part.trim()) continue;
    const r = fn(part);
    if (!r) return null;
    out.push(...r);
  }
  return out.length ? out : null;
}

function rangesIntersect(a, b) { return a.start <= b.end && b.start <= a.end; }

function normalizeRules(rules) {
  const ips = (rules?.ips || []).map(r => ({
    start: typeof r.start === "number" ? r.start : parseIpv4(r.start),
    end: typeof r.end === "number" ? r.end : parseIpv4(r.end),
  })).filter(r => r.start !== null && r.end !== null);
  const ports = (rules?.ports || []).map(r => ({
    start: Number(r.start), end: Number(r.end),
  })).filter(r => !isNaN(r.start) && !isNaN(r.end));
  return { ips, ports };
}

function checkConflict(ruleA, ruleB) {
  const a = normalizeRules(ruleA), b = normalizeRules(ruleB);
  return a.ips.some(r1 => b.ips.some(r2 => rangesIntersect(r1, r2)))
    && a.ports.some(r1 => b.ports.some(r2 => rangesIntersect(r1, r2)));
}

function detectAllConflicts(services = []) {
  const conflicts = [];
  for (let i = 0; i < services.length; i++) {
    for (let j = i + 1; j < services.length; j++) {
      if (checkConflict(services[i].rules, services[j].rules)) {
        conflicts.push({ a: services[i], b: services[j] });
      }
    }
  }
  return conflicts;
}

// Tabs
const tabs = [byId("tab-members"), byId("tab-devices"), byId("tab-services"), byId("tab-conflicts")];

function selectTab(index) {
  activeTabIndex = index;
  tabs.forEach((tab, i) => {
    tab.setAttribute("aria-selected", String(i === index));
    tab.tabIndex = i === index ? 0 : -1;
    byId(tab.getAttribute("aria-controls")).hidden = i !== index;
  });
  byId("new-member").hidden = index !== 0;
}

tabs.forEach((tab, index) => {
  tab.addEventListener("click", () => selectTab(index));
  tab.addEventListener("keydown", (event) => {
    let next;
    if (event.key === "ArrowRight") next = (index + 1) % tabs.length;
    else if (event.key === "ArrowLeft") next = (index - 1 + tabs.length) % tabs.length;
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = tabs.length - 1;
    if (next !== undefined) {
      event.preventDefault();
      selectTab(next);
      tabs[next].focus();
    }
  });
});

function renderAuditPanel(services, conflicts) {
  const card = byId("audit-result-card");
  card.replaceChildren();
  const ok = conflicts.length === 0;
  card.className = `audit-card ${ok ? "ok" : "has-conflict"}`;
  const head = element("div", "audit-header");
  const left = element("div", "audit-title-wrap");
  left.append(element("span", "audit-dot"), element("span", "audit-title", ok ? t("audit_ok_title") : t("audit_conflict_title")));
  head.append(left, element("span", "audit-count", `${t("audited_services")}${services.length}`));
  card.append(head);
  if (!ok) {
    const list = element("div", "conflict-list");
    conflicts.forEach(({ a, b }) => {
      list.append(element("div", "conflict-item",
        `${a.name} (${a.member}) × ${b.name} (${b.member}) · ${formatEndpoint(a.rules)} / ${formatEndpoint(b.rules)}`));
    });
    card.append(list);
  }
}

byId("precheck-form").addEventListener("submit", (e) => {
  e.preventDefault();
  const ips = parseMultiple(byId("check-ip").value, parseIpRange);
  const ports = parseMultiple(byId("check-port").value, parsePortRange);
  const resultDiv = byId("precheck-result");
  if (!ips || !ports) {
    resultDiv.hidden = false;
    resultDiv.className = "precheck-result conflict";
    resultDiv.textContent = t("invalid_rule_input");
    return;
  }
  const matched = (currentSessionData?.services || []).filter(s => checkConflict({ ips, ports }, s.rules));
  resultDiv.hidden = false;
  if (!matched.length) {
    resultDiv.className = "precheck-result";
    resultDiv.textContent = t("precheck_safe");
  } else {
    resultDiv.className = "precheck-result conflict";
    resultDiv.replaceChildren(element("div", "precheck-result-title", t("precheck_conflict")));
    matched.forEach(s => resultDiv.append(element("div", "precheck-result-line",
      `${s.name} (${s.member}) · ${formatEndpoint(s.rules)}`)));
  }
});

// Unified Settings Dialog (Theme + Language + Account/Logout)
const settingsDialog = byId("settings-dialog");
settingsDialog.addEventListener("click", (e) => { if (e.target === settingsDialog) settingsDialog.close(); });

function renderSettingsDialog() {
  settingsDialog.replaceChildren();
  const header = element("div", "dialog-header");
  const title = element("h2", "", t("settings"));
  title.id = "settings-title";
  const closeBtn = element("button", "button icon-button");
  closeBtn.type = "button";
  closeBtn.title = t("close");
  closeBtn.setAttribute("aria-label", t("close"));
  closeBtn.append(icon("close"));
  closeBtn.addEventListener("click", () => settingsDialog.close());
  header.append(title, closeBtn);

  const body = element("div", "dialog-body");

  const themeSeg = element("div", "segmented");
  for (const [id, key] of [["system", "theme_system"], ["light", "theme_light"], ["dark", "theme_dark"]]) {
    const b = element("button", id === getTheme() ? "active" : "", t(key));
    b.type = "button";
    b.setAttribute("aria-pressed", String(id === getTheme()));
    b.addEventListener("click", () => { setTheme(id); renderSettingsDialog(); });
    themeSeg.append(b);
  }
  const themeRow = element("div", "settings-row");
  themeRow.append(element("span", "settings-label", t("appearance")), themeSeg);
  body.append(themeRow);

  const langSeg = element("div", "segmented");
  for (const [id, key] of [["zh-CN", "lang_zh"], ["en", "lang_en"]]) {
    const b = element("button", id === activeLocale ? "active" : "", t(key));
    b.type = "button";
    b.setAttribute("aria-pressed", String(id === activeLocale));
    b.addEventListener("click", () => { setLocale(id); renderSettingsDialog(); });
    langSeg.append(b);
  }
  const langRow = element("div", "settings-row");
  langRow.append(element("span", "settings-label", t("language")), langSeg);
  body.append(langRow);

  if (currentSessionData) {
    const err = element("p", "feedback error");
    err.id = "account-error";
    err.setAttribute("role", "alert");
    err.hidden = true;

    const acct = element("div", "settings-account");
    acct.append(element("span", "settings-account-info", `${currentSessionData.admin} · ${currentHost}`));
    const logoutBtn = element("button", "button danger", t("logout"));
    logoutBtn.type = "button";
    logoutBtn.addEventListener("click", () => void run(logoutBtn, "account-error", async () => {
      await api("action", { op: "logout" });
      settingsDialog.close();
      show(null);
    }));
    acct.append(logoutBtn);
    body.append(element("div", "settings-divider"), acct, err);
  }

  settingsDialog.append(header, body);
}

byId("open-settings").addEventListener("click", () => {
  renderSettingsDialog();
  settingsDialog.showModal();
  settingsDialog.focus();
});

let initialResolved = false;

function show(data, preserveTab = false) {
  closeAdminMenu();
  currentSessionData = data;
  byId("loading").hidden = true;
  byId("signin").hidden = !!data;
  byId("admin").hidden = !data;
  byId("admin-actions").hidden = !data;
  byId("admin-tabs").hidden = !data;
  byId("brand-word").hidden = !!data;

  if (!data) {
    currentHost = "";
    syncBrandTip();
    topMark.setState("idle", !initialResolved);
    initialResolved = true;
    byId("link").value = "";
    byId("group-host").textContent = "";
    byId("users").replaceChildren();
    byId("devices").replaceChildren();
    byId("services-list").replaceChildren();
    selectTab(0);
    return;
  }

  currentHost = new URL(new URL(data.link).searchParams.get("server")).host;
  byId("group-host").textContent = currentHost;
  syncBrandTip();

  const services = data.services || [];
  const conflicts = detectAllConflicts(services);

  topMark.setState("active", !initialResolved);
  initialResolved = true;

  byId("member-count").textContent = String(data.users.length);
  byId("device-count").textContent = String(data.devices.length);
  byId("services-count").textContent = String(services.length);
  const cc = byId("conflict-count");
  cc.textContent = String(conflicts.length);
  cc.hidden = conflicts.length === 0;
  byId("link").value = data.link;

  byId("users").replaceChildren(...data.users.map((u) => entityRow(
    u.name,
    u.admin ? "admin" : u.disabled ? "disabled" : "enabled",
    u.admin ? "" : u.disabled ? "bad" : "good",
    u.admin ? [] : [
      option("reset", () => manage({ op: "reset", username: u.name }, u.name), false, "sliders"),
      option(u.disabled ? "enable" : "disable", () => manage({ op: "enable", username: u.name, enabled: u.disabled }, u.name), !u.disabled, u.disabled ? "check" : "alert"),
    ],
  )));

  byId("devices").replaceChildren(...data.devices.map((d) => entityRow(
    d.label,
    d.revoked ? "revoked" : d.online ? "online" : "offline",
    d.revoked ? "bad" : d.online ? "good" : "",
    d.revoked ? [] : [
      option("revoke", () => manage({ op: "revoke", device: d.id }, d.label), true, "close"),
    ],
    d.member,
  )));
  byId("devices-empty").hidden = data.devices.length !== 0;

  renderServices(services);
  byId("services-empty").hidden = services.length !== 0;

  renderAuditPanel(services, conflicts);
  selectTab(preserveTab ? activeTabIndex : activeTabIndex);
}

function feedback(id, text = "") {
  const el = byId(id);
  if (!el) return;
  el.textContent = text;
  el.hidden = !text;
}

async function run(button, error, task, spin = false) {
  if (button.disabled) return;
  button.disabled = true;
  const svg = spin ? button.querySelector(".icon") : null;
  svg?.classList.add("spin");
  feedback(error);
  try { await task(); }
  catch (e) { feedback(error, e instanceof Error ? e.message : t("internal")); }
  finally {
    button.disabled = false;
    svg?.classList.remove("spin");
  }
}

const dialog = byId("action-dialog");
const form = byId("action-form");
const submit = byId("submit-action");
let current;

function manage(command, subject = "") {
  current = command;
  const key = command.op === "enable" && !command.enabled ? "disable" : command.op;
  const danger = key === "disable" || key === "revoke";
  form.reset();
  for (const [name, visible] of [["username", key === "create"], ["password", key === "create" || key === "reset"]]) {
    byId(`action-${name}`).hidden = !visible;
    form.elements[name].disabled = !visible;
  }
  byId("action-title").textContent = t(key);
  feedback("action-subject", subject);
  feedback("action-warning", danger || key === "reset" ? t(`confirm_${key}`) : "");
  feedback("action-error");
  submit.textContent = t(key);
  submit.className = `button ${danger ? "danger" : "primary"}`;
  dialog.showModal();
  const firstInput = form.querySelector("input:not(:disabled)");
  if (firstInput) firstInput.focus();
  else dialog.focus();
}

form.addEventListener("submit", (event) => {
  event.preventDefault();
  if (submit.disabled) return;
  const body = { ...current, ...Object.fromEntries(new FormData(form)) };
  form.elements.password.value = "";
  void run(submit, "action-error", async () => {
    await api("action", body);
    show(await api("session"), true);
    dialog.close();
  });
});

const joinDialog = byId("join-dialog");
joinDialog.addEventListener("click", (e) => { if (e.target === joinDialog) joinDialog.close(); });
dialog.addEventListener("click", (e) => { if (e.target === dialog && !submit.disabled) dialog.close(); });
dialog.addEventListener("cancel", (e) => { if (submit.disabled) e.preventDefault(); });

byId("cancel-action").addEventListener("click", () => { if (!submit.disabled) dialog.close(); });
byId("close-action").addEventListener("click", () => { if (!submit.disabled) dialog.close(); });
byId("new-member").addEventListener("click", () => manage({ op: "create" }));
byId("refresh").addEventListener("click", (e) => void run(e.currentTarget, "workspace-error", async () => show(await api("session"), true), true));
byId("show-join").addEventListener("click", () => { joinDialog.showModal(); joinDialog.focus(); });
byId("close-join").addEventListener("click", () => joinDialog.close());

let copiedTimer;
byId("copy-link").addEventListener("click", async (event) => {
  const button = event.currentTarget;
  const field = byId("link");
  try { await navigator.clipboard.writeText(field.value); }
  catch { field.focus(); field.select(); return; }
  clearTimeout(copiedTimer);
  button.textContent = t("copied");
  copiedTimer = setTimeout(() => { button.textContent = t("copy_link"); }, 1700);
});

byId("login").addEventListener("submit", (event) => {
  event.preventDefault();
  const loginForm = event.currentTarget;
  const button = loginForm.querySelector("[type=submit]");
  if (button.disabled) return;
  const body = Object.fromEntries(new FormData(loginForm));
  loginForm.elements.password.value = "";
  topMark.setState("connecting");
  void run(button, "login-error", async () => {
    try {
      await api("login", body);
      show(await api("session"));
    } catch (e) {
      topMark.setState("idle");
      throw e;
    }
  });
});

api("session").then(show, (e) => {
  show(null);
  if (e.status !== 401) feedback("login-error", e.message);
});
