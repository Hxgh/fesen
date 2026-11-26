import { t } from "../i18n";
import { createIconButton, icon } from "../../../../packages/ui/icons.js";
import type { IconName, ServiceKind } from "../model/types";

export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  classes = "",
  ...children: (Node | string)[]
): HTMLElementTagNameMap[K] {
  const element = document.createElement(tag);
  element.className = classes;
  element.append(...children);
  return element;
}

export function button(text: string, action: () => void, style = "button") {
  const b = el("button", style, text);
  b.type = "button";
  b.addEventListener("click", action);
  return b;
}

export function tool(name: IconName, title: string, action: () => void) {
  return createIconButton({
    icon: name,
    title,
    variant: "ghost",
    size: "md",
    onClick: action,
  });
}

export function empty(text: string, ...controls: HTMLElement[]) {
  return el(
    "div",
    "empty",
    el("p", "", text),
    ...(controls.length ? [el("div", "empty-actions", ...controls)] : []),
  );
}

export function field(title: string, name: string, value = "", options: Partial<HTMLInputElement> = {}) {
  const input = el("input");
  Object.assign(input, { name, value, maxLength: 256, spellcheck: false }, options);
  return el("label", "", title, input);
}

export const str = (data: FormData, name: string) => String(data.get(name) ?? "");
export const submit = (text: string, tone = "primary") => el("button", `button ${tone}`, text);

export function dialogHeader(title: string, onClose: () => void) {
  const heading = el("h2", "", title);
  heading.id = "editor-title";
  const closeBtn = tool("close", t("close"), onClose);
  return el("div", "dialog-header", heading, closeBtn);
}

export function protocolSelect(
  selected: ServiceKind,
  cls = "kind-select",
  onChange?: (k: ServiceKind) => void,
) {
  const select = el("select", cls);
  select.name = "kind";
  const options: [ServiceKind, Parameters<typeof t>[0]][] = [
    ["tcp", "kind_tcp"],
    ["http", "kind_http"],
    ["https", "kind_https"],
    ["ssh", "kind_ssh"],
    ["rdp", "kind_rdp"],
    ["smb", "kind_smb"],
  ];
  for (const [value, key] of options) {
    const opt = el("option", "", t(key));
    opt.value = value;
    opt.selected = value === selected;
    select.append(opt);
  }
  if (onChange) select.addEventListener("change", () => onChange(select.value as ServiceKind));
  return select;
}

export function makeToggle(
  checked: boolean,
  label: string,
  onOffline: (() => void) | null,
  onToggle: () => void,
  fraction?: string,
) {
  const sw = button("", () => {
    if (onOffline) { onOffline(); return; }
    onToggle();
  }, "switch");
  if (fraction) sw.append(el("span", "switch-fraction", fraction));
  const track = el("span", "switch-track");
  track.setAttribute("aria-hidden", "true");
  sw.append(track);
  sw.setAttribute("role", "switch");
  sw.setAttribute("aria-label", label);
  sw.setAttribute("aria-checked", String(checked));
  if (fraction) sw.setAttribute("data-partial", "true");
  if (onOffline) {
    sw.setAttribute("data-offline", "true");
    sw.title = t("connect_prompt_title");
  }
  return sw;
}

export function collapseToggle(
  key: string,
  expanded: boolean,
  cls: string,
  onToggle: (key: string) => void,
  ...children: (Node | string)[]
) {
  const chev = icon("chevron");
  chev.classList.add("collapse-chevron");
  const b = button("", () => onToggle(key), `collapse-toggle ${cls}`.trim());
  b.dataset.collapseKey = key;
  b.setAttribute("aria-expanded", String(expanded));
  b.append(chev, ...children);
  return b;
}

const toast = el("p", "toast");
toast.setAttribute("role", "status");
document.body.append(toast);
let toastTimer: number | undefined;

export function notify(text: string, error = false) {
  window.clearTimeout(toastTimer);
  toast.classList.toggle("problem", error);
  toast.textContent = text;
  toastTimer = window.setTimeout(() => { toast.textContent = ""; }, 3200);
}
