import { t } from "../i18n";
import { osBadge } from "../../../../packages/ui/icons.js";
import { meaningfulHostName, resolveHostOs, singleIp } from "../../../../packages/ui/host-card.js";
import { openPopoverMenu } from "../../../../packages/ui/popover.js";
import { act, toggleCollapsed } from "../app/runtime";
import { confirmConnectAndShare, confirmRemoveHost, openAddShare, shareEditAction } from "../dialogs/share-dialog";
import { effectiveKind, endpoint, kindBadgeText, ranges } from "../model/net";
import { isActiveShare, isCollapsed } from "../model/state";
import type { HostOs, Share, Status } from "../model/types";
import { collapseToggle, el, makeToggle, tool } from "../ui/dom";
import { rowActions, serviceRow } from "./service-actions";

export type LocalEntry =
  | { grouped: true; ip: string; items: Share[] }
  | { grouped: false; item: Share };

function mineSingleRow(s: Share, data: Status, showDevice = false, includeMember = true) {
  const kind = effectiveKind(s);
  const active = isActiveShare(s.id, data);
  const ip = singleIp(s);
  const targetLabel = ip ? `${ip}:${ranges(s.rules.ports)}` : s.name;
  const publish = makeToggle(
    active,
    `${t("publish")} · ${s.name}`,
    !data.online && !active ? () => confirmConnectAndShare(targetLabel, { op: "share", id: s.id, enabled: true }) : null,
    () => void act({ op: "share", id: s.id, enabled: !active }),
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
    address,
    metaNodes,
    tags,
    leadOsEl,
    publish,
    rowActions(s, s.name, undefined, true, shareEditAction(s)),
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
    const active = isActiveShare(s.id, data);
    const portText = `:${ranges(s.rules.ports)}`;
    const publish = makeToggle(
      active,
      `${t("publish")} · ${s.name}`,
      !data.online && !active ? () => confirmConnectAndShare(`${ip}${portText}`, { op: "share", id: s.id, enabled: true }) : null,
      () => void act({ op: "share", id: s.id, enabled: !active }),
    );
    const subName = s.name !== rawHostName ? meaningfulHostName(s.name, ip, hostOs) : "";
    const row = el(
      "div",
      "port-row",
      el(
        "div",
        "port-main",
        el("span", "port-num", portText),
        el("span", "proto-tag", kindBadgeText(kind)),
        ...(subName ? [el("span", "port-subname", subName)] : []),
      ),
      el("div", "port-actions", publish, rowActions(s, s.name, undefined, true, shareEditAction(s))),
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
  includeMember = true,
) {
  const expanded = forceExpand || !isCollapsed(collapseKey);
  const rawHostName = items[0]!.name;
  const hostOs = resolveHostOs(items) as HostOs | null;
  const cleanName = meaningfulHostName(rawHostName, ip, hostOs);
  const activeCount = items.filter((s) => isActiveShare(s.id, data)).length;
  const allActive = activeCount === items.length;
  const partial = activeCount > 0 && activeCount < items.length;

  const hostMore = buildLocalHostMore(ip, cleanName, items);
  const master = makeToggle(
    allActive,
    `${t("publish")} · ${cleanName || ip}`,
    !data.online && activeCount === 0
      ? () => confirmConnectAndShare(cleanName ? `${cleanName} (${ip})` : ip, { op: "share_many", ids: items.map((s) => s.id), enabled: true })
      : null,
    () => void act({ op: "share_many", ids: items.map((s) => s.id), enabled: !allActive }),
    partial ? `${activeCount}/${items.length}` : undefined,
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

  const info = el(
    "div",
    "host-info",
    el("div", "host-title-line", ...titleParts),
    ...(metaParts.length ? [el("div", "service-meta", ...metaParts)] : []),
  );
  const head = el(
    "div",
    "host-head",
    collapseToggle(collapseKey, expanded, "", toggleCollapsed, info),
    el("div", "host-controls", master, hostMore),
  );

  const portsWrap = el("div", "host-ports", ...buildLocalHostPorts(ip, items, data, rawHostName, hostOs));
  portsWrap.dataset.ports = String(items.length);
  if (items.length >= 9) portsWrap.dataset.many = "1";
  portsWrap.hidden = !expanded;

  return el("article", expanded ? "host-card collapsible" : "host-card collapsible is-collapsed", head, portsWrap);
}

function mineSubSingleRow(s: Share, data: Status) {
  const kind = effectiveKind(s);
  const active = isActiveShare(s.id, data);
  const ip = singleIp(s);
  const targetLabel = ip ? `${ip}:${ranges(s.rules.ports)}` : s.name;
  const publish = makeToggle(
    active,
    `${t("publish")} · ${s.name}`,
    !data.online && !active ? () => confirmConnectAndShare(targetLabel, { op: "share", id: s.id, enabled: true }) : null,
    () => void act({ op: "share", id: s.id, enabled: !active }),
  );
  const address = endpoint(s.rules.ips, s.rules.ports);
  const os = resolveHostOs([s]) as HostOs | null;
  const subtitle = ip ? meaningfulHostName(s.name, ip, os) : s.name;
  const leadOsEl = osBadge(os, "server");
  const tags: HTMLElement[] = [];
  if (kind !== "tcp") tags.push(el("span", "proto-tag", kindBadgeText(kind)));
  const row = el(
    "div",
    "device-sub-row",
    el(
      "div",
      "service-main",
      el("div", "host-title-line", ...(leadOsEl ? [leadOsEl] : []), el("p", "endpoint", address), ...tags),
      ...(subtitle ? [el("div", "service-meta", el("span", "service-name", subtitle))] : []),
    ),
    el("div", "service-actions", publish, rowActions(s, s.name, undefined, true, shareEditAction(s))),
  );
  return el("div", "device-sub-wrap", row);
}

function mineSubHostBlock(ip: string, items: Share[], data: Status, forceExpand = false) {
  const key = `lhost:${ip}`;
  const expanded = forceExpand || !isCollapsed(key);
  const rawHostName = items[0]!.name;
  const hostOs = resolveHostOs(items) as HostOs | null;
  const cleanName = meaningfulHostName(rawHostName, ip, hostOs);
  const activeCount = items.filter((s) => isActiveShare(s.id, data)).length;
  const allActive = activeCount === items.length;
  const partial = activeCount > 0 && activeCount < items.length;

  const hostMore = buildLocalHostMore(ip, cleanName, items);
  const master = makeToggle(
    allActive,
    `${t("publish")} · ${cleanName || ip}`,
    !data.online && activeCount === 0
      ? () => confirmConnectAndShare(cleanName ? `${cleanName} (${ip})` : ip, { op: "share_many", ids: items.map((s) => s.id), enabled: true })
      : null,
    () => void act({ op: "share_many", ids: items.map((s) => s.id), enabled: !allActive }),
    partial ? `${activeCount}/${items.length}` : undefined,
  );
  const hostOsEl = osBadge(hostOs, "server");
  const titleParts: HTMLElement[] = [
    ...(hostOsEl ? [hostOsEl] : []),
    el("p", "endpoint", ip),
    el("span", "count", String(items.length)),
  ];
  const info = el(
    "div",
    "host-info",
    el("div", "host-title-line", ...titleParts),
    ...(cleanName ? [el("div", "service-meta", el("span", "service-name", cleanName))] : []),
  );
  const head = el(
    "div",
    "host-sub-head",
    collapseToggle(key, expanded, "", toggleCollapsed, info),
    el("div", "host-controls", master, hostMore),
  );
  const portsWrap = el("div", "host-sub-ports", ...buildLocalHostPorts(ip, items, data, rawHostName, hostOs));
  portsWrap.dataset.ports = String(items.length);
  if (items.length >= 9) portsWrap.dataset.many = "1";
  portsWrap.hidden = !expanded;
  return el("div", "host-sub", head, portsWrap);
}

export function renderLocalBucket(
  shares: Share[],
  entries: LocalEntry[],
  data: Status,
  includeMember: boolean,
  forceExpand = false,
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
  const activeCount = shares.filter((s) => isActiveShare(s.id, data)).length;
  const allActive = activeCount === shares.length;
  const partial = activeCount > 0 && activeCount < shares.length;
  const master = makeToggle(
    allActive,
    `${t("publish")} · ${label}`,
    !data.online && activeCount === 0
      ? () => confirmConnectAndShare(label, { op: "share_many", ids: shares.map((s) => s.id), enabled: true })
      : null,
    () => void act({ op: "share_many", ids: shares.map((s) => s.id), enabled: !allActive }),
    partial ? `${activeCount}/${shares.length}` : undefined,
  );
  const addLocalBtn = tool("plus", t("new_share"), () => openAddShare());
  const titleLine = el(
    "div",
    "host-title-line",
    ...(devOsEl ? [devOsEl] : []),
    el("span", includeMember ? "device-card-title self" : "device-card-title", label),
    el("span", "count", String(shares.length)),
  );
  const head = el(
    "div",
    "host-head",
    collapseToggle(key, expanded, "", toggleCollapsed, el("div", "host-info", titleLine)),
    el("div", "host-controls", master, addLocalBtn),
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

export function flatMineRow(s: Share, data: Status, localIpOs?: Map<string, HostOs | null>): HTMLElement {
  const kind = effectiveKind(s);
  const active = isActiveShare(s.id, data);
  const ip = singleIp(s);
  const targetLabel = ip ? `${ip}:${ranges(s.rules.ports)}` : s.name;
  const publish = makeToggle(
    active,
    `${t("publish")} · ${s.name}`,
    !data.online && !active ? () => confirmConnectAndShare(targetLabel, { op: "share", id: s.id, enabled: true }) : null,
    () => void act({ op: "share", id: s.id, enabled: !active }),
  );
  const address = endpoint(s.rules.ips, s.rules.ports);
  const os = ((ip ? localIpOs?.get(ip) : null) ?? resolveHostOs([s])) as HostOs | null;
  const subtitle = ip ? meaningfulHostName(s.name, ip, os) : s.name;
  const osEl = osBadge(os, { fallback: "server", local: true, prefix: t("badge_local") });
  const row = el(
    "div",
    "flat-row",
    el(
      "div",
      "flat-main",
      ...(osEl ? [osEl] : []),
      el("span", "flat-addr", address),
      ...(kind !== "tcp" ? [el("span", "proto-tag", kindBadgeText(kind))] : []),
      ...(subtitle ? [el("span", "flat-sub", el("span", "flat-sub-text", subtitle))] : []),
    ),
    el("div", "service-actions", publish, rowActions(s, s.name, undefined, true, shareEditAction(s))),
  );
  return el("div", "flat-row-wrap", row);
}
