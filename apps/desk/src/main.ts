import { invoke } from "@tauri-apps/api/core";
import { getCurrent, onOpenUrl } from "@tauri-apps/plugin-deep-link";
import { failure, i18n, isZh, t } from "./i18n";
import { createIconButton, osBadge } from "../../../packages/ui/icons.js";
import { closePopoverMenu } from "../../../packages/ui/popover.js";
import { createAdminWorkspace } from "../../../packages/ui/admin-view.js";
import {
  act,
  actions,
  applyJoinLink,
  content,
  editor,
  form,
  registerRenderer,
  render,
  request,
  serverHost,
  settingsDialog,
  showError,
  syncBrand,
  syncSigninSubmit,
} from "./app/runtime";
import { openSettings } from "./dialogs/settings-dialog";
import { addShare } from "./dialogs/share-dialog";
import { checkSiteDialog, refreshSiteDialogI18n } from "./dialogs/site-dialog";
import { accessIpFor, effectiveKind, toInt } from "./model/net";
import { desktop, loadCollapsed, mac, probes, serviceKey, state } from "./model/state";
import type { IconName, Reply, Status } from "./model/types";
import { button, el, field, notify, str, submit } from "./ui/dom";
import { initDesktopWindowSize } from "./ui/window";
import {
  clearSearchInput,
  closeSearch,
  openSearch,
  searchInput,
  servicesPanel,
  updateServicesFlow,
} from "./views/services-flow";
import "./styles/index.css";

const adminWorkspace = createAdminWorkspace({
  t: (k) => t(k as Parameters<typeof t>[0]),
  isZh,
  failure,
  dialog: editor,
  getContext: () => ({
    member: state.status?.member,
    machineId: state.status?.machine_id,
    deviceId: state.status?.device_id,
  }),
  transport: {
    session: async () => {
      const reply = await request({ op: "admin_session" }, false);
      state.status = reply;
      return reply.admin_data;
    },
    action: async (body) => {
      state.status = await request({ op: "admin_action", action: body }, false);
    },
  },
  onUnauthorized() {
    if (state.status) state.status = { ...state.status, admin_ready: false };
    if (state.tab === "admin") state.tab = "services";
    render(true);
  },
});

adminWorkspace.onUpdate(() => {
  if (state.tab === "admin") render(true);
});

function selectTab(next: "services" | "admin") {
  if (next === "admin" && !state.status?.admin_ready) return;
  if (state.tab === next) return;
  closePopoverMenu();
  state.tab = next;
  render(true);
  if (next === "admin") void adminWorkspace.refresh();
}

window.addEventListener("keydown", (event) => {
  if ((event.metaKey || event.ctrlKey) && event.shiftKey && event.key.toLowerCase() === "m") {
    if (!state.status?.member || !state.status.admin_ready) return;
    event.preventDefault();
    selectTab(state.tab === "admin" ? "services" : "admin");
    return;
  }
  if ((event.metaKey || event.ctrlKey) && !event.shiftKey && !event.altKey && event.key.toLowerCase() === "f") {
    if (!state.status?.member || state.tab !== "services" || editor.open || settingsDialog.open) return;
    if (!state.status.services.length && !state.status.shares.length) return;
    event.preventDefault();
    openSearch();
    return;
  }
  if (
    event.key === "Escape"
    && (document.activeElement === searchInput || !!state.searchQuery)
    && state.tab === "services"
    && !editor.open
    && !settingsDialog.open
  ) {
    event.preventDefault();
    closeSearch();
  }
});

function buildConnectionControl(data: Status): HTMLElement {
  const running = data.running;
  const online = data.online;
  const connecting = running && !online;
  const symbol: IconName = online ? "link" : connecting ? "refresh" : "unlink";
  const tip = t(online ? "online" : connecting ? "connecting" : "disconnected");
  const b = createIconButton({
    icon: symbol,
    title: tip,
    variant: "solid",
    size: "lg",
    tone: online ? "accent" : "muted",
    spinning: connecting,
    onClick: () => void act({ op: running ? "stop" : "start" }),
  });
  b.setAttribute("data-connect-btn", "1");
  b.setAttribute("aria-pressed", String(running));
  return b;
}

