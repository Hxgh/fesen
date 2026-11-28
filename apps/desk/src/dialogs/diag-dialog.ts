import { createIconButton } from "../../../../packages/ui/icons.js";
import { failure, isZh, t } from "../i18n";
import { request } from "../app/runtime";
import type { FlowKind, LogEntry, TrafficSnapshot } from "../model/types";
import { button, el, notify } from "../ui/dom";

type DiagTab = "traffic" | "logs";
type LogFilter = "all" | "issues";

const SVG_NS = "http://www.w3.org/2000/svg";
const diagDialog = document.querySelector<HTMLDialogElement>("#diag-dialog")!;

let activeTab: DiagTab = "logs";
let logFilter: LogFilter = "all";
let logsCache: LogEntry[] = [];
let verboseLogs = false;
let trafficSnap: TrafficSnapshot | null = null;
let trafficTimer: number | undefined;
let trafficInFlight = false;
let initialized = false;

function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0 B";
  if (n < 1024) return `${Math.round(n)} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(2)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function formatRate(bps: number): string {
  return `${formatBytes(bps)}/s`;
}

function formatDuration(ms: number): string {
  const sec = Math.max(0, Math.floor(ms / 1000));
  if (sec < 60) return `${sec}s`;
  const min = Math.floor(sec / 60);
  if (min < 60) return `${min}m ${sec % 60}s`;
  const hr = Math.floor(min / 60);
  return `${hr}h ${min % 60}m`;
}

function formatTime(unixSec: number): string {
  const d = new Date(unixSec * 1000);
  if (Number.isNaN(d.getTime())) return "";
  const pad = (v: number) => String(v).padStart(2, "0");
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

function dirLabel(kind: FlowKind): string {
  switch (kind) {
    case "outbound_tun": return t("diag_dir_out_tun");
    case "outbound_forward": return t("diag_dir_out_fwd");
    case "inbound_local": return t("diag_dir_in_local");
    case "inbound_lan": return t("diag_dir_in_lan");
  }
}

function scopeLabel(scope: string): string {
  if (!isZh()) return scope.toUpperCase();
  switch (scope) {
    case "flow": return "传输";
    case "core": return "核心";
    case "ctl": return "控制";
    case "net": return "网络";
    case "route": return "路由";
    case "tun": return "隧道";
    case "share": return "共享";
    default: return scope;
  }
}

function levelLabel(level: LogEntry["level"]): string {
  if (!isZh()) return level;
  switch (level) {
    case "ERR": return "错误";
    case "WARN": return "警告";
    case "INFO": return "信息";
    default: return level;
  }
}

function flowDirTagLabel(tag: string): string {
  switch (tag) {
    case "out_tun": return t("diag_dir_out_tun");
    case "out_fwd": return t("diag_dir_out_fwd");
    case "in_local": return t("diag_dir_in_local");
    case "in_lan": return t("diag_dir_in_lan");
    default: return tag;
  }
}

function flowResultLabel(res: string): string {
  const zh = isZh();
  switch (res) {
    case "ok": return zh ? "正常结束" : "closed";
    case "stopped": return zh ? "已断开" : "stopped";
    case "Offline": return zh ? "对端离线" : "peer offline";
    case "DirectFailed": return zh ? "无法直连" : "direct failed";
    case "TargetTimeout": return zh ? "目标超时" : "target timeout";
    case "TargetUnavailable": return zh ? "目标不可达" : "target unreachable";
    case "Unauthorized": return zh ? "未授权" : "unauthorized";
    default: return res;
  }
}

function formatLogMessage(entry: LogEntry): { title: string; meta?: string } {
  const msg = entry.message.trim();
  const zh = isZh();

  if (entry.scope === "flow") {
    const m = msg.match(/^dir=(\S+)\s+peer=(.+?)\s+target=(\S+)\s+tx=(\d+)\s+rx=(\d+)\s+ms=(\d+)\s+res=(\S+)$/);
    if (m) {
      const [, dir = "", peer = "", target = "", txStr = "0", rxStr = "0", msStr = "0", res = ""] = m;
      const peerText = peer === "-" ? "" : `${peer} · `;
      const tx = formatBytes(Number(txStr));
      const rx = formatBytes(Number(rxStr));
      const dur = formatDuration(Number(msStr));
      return {
        title: `${flowDirTagLabel(dir)} · ${peerText}${target}`,
        meta: `↑ ${tx}  ↓ ${rx} · ${dur} · ${flowResultLabel(res)}`,
      };
    }
  }

  if (entry.scope === "ctl") {
    const m = msg.match(/^connected\s+member=(\S+)\s+admin=(true|false)$/);
    if (m) {
      const [, member = "", isAdmin = "false"] = m;
      const role = isAdmin === "true" ? (zh ? "（管理员）" : " (admin)") : "";
      return {
        title: zh ? `控制连接已建立 · ${member}${role}` : `Control connected · ${member}${role}`,
      };
    }
  }

  if (entry.scope === "tun") {
    const m = msg.match(/^applied\s+addr=Some\(([^)]+)\)\s+routes=(\d+)\s+dns=(.+)$/);
    if (m) {
      const [, addr = "", routes = "0", dns = "None"] = m;
      const dnsNote = dns !== "None" ? ` · DNS ${dns}` : "";
      return {
        title: zh
          ? `虚拟网卡已就绪 · ${addr}（${routes} 条路由${dnsNote}）`
          : `TUN applied · ${addr} (${routes} routes${dnsNote})`,
      };
    }
    const fail = msg.match(/^apply failed attempt=(\d+):\s*(.+)$/);
    if (fail) {
      return {
        title: zh ? `虚拟网卡应用失败（第 ${fail[1] ?? "1"} 次重试）` : `TUN apply failed (attempt ${fail[1] ?? "1"})`,
        meta: fail[2],
      };
    }
  }

  if (entry.scope === "net") {
    const m = msg.match(/^changed\s+sharing=(true|false)\s+addrs=\[([^\]]*)\]->\[([^\]]*)\]/);
    if (m) {
      const [, sharing = "false", prev = "", next = ""] = m;
      const pauseNote = sharing === "true" ? (zh ? " · 共享已暂停" : " · sharing paused") : "";
      return {
        title: zh ? `物理网络变化${pauseNote}` : `Network changed${pauseNote}`,
        meta: `${prev || "-"} → ${next || "-"}`,
      };
    }
  }

  if (entry.scope === "route") {
    const m = msg.match(/^build failed err=(\S+)\s+routes=(\d+)\s+addrs=\[([^\]]*)\]$/);
    if (m) {
      const [, err = "", routes = "0", addrs = ""] = m;
      return {
        title: zh ? "路由分配冲突" : `Route conflict (${err})`,
        meta: zh ? `系统路由 ${routes} 条 · 本机地址 ${addrs || "-"}` : `${routes} routes · ${addrs || "-"}`,
      };
    }
  }

  return { title: msg };
}

function stopTrafficPolling(notifyCore: boolean) {
  if (trafficTimer !== undefined) {
    window.clearInterval(trafficTimer);
    trafficTimer = undefined;
  }
  if (notifyCore && trafficSnap?.enabled) {
    trafficSnap = null;
    void request({ op: "traffic", enabled: false }, false).catch(() => {});
  }
}

async function pollTrafficOnce() {
  if (!diagDialog.open || activeTab !== "traffic" || trafficInFlight) return;
  trafficInFlight = true;
  try {
    const status = await request({ op: "traffic", enabled: true }, false);
    if (diagDialog.open && activeTab === "traffic" && status.traffic) {
      trafficSnap = status.traffic;
      renderDiag();
    }
  } catch (err) {
    if (diagDialog.open && activeTab === "traffic") {
      notify(failure(typeof err === "string" ? err : "not_connected"), true);
    }
  } finally {
    trafficInFlight = false;
  }
}

function startTrafficPolling() {
  stopTrafficPolling(false);
  void pollTrafficOnce();
  trafficTimer = window.setInterval(() => {
    void pollTrafficOnce();
  }, 1000);
}

async function loadLogs(verbose?: boolean) {
  try {
    const status = await request(
      verbose === undefined ? { op: "logs", limit: 200 } : { op: "logs", limit: 200, verbose },
      false,
    );
    logsCache = status.logs ?? [];
    verboseLogs = !!status.verbose_logs;
    if (diagDialog.open && activeTab === "logs") renderDiag();
  } catch (err) {
    notify(failure(typeof err === "string" ? err : "not_connected"), true);
  }
}

async function clearLogs() {
  try {
    const status = await request({ op: "clear_logs" }, false);
    logsCache = status.logs ?? [];
    if (diagDialog.open && activeTab === "logs") renderDiag();
  } catch (err) {
    notify(failure(typeof err === "string" ? err : "not_connected"), true);
  }
}

async function copyTextSafe(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {}
  try {
    const area = document.createElement("textarea");
    area.value = text;
    area.style.position = "fixed";
    area.style.opacity = "0";
    document.body.append(area);
    area.focus();
    area.select();
    const ok = document.execCommand("copy");
    area.remove();
    return ok;
  } catch {
    return false;
  }
}

function renderTrafficChart(snap: TrafficSnapshot): HTMLElement {
  const wrap = el("div", "diag-chart-box");
  const w = 640;
  const h = 112;
  const samples = snap.history.length >= 2
    ? snap.history
    : [...snap.history, ...(snap.history.length === 1 ? snap.history : [{ ts_ms: 0, tx_bps: 0, rx_bps: 0 }, { ts_ms: 1, tx_bps: 0, rx_bps: 0 }])];
  const peak = Math.max(1024, ...samples.map((s) => Math.max(s.tx_bps, s.rx_bps)));

  const svg = document.createElementNS(SVG_NS, "svg");
  svg.setAttribute("viewBox", `0 0 ${w} ${h}`);
  svg.setAttribute("preserveAspectRatio", "none");
  svg.setAttribute("class", "diag-chart-svg");
  svg.setAttribute("aria-hidden", "true");

  for (const frac of [0.25, 0.5, 0.75]) {
    const line = document.createElementNS(SVG_NS, "line");
    const y = Math.round((h - 12) * frac) + 6;
    line.setAttribute("x1", "0");
    line.setAttribute("x2", String(w));
    line.setAttribute("y1", String(y));
    line.setAttribute("y2", String(y));
    line.setAttribute("class", "diag-grid-line");
    svg.append(line);
  }

  const coords = (pick: (s: { tx_bps: number; rx_bps: number }) => number) => {
    const count = samples.length;
    return samples.map((s, idx) => {
      const x = count === 1 ? w : (idx / (count - 1)) * w;
      const y = h - 6 - Math.min(1, pick(s) / peak) * (h - 14);
      return `${x.toFixed(1)},${y.toFixed(1)}`;
    });
  };

  const rxPts = coords((s) => s.rx_bps);
  const txPts = coords((s) => s.tx_bps);

  const rxArea = document.createElementNS(SVG_NS, "polygon");
  rxArea.setAttribute("points", `0,${h - 6} ${rxPts.join(" ")} ${w},${h - 6}`);
  rxArea.setAttribute("class", "diag-area-rx");

  const txArea = document.createElementNS(SVG_NS, "polygon");
  txArea.setAttribute("points", `0,${h - 6} ${txPts.join(" ")} ${w},${h - 6}`);
  txArea.setAttribute("class", "diag-area-tx");

  const rxPoly = document.createElementNS(SVG_NS, "polyline");
  rxPoly.setAttribute("points", rxPts.join(" "));
  rxPoly.setAttribute("class", "diag-line-rx");

  const txPoly = document.createElementNS(SVG_NS, "polyline");
  txPoly.setAttribute("points", txPts.join(" "));
  txPoly.setAttribute("class", "diag-line-tx");

  svg.append(rxArea, txArea, rxPoly, txPoly);

  const legend = el(
    "div",
    "diag-chart-legend",
    el(
      "div",
      "diag-legend-keys",
      el("span", "diag-legend-item is-tx", el("span", "diag-dot is-tx"), `↑ ${formatRate(snap.total_tx_bps)}`),
      el("span", "diag-legend-item is-rx", el("span", "diag-dot is-rx"), `↓ ${formatRate(snap.total_rx_bps)}`),
    ),
    el("span", "diag-chart-peak mono", `${isZh() ? "峰值" : "Peak"} ${formatRate(peak)}`),
  );

  wrap.append(legend, svg);
  return wrap;
}

function statPair(up: string, down: string): HTMLElement {
  return el(
    "div",
    "diag-stat-pair mono",
    el("span", "diag-stat-up", `↑ ${up}`),
    el("span", "diag-stat-down", `↓ ${down}`),
  );
}

function renderTrafficTab(body: HTMLElement) {
  body.className = "dialog-body diag-body";

  const snap: TrafficSnapshot = trafficSnap ?? {
    enabled: true,
    ts_ms: Date.now(),
    active_flows: 0,
    total_tx_bytes: 0,
    total_rx_bytes: 0,
    total_tx_bps: 0,
    total_rx_bps: 0,
    control_tx_bytes: 0,
    control_rx_bytes: 0,
    history: [],
    flows: [],
  };

  const anomalyCount = snap.flows.filter((f) => f.anomaly).length;
  const flowsLabel = anomalyCount > 0 && isZh()
    ? `${snap.active_flows} 条 · ${anomalyCount} 高吞吐`
    : `${snap.active_flows}`;

  const stats = el(
    "div",
    "diag-stats-grid",
    el(
      "div",
      "diag-stat-card",
      el("span", "diag-stat-label", t("diag_rate_up_down")),
      statPair(formatRate(snap.total_tx_bps), formatRate(snap.total_rx_bps)),
    ),
    el(
      "div",
      "diag-stat-card",
      el("span", "diag-stat-label", t("diag_total_bytes")),
      statPair(formatBytes(snap.total_tx_bytes), formatBytes(snap.total_rx_bytes)),
    ),
    el(
      "div",
      "diag-stat-card",
      el("span", "diag-stat-label", t("diag_control_bytes")),
      statPair(formatBytes(snap.control_tx_bytes), formatBytes(snap.control_rx_bytes)),
    ),
    el(
      "div",
      anomalyCount > 0 ? "diag-stat-card is-warn" : "diag-stat-card",
      el("span", "diag-stat-label", t("diag_active_flows_count")),
      el("div", "diag-stat-val mono", flowsLabel),
    ),
  );
  body.append(stats, renderTrafficChart(snap));

  if (!snap.flows.length) {
    body.append(el("div", "diag-empty muted", t("diag_no_flows")));
    return;
  }

  const list = el("div", "diag-flow-list");
  for (const f of snap.flows) {
    const dirPill = el("span", `diag-dir-pill is-${f.kind}`, dirLabel(f.kind));
    const targetText = f.local_port ? `127.0.0.1:${f.local_port} → ${f.target}` : f.target;
    const svcName = f.service_name && f.service_name !== f.target && !f.target.startsWith(`${f.service_name}:`)
      ? f.service_name
      : "";
    const titleParts = [f.source, f.peer, svcName].filter(Boolean).join(" · ");

    const topLeft = el("div", "diag-flow-top-left", dirPill);
    if (f.anomaly) {
      topLeft.append(el("span", "diag-anomaly-pill", t("diag_anomaly")));
    } else if (f.tx_bps + f.rx_bps > 0) {
      topLeft.append(el("span", "diag-state-pill is-live", t("diag_flow_active")));
    } else {
      topLeft.append(el("span", "diag-state-pill", t("diag_flow_idle")));
    }
    topLeft.append(el("span", "diag-flow-title", titleParts || targetText));

    const dropBtn = button(t("diag_drop_flow"), () => {
      void request({ op: "drop_flow", id: f.id }, false)
        .then((st) => {
          if (st.traffic) trafficSnap = st.traffic;
          renderDiag();
        })
        .catch(() => {});
    }, "button ghost diag-drop-btn");

    const topRow = el("div", "diag-flow-top", topLeft, dropBtn);
    const botRow = el(
      "div",
      "diag-flow-bot mono",
      el("span", "diag-flow-sub", `${targetText} · ${formatDuration(f.duration_ms)} · #${f.id}`),
      el("span", "diag-flow-metrics", `↑ ${formatRate(f.tx_bps)}  ↓ ${formatRate(f.rx_bps)} · ${formatBytes(f.tx_bytes)} / ${formatBytes(f.rx_bytes)}`),
    );

    list.append(el("div", f.anomaly ? "diag-flow-row is-anomaly" : "diag-flow-row", topRow, botRow));
  }
  body.append(list);
}

