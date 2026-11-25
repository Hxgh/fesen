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
