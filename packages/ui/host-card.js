/**
 * Shared single-IP multi-port aggregation & OS/protocol formatting helpers.
 */
export const singleSpan = (spans) =>
  Array.isArray(spans) && spans.length === 1 && spans[0].start === spans[0].end;

export const singleIp = (s) => (singleSpan(s?.rules?.ips) ? s.rules.ips[0].start : null);
export const singlePort = (s) => (singleSpan(s?.rules?.ports) ? s.rules.ports[0].start : null);
export const effectiveKind = (s) => s?.kind ?? (s?.http ? "http" : "tcp");
export const osLabel = (os) => (os === "windows" ? "Windows" : os === "linux" ? "Linux" : "");
export const deviceOsLabel = (os) =>
  os === "macos" ? "macOS"
    : os === "windows" ? "Windows"
    : os === "android" ? "Android"
    : os === "linux" ? "Linux"
    : "";

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
