import { invoke } from "@tauri-apps/api/core";
import { failure, i18n, t } from "../i18n";
import { osBadge } from "../../../../packages/ui/icons.js";
import { groupByHost, singleIp } from "../../../../packages/ui/host-card.js";
import { openSettingsModal } from "../../../../packages/ui/settings.js";
import { act, edit, editor, render, request, scale, serverHost, settingsDialog, showDialog, syncBrand, theme } from "../app/runtime";
import { desktop, mac, state } from "../model/state";
import { endpoint } from "../model/net";
import type { DeviceProfile, Share } from "../model/types";
import { button, el, field, notify, str, submit, tool } from "../ui/dom";
import { currentWinSize, getCloseBehavior, isWinSizeDefault, resetDesktopWindowSize, syncCloseBehavior } from "../ui/window";
import { refreshSiteDialogI18n } from "./site-dialog";

function openRenameDevice(currentName: string) {
  settingsDialog.close();
  edit(
    t("current_device"),
    submit(t("save")),
    (values) => {
      void act({ op: "rename_device", label: str(values, "label").trim() }, () => editor.close());
    },
    field(t("device"), "label", currentName, { required: true, maxLength: 64 }),
  );
}

function backupRuleChips(p: DeviceProfile, includeShares: boolean): HTMLElement[] {
  const chips: HTMLElement[] = [];
  if (includeShares) {
    const entries = groupByHost(p.shares, (s: Share) => singleIp(s));
    for (const entry of entries) {
      if (entry.grouped && entry.items.length >= 2) {
        chips.push(el("span", "confirm-port", `${entry.ip} (${entry.items.length})`));
      } else {
        const s = (entry.grouped ? entry.items[0] : entry.item) as Share;
        chips.push(el("span", "confirm-port", endpoint(s.rules.ips, s.rules.ports)));
      }
    }
  }
  for (const f of p.forwards) {
    chips.push(el("span", "confirm-port", `127.0.0.1:${f.local_port}`));
  }
  const maxShown = 6;
  if (chips.length > maxShown) {
    const rest = chips.length - maxShown;
    return [...chips.slice(0, maxShown), el("span", "confirm-port muted", `+${rest}`)];
  }
  return chips;
}

function visibleBackupProfiles(): DeviceProfile[] {
  const hasLocalRules = (state.status?.shares.length ?? 0) > 0 || (state.status?.forwards?.length ?? 0) > 0;
  return (state.status?.cloud_profiles ?? []).filter((p) => {
    const isSelfDev = !!state.status?.machine_id && p.machine_id === state.status.machine_id;
    if (isSelfDev) {
      return !hasLocalRules && (p.shares.length > 0 || p.forwards.length > 0);
    }
    // Never import another device's LAN shares (different device = different LAN); only 127 forwards are portable.
    return p.forwards.length > 0;
  });
}

function openRuleBackupsDialog() {
  settingsDialog.close();
  function draw() {
    const profiles = visibleBackupProfiles();
    if (!profiles.length) {
      editor.close();
      return;
    }
    const cards = profiles.map((p) => {
      const isSelfDev = !!state.status?.machine_id && p.machine_id === state.status.machine_id;
      const osEl = osBadge(p.os, { fallback: "device", local: isSelfDev, prefix: isSelfDev ? t("badge_local") : "" });
      const impBtn = button(t(isSelfDev ? "import_profile" : "import_forwards_only"), () => {
        void request({
          op: "import_profile",
          machine_id: p.machine_id,
          import_shares: isSelfDev,
          import_forwards: true,
        }, false).then((next) => {
          state.status = next;
          editor.close();
          render(true);
          notify(t(isSelfDev ? "profile_imported" : "forwards_imported"));
        }).catch((err) => notify(failure(err), true));
      }, "button");
      const delBtn = tool("close", t("delete_backup"), () => {
        void request({ op: "delete_profile", machine_id: p.machine_id }, false)
          .then((next) => {
            state.status = next;
            render(true);
            if (editor.open) draw();
          })
          .catch((err) => notify(failure(err), true));
      });
      const chips = backupRuleChips(p, isSelfDev);
      return el(
        "div",
        "confirm-card",
        el(
          "div",
          "confirm-head",
          el(
            "div",
            "backup-card-title",
            ...(osEl ? [osEl] : []),
            el("span", "settings-device-name", p.name),
            ...(isSelfDev ? [el("span", "local-tag", t("badge_local"))] : []),
          ),
          el("div", "backup-card-actions", impBtn, delBtn),
        ),
        ...(chips.length ? [el("div", "confirm-ports", ...chips)] : []),
      );
    });
    showDialog(t("rule_backups_title"), cards, [], false);
  }
  draw();
}

