import { icon } from "./icons.js";

let active = null;

export function closePopoverMenu() {
  if (!active) return;
  active.cleanup();
  active.anchor.setAttribute("aria-expanded", "false");
  active.el.remove();
  active = null;
}

/**
 * Anchored dropdown popover menu (icon + label), shared across Client and Web Admin.
 * @param {HTMLButtonElement} anchor
 * @param {Array<{sep: true} | {icon: string, label: string, danger?: boolean, onSelect: () => void}>} entries
 */
export function openPopoverMenu(anchor, entries) {
  if (active?.anchor === anchor) {
    closePopoverMenu();
    return;
  }
  closePopoverMenu();

  const menu = document.createElement("div");
  menu.className = "popover-menu";
  menu.setAttribute("role", "menu");
  const buttons = [];

  for (const entry of entries) {
    if ("sep" in entry) {
      const sep = document.createElement("div");
      sep.className = "menu-sep";
      sep.setAttribute("role", "separator");
      menu.append(sep);
      continue;
    }
    const item = document.createElement("button");
    item.type = "button";
    item.className = `menu-item${entry.danger ? " danger" : ""}`;
    item.setAttribute("role", "menuitem");
    item.append(icon(entry.icon), entry.label);
    item.addEventListener("click", () => {
      closePopoverMenu();
      entry.onSelect();
    });
    buttons.push(item);
    menu.append(item);
  }

  document.body.append(menu);
  const rect = anchor.getBoundingClientRect();
  const mw = menu.offsetWidth;
  const mh = menu.offsetHeight;
  const left = Math.max(12, Math.min(rect.right - mw, window.innerWidth - mw - 12));
  const flipUp = rect.bottom + mh + 12 > window.innerHeight && rect.top - mh - 12 >= 0;
  const top = flipUp ? rect.top - mh - 6 : rect.bottom + 6;
  menu.style.left = `${Math.round(left)}px`;
  menu.style.top = `${Math.round(top)}px`;
  menu.setAttribute("data-origin", flipUp ? "bottom" : "top");
  anchor.setAttribute("aria-expanded", "true");

  const onPointerDown = (event) => {
    const target = event.target;
    if (target && (menu.contains(target) || anchor.contains(target))) return;
    closePopoverMenu();
  };
  const onKeyDown = (event) => {
    if (event.key === "Escape") {
      event.preventDefault();
      closePopoverMenu();
      anchor.focus();
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const cur = buttons.indexOf(document.activeElement);
      const next = event.key === "ArrowDown"
        ? (cur + 1) % buttons.length
        : (cur - 1 + buttons.length) % buttons.length;
      buttons[next]?.focus();
    }
  };
  const onDismiss = (event) => {
    if (event.type === "scroll" && event.target && menu.contains(event.target)) return;
    closePopoverMenu();
  };

  window.addEventListener("pointerdown", onPointerDown, true);
  window.addEventListener("keydown", onKeyDown, true);
  window.addEventListener("resize", onDismiss);
  window.addEventListener("scroll", onDismiss, true);

  active = {
    el: menu,
    anchor,
    cleanup: () => {
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("keydown", onKeyDown, true);
      window.removeEventListener("resize", onDismiss);
      window.removeEventListener("scroll", onDismiss, true);
    },
  };
}
