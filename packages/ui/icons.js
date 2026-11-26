const NS = "http://www.w3.org/2000/svg";

const paths = {
  globe: "M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0ZM3.5 12h17M12 3c2.1 2.5 3.2 5.5 3.2 9S14.1 18.5 12 21c-2.1-2.5-3.2-5.5-3.2-9S9.9 5.5 12 3Z",
  server: "M5 4.5h14a1 1 0 0 1 1 1v5H4v-5a1 1 0 0 1 1-1ZM4 14h16v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1v-5Zm3-6.5h2m-2 9.5h2",
  terminal: "M4 5.5h16a1 1 0 0 1 1 1v11a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1v-11a1 1 0 0 1 1-1ZM8 10l3 2.5L8 15m5 0h3",
  folder: "M3.5 6.5a1 1 0 0 1 1-1h4.6l1.8 2h8.6a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1h-15a1 1 0 0 1-1-1v-11Z",
  user: "M16 8a4 4 0 1 1-8 0 4 4 0 0 1 8 0ZM4.5 20c.6-3.6 3.1-5.5 7.5-5.5s6.9 1.9 7.5 5.5",
  device: "M3.5 4.5h17v12h-17zM8 20h8m-4-3.5V20",
  sliders: "M4 7h6m4 0h6M4 17h4m4 0h8M14 7a2 2 0 1 1-4 0 2 2 0 0 1 4 0ZM12 17a2 2 0 1 1-4 0 2 2 0 0 1 4 0Z",
  refresh: "M19.5 5.5v4.5H15M4.5 18.5V14H9m10.1-4.8A7.2 7.2 0 0 0 6.2 7m-1.3 7.8A7.2 7.2 0 0 0 17.8 17",
  plus: "M12 6v12M6 12h12",
  chevron: "m7.5 10 4.5 4.5L16.5 10",
  copy: "M8.5 8.5h11v11h-11zM5.5 15.5h-1v-11h11v1",
  external: "M14 4.5h5.5V10m0-5.5L10.5 13.5M10 5H5.5a1 1 0 0 0-1 1v12.5a1 1 0 0 0 1 1H18a1 1 0 0 0 1-1V14",
  link: "M10 13a4.5 4.5 0 0 0 6.36.54l2.5-2.5a4.5 4.5 0 0 0-6.36-6.36l-1.43 1.43M14 11a4.5 4.5 0 0 0-6.36-.54l-2.5 2.5a4.5 4.5 0 0 0 6.36 6.36l1.42-1.42",
  share: "M20.5 5.5a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0ZM8.5 12a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0ZM20.5 18.5a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0ZM8.2 10.8l7.6-4.1M8.2 13.2l7.6 4.1",
  unlink: "m18.84 12.25 1.72-1.71a4.5 4.5 0 0 0-6.36-6.36l-1.72 1.71M5.17 11.75l-1.72 1.71a4.5 4.5 0 1 0 6.36 6.36l1.71-1.71M8 2v3M2 8h3M16 19v3M19 16h3",
  edit: "M15.8 5.2a2.1 2.1 0 0 1 3 3L8.5 18.5 4.5 19.5l1-4Z",
  settings: "M10.325 4.317c.426-1.756 2.924-1.756 3.35 0a1.724 1.724 0 0 0 2.573 1.066c1.543-.94 3.31.826 2.37 2.37a1.724 1.724 0 0 0 1.065 2.572c1.756.426 1.756 2.924 0 3.35a1.724 1.724 0 0 0-1.066 2.573c.94 1.543-.826 3.31-2.37 2.37a1.724 1.724 0 0 0-2.572 1.065c-.426 1.756-2.924 1.756-3.35 0a1.724 1.724 0 0 0-2.573-1.066c-1.543.94-3.31-.826-2.37-2.37a1.724 1.724 0 0 0-1.065-2.572c-1.756-.426-1.756-2.924 0-3.35a1.724 1.724 0 0 0 1.066-2.573c-.94-1.543.826-3.31 2.37-2.37c1 .608 2.296.07 2.572-1.065Z M12 15a3 3 0 1 0 0-6a3 3 0 0 0 0 6Z",
  sun: "M12 3v1m0 16v1m9-9h-1M4 12H3m15.364 6.364-.707-.707M6.343 6.343l-.707-.707m12.728 0-.707.707M6.343 17.657l-.707.707M16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0Z",
  moon: "M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z",
  monitor: "M4 4.5h16a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1v-10a1 1 0 0 1 1-1ZM8 20h8m-4-3.5V20",
  shield: "M12 3.5 5 6.5v5c0 4.6 3 7.8 7 9.3 4-1.5 7-4.7 7-9.3v-5l-7-3Z",
  console: "M4 4.5h6.5a1 1 0 0 1 1 1v13a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1v-13a1 1 0 0 1 1-1ZM14.5 4.5H20a1 1 0 0 1 1 1V10a1 1 0 0 1-1 1h-5.5a1 1 0 0 1-1-1V5.5a1 1 0 0 1 1-1ZM14.5 14H20a1 1 0 0 1 1 1v3.5a1 1 0 0 1-1 1h-5.5a1 1 0 0 1-1-1V15a1 1 0 0 1 1-1Z",
  home: "M4.5 10.5 12 4l7.5 6.5V19a1 1 0 0 1-1 1H14v-5h-4v5H5.5a1 1 0 0 1-1-1v-8.5Z",
  check: "m5 13 4 4L19 7",
  alert: "m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3ZM12 9v4m0 4h.01",
  close: "M17.5 6.5 6.5 17.5M6.5 6.5l11 11",
  maximize: "M15 4.5h4.5V9M9 19.5H4.5V15M19.5 4.5 14 10M4.5 19.5 10 14",
  minimize: "M14 10h4.5V5.5M10 14H5.5v4.5M18.5 5.5 14 10M5.5 18.5 10 14",
  search: "M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16Zm10 2-4.35-4.35",
  list: "M8.5 6.5h11M8.5 12h11M8.5 17.5h11M4.5 6.5h.01M4.5 12h.01M4.5 17.5h.01",
  bars: "M4.5 7h15M4.5 12h15M4.5 17h15",
  card: "M4 5.5h16a1 1 0 0 1 1 1v11a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1v-11a1 1 0 0 1 1-1ZM3 10.5h18",
  cards: "M4 4.5h16a1 1 0 0 1 1 1v4.5a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V5.5a1 1 0 0 1 1-1ZM4 14h16a1 1 0 0 1 1 1v3.5a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V15a1 1 0 0 1 1-1Z",
  layers: "M12 3.5 3.5 8 12 12.5 20.5 8 12 3.5ZM3.5 12.2 12 16.7l8.5-4.5M3.5 16.3 12 20.8l8.5-4.5",
  collapse: "M5 12h14M8.5 5.5 12 9l3.5-3.5M8.5 18.5 12 15l3.5 3.5",
  expand: "m7.5 9.5 4.5-4.5 4.5 4.5M7.5 14.5 12 19l4.5-4.5",
};

