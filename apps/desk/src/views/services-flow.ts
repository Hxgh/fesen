import { t } from "../i18n";
import { icon, osBadge } from "../../../../packages/ui/icons.js";
import { groupByHost, resolveHostOs, singleIp } from "../../../../packages/ui/host-card.js";
import { act, render, setViewMode, toggleCollapsed } from "../app/runtime";
import { addShare, openAddShare } from "../dialogs/share-dialog";
import { localSearchHaystack, matchesTokens, parseSearchTokens, remoteSearchHaystack, sortLocalShares, sortRemoteServices } from "../model/net";
import { isCollapsed, persistViewMode, probes, serviceKey, setCollapsedKeys, state } from "../model/state";
import type { DeviceProfile, HostOs, IconName, Offering, Share, Status } from "../model/types";
import { button, collapseToggle, el, empty, notify } from "../ui/dom";
import { flatMineRow, renderLocalBucket, type LocalEntry } from "./local-cards";
import { buildRemoteAccounts, flatRemoteRow, renderDeviceBucket } from "./remote-cards";

export const searchInput = document.createElement("input");
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

export const servicesPanel = document.createElement("section");
servicesPanel.className = "panel";
servicesPanel.append(listToolbar, serviceFlowRoot);

function syncSearchBar(_data?: Status, matchCount?: number, totalCount?: number) {
  const hasQuery = !!state.searchQuery.trim();
  searchInput.placeholder = t("search_services");
  searchInput.setAttribute("aria-label", t("search_services"));
  searchClearBtn.title = t("clear_search");
  searchClearBtn.setAttribute("aria-label", t("clear_search"));
  searchClearBtn.hidden = !state.searchQuery;
  searchInlineBox.classList.toggle("has-query", hasQuery);
  if (searchInput.value !== state.searchQuery) searchInput.value = state.searchQuery;
  if (hasQuery && matchCount !== undefined && totalCount !== undefined) {
    searchCountEl.textContent = `${matchCount}/${totalCount}`;
    searchCountEl.hidden = false;
  } else {
    searchCountEl.textContent = "";
    searchCountEl.hidden = true;
  }
}

export function refreshServicesResults() {
  if (!state.status?.member || state.tab !== "services") return;
  updateServicesFlow(state.status);
}

export function openSearch() {
  if (!state.status?.member || state.tab !== "services") return;
  if (!state.status.services.length && !state.status.shares.length) return;
  searchInput.focus();
  searchInput.select();
}

export function closeSearch() {
  state.searchQuery = "";
  searchInput.value = "";
  searchInput.blur();
  if (state.status && state.tab === "services") {
    syncSearchBar(state.status);
    updateServicesFlow(state.status);
  }
}

export function clearSearchInput() {
  state.searchQuery = "";
  searchInput.value = "";
}

searchInput.addEventListener("input", () => {
  state.searchQuery = searchInput.value;
  refreshServicesResults();
});
searchClearBtn.addEventListener("click", () => {
  if (state.searchQuery) {
    state.searchQuery = "";
    searchInput.value = "";
    refreshServicesResults();
    searchInput.focus();
  } else {
    closeSearch();
  }
});

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
    remoteIpOs.set(k, resolveHostOs(items, items.map((x) => probes.get(serviceKey(x))?.result?.hostOs)) as HostOs | null);
  }

  const rows: HTMLElement[] = [
    ...sortLocalShares(localShares).map((s) => flatMineRow(s, data, localIpOs)),
    ...sortRemoteServices(remoteServices, data).map((s) => flatRemoteRow(s, data, routing, remoteIpOs)),
  ];
  const card = el("div", "list-card flat-list", ...rows);
  card.dataset.rows = String(Math.min(rows.length, 5));
  return card;
}

function restorePromptCard(profile: DeviceProfile) {
  const bodyText = t("restore_banner_body")
    .replace("{shares}", String(profile.shares.length))
    .replace("{forwards}", String(profile.forwards.length));
  const osEl = osBadge(profile.os, "device");
  return el(
    "div",
    "restore-card",
    el(
      "div",
      "restore-info",
      el(
        "div",
        "host-title-line",
        ...(osEl ? [osEl] : []),
        el("span", "restore-title", t("restore_banner_title")),
        el("span", "mono muted", `#${profile.machine_id.slice(0, 6)}`),
      ),
      el("p", "muted", bodyText),
    ),
    el(
      "div",
      "restore-actions",
      button(t("dismiss"), () => void act({ op: "dismiss_restore" }), "button"),
      button(t("restore_now"), () => void act({
        op: "import_profile",
        machine_id: profile.machine_id,
        import_shares: true,
        import_forwards: true,
      }, () => notify(t("profile_imported"))), "button primary"),
    ),
  );
}

export function updateServicesFlow(data: Status) {
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

  const tokens = parseSearchTokens(state.searchQuery);
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

  const allCollapsedActive = state.viewMode === "grouped" && !searching && itemKeys.length > 0
    && itemKeys.every((k) => isCollapsed(k));
  const activeDensity: "normal" | "collapsed" | "compact" =
    state.viewMode === "flat" ? "compact" : allCollapsedActive ? "collapsed" : "normal";

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
    persistViewMode("grouped");
    if (itemKeys.length > 0 && itemKeys.every((k) => isCollapsed(k))) {
      setCollapsedKeys(itemKeys, false);
    }
    render(true);
  });

  const flatBtn = viewIconBtn("list", t("view_flat"), "compact", () => setViewMode("flat"));

  const collapsedBtn = viewIconBtn("collapse", t("view_collapsed"), "collapsed", () => {
    if (state.searchQuery) {
      state.searchQuery = "";
      searchInput.value = "";
    }
    persistViewMode("grouped");
    setCollapsedKeys(itemKeys, true);
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
      state.searchQuery = "";
      searchInput.value = "";
      refreshServicesResults();
      searchInput.focus();
    }, "button")));
    return;
  }

  if (state.viewMode === "flat") {
    const flatEl = flatUnifiedList(localShares, remoteServices, data, routing);
    mainFlowSection.replaceChildren(
      flatEl,
      ...(!routing && hasRemote ? [el("p", "routing-note", t("routing_unavailable"))] : []),
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
    const head = el(
      "div",
      "account-head",
      collapseToggle(
        acctKey,
        acctExpanded,
        "account-toggle",
        toggleCollapsed,
        el("span", "account-name self", t("source_self")),
        el("span", "account-sub", t("devices_count").replace("{n}", String(myDeviceCount))),
        el("span", "count", String(totalSelfServices)),
      ),
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
    const head = el(
      "div",
      "account-head",
      collapseToggle(
        acctKey,
        acctExpanded,
        "account-toggle",
        toggleCollapsed,
        el("span", "account-name", acct.memberLabel),
        el("span", "account-sub", t("devices_count").replace("{n}", String(acct.buckets.length))),
        el("span", "count", String(acct.totalServices)),
      ),
    );
    const body = el("div", "account-body", ...acct.buckets.map((bucket) => renderDeviceBucket(bucket, data, routing, false, searching)));
    body.hidden = !acctExpanded;
    listNodes.push(el("div", "account-group", head, body));
  }

  mainFlowSection.replaceChildren(
    el("div", "service-list", ...listNodes),
    ...(!routing && hasRemote ? [el("p", "routing-note", t("routing_unavailable"))] : []),
  );
}
