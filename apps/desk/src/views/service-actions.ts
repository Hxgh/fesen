import { invoke } from "@tauri-apps/api/core";
import { failure, t } from "../i18n";
import { meaningfulHostName, singleIp, singlePort, singleSpan } from "../../../../packages/ui/host-card.js";
import { openPopoverMenu } from "../../../../packages/ui/popover.js";
import { edit, editor, render, request, showError, switchForwardPort } from "../app/runtime";
import { customize } from "../dialogs/access-dialog";
import { confirmRemoveShare, openAddShare } from "../dialogs/share-dialog";
import { showSiteDialog } from "../dialogs/site-dialog";
import { accessIpFor, addresses, effectiveKind, kindBadgeText, mapsForService, toInt, toIp } from "../model/net";
import { forwardFor, forwardsOfService, labelFor, probes, serviceKey, state } from "../model/state";
import type { LocalForwardStatus, MenuEntry, Offering, Probe, Share, Status } from "../model/types";
import { button, el, field, notify, str, submit } from "../ui/dom";
import { tool } from "../ui/dom";

export async function testConnection(service: Offering, ip: string, port: number) {
  const key = serviceKey(service);
  const previous = probes.get(key);
  if (state.busy || !state.status?.online || (previous && previous.result === undefined)) return;
  const kind = effectiveKind(service);
  const current: Probe = { ip, port, access: accessIpFor(service.id, ip, state.status), kind };
  probes.set(key, current);
  render(true);
  try {
    const reply = await request({ op: "test_connection", id: service.id, ip, port }, false);
    const outcome = reply.probe;
    const httpStatus = outcome?.http_status ?? reply.http_status;
    current.result = kind === "http" && httpStatus === undefined
      ? { error: "http_invalid" }
      : {
        httpStatus,
        rttMs: outcome?.rtt_ms,
        hostOs: outcome?.host_os,
        detail: outcome?.detail,
      };
  } catch (error) {
    current.result = { error: typeof error === "string" ? error : "internal" };
  }
  if (probes.get(key) === current) render(true);
}

export function testHostServices(items: Offering[]) {
  if (state.busy || !state.status?.online) return;
  const tasks: { service: Offering; ip: string; port: number }[] = [];
  for (const service of items) {
    const ip = singleIp(service);
    const port = singlePort(service);
    if (ip && port) tasks.push({ service, ip, port });
  }
  let cursor = 0;
  const workers = Math.min(4, tasks.length);
  for (let i = 0; i < workers; i++) {
    void (async () => {
      while (cursor < tasks.length) {
        const next = tasks[cursor++]!;
        await testConnection(next.service, next.ip, next.port);
      }
    })();
  }
}

export function hostProbeBadge(items: Offering[], data: Status, routing: boolean): HTMLElement | null {
  const testable = items.filter((s) => singleIp(s) !== null && singlePort(s) !== null);
  if (!testable.length) return null;

  let pendingCount = 0;
  let okCount = 0;
  let badCount = 0;
  let firstOkRtt: number | undefined;
  let firstErr: string | undefined;
  for (const s of testable) {
    const res = probes.get(serviceKey(s))?.result;
    if (!res) {
      if (probes.has(serviceKey(s))) pendingCount += 1;
      continue;
    }
    const httpBad = res.httpStatus !== undefined && (res.httpStatus < 200 || res.httpStatus >= 400);
    if (res.error || httpBad) {
      badCount += 1;
      firstErr ??= res.error ? failure(res.error) : `HTTP ${res.httpStatus}`;
    } else {
      okCount += 1;
      firstOkRtt ??= res.rttMs;
    }
  }

  const doneCount = okCount + badCount;
  const pending = pendingCount > 0;
  let caption: string;
  let tone = "";

  if (pending) {
    caption = t("testing");
  } else if (doneCount === 0) {
    caption = t("unchecked");
  } else if (testable.length === 1) {
    if (badCount === 1) {
      caption = firstErr ?? t("unavailable");
      tone = "bad";
    } else {
      caption = firstOkRtt !== undefined ? `${firstOkRtt}ms` : t("reachable");
      const s = testable[0]!;
      const ip = singleIp(s)!;
      const port = singlePort(s)!;
      const fwd = forwardFor(s.id, ip, port, data);
      tone = (routing || !!fwd?.active) ? "good" : "";
    }
  } else if (badCount === testable.length) {
    caption = t("unavailable");
    tone = "bad";
  } else if (badCount > 0) {
    caption = `${okCount}/${testable.length}`;
    tone = "warn";
  } else if (okCount === testable.length) {
    caption = `${okCount}/${testable.length}`;
    tone = "good";
  } else {
    caption = `${okCount}/${testable.length}`;
  }

  const badge = button(caption, () => testHostServices(items), `probe-badge badge ${tone}`);
  badge.title = `${t("test_host")} · ${caption}`;
  badge.setAttribute("aria-label", `${t("test_host")}：${caption}`);
  badge.setAttribute("data-pending", String(pending));
  badge.disabled = pending || !data.online || state.busy;
  return badge;
}

