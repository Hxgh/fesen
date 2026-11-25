import { icon } from "./icons.js";

function el(tag, cls = "", ...children) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  node.append(...children);
  return node;
}

/**
 * Shared Settings modal (Appearance + Language + optional extra rows/sections + version footer).
 * Instant-apply, unified 216px equal-width segmented controls, focuses dialog itself to prevent close-button focus ring.
 * @param {HTMLDialogElement} dialog
 * @param {{
 *   t: (k: string) => string,
 *   theme: { get: () => string, set: (v: string) => void },
 *   i18n: { getMode?: () => string, getLocale: () => string, setLocale: (v: string) => void },
 *   onLocaleChange?: () => void,
 *   extraBody?: (redraw: () => void) => HTMLElement[],
 *   footerText?: () => string,
 * }} opts
 */
export function openSettingsModal(dialog, opts) {
  const { t, theme, i18n } = opts;

  function draw() {
    dialog.replaceChildren();
    const title = el("h2", "", t("settings"));
    title.id = "settings-heading";
    const closeBtn = el("button", "button icon-button", icon("close"));
    closeBtn.type = "button";
    closeBtn.title = t("close");
    closeBtn.setAttribute("aria-label", t("close"));
    closeBtn.addEventListener("click", () => dialog.close());
    const header = el("div", "dialog-header", title, closeBtn);

    const body = el("div", "dialog-body");

    const themeSeg = el("div", "segmented");
    for (const [id, key] of [["system", "theme_system"], ["light", "theme_light"], ["dark", "theme_dark"]]) {
      const btn = el("button", id === theme.get() ? "active" : "", t(key));
      btn.type = "button";
      btn.setAttribute("aria-pressed", String(id === theme.get()));
      btn.addEventListener("click", () => { theme.set(id); draw(); });
      themeSeg.append(btn);
    }
    body.append(el("div", "settings-row", el("span", "settings-label", t("appearance")), themeSeg));

    const langMode = i18n.getMode ? i18n.getMode() : i18n.getLocale();
    const langSeg = el("div", "segmented");
    for (const [id, key] of [["auto", "lang_auto"], ["zh-CN", "lang_zh_short"], ["en", "lang_en_short"]]) {
      const btn = el("button", id === langMode ? "active" : "", t(key));
      btn.type = "button";
      btn.setAttribute("aria-pressed", String(id === langMode));
      btn.addEventListener("click", () => {
        i18n.setLocale(id);
        opts.onLocaleChange?.();
        draw();
      });
      langSeg.append(btn);
    }
    body.append(el("div", "settings-row", el("span", "settings-label", t("language")), langSeg));

    if (opts.extraBody) {
      body.append(...opts.extraBody(draw));
    }

    dialog.append(header, body);
    const foot = opts.footerText?.();
    if (foot) {
      dialog.append(el("div", "settings-footer", el("span", "settings-version", foot)));
    }
  }

  dialog.tabIndex = -1;
  draw();
  if (!dialog.open) {
    dialog.showModal();
    dialog.focus();
  }
}