const osPaths = {
  windows: "M2.5 2.5h8.8v8.8H2.5zm10.2 0h8.8v8.8h-8.8zm-10.2 10.2h8.8v8.8H2.5zm10.2 0h8.8v8.8h-8.8z",
  macos: "M14.95 5.45c.8-1 1.35-2.35 1.2-3.7-1.15.05-2.55.77-3.35 1.75-.72.84-1.35 2.2-1.18 3.5 1.3.1 2.55-.65 3.33-1.55ZM12 8.2C10.5 8.2 9.7 7.4 8.2 7.4 5.2 7.4 3.8 10.1 3.8 13c0 4.5 2.6 9.1 5 9.1 1.4 0 2-.9 3.2-.9s1.8.9 3.2.9c1.9 0 4-2.9 4.8-5.2-.2-.1-2.9-1.2-2.9-4.1 0-2.6 2.1-3.8 2.2-3.9C18.1 7.3 16.7 7.4 15.8 7.4 14.3 7.4 13.5 8.2 12 8.2Z",
  android: "M6.6 8.3 4.6 4.8a.9.9 0 1 1 1.55-.9l2.05 3.55A9.6 9.6 0 0 1 12 6.7c1.35 0 2.65.27 3.8.75l2.05-3.55a.9.9 0 1 1 1.55.9l-2 3.5A9.2 9.2 0 0 1 21.5 16v2.3a1.2 1.2 0 0 1-1.2 1.2H3.7a1.2 1.2 0 0 1-1.2-1.2V16c0-3.25 1.6-6.1 4.1-7.7ZM8.3 13a1.3 1.3 0 1 0 0 2.6 1.3 1.3 0 0 0 0-2.6Zm7.4 0a1.3 1.3 0 1 0 0 2.6 1.3 1.3 0 0 0 0-2.6Z",
  linux: "M12 2c-2.85 0-4.7 1.85-4.7 4.75 0 1.3-.55 2.4-1.65 3.9-1.1 1.65-1.85 3.35-1.85 5.2 0 2.25 1.2 4 3.05 4.95-.32.45-.32 1.2.32 1.52.88.44 2.3.32 3.28-.1.54.1.98.1 1.55.1s1.01 0 1.55-.1c.98.42 2.4.54 3.28.1.64-.32.64-1.07.32-1.52 1.85-.95 3.05-2.7 3.05-4.95 0-1.85-.75-3.55-1.85-5.2-1.1-1.5-1.65-2.6-1.65-3.9C16.7 3.85 14.85 2 12 2Zm-1.85 3.3a1 1 0 1 1 0 2 1 1 0 0 1 0-2Zm3.7 0a1 1 0 1 1 0 2 1 1 0 0 1 0-2ZM10.25 7.7 12 9.25l1.75-1.55-1.75-.85-1.75.85ZM12 10.75c-2.4 0-3.9 2.25-3.9 5.05 0 2.6 1.5 4.25 3.9 4.25s3.9-1.65 3.9-4.25c0-2.8-1.5-5.05-3.9-5.05Z",
};