function renderApp(force = false) {
  checkSiteDialog();
  showError();
  const data = state.status;
  const blocked = !!data?.auto_share_blocked;
  if (blocked && !state.lastAutoShareBlocked) {
    notify(t("auto_share_network_changed"));
  }
  state.lastAutoShareBlocked = blocked;
  if (!data?.admin_ready && state.tab === "admin") state.tab = "services";
  syncBrand(!state.brandBooted);
  const owner = data?.member ? JSON.stringify([data.server, data.member]) : "";
  if (owner !== state.tabOwner) {
    state.tabOwner = owner;
    state.tab = "services";
    clearSearchInput();
  }
  loadCollapsed(owner);
  if (!data?.online) probes.clear();
  else if (probes.size) {
    const listed = new Map(data.services.map((s) => [serviceKey(s), s]));
    for (const [key, probe] of probes) {
      const s = listed.get(key);
      const ip = toInt(probe.ip);
      if (
        !s
        || effectiveKind(s) !== probe.kind
        || accessIpFor(s.id, probe.ip, data) !== probe.access
        || !s.rules.ips.some((r) => toInt(r.start) <= ip && ip <= toInt(r.end))
        || !s.rules.ports.some((r) => r.start <= probe.port && probe.port <= r.end)
      ) {
        probes.delete(key);
      }
    }
  }
  const next = JSON.stringify([data && {
    ...data,
    version: null,
    error: null,
    background: null,
    active_flows: null,
    probe: null,
    host_probe: null,
    admin_data: null,
  }, state.tab, state.joining]);
  if (!force && state.applied === next) return;
  if (!force && !data?.member && content.contains(document.activeElement)) return;
  state.applied = next;
  const prevActive = document.activeElement as HTMLElement | null;
  const restoreAdminBtnFocus = !!prevActive && actions.contains(prevActive) && (() => {
    try { return prevActive.matches('[data-admin-btn="1"]:focus-visible'); } catch { return false; }
  })();
  closePopoverMenu();
  actions.replaceChildren();
  const settingsBtn = createIconButton({
    icon: "settings",
    title: t("settings"),
    variant: "solid",
    size: "lg",
    onClick: () => openSettings(),
  });
  settingsBtn.setAttribute("aria-haspopup", "dialog");
  actions.append(settingsBtn);

  if (!data) {
    const attach = (caption: string) => button(caption, () => void act({ op: "attach" }), "button primary");
    if (mac && state.message === "extension_permission") {
      content.replaceChildren(el(
        "div",
        "inline-actions",
        button(t("system_settings"), () => {
          void invoke("open_extension_settings").catch(() => { state.message = "extension_failed"; showError(); });
        }),
        attach(t("continue")),
      ));
    } else if (mac && ["extension_unsigned", "extension_restart", "extension_location"].includes(state.message)) {
      content.replaceChildren();
    } else {
      const caption = state.message === "extension_setup"
        ? t("enable_extension")
        : state.message === "extension_failed"
          ? t("retry")
          : t("open");
      content.replaceChildren(attach(caption));
    }
    return;
  }
  if (!data.member) {
    const inlineAlert = el("p", "feedback error");
    inlineAlert.setAttribute("role", "alert");
    inlineAlert.hidden = true;
    if (!data.server || state.joining) {
      content.replaceChildren(el(
        "section",
        "signin",
        form(
          (values) => {
            void act({ op: "join", address: str(values, "address").trim() }, () => { state.joining = false; });
          },
          el("h2", "form-title", t("join_group")),
          field(t("join_link"), "address", data.server, { required: true, maxLength: 1024, placeholder: "fesen://join…" }),
          inlineAlert,
          data.server
            ? el(
              "div",
              "signin-actions",
              button(t("cancel"), () => { state.joining = false; render(true); }, "button"),
              submit(t("save")),
            )
            : submit(t("save")),
        ),
      ));
    } else {
      const loginBtn = submit(t("login"));
      loginBtn.dataset.loginBtn = "1";
      const devOsEl = osBadge(data.device_os, { fallback: "device", local: true, prefix: t("badge_local") });
      const shortId = data.machine_id ? `#${data.machine_id.slice(0, 6)}` : "";
      const devChip = data.device_name ? el(
        "div",
        "signin-device",
        ...(devOsEl ? [devOsEl] : []),
        el("span", "signin-device-name", data.device_name),
        ...(shortId ? [el("span", "mono muted", shortId)] : []),
      ) : null;
      content.replaceChildren(el(
        "section",
        "signin",
        form(
          (values) => void act({
            op: "login",
            username: str(values, "username").trim(),
            password: str(values, "password"),
            label: data.device_name ?? "",
          }),
          el("h2", "form-title", t("login")),
          el(
            "div",
            "signin-server",
            el("span", "mono signin-server-host", serverHost(data.server)),
            button(t("change"), () => { state.joining = true; render(true); }, "button ghost"),
          ),
          field(t("username"), "username", "", { required: true, autocomplete: "username", maxLength: 64 }),
          field(t("password"), "password", "", { required: true, type: "password", autocomplete: "current-password" }),
          ...(devChip ? [devChip] : []),
          inlineAlert,
          loginBtn,
        ),
      ));
    }
    showError();
    syncSigninSubmit();
    return;
  }

  const adminBtn = data.admin_ready ? createIconButton({
    icon: state.tab === "admin" ? "home" : "console",
    title: state.tab === "admin" ? t("exit_admin") : t("admin_badge"),
    variant: "solid",
    size: "lg",
    onClick: () => selectTab(state.tab === "admin" ? "services" : "admin"),
  }) : null;
  if (adminBtn) {
    adminBtn.setAttribute("aria-pressed", String(state.tab === "admin"));
    adminBtn.dataset.adminBtn = "1";
  }

  const adminSep = adminBtn ? el("span", "toolbar-sep") : null;
  if (adminSep) adminSep.setAttribute("aria-hidden", "true");

  actions.prepend(
    ...(adminBtn && adminSep ? [adminBtn, adminSep] : []),
    ...(state.tab === "services" ? [addShare()] : []),
    buildConnectionControl(data),
  );
  if (restoreAdminBtnFocus && adminBtn) {
    adminBtn.focus({ preventScroll: true });
  }

  if (state.tab === "admin" && data.admin_ready) {
    const activeEl = document.activeElement as HTMLElement | null;
    const restoreTabFocus = !!activeEl && content.contains(activeEl) && (() => {
      try { return activeEl.matches('[role="tab"]:focus-visible'); } catch { return false; }
    })();
    const viewKey = `admin:${adminWorkspace.getTabIndex()}`;
    const enterPanel = !state.lastViewKey.startsWith("admin:");
    const enterBody = !enterPanel && viewKey !== state.lastViewKey;
    state.lastViewKey = viewKey;
    content.replaceChildren(adminWorkspace.renderPanel({ enterPanel, enterBody }));
    if (restoreTabFocus) {
      content.querySelector<HTMLElement>('[role="tab"][aria-selected="true"]')?.focus({ preventScroll: true });
    }
    return;
  }

  const activeEl = document.activeElement as HTMLElement | null;
  const kbFocusVisible = !!activeEl && content.contains(activeEl) && (() => {
    try { return activeEl.matches(":focus-visible"); } catch { return false; }
  })();
  const focusedCollapseKey = kbFocusVisible ? activeEl?.closest<HTMLElement>("[data-collapse-key]")?.dataset.collapseKey : undefined;
  const focusedViewMode = kbFocusVisible ? activeEl?.closest<HTMLElement>("[data-view-mode]")?.dataset.viewMode : undefined;

  const viewKey = "client:services";
  const animateEnter = viewKey !== state.lastViewKey;
  state.lastViewKey = viewKey;
  servicesPanel.className = animateEnter ? "panel panel-enter" : "panel";
  if (servicesPanel.parentElement !== content) {
    content.replaceChildren(servicesPanel);
  }
  updateServicesFlow(data);

  if (focusedCollapseKey) {
    const target = [...content.querySelectorAll<HTMLElement>("[data-collapse-key]")]
      .find((n) => n.dataset.collapseKey === focusedCollapseKey);
    target?.focus({ preventScroll: true });
  } else if (focusedViewMode) {
    const target = [...content.querySelectorAll<HTMLElement>("[data-view-mode]")]
      .find((n) => n.dataset.viewMode === focusedViewMode);
    target?.focus({ preventScroll: true });
  }
}

