/**
 * Shared theme controller (system | light | dark) for both Client and Web Admin.
 * @param {string} [storageKey]
 */
export function createTheme(storageKey = "fesen_theme") {
  function get() {
    try {
      const v = localStorage.getItem(storageKey);
      if (v === "light" || v === "dark" || v === "system") return v;
    } catch {}
    return "system";
  }

  function apply(theme = get()) {
    const root = document.documentElement;
    root.setAttribute("data-theme", theme);
    const dark = theme === "dark" || (theme === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
    root.classList.toggle("dark", dark);
  }

  function set(theme) {
    try { localStorage.setItem(storageKey, theme); } catch {}
    apply(theme);
  }

  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    if (get() === "system") apply("system");
  });

  apply(get());
  return { get, set, apply };
}

/**
 * Shared UI element scale controller (sm | md | lg) for Client and Web Admin.
 * @param {string} [storageKey]
 */
export function createScale(storageKey = "fesen_scale") {
  function get() {
    try {
      const v = localStorage.getItem(storageKey);
      if (v === "sm" || v === "md" || v === "lg") return v;
    } catch {}
    return "md";
  }

  function apply(scale = get()) {
    document.documentElement.setAttribute("data-scale", scale);
  }

  function set(scale) {
    const next = scale === "sm" || scale === "lg" ? scale : "md";
    try { localStorage.setItem(storageKey, next); } catch {}
    apply(next);
  }

  apply(get());
  return { get, set, apply };
}
