import { createIconButton, icon } from "../../../../packages/ui/icons.js";
import { t } from "../i18n";
import {
  cancelUpdateDownload,
  checkForUpdates,
  confirmInstallUpdate,
  onUpdateState,
  openUpdateReleaseUrl,
  pauseUpdateDownload,
  startUpdateDownload,
} from "../app/updater";
import { desktop, state } from "../model/state";
import type { UpdateState } from "../model/types";
import { button, el, notify } from "../ui/dom";

const updateDialog = document.querySelector<HTMLDialogElement>("#update-dialog")!;

const canHover = () => window.matchMedia?.("(hover: hover) and (pointer: fine)")?.matches ?? desktop;

function isActionable(u: UpdateState): boolean {
  return u.status === "available"
    || u.status === "downloading"
    || u.status === "paused"
    || u.status === "ready"
    || u.status === "installing"
    || (u.status === "error" && u.stage !== "check");
}

function formatBytes(bytes?: number | null): string {
  if (!bytes || bytes <= 0) return "";
  if (bytes < 1024 * 1024) return `${Math.max(1, Math.round(bytes / 1024))} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function formatDate(iso?: string | null): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return d.toISOString().slice(0, 10);
}

function appendInlineCode(parent: HTMLElement, text: string) {
  const parts = text.split(/(`[^`]+`)/g);
  for (const part of parts) {
    if (part.startsWith("`") && part.endsWith("`") && part.length >= 2) {
      parent.append(el("code", "update-inline-code", part.slice(1, -1)));
    } else if (part) {
      parent.append(document.createTextNode(part));
    }
  }
}

function renderReleaseNotes(raw?: string | null): HTMLElement {
  const box = el("div", "update-notes-body");
  const clean = (raw ?? "").trim();
  if (!clean) {
    box.append(el("p", "muted", t("update_empty_notes")));
    return box;
  }
  let list: HTMLUListElement | null = null;
  const flushList = () => {
    if (list) {
      box.append(list);
      list = null;
    }
  };
  for (const line of clean.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) {
      flushList();
      continue;
    }
    const heading = trimmed.match(/^#{1,4}\s+(.+)$/);
    if (heading?.[1]) {
      flushList();
      box.append(el("div", "update-notes-heading", heading[1]));
      continue;
    }
    const bullet = trimmed.match(/^[-*]\s+(.+)$/);
    if (bullet?.[1]) {
      if (!list) list = document.createElement("ul");
      const li = document.createElement("li");
      appendInlineCode(li, bullet[1]);
      list.append(li);
      continue;
    }
    flushList();
    const p = document.createElement("p");
    appendInlineCode(p, trimmed);
    box.append(p);
  }
  flushList();
  return box;
}

function currentPercent(u: UpdateState): number | null {
  if (u.status !== "downloading" && u.status !== "paused") return null;
  if (!u.total || u.total <= 0) return null;
  return Math.min(100, Math.round((u.downloaded / u.total) * 100));
}

function progressSummary(u: UpdateState): string {
  if (u.status !== "downloading" && u.status !== "paused") return "";
  const pct = currentPercent(u);
  const done = formatBytes(u.downloaded);
  const total = formatBytes(u.total);
  if (pct !== null && total) return `${pct}% · ${done} / ${total}`;
  return done || t(u.status === "paused" ? "update_paused_title" : "update_downloading_title").replace("{ver}", u.version);
}

export const updateTrigger = document.createElement("button");
updateTrigger.type = "button";
updateTrigger.id = "update-btn";
updateTrigger.className = "icon-btn solid lg update-trigger";
updateTrigger.hidden = true;
updateTrigger.append(icon("arrowUp"));

let popoverEl: HTMLDivElement | null = null;
let hideTimer: number | undefined;

function clearHideTimer() {
  if (hideTimer !== undefined) {
    window.clearTimeout(hideTimer);
    hideTimer = undefined;
  }
}

export function closeUpdatePopover() {
  clearHideTimer();
  if (!popoverEl) return;
  popoverEl.remove();
  popoverEl = null;
}