const osTitles = {
  windows: "Windows",
  macos: "macOS",
  linux: "Linux",
  android: "Android",
};

const osIconCache = new Map();
const iconCache = new Map();

/**
 * Monochrome operating system icon SVG.
 * @param {"windows" | "macos" | "linux" | "android"} os
 */
export function osIcon(os) {
  const key = osPaths[os] ? os : "linux";
  let proto = osIconCache.get(key);
  if (!proto) {
    proto = document.createElementNS(NS, "svg");
    proto.setAttribute("viewBox", "0 0 24 24");
    proto.setAttribute("fill", "currentColor");
    proto.setAttribute("aria-hidden", "true");
    proto.setAttribute("class", "icon os-icon");
    const path = document.createElementNS(NS, "path");
    path.setAttribute("d", osPaths[key]);
    if (key === "linux" || key === "android") path.setAttribute("fill-rule", "evenodd");
    proto.append(path);
    osIconCache.set(key, proto);
  }
  return /** @type {SVGSVGElement} */ (proto.cloneNode(true));
}

/**
 * Encapsulated OS icon badge with optional fallback glyph, local theme tint, and hover details.
 * @param {"windows" | "macos" | "linux" | "android" | null | undefined} os
 * @param {"server" | "device" | false | {
 *   fallback?: "server" | "device" | false,
 *   local?: boolean,
 *   prefix?: string,
 *   fallbackTitle?: string,
 * }} [opts]
 * @param {string} [fallbackTitle]
 */
