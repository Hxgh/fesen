import { failure, t } from "../i18n";
import { createIconButton, osBadge } from "../../../../packages/ui/icons.js";
import { singleIp, singlePort } from "../../../../packages/ui/host-card.js";
import { act, applyJoinLink, edit, editor, render, request, showDialog, showError, syncBrand } from "../app/runtime";
import { effectiveKind, endpoint, inferDefaultKind, inputRanges, isUsableHostIp, parsePortEntries, ranges } from "../model/net";
import { isActiveShare, state } from "../model/state";
import type { HostOs, HostProbe, Request, ServiceKind, Share } from "../model/types";
import { button, dialogHeader, el, field, protocolSelect, str, submit } from "../ui/dom";

export function confirmRemoveShare(share: Share) {
  state.message = "";
  edit(
    t("confirm_remove"),
    submit(t("remove"), "danger"),
    () => {
      void act({ op: "remove", id: share.id }, () => editor.close());
    },
    el("p", "muted", share.name),
    el("p", "endpoint", endpoint(share.rules.ips, share.rules.ports)),
  );
}

export function confirmRemoveHost(ip: string, hostLabel: string, items: Share[]) {
  state.message = "";
  edit(
    t("confirm_remove_host"),
    submit(t("remove_host"), "danger"),
    () => {
      void act({ op: "remove_many", ids: items.map((s) => s.id) }, () => editor.close());
    },
    el("p", "muted", t("confirm_remove_host_body").replace("{host}", hostLabel).replace("{n}", String(items.length))),
    el("p", "endpoint", `${ip} · ${items.map((s) => `:${ranges(s.rules.ports)}`).join(", ")}`),
  );
}

export function editShare(share?: Share) {
  const currentKind = share ? effectiveKind(share) : "tcp";
  const wasActive = !!share && isActiveShare(share.id, state.status);
  let republishChecked = wasActive && !!state.status?.online;
  const rows: HTMLElement[] = [
    field(t("service_name"), "name", share?.name ?? "", { required: true, maxLength: 64 }),
    field(t("ipv4"), "ips", share ? inputRanges(share.rules.ips) : "", {
      required: true,
      maxLength: 900,
      placeholder: "198.51.100.10, 203.0.113.0/29",
    }),
    field(t("port"), "ports", share ? inputRanges(share.rules.ports) : "", {
      required: true,
      maxLength: 500,
      placeholder: "80, 8000-8008",
    }),
    el("label", "", t("protocol_type"), protocolSelect(currentKind)),
  ];
  if (wasActive && state.status?.online) {
    const repubCheck = el("input");
    Object.assign(repubCheck, { type: "checkbox", checked: republishChecked });
    repubCheck.addEventListener("change", () => { republishChecked = repubCheck.checked; });
    rows.push(el("label", "check-row", repubCheck, t("republish_on_save")));
  }
  const controls = edit(t(share ? "edit_share" : "new_share"), submit(t("save")), (data) => {
    const kind = str(data, "kind") as ServiceKind;
    void act({
      op: "save",
      id: share?.id ?? null,
      name: str(data, "name"),
      ips: str(data, "ips"),
      ports: str(data, "ports"),
      http: kind === "http",
      kind,
      host_os: share?.host_os ?? null,
      republish: wasActive ? republishChecked : false,
    }, () => editor.close());
  }, ...rows);
  if (share) {
    controls.prepend(button(t("remove"), () => confirmRemoveShare(share), "button ghost text-danger leading"));
  }
}

export function shareEditAction(s: Share) {
  return () => editShare(s);
}