export function openSettings() {
  let currentBehavior = getCloseBehavior();
  let saveError = false;
  let prefAutoConnect = state.status?.auto_connect ?? true;
  let prefAutoShare = state.status?.auto_share ?? true;
  let prefCloudBackup = state.status?.cloud_backup ?? true;

  openSettingsModal(settingsDialog, {
    t: (k) => t(k as Parameters<typeof t>[0]),
    theme,
    scale,
    i18n,
    onLocaleChange: () => {
      refreshSiteDialogI18n();
      syncBrand();
      render(true);
    },
    extraBody(redraw) {
      const nodes: HTMLElement[] = [];
      if (mac) {
        const closeSeg = el("div", "segmented");
        for (const [id, key] of [["tray", "close_to_tray"], ["quit", "close_quit"]] as const) {
          const btn = button(t(key), () => {
            if (currentBehavior === id) return;
            currentBehavior = id;
            saveError = false;
            void syncCloseBehavior(id).then((ok) => {
              saveError = !ok;
              if (settingsDialog.open) redraw();
            });
            redraw();
          }, id === currentBehavior ? "active" : "");
          btn.setAttribute("aria-pressed", String(id === currentBehavior));
          closeSeg.append(btn);
        }
        nodes.push(el("div", "settings-row", el("span", "settings-label", t("on_window_close")), closeSeg));
        if (saveError) {
          const fb = el("p", "feedback error", t("menu_save_failed"));
          fb.setAttribute("role", "alert");
          nodes.push(fb);
        }
      }

      if (!desktop) {
        const tileBtn = button(t("quick_tile_add"), () => {
          tileBtn.disabled = true;
          void invoke<string>("add_quick_tile")
            .then((res) => {
              if (res === "added") notify(t("quick_tile_added"));
              else if (res === "already_added") notify(t("quick_tile_already"));
            })
            .catch(() => notify(t("quick_tile_failed"), true))
            .finally(() => { tileBtn.disabled = false; });
        }, "button");
        nodes.push(el(
          "div",
          "settings-row",
          el("span", "settings-label", t("quick_tile")),
          tileBtn,
        ));
      }

      if (desktop) {
        const sz = currentWinSize();
        const sizeVal = el("span", "settings-window-val", `${sz.w} × ${sz.h}`);
        sizeVal.setAttribute("data-win-size-val", "1");
        const resetBtn = button(t("reset_window"), () => void resetDesktopWindowSize(settingsDialog), "button");
        resetBtn.setAttribute("data-win-size-reset", "1");
        resetBtn.disabled = isWinSizeDefault();
        nodes.push(el(
          "div",
          "settings-row",
          el("span", "settings-label", t("window_size")),
          el("div", "settings-window", sizeVal, resetBtn),
        ));
      }

      if (state.status) {
        nodes.push(el("div", "settings-divider"));

        const connSeg = el("div", "segmented");
        for (const [val, key] of [[true, "connect_auto"], [false, "connect_manual"]] as const) {
          const btn = button(t(key), () => {
            if (prefAutoConnect === val) return;
            const prev = prefAutoConnect;
            prefAutoConnect = val;
            redraw();
            void request({ op: "set_preferences", auto_connect: val }, false)
              .then((next) => { state.status = next; render(); })
              .catch(() => {
                prefAutoConnect = prev;
                if (settingsDialog.open) redraw();
              });
          }, val === prefAutoConnect ? "active" : "");
          btn.setAttribute("aria-pressed", String(val === prefAutoConnect));
          connSeg.append(btn);
        }
        nodes.push(el("div", "settings-row", el("span", "settings-label", t("on_launch_connect")), connSeg));

        const shareSeg = el("div", "segmented");
        for (const [val, key] of [[true, "restore_same_net"], [false, "restore_off"]] as const) {
          const btn = button(t(key), () => {
            if (prefAutoShare === val) return;
            const prev = prefAutoShare;
            prefAutoShare = val;
            redraw();
            void request({ op: "set_preferences", auto_share: val }, false)
              .then((next) => { state.status = next; render(); })
              .catch(() => {
                prefAutoShare = prev;
                if (settingsDialog.open) redraw();
              });
          }, val === prefAutoShare ? "active" : "");
          btn.setAttribute("aria-pressed", String(val === prefAutoShare));
          shareSeg.append(btn);
        }
        nodes.push(el("div", "settings-row", el("span", "settings-label", t("restore_shares")), shareSeg));

        const backupSeg = el("div", "segmented");
        for (const [val, key] of [[true, "backup_on"], [false, "backup_off"]] as const) {
          const btn = button(t(key), () => {
            if (prefCloudBackup === val) return;
            const prev = prefCloudBackup;
            prefCloudBackup = val;
            redraw();
            void request({ op: "set_preferences", cloud_backup: val }, false)
              .then((next) => { state.status = next; render(); })
              .catch(() => {
                prefCloudBackup = prev;
                if (settingsDialog.open) redraw();
              });
          }, val === prefCloudBackup ? "active" : "");
          btn.setAttribute("aria-pressed", String(val === prefCloudBackup));
          backupSeg.append(btn);
        }
        nodes.push(el("div", "settings-row", el("span", "settings-label", t("cloud_backup")), backupSeg));
      }

      if (state.status?.device_name) {
        nodes.push(el("div", "settings-divider"));
        const devOsEl = osBadge(state.status.device_os, { fallback: "device", local: true, prefix: t("badge_local") });
        const shortId = state.status.machine_id ? `#${state.status.machine_id.slice(0, 6)}` : "";
        const devVal = el(
          "div",
          "settings-window",
          el(
            "span",
            "settings-device-val",
            ...(devOsEl ? [devOsEl] : []),
            el("span", "settings-device-name", state.status.device_name),
            ...(shortId ? [el("span", "mono muted", shortId)] : []),
          ),
          button(t("rename_device"), () => openRenameDevice(state.status?.device_name ?? ""), "button"),
        );
        nodes.push(el("div", "settings-row", el("span", "settings-label", t("current_device")), devVal));
      }

      const profiles = visibleBackupProfiles();
      if (state.status?.member && profiles.length) {
        nodes.push(el(
          "div",
          "settings-row",
          el("span", "settings-label", t("rule_backups")),
          el(
            "div",
            "settings-window",
            el("span", "settings-window-val", t("devices_count").replace("{n}", String(profiles.length))),
            button(t("rule_backups_open"), () => openRuleBackupsDialog(), "button"),
          ),
        ));
      }

      if (state.status?.member && state.status.server) {
        nodes.push(el("div", "settings-divider"));

        const acctInfo = el("span", "settings-account-info", `${state.status.member} · ${serverHost(state.status.server)}`);
        const logoutBtn = button(t("logout"), () => {
          settingsDialog.close();
          state.tab = "services";
          void act({ op: "logout" });
        }, "button danger");

        nodes.push(el(
          "div",
          "settings-account-block",
          el("div", "settings-account-top", el("span", "settings-label", t("account")), acctInfo),
          el("div", "settings-account-btns", el("div", "settings-btn-pair", logoutBtn)),
        ));
      }
      return nodes;
    },
    footerText: () => (state.appVersion ? `${t("brand")} v${state.appVersion}` : t("brand")),
  });
}
