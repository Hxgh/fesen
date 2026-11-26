import { createIconButton, createTrigram } from "./icons.js";
import { createScale, createTheme } from "./theme.js";
import { createI18n } from "./i18n.js";
import { openSettingsModal } from "./settings.js";
import { createAdminWorkspace } from "./admin-view.js";

const byId = (id) => document.getElementById(id);
const theme = createTheme("fesen_admin_theme");
const scale = createScale("fesen_admin_scale");
const i18n = createI18n("fesen_admin_locale");
const { t, isZh, failure } = i18n;

const topMark = createTrigram("connecting");
byId("brand-mark").replaceWith(topMark.el);

const actions = byId("actions");
const content = byId("content");
const signin = byId("signin");
const loadingEl = byId("loading");
const brandBadge = byId("brand-badge");
const editor = byId("editor");
const settingsDialog = byId("settings-dialog");

editor.addEventListener("click", (e) => { if (e.target === editor) editor.close(); });
settingsDialog.addEventListener("click", (e) => { if (e.target === settingsDialog) settingsDialog.close(); });

let signedIn = false;
let currentHost = "";
let booted = false;
let lastViewKey = "";

async function api(path, body) {
  const response = await fetch(path, body ? {
    method: "POST",
    headers: { "Content-Type": "application/json", "X-Fesen-Admin": "1" },
    body: JSON.stringify(body),
  } : undefined).catch(() => { throw new Error(t("network")); });
  if (!response.ok) {
    const payload = await response.json().catch(() => ({}));
    throw Object.assign(new Error(failure(payload.error)), { status: response.status });
  }
  return response.status === 204 ? null : response.json();
}

const admin = createAdminWorkspace({
  t,
  isZh,
  failure,
  dialog: editor,
  transport: {
    session: () => api("session"),
    action: (body) => api("action", body),
  },
  onData(data) {
    signedIn = true;
    try { currentHost = new URL(new URL(data.link).searchParams.get("server")).host; }
    catch { currentHost = location.host; }
    topMark.setState("active", !booted);
    booted = true;
  },
  onUnauthorized() {
    signedIn = false;
    currentHost = "";
    topMark.setState("idle", !booted);
    booted = true;
  },
});

function openSettings() {
  openSettingsModal(settingsDialog, {
    t,
    theme,
    scale,
    i18n,
    onLocaleChange: () => { syncCopy(); render(); },
    extraBody() {
      const data = admin.getData();
      if (!signedIn || !data) return [];
      const divider = document.createElement("div");
      divider.className = "settings-divider";
      const row = document.createElement("div");
      row.className = "settings-account";
      const info = document.createElement("span");
      info.className = "settings-account-info";
      info.textContent = `${data.admin} · ${currentHost}`;
      const out = document.createElement("button");
      out.type = "button";
      out.className = "button danger";
      out.textContent = t("logout");
      out.addEventListener("click", async () => {
        out.disabled = true;
        try {
          await api("action", { op: "logout" });
          settingsDialog.close();
          signedIn = false;
          currentHost = "";
          admin.setData(null);
          topMark.setState("idle");
          render();
        } finally {
          out.disabled = false;
        }
      });
      row.append(info, out);
      return [divider, row];
    },
  });
}

const copyNodes = [...document.querySelectorAll("[data-copy]")];
const settingsBtn = createIconButton({
  icon: "settings",
  title: t("settings"),
  variant: "solid",
  size: "lg",
  onClick: openSettings,
});
settingsBtn.setAttribute("aria-haspopup", "dialog");
actions.append(settingsBtn);

function syncCopy() {
  document.title = t("admin_title");
  for (const el of copyNodes) el.textContent = t(el.dataset.copy);
  const settingsLabel = t("settings");
  settingsBtn.title = settingsLabel;
  settingsBtn.setAttribute("aria-label", settingsLabel);
  brandBadge.title = [t("brand"), currentHost].filter(Boolean).join(" · ");
}

function render() {
  syncCopy();
  loadingEl.hidden = booted;
  signin.hidden = !booted || signedIn;
  content.hidden = !signedIn;

  if (signedIn) {
    const activeEl = document.activeElement;
    const restoreTabFocus = !!activeEl && content.contains(activeEl) && (() => {
      try { return activeEl.matches('[role="tab"]:focus-visible'); } catch { return false; }
    })();
    const viewKey = `admin:${admin.getTabIndex()}`;
    const enterPanel = !lastViewKey.startsWith("admin:");
    const enterBody = !enterPanel && viewKey !== lastViewKey;
    lastViewKey = viewKey;
    content.replaceChildren(admin.renderPanel({ enterPanel, enterBody }));
    if (restoreTabFocus) {
      content.querySelector('[role="tab"][aria-selected="true"]')?.focus({ preventScroll: true });
    }
  } else {
    lastViewKey = "";
    content.replaceChildren();
  }
}

admin.onUpdate(render);
i18n.onChange(() => { syncCopy(); render(); });

byId("login").addEventListener("submit", async (e) => {
  e.preventDefault();
  const form = e.currentTarget;
  const btn = form.querySelector("[type=submit]");
  const errEl = byId("login-error");
  if (btn.disabled) return;
  const raw = new FormData(form);
  const body = {
    username: String(raw.get("username") || "").trim(),
    password: String(raw.get("password") || ""),
  };
  form.elements.password.value = "";
  btn.disabled = true;
  btn.textContent = t("logging_in");
  errEl.hidden = true;
  topMark.setState("connecting");
  try {
    await api("login", body);
    try {
      await admin.refresh({ throwOnError: true });
    } catch (err) {
      if (err?.status === 401) throw new Error(t("cookie_blocked"));
      throw err;
    }
  } catch (err) {
    topMark.setState("idle");
    errEl.textContent = err instanceof Error ? err.message : failure(err);
    errEl.hidden = false;
  } finally {
    btn.disabled = false;
    btn.textContent = t("login");
  }
});

syncCopy();
render();
void admin.refresh();
