import { createTrigram, icon } from "./icons.js";
import { createTheme } from "./theme.js";
import { createI18n } from "./i18n.js";
import { openSettingsModal } from "./settings.js";
import { createAdminWorkspace } from "./admin-view.js";

const byId = (id) => document.getElementById(id);
const theme = createTheme("fesen_admin_theme");
const i18n = createI18n("fesen_admin_locale");
const { t, isZh, failure } = i18n;

const topMark = createTrigram("connecting");
byId("brand-mark").replaceWith(topMark.el);

const navTabs = byId("nav-tabs");
const actions = byId("actions");
const content = byId("content");
const signin = byId("signin");
const loadingEl = byId("loading");
const brandWord = byId("brand-word");
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

function syncCopy() {
  document.title = t("admin_title");
  document.querySelectorAll("[data-copy]").forEach((el) => { el.textContent = t(el.dataset.copy); });
  brandBadge.title = [t("brand"), t("admin_badge"), currentHost].filter(Boolean).join(" · ");
}

function render() {
  syncCopy();
  loadingEl.hidden = booted;
  signin.hidden = !booted || signedIn;
  content.hidden = !signedIn;
  navTabs.hidden = !signedIn;
  brandWord.hidden = signedIn;

  actions.replaceChildren();
  if (signedIn) {
    actions.append(...admin.buildToolbarButtons());
    admin.renderTabs(navTabs);
    const viewKey = `admin:${admin.getTabIndex()}`;
    const panel = admin.renderPanel();
    if (viewKey !== lastViewKey) {
      panel.classList.add("panel-enter");
      lastViewKey = viewKey;
    }
    content.replaceChildren(panel);
  } else {
    lastViewKey = "";
    navTabs.replaceChildren();
    content.replaceChildren();
  }
  const settingsBtn = document.createElement("button");
  settingsBtn.type = "button";
  settingsBtn.className = "tool-btn icon-only";
  settingsBtn.title = t("settings");
  settingsBtn.setAttribute("aria-label", t("settings"));
  settingsBtn.setAttribute("aria-haspopup", "dialog");
  settingsBtn.append(icon("settings"));
  settingsBtn.addEventListener("click", openSettings);
  actions.append(settingsBtn);
}

admin.onUpdate(render);
i18n.onChange(() => { syncCopy(); render(); });

byId("login").addEventListener("submit", async (e) => {
  e.preventDefault();
  const form = e.currentTarget;
  const btn = form.querySelector("[type=submit]");
  const errEl = byId("login-error");
  if (btn.disabled) return;
  const body = Object.fromEntries(new FormData(form));
  form.elements.password.value = "";
  btn.disabled = true;
  errEl.hidden = true;
  topMark.setState("connecting");
  try {
    await api("login", body);
    await admin.refresh();
  } catch (err) {
    topMark.setState("idle");
    errEl.textContent = err instanceof Error ? err.message : failure(err);
    errEl.hidden = false;
  } finally {
    btn.disabled = false;
  }
});

syncCopy();
render();
void admin.refresh();