export function chooseTarget(
  service: Share,
  title: string,
  action: (ip: string, port: number) => void,
  previous?: Probe,
  submitLabel = t("test"),
) {
  const { ips, ports } = service.rules;
  if (singleSpan(ips) && singleSpan(ports)) {
    action(ips[0]!.start, ports[0]!.start);
    return;
  }
  edit(
    title,
    submit(submitLabel),
    (data) => {
      const ip = str(data, "ip").trim();
      const port = Number(str(data, "port"));
      if (
        !/^\d{1,3}(\.\d{1,3}){3}$/.test(ip)
        || toIp(toInt(ip)) !== ip
        || !ips.some((r) => toInt(r.start) <= toInt(ip) && toInt(ip) <= toInt(r.end))
        || !Number.isInteger(port)
        || !ports.some((r) => r.start <= port && port <= r.end)
      ) {
        state.message = "invalid_target";
        showError();
        return;
      }
      state.message = "";
      editor.close();
      action(ip, port);
    },
    field(t("original_ip"), "ip", previous?.ip ?? ips[0]!.start, { required: true, maxLength: 15 }),
    field(t("port"), "port", String(previous?.port ?? ports[0]!.start), { required: true, type: "number", min: "1", max: "65535" }),
  );
}

export function runServiceAction(
  service: Share,
  owner: string | undefined,
  kind: "copy" | "command" | "browser",
  label: string,
) {
  chooseTarget(service, label, (ip, port) => {
    if (owner && state.status?.system_routing === false) {
      const fwd = forwardFor(service.id, ip, port, state.status);
      if (!fwd) {
        const offering = service as Offering;
        const lbl = labelFor(service.id, state.status);
        customize(offering, lbl, false, true, () => runServiceAction(service, owner, kind, label));
        return;
      }
      if (!fwd.active) {
        notify(t("local_port_conflict"), true);
        return;
      }
    }
    void invoke<string>("service_action", { id: service.id, owner: owner ?? null, ip, port, action: kind })
      .then((value) => { if (kind !== "browser") notify(`${t("address_copied")} · ${value}`); })
      .catch((error) => notify(failure(typeof error === "string" ? error
        : kind === "browser" ? "external_browser_failed" : "clipboard_failed"), true));
  }, owner ? probes.get(`${owner}:${service.id}`) : undefined, label);
}

