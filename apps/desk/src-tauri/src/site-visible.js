expected => {
  if (document.readyState !== "complete" || location.origin !== new URL(expected).origin) return "loading";
  const http = performance.getEntriesByType("navigation")[0]?.responseStatus;
  if (http >= 400) return "HTTP " + http;
  const body = document.body;
  if (!body?.getBoundingClientRect().height || getComputedStyle(body).visibility === "hidden") return "loading";
  const drawn = [...body.querySelectorAll("img,video,canvas,input,svg")].some(el => {
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0 && getComputedStyle(el).visibility !== "hidden"
      && (!(el instanceof HTMLImageElement) || el.naturalWidth > 0);
  });
  return body.innerText.trim() || drawn ? "visible" : "loading";
}
