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
  unlink: "m18.84 12.25 1.72-1.71a4.5 4.5 0 0 0-6.36-6.36l-1.72 1.71M5.17 11.75l-1.72 1.71a4.5 4.5 0 1 0 6.36 6.36l1.71-1.71M8 2v3M2 8h3M16 19v3M19 16h3",
  edit: "M15.8 5.2a2.1 2.1 0 0 1 3 3L8.5 18.5 4.5 19.5l1-4Z",
  settings: "M10.325 4.317c.426-1.756 2.924-1.756 3.35 0a1.724 1.724 0 0 0 2.573 1.066c1.543-.94 3.31.826 2.37 2.37a1.724 1.724 0 0 0 1.065 2.572c1.756.426 1.756 2.924 0 3.35a1.724 1.724 0 0 0-1.066 2.573c.94 1.543-.826 3.31-2.37 2.37a1.724 1.724 0 0 0-2.572 1.065c-.426 1.756-2.924 1.756-3.35 0a1.724 1.724 0 0 0-2.573-1.066c-1.543.94-3.31-.826-2.37-2.37a1.724 1.724 0 0 0-1.065-2.572c-1.756-.426-1.756-2.924 0-3.35a1.724 1.724 0 0 0 1.066-2.573c-.94-1.543.826-3.31 2.37-2.37c1 .608 2.296.07 2.572-1.065Z M12 15a3 3 0 1 0 0-6a3 3 0 0 0 0 6Z",
  sun: "M12 3v1m0 16v1m9-9h-1M4 12H3m15.364 6.364-.707-.707M6.343 6.343l-.707-.707m12.728 0-.707.707M6.343 17.657l-.707.707M16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0Z",
  moon: "M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z",
  monitor: "M4 4.5h16a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1v-10a1 1 0 0 1 1-1ZM8 20h8m-4-3.5V20",
  shield: "M12 3s7 3 7 8c0 5-7 10-7 10S5 16 5 11c0-5 7-8 7-8Z",
  check: "m5 13 4 4L19 7",
  alert: "m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3ZM12 9v4m0 4h.01",
  close: "M17.5 6.5 6.5 17.5M6.5 6.5l11 11",
  maximize: "M15 4.5h4.5V9M9 19.5H4.5V15M19.5 4.5 14 10M4.5 19.5 10 14",
  minimize: "M14 10h4.5V5.5M10 14H5.5v4.5M18.5 5.5 14 10M5.5 18.5 10 14",
};

/** @param {keyof typeof paths | "more"} name */
export function icon(name) {
  const svg = document.createElementNS(NS, "svg");
  for (const [key, value] of Object.entries({
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    "stroke-width": "1.6",
    "stroke-linecap": "round",
    "stroke-linejoin": "round",
    "aria-hidden": "true",
    class: "icon",
  })) svg.setAttribute(key, value);

  if (name === "more") {
    svg.setAttribute("fill", "currentColor");
    svg.setAttribute("stroke", "none");
    for (const cx of [6.5, 12, 17.5]) {
      const c = document.createElementNS(NS, "circle");
      c.setAttribute("cx", String(cx));
      c.setAttribute("cy", "12");
      c.setAttribute("r", "1.35");
      svg.append(c);
    }
    return svg;
  }

  const path = document.createElementNS(NS, "path");
  path.setAttribute("d", paths[name] || paths.server);
  svg.append(path);
  return svg;
}

const TRIGRAM_PATHS = [
  [0b001, 0b101],
  [0b010, 0b110],
  [0b001, 0b011, 0b111, 0b110],
  [0b010, 0b011, 0b111, 0b101],
];

const YAO_ROWS = [
  { cls: "yao-top", bit: 4, y: 6 },
  { cls: "yao-mid", bit: 2, y: 26 },
  { cls: "yao-bot", bit: 1, y: 46 },
];