export function confirmConnectAndShare(targetLabel: string, shareReq: Request) {
  if (state.busy) return;
  state.message = "";
  const cancelBtn = button(t("cancel"), () => { if (!state.busy) editor.close(); }, "button");
  const confirmBtn = button(t("connect_and_share"), () => void run(), "button primary");

  async function run() {
    if (state.busy) return;
    state.busy = true;
    state.message = "";
    showError();
    syncBrand();
    cancelBtn.disabled = true;
    confirmBtn.disabled = true;
    confirmBtn.textContent = t("connecting_now");
    try {
      let cur = await request({ op: "start" }, true);
      state.status = cur;
      if (!cur.online) {
        for (let i = 0; i < 25; i++) {
          await new Promise((r) => window.setTimeout(r, 120));
          cur = await request({ op: "status", ui: true }, false);
          state.status = cur;
          if (cur.online) break;
          if (!cur.running) break;
        }
      }
      if (!cur.online) {
        throw cur.error ?? "not_connected";
      }
      state.status = await request(shareReq, true);
      editor.close();
      render(true);
    } catch (error) {
      state.message = typeof error === "string" ? error : "internal";
      showError();
    } finally {
      state.busy = false;
      cancelBtn.disabled = false;
      confirmBtn.disabled = false;
      confirmBtn.textContent = t("connect_and_share");
      syncBrand();
      applyJoinLink();
    }
  }

  showDialog(
    t("connect_prompt_title"),
    [el("p", "muted", t("connect_prompt_body").replace("{target}", targetLabel))],
    [cancelBtn, confirmBtn],
    true,
  );
}

type DraftPort = {
  spec: string;
  port?: number;
  kind: ServiceKind;
  checked: boolean;
  saved: boolean;
  active?: boolean;
  osHint?: HostOs;
  probing?: boolean;
  open?: boolean;
  rttMs?: number;
  httpStatus?: number;
  detail?: string;
};

