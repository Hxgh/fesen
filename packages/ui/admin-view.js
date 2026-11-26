import { icon } from "./icons.js";
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
 * @param {{
 *   t: (k: string) => string,
 *   isZh: () => boolean,
 *   failure: (code: unknown) => string,
 *   transport: {
 *     session: () => Promise<any>,
 *     action: (body: Record<string, unknown>) => Promise<void>,
 *   },
 *   dialog: HTMLDialogElement,
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
    const closeBtn = el("button", "button icon-button", icon("close"));
    closeBtn.type = "button";
    closeBtn.title = t("close");
    closeBtn.setAttribute("aria-label", t("close"));
    closeBtn.addEventListener("click", () => dialog.close());
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
    const closeBtn = el("button", "button icon-button", icon("close"));
    closeBtn.type = "button";
    closeBtn.title = t("close");
    closeBtn.setAttribute("aria-label", t("close"));
    closeBtn.addEventListener("click", () => { if (!submitting) dialog.close(); });
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

  function entityRow(name, statusKey, tone, actions, meta = "", tagText = "") {
    const main = el("div", "entity-main", el("span", "entity-name", name));
    if (tagText) main.append(el("span", "host-os-tag", tagText));
    if (meta) main.append(el("span", "entity-meta", meta));

    const right = el("div", "entity-actions", el("span", `state-dot${tone ? ` ${tone}` : ""}`, t(statusKey)));
    if (actions.length) {
      const more = el("button", "button icon-button", icon("more"));
      more.type = "button";
      more.title = `${t("configure")} · ${name}`;
      more.setAttribute("aria-label", more.title);
      more.setAttribute("aria-haspopup", "menu");
      more.setAttribute("aria-expanded", "false");
      more.addEventListener("click", () => openPopoverMenu(more, actions));
      right.append(more);
    } else {
      right.append(el("span", "entity-slot"));
    }
    return el("div", "entity-row", main, right);
  }

  function renderServices(services) {
    const wrap = el("div", "service-list");
    const entries = groupByHost(services, (s) => `${s.owner || s.member}:${singleIp(s)}`);
    for (const entry of entries) {
      if (!entry.grouped || entry.items.length === 1) {
        const s = entry.grouped ? entry.items[0] : entry.item;
        const kind = effectiveKind(s);
        const os = resolveHostOs([s]);
        const ip = singleIp(s);
        const title = el("div", "host-title-line", el("p", "endpoint", formatEndpoint(s.rules, isZh())));
        if (os) title.append(el("span", "host-os-tag", osLabel(os)));
        if (kind !== "tcp") title.append(el("span", "proto-tag", kind.toUpperCase()));
        const clean = ip ? meaningfulHostName(s.name, ip, os) : s.name;
        const devOs = deviceOsLabel(s.device_os);
        const devTag = [s.device, devOs].filter(Boolean).join(" ");
        const source = [s.member, devTag].filter(Boolean).join(" · ");
        const sub = [clean, source].filter(Boolean).join(" · ");
        const main = el("div", "service-main", title, ...(sub ? [el("span", "entity-meta", sub)] : []));
        wrap.append(el("article", "service-row", main, el("span", "state-dot good", t("broadcasting"))));
        continue;
      }
      const os = resolveHostOs(entry.items);
      const first = entry.items[0];
      const title = el("div", "host-title-line", el("p", "endpoint", entry.ip));
      if (os) title.append(el("span", "host-os-tag", osLabel(os)));
      const clean = meaningfulHostName(first.name, entry.ip, os);
      const devOs = deviceOsLabel(first.device_os);
      const devTag = [first.device, devOs].filter(Boolean).join(" ");
      const source = [first.member, devTag].filter(Boolean).join(" · ");
      const sub = [clean, source].filter(Boolean).join(" · ");
      const head = el("div", "host-head",
        el("div", "host-info", title, ...(sub ? [el("span", "entity-meta", sub)] : [])),
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
      const addBtn = el("button", "tool-btn icon-only", icon("plus"));
      addBtn.type = "button";
      addBtn.title = t("create");
      addBtn.setAttribute("aria-label", t("create"));
      addBtn.addEventListener("click", () => openActionDialog({ op: "create" }));
      btns.push(addBtn);
    }
    const refreshBtn = el("button", loading ? "tool-btn icon-only spinning" : "tool-btn icon-only", icon("refresh"));
    refreshBtn.type = "button";
    refreshBtn.title = t("refresh");
    refreshBtn.setAttribute("aria-label", t("refresh"));
    refreshBtn.disabled = loading;
    refreshBtn.addEventListener("click", () => void refresh());
    btns.push(refreshBtn);

    const joinBtn = el("button", "tool-btn icon-only", icon("link"));
    joinBtn.type = "button";
    joinBtn.title = t("join");
    joinBtn.setAttribute("aria-label", t("join"));
    joinBtn.disabled = !data?.link;
    joinBtn.addEventListener("click", openJoinDialog);
    btns.push(joinBtn);
    return btns;
  }

  function renderPanel() {
    const panel = el("section", "panel");
    if (errorText) {
      const err = el("p", "feedback error panel-feedback", errorText);
      err.setAttribute("role", "alert");
      panel.append(err);
    }
    if (!data) {
      panel.append(el("p", "loading", loading ? t("loading") : (errorText || t("loading"))));
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
          u.admin ? "admin" : u.disabled ? "disabled" : "enabled",
          u.admin ? "" : u.disabled ? "bad" : "good",
          menu
        );
      }));
      panel.append(list);
    } else if (tab === 1) {
      if (!data.devices.length) {
        panel.append(el("p", "empty", t("devices_empty")));
      } else {
        const list = el("div", "list-card", ...data.devices.map((d) => {
          const shortId = `#${(d.machine_id || d.id || "").slice(0, 6)}`;
          const meta = [d.member, shortId].filter(Boolean).join(" · ");
          return entityRow(
            d.label,
            d.revoked ? "revoked" : d.online ? "online_state" : "offline_state",
            d.revoked ? "bad" : d.online ? "good" : "",
            d.revoked ? [] : [
              { icon: "close", label: t("revoke"), danger: true, onSelect: () => openActionDialog({ op: "revoke", device: d.id }, d.label) },
            ],
            meta,
            deviceOsLabel(d.os)
          );
        }));
        panel.append(list);
      }
    } else {
      panel.append(services.length ? renderServices(services) : el("p", "empty", t("services_empty")));
    }
    return panel;
  }

  function renderSubbar() {
    const subtabs = el("div", "admin-subtabs");
    renderTabs(subtabs);
    const subactions = el("div", "admin-subactions", ...buildToolbarButtons());
    return el("div", "admin-subbar", subtabs, subactions);
  }

  return {
    refresh,
    getData,
    setData(next) { data = next; emitUpdate(); },
    getTabIndex,
    setTabIndex,
    openJoinDialog,
    renderTabs,
    renderSubbar,
    buildToolbarButtons,
    renderPanel,
    onUpdate(fn) {
      onUpdateListeners.add(fn);
      return () => onUpdateListeners.delete(fn);
    },
  };
}