export function osBadge(os, opts = false, fallbackTitle = "") {
  const cfg = typeof opts === "object" && opts !== null
    ? opts
    : { fallback: opts, fallbackTitle };
  const fallback = cfg.fallback ?? false;
  const local = !!cfg.local;
  const prefix = cfg.prefix ? String(cfg.prefix).trim() : "";
  const fbTitle = cfg.fallbackTitle ?? fallbackTitle;

  if (os && osTitles[os]) {
    const tag = document.createElement("span");
    tag.className = local ? "host-os-tag is-local" : "host-os-tag";
    const label = prefix ? `${prefix} · ${osTitles[os]}` : osTitles[os];
    tag.title = label;
    tag.setAttribute("aria-label", label);
    tag.append(osIcon(os));
    return tag;
  }
  if (!fallback) return null;
  const tag = document.createElement("span");
  tag.className = local ? "host-os-tag is-fallback is-local" : "host-os-tag is-fallback";
  const label = prefix || fbTitle;
  if (label) {
    tag.title = label;
    tag.setAttribute("aria-label", label);
  } else {
    tag.setAttribute("aria-hidden", "true");
  }
  const svg = icon(fallback === "device" ? "device" : "server");
  svg.classList.add("os-icon");
  tag.append(svg);
  return tag;
}

/** @param {keyof typeof paths | "more"} name */
export function icon(name) {
  const key = name === "more" || paths[name] ? name : "server";
  let proto = iconCache.get(key);
  if (!proto) {
    proto = document.createElementNS(NS, "svg");
    proto.setAttribute("viewBox", "0 0 24 24");
    proto.setAttribute("aria-hidden", "true");
    proto.setAttribute("class", "icon");
    if (key === "more") {
      proto.setAttribute("fill", "currentColor");
      proto.setAttribute("stroke", "none");
      for (const cx of ["6.5", "12", "17.5"]) {
        const c = document.createElementNS(NS, "circle");
        c.setAttribute("cx", cx);
        c.setAttribute("cy", "12");
        c.setAttribute("r", "1.35");
        proto.append(c);
      }
    } else {
      proto.setAttribute("fill", "none");
      proto.setAttribute("stroke", "currentColor");
      proto.setAttribute("stroke-width", "1.6");
      proto.setAttribute("stroke-linecap", "round");
      proto.setAttribute("stroke-linejoin", "round");
      const path = document.createElementNS(NS, "path");
      path.setAttribute("d", paths[key]);
      proto.append(path);
    }
    iconCache.set(key, proto);
  }
  return /** @type {SVGSVGElement} */ (proto.cloneNode(true));
}

/**
 * Update an icon button's icon(s) and accessible tooltip/label.
 * @param {HTMLButtonElement} btn
 * @param {keyof typeof paths | "more"} name
 * @param {string} title
 * @param {boolean} [dropdown]
 */
export function setIconButton(btn, name, title, dropdown = false) {
  const nodes = [icon(name)];
  if (dropdown) {
    const caret = icon("chevron");
    caret.classList.add("dropdown-caret");
    nodes.push(caret);
  }
  btn.replaceChildren(...nodes);
  btn.title = title;
  btn.setAttribute("aria-label", title);
}

/**
 * Unified icon button component shared across all Fesen surfaces.
 * @param {{
 *   icon: keyof typeof paths | "more",
 *   title: string,
 *   label?: string,
 *   variant?: "solid" | "ghost",
 *   size?: "sm" | "md" | "lg",
 *   tone?: "default" | "accent" | "muted",
 *   dropdown?: boolean,
 *   active?: boolean,
 *   spinning?: boolean,
 *   className?: string,
 *   onClick?: (event: MouseEvent) => void,
 * }} opts
 */
export function createIconButton(opts) {
  const btn = document.createElement("button");
  btn.type = "button";
  const variant = opts.variant === "solid" ? "solid" : "ghost";
  const size = opts.size === "lg" ? "lg" : opts.size === "sm" ? "sm" : "md";
  const classes = ["icon-btn", variant, size];
  if (opts.tone === "accent") classes.push("accent");
  else if (opts.tone === "muted") classes.push("muted");
  if (opts.dropdown) classes.push("dropdown");
  if (opts.active) classes.push("is-active");
  if (opts.spinning) classes.push("spinning");
  if (opts.className) classes.push(opts.className);
  btn.className = classes.join(" ");
  setIconButton(btn, opts.icon, opts.title, !!opts.dropdown);
  if (opts.label) btn.setAttribute("aria-label", opts.label);
  if (opts.dropdown) {
    btn.setAttribute("aria-haspopup", "menu");
    btn.setAttribute("aria-expanded", "false");
  }
  if (opts.onClick) btn.addEventListener("click", opts.onClick);
  return btn;
}