function buildTrigramSvg(mask, state) {
  const svg = document.createElementNS(NS, "svg");
  for (const [key, value] of Object.entries({
    viewBox: "0 0 64 64",
    fill: "currentColor",
    "aria-hidden": "true",
    class: "brand-mark",
    "data-state": state,
    "data-mask": String(mask),
  })) svg.setAttribute(key, value);

  for (const row of YAO_ROWS) {
    const g = document.createElementNS(NS, "g");
    g.setAttribute("class", row.cls);
    g.setAttribute("data-yang", (mask & row.bit) ? "1" : "0");
    for (const [x, w, cls] of [[4, 24, "yao-side"], [36, 24, "yao-side"], [27.5, 9, "yao-bridge"]]) {
      const rect = document.createElementNS(NS, "rect");
      rect.setAttribute("x", String(x));
      rect.setAttribute("y", String(row.y));
      rect.setAttribute("width", String(w));
      rect.setAttribute("height", "12");
      rect.setAttribute("class", cls);
      g.append(rect);
    }
    svg.append(g);
  }
  return svg;
}

function applyTrigramMask(svg, mask) {
  svg.setAttribute("data-mask", String(mask));
  for (const row of YAO_ROWS) {
    const g = svg.querySelector(`.${row.cls}`);
    if (g) g.setAttribute("data-yang", (mask & row.bit) ? "1" : "0");
  }
}

function shortestSingleYaoPath(from, to) {
  const steps = [];
  let cur = from;
  // Close top yang first when resolving into Gen (0b100), or drop extra bits cleanly one yao at a time.
  const order = [4, 1, 2];
  for (const bit of order) {
    if ((cur & bit) !== (to & bit)) {
      cur ^= bit;
      steps.push(cur);
    }
  }
  return steps;
}

/**
 * Reusable Bagua state-machine brand mark component.
 * - idle: Kun ☷ (0b000, Death Gate / disconnected)
 * - connecting: orderly single-yao transitions along classical paths (with controlled randomness)
 * - active: Gen ☶ (0b100, Life Gate / connected), resolving smoothly via shortest single-yao steps
 * @param {"idle" | "connecting" | "active"} [initialState]
 */
export function createTrigram(initialState = "idle") {
  const initialMask = initialState === "active" ? 0b100 : initialState === "connecting" ? 0b001 : 0b000;
  const el = buildTrigramSvg(initialMask, initialState);
  let state = initialState;
  let mask = initialMask;
  let prevMask = -1;
  let timer = 0;
  let queue = [];
  let lastPathIdx = -1;

  const reducedMotion = () =>
    typeof window !== "undefined" && window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;

  function clearTimer() {
    if (timer) {
      window.clearTimeout(timer);
      timer = 0;
    }
  }

  function setMask(next) {
    prevMask = mask;
    mask = next;
    applyTrigramMask(el, mask);
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

  function stepConnecting() {
    if (state !== "connecting") return;
    const next = queue.length ? queue.shift() : nextWalkMask();
    setMask(next);
    timer = window.setTimeout(stepConnecting, 340);
  }

  function stepClosing() {
    if (state !== "active") return;
    if (!queue.length) {
      setMask(0b100);
      el.setAttribute("data-state", "active");
      return;
    }
    const next = queue.shift();
    setMask(next);
    if (!queue.length) {
      el.setAttribute("data-state", "active");
    } else {
      timer = window.setTimeout(stepClosing, 150);
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
      setMask(nextState === "active" ? 0b100 : nextState === "connecting" ? 0b010 : 0b000);
      return;
    }

    if (nextState === "idle") {
      el.setAttribute("data-state", "idle");
      setMask(0b000);
      return;
    }

    if (nextState === "connecting") {
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
      // Fast connect skipped connecting state: play a crisp 2-step classical resolution into ☶.
      const quick = Math.random() < 0.5 ? [0b001, 0b101, 0b100] : [0b010, 0b110, 0b100];
      el.setAttribute("data-state", "connecting");
      queue = quick;
      timer = window.setTimeout(stepClosing, 40);
      return;
    }

    const closing = shortestSingleYaoPath(mask, 0b100);
    if (!closing.length) {
      el.setAttribute("data-state", "active");
      setMask(0b100);
      return;
    }
    el.setAttribute("data-state", "connecting");
    queue = closing;
    timer = window.setTimeout(stepClosing, 120);
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
  return buildTrigramSvg(mask, state === "ready" ? "active" : state);
}