function renderLogsTab(body: HTMLElement) {
  body.className = "dialog-body diag-body is-logs";

  const filterSeg = el("div", "segmented diag-filter-seg");
  for (const [id, key] of [["all", "diag_logs_all"], ["issues", "diag_logs_issues"]] as const) {
    const btn = button(t(key), () => {
      if (logFilter === id) return;
      logFilter = id;
      renderDiag();
    }, id === logFilter ? "active" : "");
    btn.setAttribute("aria-pressed", String(id === logFilter));
    filterSeg.append(btn);
  }

  const filtered = logFilter === "issues"
    ? logsCache.filter((e) => e.level === "ERR" || e.level === "WARN")
    : logsCache;
  const rows = [...filtered].reverse();

  const refreshBtn = createIconButton({
    icon: "refresh",
    title: t("refresh"),
    variant: "solid",
    size: "sm",
    onClick: () => void loadLogs(),
  });
  const copyBtn = createIconButton({
    icon: "copy",
    title: t("diag_copy_logs"),
    variant: "solid",
    size: "sm",
    onClick: () => {
      const text = rows
        .map((e) => `${formatTime(e.ts)} [${e.level}] [${e.scope}] ${e.message}`)
        .join("\n");
      void copyTextSafe(text).then((ok) => notify(t(ok ? "diag_copied" : "clipboard_failed"), !ok));
    },
  });
  copyBtn.disabled = !rows.length;
  const clearBtn = createIconButton({
    icon: "trash",
    title: t("diag_clear_logs"),
    variant: "solid",
    size: "sm",
    onClick: () => void clearLogs(),
  });
  clearBtn.classList.add("diag-clear-btn");
  clearBtn.disabled = !logsCache.length;

  const verboseBtn = button(
    t("diag_verbose_logs"),
    () => void loadLogs(!verboseLogs),
    verboseLogs ? "button diag-verbose-btn is-on" : "button diag-verbose-btn",
  );
  verboseBtn.title = t("diag_verbose_tip");
  verboseBtn.setAttribute("aria-pressed", String(verboseLogs));

  const actions = el("div", "diag-log-actions", verboseBtn, refreshBtn, copyBtn, clearBtn);
  const leftBar = el("div", "diag-log-filters", filterSeg, el("span", "diag-log-count mono", String(rows.length)));
  body.append(el("div", "diag-log-bar", leftBar, actions));

  if (!rows.length) {
    body.append(el("div", "diag-empty muted", t("diag_logs_empty")));
    return;
  }

  const box = el("div", "diag-log-list");
  for (const entry of rows) {
    const lvlCls = entry.level === "ERR"
      ? "diag-lvl-pill is-err"
      : entry.level === "WARN"
        ? "diag-lvl-pill is-warn"
        : "diag-lvl-pill";
    const formatted = formatLogMessage(entry);
    const head = el(
      "div",
      "diag-log-head",
      el(
        "div",
        "diag-log-badges",
        el("span", lvlCls, levelLabel(entry.level)),
        el("span", "diag-scope-pill", scopeLabel(entry.scope)),
      ),
      el("span", "diag-log-time mono", formatTime(entry.ts)),
    );
    const msg = el("div", "diag-log-msg", formatted.title);
    const card = el("div", entry.level === "ERR" ? "diag-log-row is-err" : "diag-log-row", head, msg);
    if (formatted.meta) {
      card.append(el("div", "diag-log-meta mono", formatted.meta));
    }
    box.append(card);
  }
  body.append(box);
}

