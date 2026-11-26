import { createIconButton, osBadge } from "./icons.js";
import { closePopoverMenu, openPopoverMenu } from "./popover.js";
import {
  deviceOsLabel,
  effectiveKind,
  formatEndpoint,
  formatRanges,
  groupByHost,
  meaningfulHostName,
  osLabel,
  resolveHostOs,
  singleIp,
} from "./host-card.js";

function el(tag, cls = "", ...children) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  node.append(...children);
  return node;
}

/**
 * Shared Server Administration Workspace component.
 * Driven purely by an injected transport (fetch in Web /admin/, core IPC in Client).
 * Renders an identical unified `.panel-bar` + `.panel-body` across Client and Web `/admin/`.
 * @param {{
 *   t: (k: string) => string,
 *   isZh: () => boolean,
 *   failure: (code: unknown) => string,
 *   transport: {
 *     session: () => Promise<any>,
 *     action: (body: Record<string, unknown>) => Promise<void>,
 *   },
 *   dialog: HTMLDialogElement,
 *   getContext?: () => {
 *     member?: string | null,
 *     machineId?: string,
 *     deviceId?: string | null,
 *   },
 *   onData?: (data: any) => void,
 *   onUnauthorized?: () => void,
 * }} opts
 */
export function createAdminWorkspace(opts) {
  const { t, isZh, failure, transport, dialog } = opts;
  let data = null;
  let tab = 0; // 0=members, 1=devices, 2=services
  let loading = false;
  let errorText = "";

  const onUpdateListeners = new Set();
  const emitUpdate = () => { for (const fn of onUpdateListeners) fn(); };

  function getTabIndex() { return tab; }
  function setTabIndex(index) {
    tab = index;
    closePopoverMenu();
    emitUpdate();
  }

  function getData() { return data; }

  async function refresh({ throwOnError = false } = {}) {
    if (loading) {
      if (throwOnError) throw new Error(t("busy"));
      return;
    }
    loading = true;
    errorText = "";
    emitUpdate();
    try {
      data = await transport.session();
      opts.onData?.(data);
    } catch (err) {
      if (err === "unauthorized" || err?.status === 401) {
        data = null;
        opts.onUnauthorized?.();
      } else {
        errorText = err instanceof Error ? err.message : failure(err);
      }
      if (throwOnError) throw err;
    } finally {
      loading = false;
      emitUpdate();
    }
  }

  function openJoinDialog() {
    if (!data?.link) return;
    const host = (() => {
      try { return new URL(new URL(data.link).searchParams.get("server")).host; }
      catch { return ""; }
    })();
    const title = el("h2", "", t("join"));
    title.id = "editor-title";
    const closeBtn = createIconButton({
      icon: "close",
      title: t("close"),
      variant: "ghost",
      size: "md",
      onClick: () => dialog.close(),
    });
    const header = el("div", "dialog-header", title, closeBtn);

    const textarea = el("textarea", "join-address");
    textarea.readOnly = true;
    textarea.rows = 2;
    textarea.spellcheck = false;
    textarea.value = data.link;
    textarea.setAttribute("aria-label", t("join"));

    const body = el("div", "dialog-body",
      ...(host ? [el("p", "muted mono", host)] : []),
      textarea
    );

    const copyBtn = el("button", "button primary", t("copy_link"));
    copyBtn.type = "button";
    let timer;
    copyBtn.addEventListener("click", async () => {
      try { await navigator.clipboard.writeText(data.link); }
      catch { textarea.focus(); textarea.select(); return; }
      clearTimeout(timer);
      copyBtn.textContent = t("copied");
      timer = setTimeout(() => { copyBtn.textContent = t("copy_link"); }, 1600);
    });

    const footerNodes = [];
    if (location.protocol.startsWith("http") && location.pathname.startsWith("/admin")) {
      const openLink = el("a", "button", t("open_join"));
      openLink.href = data.link;
      footerNodes.push(openLink);
    }
    footerNodes.push(copyBtn);

    dialog.tabIndex = -1;
    dialog.replaceChildren(header, body, el("div", "dialog-footer", ...footerNodes));
    if (!dialog.open) {
      dialog.showModal();
      dialog.focus();
    }
  }

  function openActionDialog(command, subject = "") {
    const key = command.op === "enable" && !command.enabled
      ? "disable"
      : command.op === "set_admin"
        ? (command.admin ? "grant_admin" : "revoke_admin")
        : command.op;
    const danger = key === "disable" || key === "revoke" || key === "revoke_admin";
    const needsUser = key === "create";
    const needsPass = key === "create" || key === "reset";

    let submitting = false;
    const title = el("h2", "", t(key));
    title.id = "editor-title";
    const closeBtn = createIconButton({
      icon: "close",
      title: t("close"),
      variant: "ghost",
      size: "md",
      onClick: () => { if (!submitting) dialog.close(); },
    });
    const header = el("div", "dialog-header", title, closeBtn);

    const body = el("div", "dialog-body");
    if (subject) body.append(el("p", "subject", subject));
    if (danger || key === "reset" || key === "grant_admin") body.append(el("p", "muted", t(`confirm_${key}`)));

    let userInput = null;
    if (needsUser) {
      userInput = el("input");
      Object.assign(userInput, { name: "username", required: true, maxLength: 64, autocomplete: "off", pattern: "[A-Za-z0-9._\\-]+" });
      body.append(el("label", "", t("account"), userInput));
    }
    let passInput = null;
    if (needsPass) {
      passInput = el("input");
      Object.assign(passInput, { name: "password", type: "password", required: true, minLength: 8, maxLength: 256, autocomplete: "new-password" });
      body.append(el("label", "", t("new_password"), passInput));
    }
    let adminCheck = null;
    if (needsUser) {
      adminCheck = el("input");
      adminCheck.type = "checkbox";
      adminCheck.name = "admin";
      body.append(el("label", "check-row", adminCheck, t("create_as_admin")));
    }

    const errBox = el("p", "feedback error");
    errBox.setAttribute("role", "alert");
    errBox.hidden = true;
    body.append(errBox);

    const cancelBtn = el("button", "button", t("cancel"));
    cancelBtn.type = "button";
    cancelBtn.addEventListener("click", () => { if (!submitting) dialog.close(); });
    const submitBtn = el("button", `button ${danger ? "danger" : "primary"}`, t(key));
    submitBtn.type = "submit";
    const footer = el("div", "dialog-footer", cancelBtn, submitBtn);

    const formEl = el("form", "", header, body, footer);
    formEl.addEventListener("submit", async (e) => {
      e.preventDefault();
      if (submitting) return;
      const payload = { ...command };
      if (userInput) payload.username = userInput.value.trim();
      if (passInput) {
        payload.password = passInput.value;
        passInput.value = "";
      }
      if (adminCheck) payload.admin = adminCheck.checked;
      submitting = true;
      submitBtn.disabled = true;
      cancelBtn.disabled = true;
      errBox.hidden = true;
      try {
        await transport.action(payload);
        dialog.close();
        await refresh();
      } catch (err) {
        if (err === "unauthorized" || err?.status === 401) {
          dialog.close();
          data = null;
          opts.onUnauthorized?.();
          return;
        }
        errBox.textContent = err instanceof Error ? err.message : failure(err);
        errBox.hidden = false;
      } finally {
        submitting = false;
        submitBtn.disabled = false;
        cancelBtn.disabled = false;
      }
    });

    dialog.tabIndex = -1;
    dialog.replaceChildren(formEl);
    if (!dialog.open) {
      dialog.showModal();
      const firstInput = userInput || passInput;
      if (firstInput) firstInput.focus();
      else dialog.focus();
    }
  }

  function entityRow(name, statusKey, tone, actions, { metaNodes = [], osType = null, fallbackOs = false, local = false, selfName = false } = {}) {
    const badge = osBadge(osType, {
      fallback: fallbackOs ? "device" : false,
      local,
      prefix: local ? t("badge_local") : "",
    });
    const nameEl = el("span", selfName ? "entity-name self" : "entity-name", name);
    const main = el("div", "entity-main", ...(badge ? [badge] : []), nameEl);
    if (metaNodes.length) {
      const metaWrap = el("span", "entity-meta-wrap");
      metaNodes.forEach((node, idx) => {
        if (idx > 0) metaWrap.append(el("span", "meta-sep", "·"));
        metaWrap.append(node);
      });
      main.append(metaWrap);
    }

    const right = el("div", "entity-actions", el("span", `state-dot${tone ? ` ${tone}` : ""}`, t(statusKey)));
    if (actions.length) {
      const more = createIconButton({
        icon: "more",
        title: `${t("configure")} · ${name}`,
        variant: "ghost",
        size: "md",
        onClick: () => openPopoverMenu(more, actions),
      });
      more.setAttribute("aria-haspopup", "menu");
      more.setAttribute("aria-expanded", "false");
      right.append(more);
    } else {
      right.append(el("span", "entity-slot"));
    }
    return el("div", "entity-row", main, right);
  }

  function buildServiceMeta(s, clean, ctx) {
    const isLocal = !!(ctx.deviceId && s.owner === ctx.deviceId);
    const currentMember = ctx.member || data?.admin || "";
    const isSelfAccount = !!(currentMember && s.member === currentMember);
    const parts = [];
    if (clean) parts.push(el("span", "service-name", clean));
    if (isLocal) {
      parts.push(el("span", "source-tag self", t("badge_local")));
    }
    if (s.member) {
      parts.push(el("span", isSelfAccount ? "source-tag self" : "source-tag", s.member));
    }
    if (s.device) {
      parts.push(el("span", "service-name", s.device));
    }
    return { isLocal, isSelfAccount, parts };
  }

  function appendMetaWithSeps(container, parts) {
    parts.forEach((node, idx) => {
      if (idx > 0) container.append(el("span", "meta-sep", "·"));
      container.append(node);
    });
    return container;
  }

  function renderServices(services, ctx) {
    const wrap = el("div", "service-list");
    const entries = groupByHost(services, (s) => `${s.owner || s.member}:${singleIp(s)}`);
    for (const entry of entries) {
      if (!entry.grouped || entry.items.length === 1) {
        const s = entry.grouped ? entry.items[0] : entry.item;
        const kind = effectiveKind(s);
        const os = resolveHostOs([s]);
        const ip = singleIp(s);
        const leadOs = os || s.device_os || null;
        const clean = ip ? meaningfulHostName(s.name, ip, os) : s.name;
        const { isLocal, isSelfAccount, parts } = buildServiceMeta(s, clean, ctx);
        const localPrefix = isLocal ? (isSelfAccount ? `${t("badge_local")} · ${t("source_self")}` : t("badge_local")) : "";
        const osTag = osBadge(leadOs, { fallback: "server", local: isLocal, prefix: localPrefix });
        const title = el("div", "host-title-line", ...(osTag ? [osTag] : []), el("p", "endpoint", formatEndpoint(s.rules, isZh())));
        if (kind !== "tcp") title.append(el("span", "proto-tag", kind.toUpperCase()));
        const devTag = os && s.device_os && deviceOsLabel(s.device_os) !== osLabel(os) ? osBadge(s.device_os) : null;
        const metaNodes = [];
        if (parts.length || devTag) {
          const metaLine = appendMetaWithSeps(el("div", "service-meta"), parts);
          if (devTag) metaLine.append(devTag);
          metaNodes.push(metaLine);
        }
        const main = el("div", "service-main", title, ...metaNodes);
        wrap.append(el("article", "service-row", main, el("span", "state-dot good", t("broadcasting"))));
        continue;
      }
      const os = resolveHostOs(entry.items);
      const first = entry.items[0];
      const leadOs = os || first.device_os || null;
      const clean = meaningfulHostName(first.name, entry.ip, os);
      const { isLocal, isSelfAccount, parts } = buildServiceMeta(first, clean, ctx);
      const localPrefix = isLocal ? (isSelfAccount ? `${t("badge_local")} · ${t("source_self")}` : t("badge_local")) : "";
      const osTag = osBadge(leadOs, { fallback: "server", local: isLocal, prefix: localPrefix });
      const title = el("div", "host-title-line", ...(osTag ? [osTag] : []), el("p", "endpoint", entry.ip));
      const devTag = os && first.device_os && deviceOsLabel(first.device_os) !== osLabel(os) ? osBadge(first.device_os) : null;
      const subNodes = [];
      if (parts.length || devTag) {
        const metaLine = appendMetaWithSeps(el("div", "service-meta"), parts);
        if (devTag) metaLine.append(devTag);
        subNodes.push(metaLine);
      }
      const head = el("div", "host-head",
        el("div", "host-info", title, ...subNodes),
        el("span", "state-dot good", t("broadcasting"))
      );
      const portsWrap = el("div", "host-ports", ...entry.items.map((s) => {
        const kind = effectiveKind(s);
        const pm = el("div", "port-main",
          el("span", "port-num", `:${formatRanges(s.rules?.ports, isZh())}`),
          el("span", "proto-tag", kind.toUpperCase())
        );
        const subName = s.name !== first.name ? meaningfulHostName(s.name, entry.ip, os) : "";
        if (subName) pm.append(el("span", "port-sub", subName));
        return el("div", "port-row-wrap", el("div", "port-row", pm));
      }));
      wrap.append(el("article", "host-card", head, portsWrap));
    }
    return wrap;
  }

  function renderTabs(navEl) {
    const services = data?.services || [];
    const counts = [
      data?.users?.length ?? 0,
      data?.devices?.length ?? 0,
      services.length,
    ];
    const keys = ["members", "devices", "services"];
    if (tab >= keys.length) tab = 0;
    navEl.setAttribute("role", "tablist");
    navEl.setAttribute("aria-label", t("workspace"));
    navEl.replaceChildren(...keys.map((key, idx) => {
      const b = el("button", "", t(key));
      b.type = "button";
      b.setAttribute("role", "tab");
      b.setAttribute("aria-selected", String(idx === tab));
      b.setAttribute("aria-current", idx === tab ? "page" : "false");
      b.append(el("span", "count", String(counts[idx])));
      b.addEventListener("click", () => setTabIndex(idx));
      return b;
    }));
  }

  function buildToolbarButtons() {
    const btns = [];
    if (tab === 0) {
      btns.push(createIconButton({
        icon: "plus",
        title: t("create"),
        variant: "ghost",
        size: "md",
        onClick: () => openActionDialog({ op: "create" }),
      }));
    }
    const refreshBtn = createIconButton({
      icon: "refresh",
      title: t("refresh"),
      variant: "ghost",
      size: "md",
      spinning: loading,
      onClick: () => void refresh(),
    });
    refreshBtn.disabled = loading;
    btns.push(refreshBtn);

    const joinBtn = createIconButton({
      icon: "share",
      title: t("join"),
      variant: "ghost",
      size: "md",
      onClick: openJoinDialog,
    });
    joinBtn.disabled = !data?.link;
    btns.push(joinBtn);
    return btns;
  }

  function renderPanel({ enterPanel = false, enterBody = false } = {}) {
    const panel = el("section", enterPanel ? "panel panel-enter" : "panel");
    const ctx = opts.getContext?.() || {};

    if (data) {
      const nav = el("nav", "tabs");
      renderTabs(nav);
      const toolbar = el("div", "toolbar", ...buildToolbarButtons());
      panel.append(el("div", "panel-bar", nav, toolbar));
    }

    const body = el("div", enterBody && !enterPanel ? "panel-body panel-enter" : "panel-body");
    if (errorText) {
      const err = el("p", "feedback error panel-feedback", errorText);
      err.setAttribute("role", "alert");
      body.append(err);
    }
    if (!data) {
      body.append(el("p", "loading", loading ? t("loading") : (errorText || t("loading"))));
      panel.append(body);
      return panel;
    }

    const services = data.services || [];

    if (tab === 0) {
      const list = el("div", "list-card", ...data.users.map((u) => {
        const isSelf = u.name === data.admin;
        const menu = [];
        if (u.admin) {
          if (!isSelf) {
            menu.push({
              icon: "shield",
              label: t("revoke_admin"),
              danger: true,
              onSelect: () => openActionDialog({ op: "set_admin", username: u.name, admin: false }, u.name),
            });
          }
        } else {
          if (!u.disabled) {
            menu.push({
              icon: "shield",
              label: t("grant_admin"),
              onSelect: () => openActionDialog({ op: "set_admin", username: u.name, admin: true }, u.name),
            });
          }
          menu.push({ icon: "sliders", label: t("reset"), onSelect: () => openActionDialog({ op: "reset", username: u.name }, u.name) });
          menu.push({
            icon: u.disabled ? "check" : "alert",
            label: t(u.disabled ? "enable" : "disable"),
            danger: !u.disabled,
            onSelect: () => openActionDialog({ op: "enable", username: u.name, enabled: u.disabled }, u.name),
          });
        }
        return entityRow(
          u.name,
          u.disabled ? "disabled" : u.admin ? "admin" : "enabled",
          u.disabled ? "bad" : "good",
          menu,
          {
            metaNodes: isSelf ? [el("span", "source-tag self", t("source_self"))] : [],
            selfName: isSelf,
          }
        );
      }));
      if (data.users.length <= 1) list.dataset.single = "1";
      body.append(list);
    } else if (tab === 1) {
      if (!data.devices.length) {
        body.append(el("p", "empty", t("devices_empty")));
      } else {
        const isLocalDevice = (d) =>
          !!(ctx.deviceId && d.id === ctx.deviceId)
          || (!d.revoked && !!ctx.machineId && d.machine_id === ctx.machineId
            && (!ctx.member || d.member === ctx.member));
        const ordered = [...data.devices].sort((a, b) => {
          const al = isLocalDevice(a) ? 1 : 0;
          const bl = isLocalDevice(b) ? 1 : 0;
          return bl - al;
        });
        const list = el("div", "list-card", ...ordered.map((d) => {
          const local = isLocalDevice(d);
          const shortId = `#${(d.machine_id || d.id || "").slice(0, 6)}`;
          const metaNodes = [];
          if (local) metaNodes.push(el("span", "source-tag self", t("badge_local")));
          if (d.member) metaNodes.push(el("span", "entity-meta", d.member));
          if (shortId) metaNodes.push(el("span", "entity-meta", shortId));
          return entityRow(
            d.label,
            d.revoked ? "revoked" : d.online ? "online_state" : "offline_state",
            d.revoked ? "bad" : d.online ? "good" : "",
            d.revoked || local ? [] : [
              { icon: "close", label: t("revoke"), danger: true, onSelect: () => openActionDialog({ op: "revoke", device: d.id }, d.label) },
            ],
            {
              metaNodes,
              osType: d.os || null,
              fallbackOs: true,
              local,
              selfName: local,
            }
          );
        }));
        if (ordered.length <= 1) list.dataset.single = "1";
        body.append(list);
      }
    } else {
      body.append(services.length ? renderServices(services, ctx) : el("p", "empty", t("services_empty")));
    }
    panel.append(body);
    return panel;
  }

  return {
    refresh,
    getData,
    setData(next) { data = next; emitUpdate(); },
    getTabIndex,
    setTabIndex,
    openJoinDialog,
    renderTabs,
    buildToolbarButtons,
    renderPanel,
    onUpdate(fn) {
      onUpdateListeners.add(fn);
      return () => onUpdateListeners.delete(fn);
    },
  };
}