const TRIGRAM_PATHS = [
  [0b001, 0b101],
  [0b010, 0b110],
  [0b001, 0b011, 0b111, 0b110],
  [0b010, 0b011, 0b111, 0b101],
  [0b001, 0b011, 0b010, 0b110],
  [0b010, 0b110, 0b111, 0b101],
];

const YAO_ROWS = [
  { cls: "yao-top", bit: 4, y: 6 },
  { cls: "yao-mid", bit: 2, y: 26 },
  { cls: "yao-bot", bit: 1, y: 46 },
];

function buildTrigramSvg(mask, state) {
  const svg = document.createElementNS(NS, "svg");
  svg.setAttribute("viewBox", "0 0 64 64");
  svg.setAttribute("fill", "currentColor");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("class", "brand-mark");
  svg.setAttribute("data-state", state);
  svg.setAttribute("data-mask", String(mask));

  const groups = [];
  for (const row of YAO_ROWS) {
    const g = document.createElementNS(NS, "g");
    g.setAttribute("class", row.cls);
    g.setAttribute("data-yang", (mask & row.bit) ? "1" : "0");
    g.setAttribute("data-hot", "0");
    for (const [x, cls] of [["4", "yao-l"], ["32", "yao-r"]]) {
      const rect = document.createElementNS(NS, "rect");
      rect.setAttribute("x", x);
      rect.setAttribute("y", String(row.y));
      rect.setAttribute("width", "28");
      rect.setAttribute("height", "12");
      rect.setAttribute("class", cls);
      g.append(rect);
    }
    svg.append(g);
    groups.push(g);
  }
  return { svg, groups };
}

function applyTrigramMask(svg, groups, mask, changedBit = 0) {
  svg.setAttribute("data-mask", String(mask));
  for (let i = 0; i < YAO_ROWS.length; i++) {
    const g = groups[i];
    if (!g) continue;
    const bit = YAO_ROWS[i].bit;
    g.setAttribute("data-yang", (mask & bit) ? "1" : "0");
    g.setAttribute("data-hot", changedBit === 0 || (changedBit & bit) ? "1" : "0");
  }
}

function shortestSingleYaoPath(from, to) {
  const steps = [];
  let cur = from;
  const order = Math.random() < 0.5 ? [4, 1, 2] : [4, 2, 1];
  for (const bit of order) {
    if ((cur & bit) !== (to & bit)) {
      cur ^= bit;
      steps.push(cur);
    }
  }
  return steps;
}

const reducedMotionQuery = typeof window !== "undefined"
  ? window.matchMedia?.("(prefers-reduced-motion: reduce)")
  : null;

/**
 * Reusable Bagua state-machine brand mark component.
 * Uses organic 2-stroke ink fusion (left & right strokes extend/retract toward center)
 * + active-yao ink breath + calligraphic variable cadence.
 * - idle: Kun ☷ (0b000, Death Gate / disconnected)
 * - connecting: orderly single-yao transitions with organic rhythm
 * - active: Gen ☶ (0b100, Life Gate / connected)
 * @param {"idle" | "connecting" | "active"} [initialState]
 */