function renderDiag() {
  if (!diagDialog.open) return;

  const prevBodyScroll = diagDialog.querySelector<HTMLElement>(".diag-body")?.scrollTop ?? 0;
  const prevLogScroll = diagDialog.querySelector<HTMLElement>(".diag-log-list")?.scrollTop ?? 0;

  const title = el("h2", "", t("diag_title"));
  title.id = "diag-title";

  const tabs = el("div", "segmented diag-head-tabs");
  for (const [id, key] of [["traffic", "diag_live_traffic"], ["logs", "diag_view_logs"]] as const) {
    const btn = button(t(key), () => {
      if (activeTab === id) return;
      activeTab = id;
      if (activeTab === "traffic") {
        startTrafficPolling();
      } else {
        stopTrafficPolling(true);
        void loadLogs();
      }
      renderDiag();
    }, id === activeTab ? "active" : "");
    btn.setAttribute("aria-pressed", String(id === activeTab));
    tabs.append(btn);
  }

  const closeBtn = createIconButton({
    icon: "close",
    title: t("close"),
    variant: "ghost",
    size: "md",
    onClick: () => diagDialog.close(),
  });
  const header = el("div", "dialog-header diag-header", el("div", "diag-head-left", title, tabs), closeBtn);
  const body = el("div", "dialog-body diag-body");

  if (activeTab === "traffic") {
    renderTrafficTab(body);
  } else {
    renderLogsTab(body);
  }

  diagDialog.replaceChildren(header, body);
  if (prevBodyScroll) body.scrollTop = prevBodyScroll;
  const list = body.querySelector<HTMLElement>(".diag-log-list");
  if (list && prevLogScroll) list.scrollTop = prevLogScroll;
}

export function openDiagnostics(tab: DiagTab = "logs") {
  if (!initialized) {
    initialized = true;
    diagDialog.addEventListener("click", (event) => {
      if (event.target === diagDialog) diagDialog.close();
    });
    diagDialog.addEventListener("close", () => {
      stopTrafficPolling(true);
    });
  }
  activeTab = tab;
  diagDialog.tabIndex = -1;
  if (!diagDialog.open) {
    diagDialog.showModal();
  }
  renderDiag();
  if (activeTab === "traffic") {
    startTrafficPolling();
  } else {
    stopTrafficPolling(true);
    void loadLogs();
  }
  diagDialog.focus();
}