export function openWebPreview(service: Share, owner?: string) {
  const title = t(owner ? "web_test" : "local_web_test");
  state.message = "";
  showError();
  chooseTarget(service, title, (ip, port) => {
    const launchPreview = (fwd = owner ? forwardFor(service.id, ip, port, state.status) : undefined) => {
      if (fwd && !fwd.active) {
        notify(t("local_port_conflict"), true);
        return;
      }
      const access = owner ? (fwd ? "127.0.0.1" : accessIpFor(service.id, ip, state.status)) : ip;
      const accessPort = fwd ? fwd.local_port : port;
      const session = state.tabOwner;
      const valid = () => {
        const s = owner
          ? state.status?.services.find((svc) => svc.owner === owner && svc.id === service.id)
          : state.status?.shares.find((svc) => svc.id === service.id);
        if (!state.status?.member || session !== JSON.stringify([state.status.server, state.status.member]) || !s || effectiveKind(s) !== "http") {
          return false;
        }
        if (owner) {
          if (!state.status.online) return false;
          const curFwd = forwardFor(service.id, ip, port, state.status);
          if (fwd) {
            if (!curFwd?.active || curFwd.local_port !== accessPort) return false;
          } else if (accessIpFor(service.id, ip, state.status) !== access) {
            return false;
          }
        }
        return s.rules.ips.some((r) => toInt(r.start) <= toInt(ip) && toInt(ip) <= toInt(r.end))
          && s.rules.ports.some((r) => r.start <= port && port <= r.end);
      };
      showSiteDialog({ id: service.id, owner: owner ?? null, ip, port }, access, valid, (result) => {
        if (!owner) return;
        const key = `${owner}:${service.id}`;
        const prev = probes.get(key)?.result;
        probes.set(key, {
          ip,
          port,
          access,
          kind: "http",
          result: result === "visible"
            ? { visible: true, rttMs: prev?.rttMs, httpStatus: prev?.httpStatus }
            : result,
        });
        render(true);
      }, accessPort);
    };

    const existing = owner ? forwardFor(service.id, ip, port, state.status) : undefined;
    if (owner && state.status?.system_routing === false && !existing) {
      if (state.busy) return;
      state.busy = true;
      void (async () => {
        try {
          const suggested = await request({ op: "suggest_local_port", preferred: port }, false);
          if (!suggested.suggested_port) throw "conflict";
          state.status = await request({
            op: "set_local_forward",
            service: service.id,
            target_ip: ip,
            target_port: port,
            local_port: suggested.suggested_port,
          }, false);
          render(true);
          notify(`127.0.0.1:${suggested.suggested_port}`);
          state.busy = false;
          launchPreview(forwardFor(service.id, ip, port, state.status));
        } catch {
          state.busy = false;
          const lbl = labelFor(service.id, state.status);
          customize(service as Offering, lbl, false, true, () => openWebPreview(service, owner));
        }
      })();
      return;
    }
    launchPreview(existing);
  });
}

export function rowActions(
  service: Share,
  name: string,
  owner: string | undefined,
  routing: boolean,
  configure: () => void,
) {
  const more = tool("more", `${t("more_actions")} · ${name}`, () => {
    const kind = effectiveKind(service);
    const primary: MenuEntry[] = [];
    const conflictFwd = owner ? forwardsOfService(service.id, state.status).find((f) => f.error === "conflict") : undefined;
    if (conflictFwd?.suggested_port) {
      primary.push({
        icon: "alert",
        label: t("switch_to_port").replace("{port}", String(conflictFwd.suggested_port)),
        onSelect: () => void switchForwardPort(
          conflictFwd.service,
          conflictFwd.target_ip,
          conflictFwd.target_port,
          conflictFwd.suggested_port!,
        ),
      });
    }
    if (kind === "http") {
      primary.push({
        icon: "globe",
        label: t(owner ? "web_test" : "local_web_test"),
        onSelect: () => openWebPreview(service, owner),
      });
    }
    if (kind === "http" || kind === "https") {
      primary.push({
        icon: "external",
        label: t("open_browser"),
        onSelect: () => runServiceAction(service, owner, "browser", t("open_browser")),
      });
    }
    if (owner && (kind === "ssh" || kind === "rdp" || kind === "smb")) {
      primary.push({
        icon: "terminal",
        label: t("copy_command"),
        onSelect: () => runServiceAction(service, owner, "command", t("copy_command")),
      });
    }
    if (!owner || routing || forwardsOfService(service.id, state.status).some((f) => f.active)) {
      primary.push({
        icon: "copy",
        label: t("copy_address"),
        onSelect: () => runServiceAction(service, owner, "copy", t("copy_address")),
      });
    }

    const secondary: MenuEntry[] = [
      {
        icon: owner ? "sliders" : "edit",
        label: t(owner ? "access_settings" : "edit_share"),
        onSelect: configure,
      },
    ];
    if (!owner) {
      const hostIp = singleIp(service);
      if (hostIp) {
        secondary.push({
          icon: "plus",
          label: t("manage_host_ports"),
          onSelect: () => openAddShare({ ip: hostIp, name: meaningfulHostName(service.name, hostIp, service.host_os ?? null) }),
        });
      }
      secondary.push({
        icon: "close",
        label: t("remove"),
        danger: true,
        onSelect: () => confirmRemoveShare(service),
      });
    }
    openPopoverMenu(more, [...primary, { sep: true }, ...secondary]);
  });
  more.setAttribute("aria-haspopup", "menu");
  more.setAttribute("aria-expanded", "false");
  return more;
}