function scheduleClosePopover() {
  clearHideTimer();
  hideTimer = window.setTimeout(() => {
    closeUpdatePopover();
  }, 160);
}

function positionPopover(pop: HTMLDivElement) {
  const rect = updateTrigger.getBoundingClientRect();
  if (rect.width === 0 && rect.height === 0) {
    closeUpdatePopover();
    return;
  }
  const pw = pop.offsetWidth;
  const left = Math.max(12, Math.min(rect.right - pw, window.innerWidth - pw - 12));
  const top = Math.min(rect.bottom + 8, Math.max(12, window.innerHeight - pop.offsetHeight - 12));
  pop.style.left = `${Math.round(left)}px`;
  pop.style.top = `${Math.round(top)}px`;
}

function renderPopoverContent(pop: HTMLDivElement) {
  const u = state.update;
  if (!isActionable(u) || updateDialog.open) {
    closeUpdatePopover();
    return;
  }
  const ver = "version" in u && u.version ? u.version : "";
  const headingText = u.status === "ready"
    ? t("update_ready_title").replace("{ver}", ver)
    : u.status === "downloading"
      ? t("update_downloading_title").replace("{ver}", ver)
      : u.status === "paused"
        ? t("update_paused_title").replace("{ver}", ver)
        : u.status === "installing"
          ? t("update_installing_title")
          : ver
            ? t("update_available_title").replace("{ver}", ver)
            : t("update_confirm_title");

  const metaParts: string[] = [];
  if ("current" in u && u.current && ver) metaParts.push(`v${u.current} → v${ver}`);
  if ("size" in u && u.size) metaParts.push(formatBytes(u.size));
  else if ((u.status === "downloading" || u.status === "paused") && u.total) metaParts.push(formatBytes(u.total));
  if ("pub_date" in u && u.pub_date) {
    const dt = formatDate(u.pub_date);
    if (dt) metaParts.push(dt);
  }

  const head = el(
    "div",
    "update-pop-head",
    el("div", "update-pop-title", headingText),
    ...(metaParts.length ? [el("div", "update-pop-meta mono", metaParts.join(" · "))] : []),
  );

  const notesWrap = el(
    "div",
    "update-pop-notes",
    el("div", "update-pop-section", t("update_notes")),
    renderReleaseNotes("notes" in u ? u.notes : ""),
  );

  const foot = el("div", "update-pop-foot");
  if (u.status === "downloading" || u.status === "paused") {
    const pct = currentPercent(u);
    const barFill = el("div", "update-progress-fill");
    if (pct !== null) barFill.style.width = `${pct}%`;
    else if (u.status === "downloading") barFill.classList.add("indeterminate");
    foot.append(
      el("div", "update-progress-track", barFill),
      el("span", "update-pop-hint mono", progressSummary(u)),
      el("span", "update-pop-hint is-accent", t(u.status === "paused" ? "update_click_resume" : "update_click_pause")),
    );
  } else if (u.status === "ready") {
    foot.append(el("span", "update-pop-hint is-good", t("update_click_install")));
  } else if (u.status === "error") {
    const errText = t(u.message as Parameters<typeof t>[0]);
    foot.append(el("span", "update-pop-hint is-bad", errText));
  } else {
    foot.append(el("span", "update-pop-hint is-accent", t("update_click_download")));
  }

  pop.replaceChildren(head, notesWrap, foot);
}

function openUpdatePopover() {
  const u = state.update;
  if (!isActionable(u) || updateDialog.open) return;
  clearHideTimer();
  if (!popoverEl) {
    popoverEl = el("div", "update-popover");
    popoverEl.setAttribute("role", "tooltip");
    popoverEl.addEventListener("pointerenter", () => clearHideTimer());
    popoverEl.addEventListener("pointerleave", () => scheduleClosePopover());
    document.body.append(popoverEl);
  }
  renderPopoverContent(popoverEl);
  if (popoverEl) positionPopover(popoverEl);
}