export function openAddShare(prefill?: { ip?: string; name?: string }) {
  state.message = "";
  let ipVal = prefill?.ip ?? "";
  let portDraft = "";
  let nameVal = prefill?.name ?? "";
  let probingAll = false;
  let probingDeep = false;
  let probeSeq = 0;
  let localError = "";
  let emptyDetect = false;
  let items: DraftPort[] = [];
  let publishNow = !!state.status?.online;
  let dialogOpen = true;
  let keepPortFocus = false;

  function cancelHostProbe(redraw = true) {
    probeSeq += 1;
    const wasProbing = probingAll;
    probingAll = false;
    probingDeep = false;
    if (wasProbing) {
      void request({ op: "cancel_probe_host" }, false).catch(() => {});
      if (redraw && dialogOpen && editor.open) draw();
    }
  }

  editor.addEventListener("close", () => {
    dialogOpen = false;
    editor.classList.remove("dialog-wide");
    cancelHostProbe(false);
  }, { once: true });

  const existingForIp = (ip: string) =>
    (state.status?.shares ?? []).filter((s) => singleIp(s) === ip && singlePort(s) !== null);

  const initialExisting = ipVal ? existingForIp(ipVal) : [];
  const manageMode = initialExisting.length > 0;
  const oldHostName = manageMode ? (initialExisting[0]!.name || ipVal) : "";

  if (manageMode) {
    items = initialExisting
      .map((s) => {
        const port = singlePort(s)!;
        const kind = effectiveKind(s);
        const active = isActiveShare(s.id, state.status);
        const osHint: HostOs | undefined = kind === "rdp" ? "windows" : (kind === "ssh" ? s.host_os : undefined);
        return { spec: String(port), port, kind, checked: true, saved: true, active, osHint };
      })
      .sort((a, b) => (a.port ?? 0) - (b.port ?? 0));
  }

  function effectiveHostOs(): HostOs | null {
    if (!isUsableHostIp(ipVal.trim())) return null;
    const active = items.filter((i) => i.checked && i.port !== undefined);
    if (active.some((i) => i.kind === "rdp" || (i.kind === "ssh" && i.osHint === "windows"))) return "windows";
    if (active.some((i) => i.kind === "ssh" && i.osHint === "linux")) return "linux";
    return null;
  }

  function sortItems() {
    items.sort((a, b) => {
      const pa = a.port ?? Number(a.spec.split("-")[0] ?? 0);
      const pb = b.port ?? Number(b.spec.split("-")[0] ?? 0);
      return pa - pb || a.spec.localeCompare(b.spec);
    });
  }

  function mergeProbeResults(result: HostProbe, onlySpecific: boolean) {
    const existingSet = new Set(existingForIp(result.ip).map((s) => singlePort(s)!));
    const bySpec = new Map(items.map((i) => [i.spec, i]));
    let anyOpen = false;
    for (const p of result.ports) {
      if (p.open) anyOpen = true;
      const spec = String(p.port);
      const prev = bySpec.get(spec);
      const hint: HostOs | undefined = p.os_hint ?? (p.kind === "rdp" ? "windows" : undefined);
      if (onlySpecific) {
        if (!prev) continue;
        prev.probing = false;
        prev.open = p.open;
        prev.rttMs = p.rtt_ms;
        prev.httpStatus = p.http_status;
        prev.detail = p.detail;
        if (p.open) { prev.kind = p.kind; prev.osHint = hint; }
      } else if (p.open) {
        if (prev) {
          prev.probing = false;
          prev.open = true;
          prev.kind = p.kind;
          prev.osHint = hint;
          prev.rttMs = p.rtt_ms;
          prev.httpStatus = p.http_status;
          prev.detail = p.detail;
        } else {
          bySpec.set(spec, {
            spec,
            port: p.port,
            kind: p.kind,
            checked: p.recommended && !existingSet.has(p.port),
            saved: existingSet.has(p.port),
            osHint: hint,
            open: true,
            rttMs: p.rtt_ms,
            httpStatus: p.http_status,
            detail: p.detail,
          });
        }
      }
    }
    if (!onlySpecific) {
      emptyDetect = !anyOpen;
      if (![...bySpec.values()].some((i) => i.checked)) {
        for (const p of result.ports) {
          if (p.open && p.recommended) {
            const entry = bySpec.get(String(p.port));
            if (entry) entry.checked = true;
          }
        }
      }
    }
    items = [...bySpec.values()];
    sortItems();
  }

  async function detectCommonServices(deep = false) {
    if (probingAll || state.busy) return;
    const ip = ipVal.trim();
    if (!isUsableHostIp(ip)) { localError = t("invalid_host_ip"); draw(); return; }
    const seq = ++probeSeq;
    localError = "";
    state.message = "";
    emptyDetect = false;
    probingAll = true;
    probingDeep = deep;
    draw();
    try {
      const extra = items
        .map((i) => i.port)
        .filter((p): p is number => p !== undefined && ![22, 3389, 445, 80, 443].includes(p))
        .slice(0, 5);
      const reply = await request({ op: "probe_host", ip, extra_ports: extra, deep }, true);
      if (seq !== probeSeq) return;
      if (!reply.host_probe) throw "internal";
      if (reply.host_probe.cancelled) return;
      mergeProbeResults(reply.host_probe, false);
    } catch (error) {
      if (seq !== probeSeq) return;
      localError = failure(typeof error === "string" ? error : "internal");
    } finally {
      if (seq === probeSeq) {
        probingAll = false;
        probingDeep = false;
        if (dialogOpen && editor.open) draw();
      }
    }
  }

  function commitPortInput(refocus = true) {
    const raw = portDraft.trim();
    if (!raw) return true;
    const parsed = parsePortEntries(raw);
    if (!parsed || !parsed.length) { localError = t("invalid_port_input"); draw(); return false; }
    const ip = ipVal.trim();
    const singleHost = isUsableHostIp(ip);
    const existingSet = new Set(singleHost ? existingForIp(ip).map((s) => singlePort(s)!) : []);
    const addedSingle: number[] = [];
    for (const token of parsed) {
      const prev = items.find((i) => i.spec === token.spec);
      if (prev) {
        prev.checked = true;
      } else {
        if (items.length >= 24) { localError = t("invalid_port_input"); draw(); return false; }
        const kind = token.port !== undefined ? inferDefaultKind(token.port) : "tcp";
        items.push({
          spec: token.spec,
          port: token.port,
          kind,
          checked: true,
          saved: token.port !== undefined && existingSet.has(token.port),
          osHint: kind === "rdp" ? "windows" : undefined,
          probing: singleHost && token.port !== undefined,
        });
        if (singleHost && token.port !== undefined) addedSingle.push(token.port);
      }
    }
    sortItems();
    portDraft = "";
    localError = "";
    emptyDetect = false;
    keepPortFocus = refocus;
    draw();

    if (addedSingle.length && singleHost) {
      void request({ op: "probe_host", ip, ports: addedSingle }, true)
        .then((reply) => { if (reply.host_probe) mergeProbeResults(reply.host_probe, true); })
        .catch(() => {
          for (const port of addedSingle) {
            const entry = items.find((i) => i.port === port);
            if (entry) { entry.probing = false; entry.open = false; }
          }
        })
        .finally(() => { if (dialogOpen && editor.open) draw(); });
    }
    return true;
  }

  function itemMetaText(item: DraftPort): string {
    if (item.probing) return t("port_checking");
    if (item.saved && !item.checked) return t("will_remove");
    const parts: string[] = [];
    if (item.saved) parts.push(t(item.active ? "published" : "already_added"));
    if (item.open === false) parts.push(t("port_offline"));
    else if (item.detail && item.kind !== "ssh") parts.push(item.detail);
    return parts.join(" · ");
  }

  function draw() {
    const activeName = document.activeElement?.getAttribute("name") ?? "";
    const header = dialogHeader(t(manageMode ? "manage_host" : "new_share"), () => { if (!state.busy) editor.close(); });
    const body = el("div", "dialog-body");
    const alertBox = el("p", "feedback error");
    alertBox.setAttribute("role", "alert");

    const osTagHost = el("span");
    const deepBtn = probingAll
      ? button(t("cancel_scan"), () => cancelHostProbe(true), "deep-scan-btn cancel")
      : button(t("deep_scan"), () => void detectCommonServices(true), "deep-scan-btn");
    function syncHeaderMeta() {
      const osEl = osBadge(effectiveHostOs(), "server");
      osTagHost.replaceChildren(...(osEl ? [osEl] : []));
      const validIp = isUsableHostIp(ipVal.trim());
      detectBtn.disabled = state.busy || (!probingAll && !validIp);
      deepBtn.disabled = state.busy || (!probingAll && !validIp);
    }

    const ipInput = el("input");
    Object.assign(ipInput, {
      name: "host_ip",
      value: ipVal,
      maxLength: 900,
      placeholder: "198.51.100.35 / 203.0.113.0/29",
      spellcheck: false,
      disabled: manageMode || probingAll,
    });
    ipInput.addEventListener("input", () => {
      ipVal = ipInput.value;
      if (localError) { localError = ""; alertBox.hidden = true; alertBox.textContent = ""; }
      syncHeaderMeta();
    });
    ipInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        if (isUsableHostIp(ipVal.trim())) void detectCommonServices(false);
        else portInput.focus();
      }
    });

    const detectBtn = createIconButton({
      icon: probingAll ? "close" : "refresh",
      title: t(probingAll ? "cancel_scan" : "detect_host"),
      variant: "solid",
      size: "lg",
      onClick: () => {
        if (probingAll) cancelHostProbe(true);
        else void detectCommonServices(false);
      },
    });

    editor.classList.toggle("dialog-wide", manageMode || items.length > 0 || !!portDraft.trim());
    const ipHead = el("div", "host-ip-head", el("span", "field-label", t("host_ip")), osTagHost);
    const ipGroup = el("div", "field-group", ipHead, el("div", "host-ip-row", ipInput, detectBtn));

    const showName = manageMode || items.length > 0 || !!portDraft.trim();
    if (showName) {
      const nameInput = el("input");
      Object.assign(nameInput, {
        name: "host_name",
        value: nameVal,
        maxLength: 64,
        placeholder: ipVal.trim() || "198.51.100.35",
        spellcheck: false,
      });
      nameInput.addEventListener("input", () => { nameVal = nameInput.value; });
      const nameGroup = el("div", "field-group", el("span", "field-label", t("host_name")), nameInput);
      body.append(el("div", "dialog-pair", ipGroup, nameGroup));
    } else {
      body.append(ipGroup);
    }

    const portsBox = el("div", "ports-box");
    if (items.length >= 6) portsBox.dataset.multi = "1";
    if (emptyDetect && !items.length) portsBox.append(el("div", "ports-empty-note", t("host_none_open")));
    for (const item of items) {
      const cb = el("input");
      Object.assign(cb, { type: "checkbox", checked: item.checked });
      const rowCls = `host-service-item${item.checked ? " checked" : item.saved ? " removing" : ""}`;
      const row = el("div", rowCls);
      const detailEl = el("span", "host-service-detail", itemMetaText(item));
      cb.addEventListener("change", () => {
        item.checked = cb.checked;
        row.classList.toggle("checked", cb.checked);
        row.classList.toggle("removing", item.saved && !cb.checked);
        detailEl.textContent = itemMetaText(item);
        syncHeaderMeta();
        updateSubmit();
      });
      const kindSel = protocolSelect(item.kind, "kind-inline", (k) => {
        item.kind = k;
        if (k === "rdp") item.osHint = "windows";
        else if (k !== "ssh") item.osHint = undefined;
        syncHeaderMeta();
      });
      const removeBtn = createIconButton({
        icon: item.saved && !item.checked ? "refresh" : "close",
        title: t(item.saved && !item.checked ? "save" : "remove"),
        label: `${t("remove")} :${item.spec}`,
        variant: "ghost",
        size: "sm",
        onClick: () => {
          if (item.saved) {
            item.checked = !item.checked;
            draw();
          } else {
            items = items.filter((i) => i.spec !== item.spec);
            draw();
          }
        },
      });

      const httpCode = item.httpStatus !== undefined
        ? el("span", item.httpStatus >= 400 ? "http-code bad" : "http-code", String(item.httpStatus))
        : null;
      if (httpCode) httpCode.title = `HTTP ${item.httpStatus}`;
      row.append(
        cb,
        el("span", "host-service-port", `:${item.spec}`),
        kindSel,
        ...(httpCode ? [httpCode] : []),
        detailEl,
        ...(item.rttMs !== undefined ? [el("span", "host-service-rtt", `${item.rttMs}ms`)] : []),
        removeBtn,
      );
      portsBox.append(row);
    }

    const portInput = el("input", "port-inline-input");
    Object.assign(portInput, {
      name: "add_port",
      value: portDraft,
      maxLength: 120,
      placeholder: items.length ? t("add_more_ports_placeholder") : t("add_ports_placeholder"),
      spellcheck: false,
      disabled: probingAll,
    });
    portInput.addEventListener("input", () => {
      portDraft = portInput.value;
      if (localError) { localError = ""; alertBox.hidden = true; alertBox.textContent = ""; }
      if (/[,，]$/.test(portDraft)) {
        const trimmed = portDraft.replace(/[,，]+$/, "").trim();
        if (trimmed) { portDraft = trimmed; commitPortInput(true); return; }
      }
      updateSubmit();
    });
    portInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") { e.preventDefault(); commitPortInput(true); }
    });
    portsBox.append(portInput);
    const portsHead = el(
      "div",
      "host-ip-head",
      el("span", "field-label", probingAll ? t(probingDeep ? "deep_scanning" : "detecting_host") : t("add_ports_label")),
      deepBtn,
    );
    body.append(el("div", "field-group", portsHead, portsBox));

    if (items.length || portDraft.trim()) {
      if (state.status?.online && isUsableHostIp(ipVal.trim()) && items.every((i) => i.port !== undefined)) {
        const pubCheck = el("input");
        Object.assign(pubCheck, { type: "checkbox", checked: publishNow });
        pubCheck.addEventListener("change", () => { publishNow = pubCheck.checked; updateSubmit(); });
        body.append(el("label", "check-row", pubCheck, t("save_and_publish")));
      }
    }

    const errText = localError || (state.message ? failure(state.message) : "");
    alertBox.hidden = !errText;
    alertBox.textContent = errText;
    body.append(alertBox);

    const saveBtn = button("", () => {
      if (portDraft.trim() && !commitPortInput(false)) return;
      const ipsText = ipVal.trim();
      if (!ipsText) { localError = t("invalid_host_ip"); draw(); return; }
      const chosen = items.filter((c) => c.checked);
      if (!chosen.length) { localError = t(manageMode ? "host_select_or_remove" : "select_one_service"); draw(); return; }
      localError = "";
      const singleHost = isUsableHostIp(ipsText);
      const allDiscrete = chosen.every((c) => c.port !== undefined);
      if (singleHost && allDiscrete) {
        const os = effectiveHostOs();
        void act({
          op: "save_host",
          ip: ipsText,
          name: nameVal.trim() || ipsText,
          host_os: os,
          services: chosen.map((c) => ({ port: c.port!, kind: c.kind })),
          publish: !!state.status?.online && publishNow,
          replace: manageMode,
          old_host_name: manageMode ? oldHostName : null,
        }, () => editor.close());
      } else {
        const kind = chosen.length === 1 ? chosen[0]!.kind : "tcp";
        void act({
          op: "save",
          id: null,
          name: nameVal.trim() || ipsText,
          ips: ipsText,
          ports: chosen.map((c) => c.spec).join(", "),
          http: kind === "http",
          kind,
          host_os: null,
        }, () => editor.close());
      }
    }, "button primary");

    function updateSubmit() {
      const count = items.filter((c) => c.checked).length;
      const removeCount = manageMode ? items.filter((c) => c.saved && !c.checked).length : 0;
      if (manageMode) {
        saveBtn.textContent = removeCount > 0 ? `${t("save")} (-${removeCount})` : count > 1 ? `${t("save")} (${count})` : t("save");
      } else {
        saveBtn.textContent = count > 1 ? `${t("save")} (${count})` : t("save");
      }
      saveBtn.disabled = probingAll || state.busy || (count === 0 && !portDraft.trim());
    }
    syncHeaderMeta();
    updateSubmit();

    const footer = el(
      "div",
      "dialog-footer",
      ...(manageMode ? [button(t("remove_host"), () => confirmRemoveHost(ipVal.trim(), nameVal.trim() || ipVal.trim(), initialExisting), "button ghost text-danger leading")] : []),
      button(t("cancel"), () => { if (!state.busy) editor.close(); }, "button"),
      saveBtn,
    );

    editor.tabIndex = -1;
    editor.replaceChildren(header, body, footer);
    if (!editor.open) {
      editor.showModal();
      if (ipVal) portInput.focus();
      else ipInput.focus();
    }
    if (keepPortFocus || activeName === "add_port") {
      keepPortFocus = false;
      portInput.focus();
      portInput.setSelectionRange(portInput.value.length, portInput.value.length);
    } else if (activeName === "host_name") {
      const ni = editor.querySelector<HTMLInputElement>('input[name="host_name"]');
      if (ni) { ni.focus(); ni.setSelectionRange(ni.value.length, ni.value.length); }
    } else if (activeName === "host_ip") {
      ipInput.focus();
      ipInput.setSelectionRange(ipInput.value.length, ipInput.value.length);
    }
  }

  draw();
}

export function addShare(emptyState = false) {
  if (emptyState) return button(t("new_share"), () => openAddShare(), "button primary");
  return createIconButton({
    icon: "plus",
    title: t("new_share"),
    variant: "solid",
    size: "lg",
    onClick: () => openAddShare(),
  });
}