export function probeVisual(service: Offering, data: Status, routing: boolean) {
  const kind = effectiveKind(service);
  const ips = addresses(service.rules.ips, mapsForService(service.id, data));
  const overlapped = ips.virtual.length > 0;
  const one = singleSpan(service.rules.ips) && singleSpan(service.rules.ports);
  const fwds = forwardsOfService(service.id, data);
  const singleFwd = one
    ? fwds.find((f) => f.target_ip === service.rules.ips[0]!.start && f.target_port === service.rules.ports[0]!.start)
    : undefined;
  const probe = probes.get(serviceKey(service));
  const result = probe?.result;
  const http = result?.httpStatus;
  const pending = !!probe && !result;
  const httpBad = http !== undefined && (http < 200 || http >= 400);
  const errCode = result?.error;

  let caption: string;
  let tone = "";
  let detailTip = t("reprobe");

  if (pending) {
    caption = t("testing");
  } else if (!result) {
    caption = t("unchecked");
  } else if (errCode) {
    caption = failure(errCode);
    tone = errCode === "conflict" ? "warn" : "bad";
    detailTip = `${caption} · ${t("reprobe")}`;
  } else if (httpBad) {
    caption = result.rttMs !== undefined ? `${result.rttMs}ms` : `HTTP ${http}`;
    tone = "bad";
    detailTip = `HTTP ${http}${result.rttMs !== undefined ? ` · ${result.rttMs}ms` : ""} · ${t("reprobe")}`;
  } else {
    caption = result.rttMs !== undefined
      ? `${result.rttMs}ms`
      : result.visible ? t("page_visible") : t("reachable");
    tone = (routing || !!singleFwd?.active) ? "good" : "";
    detailTip = [http !== undefined ? `HTTP ${http}` : "", result.rttMs !== undefined ? `${result.rttMs}ms` : "", t("reprobe")]
      .filter(Boolean).join(" · ");
  }

  const badge = button(
    caption,
    () => chooseTarget(service, t("test_connection"), (ip, port) => void testConnection(service, ip, port), probe),
    `probe-badge badge ${tone}`,
  );
  badge.title = detailTip;
  badge.setAttribute("aria-label", `${t("refresh_status")}：${caption}`);
  badge.setAttribute("data-pending", String(pending));
  badge.disabled = pending || !data.online;

  let httpTag: HTMLElement | null = null;
  if (http !== undefined && !pending && !errCode) {
    httpTag = el("span", httpBad ? "http-code bad" : "http-code", String(http));
    httpTag.title = `HTTP ${http}`;
  }

  let note: HTMLElement | null = null;
  if (probe && result && !one) {
    const outcome = errCode ? failure(errCode)
      : result.visible ? t("page_visible")
      : http !== undefined ? `HTTP ${http}`
      : kind === "tcp" ? t("reachable") : kindBadgeText(kind);
    note = el("p", errCode || httpBad ? "probe-note problem" : "probe-note", `${probe.access}:${probe.port} · ${outcome}`);
    note.setAttribute("role", "status");
  }
  return { ips, overlapped, fwds, singleFwd, badge, httpTag, note, pending, result };
}

export function portConflictPill(fwd: LocalForwardStatus): HTMLElement {
  const label = t("local_port_occupied").replace("{port}", String(fwd.local_port));
  if (fwd.suggested_port) {
    return button(`${label} · ${t("switch_to_port").replace("{port}", String(fwd.suggested_port))}`, () => {
      void switchForwardPort(fwd.service, fwd.target_ip, fwd.target_port, fwd.suggested_port!);
    }, "badge warn");
  }
  return el("span", "badge warn", label);
}
