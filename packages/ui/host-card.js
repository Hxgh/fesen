/**
 * Shared single-IP multi-port aggregation, OS/protocol helpers & IP/port conflict math.
 */
export const singleSpan = (spans) =>
  Array.isArray(spans) && spans.length === 1 && spans[0].start === spans[0].end;

export const singleIp = (s) => (singleSpan(s?.rules?.ips) ? s.rules.ips[0].start : null);
export const singlePort = (s) => (singleSpan(s?.rules?.ports) ? s.rules.ports[0].start : null);
export const effectiveKind = (s) => s?.kind ?? (s?.http ? "http" : "tcp");
export const osLabel = (os) => (os === "windows" ? "Windows" : os === "linux" ? "Linux" : "");

export function formatRanges(spans, zh = false) {
  return (spans || [])
    .map(({ start, end }) => (start === end ? String(start) : `${start}–${end}`))
    .join(zh ? "、" : ", ");
}

export function formatEndpoint(rules, zh = false) {
  if (!rules) return "-";
  const ips = formatRanges(rules.ips, zh);
  const ports = formatRanges(rules.ports, zh);
  return `${ips || "*"}${singleSpan(rules.ips) && singleSpan(rules.ports) ? ":" : " · "}${ports || "*"}`;
}

export function groupByHost(items, keyOf) {
  const entries = [];
  const buckets = new Map();
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
      entry.items.sort((a, b) => (a.rules?.ports?.[0]?.start ?? 0) - (b.rules?.ports?.[0]?.start ?? 0));
    }
  }
  return entries;
}

export function resolveHostOs(items, probedOs = []) {
  const hasOsCapable = items.some((s) => {
    const k = effectiveKind(s);
    return k === "ssh" || k === "rdp";
  });
  if (!hasOsCapable) return null;
  for (const s of items) if (s.host_os === "windows") return "windows";
  for (const os of probedOs) if (os === "windows") return "windows";
  for (const s of items) if (effectiveKind(s) === "rdp") return "windows";
  for (const s of items) if (s.host_os === "linux") return "linux";
  for (const os of probedOs) if (os === "linux") return "linux";
  return null;
}

export function meaningfulHostName(name, ip, os) {
  const trimmed = String(name || "").trim();
  if (!trimmed || trimmed === ip) return "";
  const tag = osLabel(os);
  if (tag && trimmed === `${tag} · ${ip}`) return "";
  return trimmed;
}

export function parseIpv4(s) {
  const parts = String(s || "").trim().split(".");
  if (parts.length !== 4) return null;
  let num = 0;
  for (const p of parts) {
    if (!/^\d+$/.test(p)) return null;
    const n = Number(p);
    if (n < 0 || n > 255) return null;
    num = num * 256 + n;
  }
  return num >>> 0;
}

export function parseIpRange(s) {
  s = String(s || "").trim();
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
    const start = parseIpv4(a);
    const end = parseIpv4(b);
    if (start === null || end === null || start > end) return null;
    return [{ start, end }];
  }
  const ip = parseIpv4(s);
  return ip === null ? null : [{ start: ip, end: ip }];
}

export function parsePortRange(s) {
  s = String(s || "").trim();
  if (s.includes("-")) {
    const [a, b] = s.split("-");
    const start = Number(a);
    const end = Number(b);
    if (isNaN(start) || isNaN(end) || start < 1 || end > 65535 || start > end) return null;
    return [{ start, end }];
  }
  const p = Number(s);
  return isNaN(p) || p < 1 || p > 65535 ? null : [{ start: p, end: p }];
}

export function parseMultiple(str, fn) {
  const out = [];
  for (const part of String(str || "").split(/[,，]/)) {
    if (!part.trim()) continue;
    const r = fn(part);
    if (!r) return null;
    out.push(...r);
  }
  return out.length ? out : null;
}

const rangesIntersect = (a, b) => a.start <= b.end && b.start <= a.end;

function normalizeRules(rules) {
  const ips = (rules?.ips || []).map((r) => ({
    start: typeof r.start === "number" ? r.start : parseIpv4(r.start),
    end: typeof r.end === "number" ? r.end : parseIpv4(r.end),
  })).filter((r) => r.start !== null && r.end !== null);
  const ports = (rules?.ports || []).map((r) => ({
    start: Number(r.start),
    end: Number(r.end),
  })).filter((r) => !isNaN(r.start) && !isNaN(r.end));
  return { ips, ports };
}

export function checkConflict(ruleA, ruleB) {
  const a = normalizeRules(ruleA);
  const b = normalizeRules(ruleB);
  return a.ips.some((r1) => b.ips.some((r2) => rangesIntersect(r1, r2)))
    && a.ports.some((r1) => b.ports.some((r2) => rangesIntersect(r1, r2)));
}

export function detectAllConflicts(services = []) {
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