function renderUpdateDialog() {
  if (!updateDialog.open) return;
  const u = state.update;
  const ver = "version" in u && u.version ? u.version : "";
  const cur = "current" in u && u.current ? u.current : state.appVersion;
  const busyInstalling = u.status === "installing";

  const title = el("h2", "", t(
    u.status === "ready" || u.status === "installing"
      ? "update_confirm_title"
      : u.status === "downloading"
        ? "update_downloading_title"
        : u.status === "paused"
          ? "update_paused_title"
          : "update_confirm_title",
  ).replace("{ver}", ver));
  title.id = "update-title";

  const closeBtn = createIconButton({
    icon: "close",
    title: t("close"),
    variant: "ghost",
    size: "md",
    onClick: () => { if (!busyInstalling) updateDialog.close(); },
  });
  closeBtn.disabled = busyInstalling;
  const header = el("div", "dialog-header", title, closeBtn);

  const body = el("div", "dialog-body");

  if (ver) {
    const metaRight: HTMLElement[] = [];
    const sizeText = u.status === "ready" || (u.status === "available" && u.size) || (u.status === "error" && u.size)
      ? formatBytes("size" in u ? u.size : null)
      : (u.status === "downloading" || u.status === "paused") && u.total
        ? formatBytes(u.total)
        : "";
    if (sizeText) metaRight.push(el("span", "confirm-port", sizeText));
    if (u.status === "ready" || u.status === "installing") {
      metaRight.push(el("span", "local-tag", t("update_verified")));
    }
    const card = el(
      "div",
      "confirm-card",
      el(
        "div",
        "confirm-head",
        el("span", "confirm-addr", cur ? `v${cur} → v${ver}` : `v${ver}`),
        el("div", "update-card-badges", ...metaRight),
      ),
      ...("pub_date" in u && u.pub_date && formatDate(u.pub_date)
        ? [el("span", "confirm-name", formatDate(u.pub_date))]
        : []),
    );
    body.append(card);
  }

  if (u.status === "downloading" || u.status === "paused") {
    const pct = currentPercent(u);
    const barFill = el("div", "update-progress-fill");
    if (pct !== null) barFill.style.width = `${pct}%`;
    else if (u.status === "downloading") barFill.classList.add("indeterminate");
    body.append(el(
      "div",
      "update-progress-box",
      el("div", "update-progress-track", barFill),
      el("span", "muted mono", progressSummary(u)),
    ));
  }

  if ("notes" in u && u.notes !== undefined) {
    body.append(el(
      "div",
      "update-notes-panel",
      el("div", "field-label", t("update_notes")),
      renderReleaseNotes(u.notes),
    ));
  }

  if (u.status === "ready" || u.status === "installing") {
    const noticeKey = !desktop
      ? "update_confirm_android"
      : (state.status?.online || state.status?.running)
        ? "update_confirm_active"
        : "update_confirm_idle";
    body.append(el("p", "muted", t(noticeKey)));
  }

  if (u.status === "error") {
    const msg = t(u.message as Parameters<typeof t>[0]);
    const alert = el("p", "feedback error", msg);
    alert.setAttribute("role", "alert");
    body.append(alert);
  }

  const footer = el("div", "dialog-footer");
  const releaseUrl = "release_url" in u ? u.release_url : null;
  if (releaseUrl) {
    const linkBtn = button(t("update_view_github"), () => void openUpdateReleaseUrl(releaseUrl), "button ghost leading");
    footer.append(linkBtn);
  }

  if (u.status === "available") {
    footer.append(
      button(t("update_later"), () => updateDialog.close(), "button"),
      button(t("update_download_now"), () => void startUpdateDownload(), "button primary"),
    );
  } else if (u.status === "downloading") {
    footer.append(
      button(t("update_cancel_download"), () => {
        void cancelUpdateDownload();
        updateDialog.close();
      }, "button danger"),
      button(t("update_pause_download"), () => void pauseUpdateDownload(), "button primary"),
    );
  } else if (u.status === "paused") {
    footer.append(
      button(t("update_cancel_download"), () => {
        void cancelUpdateDownload();
        updateDialog.close();
      }, "button danger"),
      button(t("update_resume_download"), () => void startUpdateDownload(), "button primary"),
    );
  } else if (u.status === "ready") {
    footer.append(
      button(t("update_later"), () => updateDialog.close(), "button"),
      button(t(desktop ? "update_install_now" : "update_install_android"), () => void confirmInstallUpdate(), "button primary"),
    );
  } else if (u.status === "installing") {
    const btn = button(t("update_installing_title"), () => {}, "button primary");
    btn.disabled = true;
    footer.append(btn);
  } else if (u.status === "error") {
    footer.append(button(t("close"), () => updateDialog.close(), "button"));
    if (u.stage === "install" && u.version) {
      footer.append(button(t("update_retry"), () => void confirmInstallUpdate(), "button primary"));
    } else if (u.stage === "download" && u.version) {
      footer.append(button(t("update_retry"), () => void startUpdateDownload(), "button primary"));
    } else {
      footer.append(button(t("update_retry"), () => void checkForUpdates(false), "button primary"));
    }
  } else {
    footer.append(button(t("close"), () => updateDialog.close(), "button"));
  }

  updateDialog.replaceChildren(header, body, footer);
}