export function createTrigram(initialState = "idle") {
  const initialMask = initialState === "active" ? 0b100 : initialState === "connecting" ? 0b001 : 0b000;
  const { svg: el, groups } = buildTrigramSvg(initialMask, initialState);
  let state = initialState;
  let mask = initialMask;
  let prevMask = -1;
  let timer = 0;
  let queue = [];
  let lastPathIdx = -1;
  let stepIndex = 0;

  const reducedMotion = () => !!reducedMotionQuery?.matches;

  function clearTimer() {
    if (timer) {
      window.clearTimeout(timer);
      timer = 0;
    }
  }

  function setMask(next, highlightChanged = false) {
    const diff = highlightChanged ? (mask ^ next) : 0;
    prevMask = mask;
    mask = next;
    applyTrigramMask(el, groups, mask, diff);
  }

  function nextWalkMask() {
    const candidates = [1, 2, 4]
      .map(bit => mask ^ bit)
      .filter(m => m !== 0b100 && m !== 0b000 && m !== prevMask);
    if (!candidates.length) {
      return [1, 2, 4].map(bit => mask ^ bit).find(m => m !== 0b100) ?? 0b010;
    }
    return candidates[Math.floor(Math.random() * candidates.length)];
  }

  // Calligraphic variable rhythm (no robotic metronome).
  function nextDelay() {
    const base = [240, 320, 210, 360, 260][stepIndex % 5];
    stepIndex += 1;
    return base + Math.round((Math.random() - 0.5) * 70);
  }

  function stepConnecting() {
    if (state !== "connecting") return;
    const next = queue.length ? queue.shift() : nextWalkMask();
    setMask(next, true);
    timer = window.setTimeout(stepConnecting, nextDelay());
  }

  function stepClosing() {
    if (state !== "active") return;
    if (!queue.length) {
      setMask(0b100, false);
      el.setAttribute("data-state", "active");
      return;
    }
    const next = queue.shift();
    const isFinal = queue.length === 0;
    setMask(next, !isFinal);
    if (isFinal) {
      el.setAttribute("data-state", "active");
    } else {
      timer = window.setTimeout(stepClosing, queue.length === 1 ? 230 : 170);
    }
  }

  function pickPath() {
    let idx = Math.floor(Math.random() * TRIGRAM_PATHS.length);
    if (idx === lastPathIdx) idx = (idx + 1) % TRIGRAM_PATHS.length;
    lastPathIdx = idx;
    return [...TRIGRAM_PATHS[idx]];
  }

  /**
   * @param {"idle" | "connecting" | "active"} nextState
   * @param {boolean} [instant]
   */
  function setState(nextState, instant = false) {
    if (nextState === state && (!instant || mask === (nextState === "active" ? 0b100 : 0b000))) {
      return;
    }
    const prior = state;
    state = nextState;
    clearTimer();

    if (instant || reducedMotion()) {
      el.setAttribute("data-state", nextState);
      setMask(nextState === "active" ? 0b100 : nextState === "connecting" ? 0b010 : 0b000, false);
      return;
    }

    if (nextState === "idle") {
      el.setAttribute("data-state", "idle");
      setMask(0b000, false);
      return;
    }

    if (nextState === "connecting") {
      stepIndex = 0;
      el.setAttribute("data-state", "connecting");
      const path = pickPath();
      if (mask !== 0b000) {
        const prefix = shortestSingleYaoPath(mask, path[0]).filter(m => m !== 0b100);
        queue = [...prefix, ...path.slice(prefix.length ? 1 : 0)];
      } else {
        queue = path;
      }
      timer = window.setTimeout(stepConnecting, 60);
      return;
    }

    // nextState === "active": resolve cleanly into Life Gate ☶ (0b100).
    if (prior === "idle") {
      const quick = Math.random() < 0.5 ? [0b001, 0b101, 0b100] : [0b010, 0b110, 0b100];
      el.setAttribute("data-state", "connecting");
      queue = quick;
      timer = window.setTimeout(stepClosing, 50);
      return;
    }

    const closing = shortestSingleYaoPath(mask, 0b100);
    if (!closing.length) {
      el.setAttribute("data-state", "active");
      setMask(0b100, false);
      return;
    }
    el.setAttribute("data-state", "connecting");
    queue = closing;
    timer = window.setTimeout(stepClosing, 140);
  }

  return {
    el,
    setState,
    destroy: clearTimer,
  };
}

/**
 * Static Pure Gen (☶) or Kun (☷) trigram mark helper.
 * @param {"idle" | "connecting" | "ready" | "active"} [state]
 */
export function brandMark(state = "active") {
  const mask = state === "idle" ? 0b000 : 0b100;
  return buildTrigramSvg(mask, state === "ready" ? "active" : state).svg;
}
