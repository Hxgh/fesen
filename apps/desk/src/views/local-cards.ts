import { t } from "../i18n";
import { targetOsPill } from "../../../../packages/ui/icons.js";
import { meaningfulHostName, resolveHostOs, singleIp } from "../../../../packages/ui/host-card.js";
import { openPopoverMenu } from "../../../../packages/ui/popover.js";
import { act, toggleCollapsed } from "../app/runtime";
import { confirmConnectAndShare, confirmRemoveHost, openAddShare, shareEditAction } from "../dialogs/share-dialog";
import { effectiveKind, endpoint, kindBadgeText, ranges } from "../model/net";
import { isActiveShare, isCollapsed } from "../model/state";
import type { HostOs, Share, Status } from "../model/types";
import { collapseToggle, el, makeToggle, tool } from "../ui/dom";
import { rowActions } from "./service-actions";

export type LocalEntry =
  | { grouped: true; ip: string; items: Share[] }
  | { grouped: false; item: Share };

function localDeviceName(data: Status): string {
  return data.device_name?.trim() || t("badge_local");
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
        onSelect: () => confirmRemoveHost(ip, cleanName, items),
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
  const hostOsPill = targetOsPill(os);
  const tags: HTMLElement[] = [];
  if (hostOsPill) tags.push(hostOsPill);
  if (kind !== "tcp") tags.push(el("span", "proto-tag", kindBadgeText(kind)));
  const row = el(
    "div",
    "device-sub-row",
    el(
      "div",
      "service-main",
      el("div", "host-title-line", el("p", "endpoint", address), ...tags),
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
  const hostOsPill = targetOsPill(hostOs);
  const titleParts: HTMLElement[] = [
    el("p", "endpoint", ip),
    ...(hostOsPill ? [hostOsPill] : []),
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
  _includeMember: boolean,
  forceExpand = false,
): HTMLElement {
  const key = "dev:local";
  const expanded = forceExpand || !isCollapsed(key);
  const label = localDeviceName(data);
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
    el("span", "device-card-title", label),
    el("span", "local-tag", t("badge_local")),
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
  const hostOsPill = targetOsPill(os);
  const devName = data.device_name?.trim() || "";
  const subParts: (HTMLElement | string)[] = [];
  if (subtitle) subParts.push(el("span", "flat-sub-name", subtitle));
  if (devName) {
    if (subParts.length) subParts.push(el("span", "meta-sep", "·"));
    subParts.push(el("span", "flat-sub-src", devName));
  }
  const row = el(
    "div",
    "flat-row",
    el(
      "div",
      "flat-main",
      el("span", "flat-addr", address),
      el("span", "local-tag", t("badge_local")),
      ...(hostOsPill ? [hostOsPill] : []),
      ...(kind !== "tcp" ? [el("span", "proto-tag", kindBadgeText(kind))] : []),
      ...(subParts.length ? [el("span", "flat-sub", ...subParts)] : []),
    ),
    el("div", "service-actions", publish, rowActions(s, s.name, undefined, true, shareEditAction(s))),
  );
  return el("div", "flat-row-wrap", row);
}
