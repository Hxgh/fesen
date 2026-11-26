import { isZh, t } from "../i18n";
import {
  deviceOsLabel,
  effectiveKind as rawEffectiveKind,
  formatEndpoint,
  formatRanges,
  osLabel,
  resolveHostOs,
} from "../../../../packages/ui/host-card.js";
import { EMPTY_FORWARDS, forwardsOfService, indexOf, serviceKey } from "./state";
import type { Mapping, Offering, ServiceKind, Share, SourceInfo, Span, Status } from "./types";

export const effectiveKind = (s: Share): ServiceKind => rawEffectiveKind(s) as ServiceKind;
export const kindBadgeText = (kind: ServiceKind) => kind.toUpperCase();

export const ranges = <T>(spans: Span<T>[]) => formatRanges(spans, isZh());
export const inputRanges = <T>(spans: Span<T>[]) =>
  spans.map(({ start, end }) => (start === end ? String(start) : `${start}-${end}`)).join(", ");

export function endpoint(ips: Span<string>[], ports: Span<number>[]) {
  return formatEndpoint({ ips, ports }, isZh());
}

export const toInt = (ip: string) => ip.split(".").reduce((n, part) => n * 256 + Number(part), 0);
export const toIp = (n: number) => [n >>> 24, (n >>> 16) & 255, (n >>> 8) & 255, n & 255].join(".");

export function addresses(ips: Span<string>[], maps: Mapping[]) {
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

export const accessIp = (ip: string, maps: Mapping[]) =>
  addresses([{ start: ip, end: ip }], maps).virtual[0]?.start ?? ip;

export const mapsForService = (serviceId: string, data?: Status): Mapping[] => {
  const per = data?.service_virtual_ips?.[serviceId];
  return per && per.length ? per : (data?.virtual_ips ?? []);
};

export const accessIpFor = (serviceId: string, ip: string, data?: Status) =>
  accessIp(ip, mapsForService(serviceId, data));

export function inferDefaultKind(port: number): ServiceKind {
  if (port === 22) return "ssh";
  if (port === 3389) return "rdp";
  if (port === 445) return "smb";
  if (port === 80) return "http";
  if (port === 443) return "https";
  return "tcp";
}

export function isUsableHostIp(ip: string): boolean {
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

export function parsePortEntries(raw: string): { spec: string; port?: number }[] | null {
  const parts = raw.split(/[,，\s]+/).map((s) => s.trim()).filter(Boolean);
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

export function parseSearchTokens(q: string): string[] {
  return q.trim().toLowerCase().split(/\s+/).filter(Boolean);
}

export function matchesTokens(haystack: string, tokens: string[]): boolean {
  return tokens.every((tok) => haystack.includes(tok));
}

export function remoteSearchHaystack(s: Offering, data: Status): string {
  const idx = indexOf(data);
  const key = serviceKey(s);
  const cached = idx.remoteHaystack.get(key);
  if (cached !== undefined) return cached;
  const label = idx.labels.get(s.id);
  const kind = effectiveKind(s);
  const os = resolveHostOs([s]);
  const devOs = deviceOsLabel(s.device_os);
  const ips = addresses(s.rules.ips, mapsForService(s.id, data));
  const endpointOrig = endpoint(s.rules.ips, s.rules.ports);
  const endpointAccess = endpoint([...ips.original, ...ips.virtual], s.rules.ports);
  const fwds = idx.forwards.get(s.id) ?? EMPTY_FORWARDS;
  const text = [
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
  idx.remoteHaystack.set(key, text);
  return text;
}

export function localSearchHaystack(s: Share, data: Status): string {
  const idx = indexOf(data);
  const cached = idx.localHaystack.get(s.id);
  if (cached !== undefined) return cached;
  const kind = effectiveKind(s);
  const os = resolveHostOs([s]);
  const active = idx.active.has(s.id);
  const text = [
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
  idx.localHaystack.set(s.id, text);
  return text;
}

export function sourceText(service: Offering, currentMember: string | null, includeMember = true): SourceInfo {
  const self = !!currentMember && service.member === currentMember;
  const dev = service.device?.trim() || "";
  const memberLabel = self ? t("source_self") : service.member;
  const deviceLabel = dev || memberLabel;
  const text = includeMember
    ? (dev ? `${memberLabel} · ${dev}` : memberLabel)
    : deviceLabel;
  return { text, self, osType: service.device_os, memberKey: service.member, memberLabel, deviceLabel };
}

export function sortLocalShares(shares: Share[]): Share[] {
  return [...shares].sort((a, b) => {
    const ipCmp = toInt(a.rules.ips[0]?.start ?? "0.0.0.0") - toInt(b.rules.ips[0]?.start ?? "0.0.0.0");
    if (ipCmp !== 0) return ipCmp;
    const portCmp = (a.rules.ports[0]?.start ?? 0) - (b.rules.ports[0]?.start ?? 0);
    if (portCmp !== 0) return portCmp;
    return a.name.localeCompare(b.name) || a.id.localeCompare(b.id);
  });
}

export function sortRemoteServices(services: Offering[], data: Status): Offering[] {
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