export function openUpdateDialog() {
  closeUpdatePopover();
  updateDialog.tabIndex = -1;
  if (!updateDialog.open) {
    updateDialog.showModal();
    renderUpdateDialog();
    updateDialog.focus();
  } else {
    renderUpdateDialog();
  }
}

export function syncUpdateUi() {
  const u = state.update;
  const visible = isActionable(u);
  updateTrigger.hidden = !visible;
  if (!visible) {
    closeUpdatePopover();
    if (updateDialog.open && u.status === "up_to_date") updateDialog.close();
    return;
  }

  const ver = "version" in u && u.version ? u.version : "";
  const iconSvg = updateTrigger.querySelector("svg");
  const nextIconName = u.status === "downloading"
    ? "pause"
    : u.status === "paused"
      ? "download"
      : u.status === "installing"
        ? "refresh"
        : u.status === "ready"
          ? "check"
          : u.status === "error"
            ? "alert"
            : "arrowUp";
  const freshSvg = icon(nextIconName);
  if (iconSvg) iconSvg.replaceWith(freshSvg);
  else updateTrigger.prepend(freshSvg);

  updateTrigger.classList.toggle("spinning", u.status === "installing");
  updateTrigger.classList.toggle("is-downloading", u.status === "downloading");
  updateTrigger.classList.toggle("is-paused", u.status === "paused");
  updateTrigger.classList.toggle("is-ready", u.status === "ready");
  updateTrigger.classList.toggle("is-error", u.status === "error");
  updateTrigger.classList.toggle("accent", u.status !== "error");

  let ariaLabel = "";
  if (u.status === "available") {
    ariaLabel = t("update_available_title").replace("{ver}", ver);
  } else if (u.status === "downloading") {
    ariaLabel = t("update_click_pause");
  } else if (u.status === "paused") {
    ariaLabel = t("update_click_resume");
  } else if (u.status === "ready") {
    ariaLabel = t("update_ready_title").replace("{ver}", ver);
  } else if (u.status === "installing") {
    ariaLabel = t("update_installing_title");
  } else if (u.status === "error") {
    ariaLabel = t(u.message as Parameters<typeof t>[0]);
  }
  updateTrigger.setAttribute("aria-label", ariaLabel);
  updateTrigger.removeAttribute("title");

  if (popoverEl) {
    renderPopoverContent(popoverEl);
    if (popoverEl) positionPopover(popoverEl);
  }
  if (updateDialog.open) renderUpdateDialog();
}