registerRenderer(renderApp);

function deepLink(urls: string[] | null) {
  const link = urls?.find((url) => url.startsWith("fesen://join?"));
  if (!link) return;
  state.pendingJoin = link;
  applyJoinLink();
}

async function boot() {
  syncBrand(true);
  void initDesktopWindowSize(settingsDialog);
  i18n.onChange(() => {
    refreshSiteDialogI18n();
    syncBrand();
    render(true);
  });
  try { state.status = await request({ op: "attach" }, !mac); }
  catch (error) { state.message = typeof error === "string" ? error : "not_connected"; }
  syncBrand(true);
  state.brandBooted = true;
  render(true);
  void getCurrent().then(deepLink).catch(() => {});
  void onOpenUrl(deepLink).catch(() => {});
  if (state.status?.member && state.status.auto_connect !== false && !state.status.running && !state.status.online && !state.pendingJoin) {
    void act({ op: "start" });
  }
  let pollTimer: number | undefined;
  const schedulePoll = () => {
    window.clearTimeout(pollTimer);
    if (!desktop && document.visibilityState === "hidden") return;
    const delay = document.visibilityState === "hidden" ? 5000 : 1800;
    pollTimer = window.setTimeout(() => { void pollOnce(); }, delay);
  };
  const pollOnce = async () => {
    window.clearTimeout(pollTimer);
    if (state.busy || state.polling || (mac && !state.status) || (!desktop && document.visibilityState === "hidden")) {
      schedulePoll();
      return;
    }
    state.polling = true;
    try {
      const since = state.status?.version ?? 0;
      const reply = await invoke<Reply>("core_command", {
        request: since ? { op: "status", ui: true, since } : { op: "status", ui: true },
        launch: false,
      });
      if (reply.unchanged) return;
      if (!reply.data) throw reply.error ?? "internal";
      if (!state.busy) {
        state.status = reply.data;
        render();
      }
    } catch {
      if (mac && state.status) {
        try {
          const reattached = await request({ op: "attach" }, false);
          if (!state.busy) {
            state.status = reattached;
            render();
            return;
          }
        } catch {}
      }
      if (state.status?.online || state.status?.running) {
        state.status = {
          ...state.status,
          version: 0,
          running: false,
          online: false,
          active: [],
          active_flows: 0,
          services: [],
          virtual_ips: [],
          aliases: {},
        };
        render();
      }
    } finally {
      state.polling = false;
      schedulePoll();
    }
  };
  window.addEventListener("focus", () => { void pollOnce(); });
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") void pollOnce();
    else schedulePoll();
  });
  schedulePoll();
}

void boot();
