import { t } from "../i18n";
import { osBadge } from "../../../../packages/ui/icons.js";
import { groupByHost, meaningfulHostName, resolveHostOs, singleIp } from "../../../../packages/ui/host-card.js";
import { toggleCollapsed } from "../app/runtime";
import { customize } from "../dialogs/access-dialog";
import { accessIpFor, effectiveKind, endpoint, kindBadgeText, ranges, sourceText, toInt } from "../model/net";
import { isCollapsed, labelFor, probes, serviceKey, state } from "../model/state";
import type { DeviceOs, HostOs, Label, Offering, SourceInfo, Status } from "../model/types";
import { collapseToggle, el, tool } from "../ui/dom";
import { portConflictPill, probeVisual, rowActions, serviceRow, testHostServices } from "./service-actions";

export type HostEntry =
  | { grouped: true; ip: string; items: Offering[] }
  | { grouped: false; item: Offering };

export type DeviceBucket = {
  owner: string;
  src: SourceInfo;
  items: Offering[];
  entries: HostEntry[];
};

export type AccountGroup = {
  memberKey: string;
  memberLabel: string;
  self: boolean;
  buckets: DeviceBucket[];
  totalServices: number;
};

function groupSingleRow(service: Offering, data: Status, routing: boolean, showSource = false, includeMember = true) {
  const kind = effectiveKind(service);
  const label = labelFor(service.id, data);
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
    address,
    metaParts,
    tags,
    osBadge(leadOs, "server"),
    badge,
    rowActions(service, rawName, service.owner, routing, () => customize(service, label, routing)),
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

function buildRemotePortRows(
  ip: string,
  items: Offering[],
  data: Status,
  routing: boolean,
  firstAccess: string,
  hostOs: HostOs | null,
  rawHostName: string,
  labels: (Label | undefined)[],
) {
  return items.map((service, idx) => {
    const kind = effectiveKind(service);
    const label = labels[idx];
    const { singleFwd, badge, httpTag, note } = probeVisual(service, data, routing);
    const portText = `:${ranges(service.rules.ports)}`;
    const svcAccess = accessIpFor(service.id, ip, data);
    const rowRawName = label?.remark || service.name;
    const subName = rowRawName !== rawHostName ? meaningfulHostName(rowRawName, ip, hostOs) : "";
    const row = el(
      "div",
      "port-row",
      el(
        "div",
        "port-main",
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
                : []),
      ),
      el(
        "div",
        "port-actions",
        badge,
        rowActions(service, label?.remark || `${ip} ${portText}`, service.owner, routing, () => customize(service, label, routing)),
      ),
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
  forceExpand = false,
) {
  const expanded = forceExpand || !isCollapsed(collapseKey);
  const firstAccess = accessIpFor(items[0]!.id, ip, data);
  const overlapped = items.some((s) => accessIpFor(s.id, ip, data) !== ip);
  const labels = items.map((s) => labelFor(s.id, data));
  const rawHostName = labels.find((l) => l?.remark)?.remark || items[0]!.name;
  const alias = labels.map((l) => l?.alias).find((a): a is string => !!a && !!data.aliases[a]) ?? null;
  const probed = items.map((s) => probes.get(serviceKey(s)));
  const anyPending = probed.some((p) => p && !p.result);
  const hostOs = resolveHostOs(items, probed.map((p) => p?.result?.hostOs)) as HostOs | null;
  const cleanName = meaningfulHostName(rawHostName, ip, hostOs);

  const testAll = tool("refresh", t("test_host"), () => testHostServices(items));
  testAll.disabled = anyPending || !data.online || state.busy;
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
    el("div", "host-controls", testAll),
  );

  const portsWrap = el("div", "host-ports", ...buildRemotePortRows(ip, items, data, routing, firstAccess, hostOs, rawHostName, labels));
  portsWrap.dataset.ports = String(items.length);
  if (items.length >= 9) portsWrap.dataset.many = "1";
  portsWrap.hidden = !expanded;

  return el("article", expanded ? "host-card collapsible" : "host-card collapsible is-collapsed", head, portsWrap);
}

function groupSubSingleRow(service: Offering, data: Status, routing: boolean, _deviceOs?: DeviceOs) {
  const kind = effectiveKind(service);
  const label = labelFor(service.id, data);
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

  const row = el(
    "div",
    "device-sub-row",
    el(
      "div",
      "service-main",
      el("div", "host-title-line", ...(leadOsEl ? [leadOsEl] : []), el("p", "endpoint", address), ...tags),
      ...(metaParts.length ? [el("div", "service-meta", ...metaParts)] : []),
    ),
    el("div", "service-actions", badge, rowActions(service, rawName, service.owner, routing, () => customize(service, label, routing))),
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
  const overlapped = items.some((s) => accessIpFor(s.id, ip, data) !== ip);
  const labels = items.map((s) => labelFor(s.id, data));
  const rawHostName = labels.find((l) => l?.remark)?.remark || items[0]!.name;
  const alias = labels.map((l) => l?.alias).find((a): a is string => !!a && !!data.aliases[a]) ?? null;
  const probed = items.map((s) => probes.get(serviceKey(s)));
  const anyPending = probed.some((p) => p && !p.result);
  const hostOs = resolveHostOs(items, probed.map((p) => p?.result?.hostOs)) as HostOs | null;
  const cleanName = meaningfulHostName(rawHostName, ip, hostOs);

  const testSub = tool("refresh", t("test_host"), () => testHostServices(items));
  testSub.disabled = anyPending || !data.online || state.busy;
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

  const info = el(
    "div",
    "host-info",
    el("div", "host-title-line", ...titleParts),
    ...(metaParts.length ? [el("div", "service-meta", ...metaParts)] : []),
  );
  const head = el(
    "div",
    "host-sub-head",
    collapseToggle(key, expanded, "", toggleCollapsed, info),
    el("div", "host-controls", testSub),
  );
  const portsWrap = el("div", "host-sub-ports", ...buildRemotePortRows(ip, items, data, routing, firstAccess, hostOs, rawHostName, labels));
  portsWrap.dataset.ports = String(items.length);
  if (items.length >= 9) portsWrap.dataset.many = "1";
  portsWrap.hidden = !expanded;
  return el("div", "host-sub", head, portsWrap);
}

export function buildRemoteAccounts(services: Offering[], data: Status): AccountGroup[] {
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

export function renderDeviceBucket(
  bucket: DeviceBucket,
  data: Status,
  routing: boolean,
  includeMember: boolean,
  forceExpand = false,
): HTMLElement {
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
  testDevice.disabled = anyPending || !data.online || state.busy;
  if (anyPending) testDevice.classList.add("spinning");

  const label = includeMember ? bucket.src.text : bucket.src.deviceLabel;
  const devOsEl = osBadge(bucket.src.osType, "device");
  const titleLine = el(
    "div",
    "host-title-line",
    ...(devOsEl ? [devOsEl] : []),
    el("span", bucket.src.self && includeMember ? "device-card-title self" : "device-card-title", label),
    el("span", "count", String(bucket.items.length)),
  );
  const head = el(
    "div",
    "host-head",
    collapseToggle(key, expanded, "", toggleCollapsed, el("div", "host-info", titleLine)),
    el("div", "host-controls", testDevice),
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

export function flatRemoteRow(
  service: Offering,
  data: Status,
  routing: boolean,
  remoteIpOs?: Map<string, HostOs | null>,
): HTMLElement {
  const kind = effectiveKind(service);
  const label = labelFor(service.id, data);
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
  const row = el(
    "div",
    "flat-row",
    el(
      "div",
      "flat-main",
      ...(leadOsEl ? [leadOsEl] : []),
      el("span", "flat-addr", address),
      ...tags,
      ...(subParts.length ? [el("span", "flat-sub", ...subParts)] : []),
    ),
    el("div", "service-actions", badge, rowActions(service, rawName, service.owner, routing, () => customize(service, label, routing))),
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