export function buildSettingsUpdateControl(redrawSettings: () => void): HTMLElement {
  const u = state.update;
  const wrap = el("div", "settings-update-row");
  let statusText = "";
  let btnText = t("check_updates");
  let btnCls = "button";
  let disabled = false;
  let onClick = () => {
    void checkForUpdates(false).then(() => redrawSettings());
  };

  if (u.status === "checking") {
    statusText = t("checking_updates");
    btnText = t("checking_updates");
    disabled = true;
  } else if (u.status === "up_to_date") {
    statusText = t("up_to_date");
  } else if (u.status === "available") {
    statusText = t("update_available_short").replace("{ver}", u.version);
    btnText = t("update_download_now");
    btnCls = "button primary";
    onClick = () => {
      void startUpdateDownload();
      redrawSettings();
    };
  } else if (u.status === "downloading") {
    statusText = progressSummary(u);
    btnText = t("update_pause_download");
    onClick = () => {
      void pauseUpdateDownload();
      redrawSettings();
    };
  } else if (u.status === "paused") {
    statusText = `${t("update_paused_title").replace("{ver}", u.version)} (${progressSummary(u)})`;
    btnText = t("update_resume_download");
    btnCls = "button primary";
    onClick = () => {
      void startUpdateDownload();
      redrawSettings();
    };
  } else if (u.status === "ready") {
    statusText = t("update_ready_title").replace("{ver}", u.version);
    btnText = t("update_ready_short").replace("{ver}", u.version);
    btnCls = "button primary";
    onClick = () => {
      document.querySelector<HTMLDialogElement>("#settings-dialog")?.close();
      openUpdateDialog();
    };
  } else if (u.status === "installing") {
    statusText = t("update_installing_title");
    btnText = t("update_installing_title");
    disabled = true;
  } else if (u.status === "error") {
    statusText = t(u.message as Parameters<typeof t>[0]);
    btnText = t("update_retry");
    onClick = () => {
      if (u.stage === "install" && u.version) void confirmInstallUpdate();
      else if (u.stage === "download" && u.version) void startUpdateDownload();
      else void checkForUpdates(false);
      redrawSettings();
    };
  }

  if (statusText) {
    const note = el("span", u.status === "error" ? "settings-update-note is-bad" : "settings-update-note", statusText);
    note.title = statusText;
    wrap.append(note);
  }
  const btn = button(btnText, onClick, btnCls);
  btn.disabled = disabled;
  wrap.append(btn);
  return wrap;
}

let updateUiInitialized = false;

export function initUpdateUi(onSettingsRedraw: () => void) {
  if (updateUiInitialized) return;
  updateUiInitialized = true;

  updateDialog.addEventListener("click", (event) => {
    if (event.target === updateDialog && state.update.status !== "installing") {
      updateDialog.close();
    }
  });
  updateDialog.addEventListener("cancel", (event) => {
    if (state.update.status === "installing") event.preventDefault();
  });

  updateTrigger.addEventListener("pointerenter", (event) => {
    if (event.pointerType === "touch") return;
    openUpdatePopover();
  });
  updateTrigger.addEventListener("pointerleave", (event) => {
    if (event.pointerType === "touch") return;
    scheduleClosePopover();
  });
  updateTrigger.addEventListener("focus", () => {
    try {
      if (updateTrigger.matches(":focus-visible") && canHover()) openUpdatePopover();
    } catch {}
  });
  updateTrigger.addEventListener("blur", () => {
    scheduleClosePopover();
  });
  window.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && popoverEl) closeUpdatePopover();
  });
  window.addEventListener("resize", () => {
    if (popoverEl) positionPopover(popoverEl);
  });

  updateTrigger.addEventListener("click", () => {
    const u = state.update;
    if (u.status === "available" || u.status === "paused") {
      void startUpdateDownload();
      return;
    }
    if (u.status === "downloading") {
      void pauseUpdateDownload();
      return;
    }
    openUpdateDialog();
  });

  onUpdateState((next, prev) => {
    syncUpdateUi();
    onSettingsRedraw();
    if (next.status === "ready" && prev.status === "downloading") {
      notify(t("update_ready_toast").replace("{ver}", next.version));
    } else if (next.status === "error" && next.stage === "download" && !updateDialog.open) {
      notify(t(next.message as Parameters<typeof t>[0]), true);
    }
  });

  syncUpdateUi();
}
