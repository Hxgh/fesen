import { createIconButton } from "./icons.js";

function el(tag, cls = "", ...children) {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  node.append(...children);
  return node;
}

/**
 * Shared Settings modal (Appearance + Language + optional tabs/sections + version footer).
 * Instant-apply, unified 216px equal-width segmented controls, focuses dialog itself to prevent close-button focus ring.
 * @param {HTMLDialogElement} dialog
 * @param {{
 *   t: (k: string) => string,
 *   theme: { get: () => string, set: (v: string) => void },
 *   scale?: { get: () => string, set: (v: string) => void },
 *   i18n: { getMode?: () => string, getLocale: () => string, setLocale: (v: string) => void },
 *   onLocaleChange?: () => void,
 *   tabs?: { id: string, label: string }[],
 *   initialTab?: string,
 *   showGeneral?: (tabId: string) => boolean,
 *   extraBody?: (redraw: () => void, tabId: string) => HTMLElement[],
 *   footerText?: () => string,
 *   footerLeft?: (redraw: () => void) => HTMLElement | null,
 *   footerRight?: (redraw: () => void) => HTMLElement | null,
 * }} opts
 */
export function openSettingsModal(dialog, opts) {
  const { t, theme, scale, i18n } = opts;
  let activeTab = opts.initialTab || opts.tabs?.[0]?.id || "general";

  function draw() {
    dialog.replaceChildren();
    const title = el("h2", "", t("settings"));
    title.id = "settings-heading";
    const closeBtn = createIconButton({
      icon: "close",
      title: t("close"),
      variant: "ghost",
      size: "md",
      onClick: () => dialog.close(),
    });

    const tabsList = opts.tabs?.length ? opts.tabs : null;
    let headerLeft;
    if (tabsList) {
      const seg = el("div", "segmented settings-head-tabs");
      for (const tab of tabsList) {
        const btn = el("button", tab.id === activeTab ? "active" : "", tab.label);
        btn.type = "button";
        btn.setAttribute("aria-pressed", String(tab.id === activeTab));
        btn.addEventListener("click", () => {
          if (activeTab === tab.id) return;
          activeTab = tab.id;
          draw();
        });
        seg.append(btn);
      }
      headerLeft = el("div", "settings-head-left", title, seg);
    } else {
      headerLeft = title;
    }

    const header = el("div", "dialog-header", headerLeft, closeBtn);
    const body = el("div", "dialog-body");
    const includeGeneral = opts.showGeneral ? opts.showGeneral(activeTab) : true;

    if (includeGeneral) {
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

      if (scale) {
        const curScale = scale.get();
        const scaleSeg = el("div", "segmented");
        for (const [id, key] of [["sm", "scale_sm"], ["md", "scale_md"], ["lg", "scale_lg"]]) {
          const btn = el("button", id === curScale ? "active" : "", t(key));
          btn.type = "button";
          btn.setAttribute("aria-pressed", String(id === curScale));
          btn.addEventListener("click", () => { scale.set(id); draw(); });
          scaleSeg.append(btn);
        }
        body.append(el("div", "settings-row", el("span", "settings-label", t("ui_scale")), scaleSeg));
      }
    }

    if (opts.extraBody) {
      body.append(...opts.extraBody(draw, activeTab));
    }

    dialog.append(header, body);
    const left = opts.footerLeft?.(draw) ?? null;
    const foot = opts.footerText?.();
    const right = opts.footerRight?.(draw) ?? null;
    if (left || foot || right) {
      const footerEl = el("div", "settings-footer");
      if (left) footerEl.append(left);
      else if (foot) footerEl.append(el("span", "settings-version", foot));
      if (right) footerEl.append(right);
      dialog.append(footerEl);
    }
  }

  dialog.tabIndex = -1;
  draw();
  if (!dialog.open) {
    dialog.showModal();
    dialog.focus();
  }
}
