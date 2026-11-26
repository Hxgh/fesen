import { failure, i18n, t } from "../i18n";
import { osBadge } from "../../../../packages/ui/icons.js";
import { openSettingsModal } from "../../../../packages/ui/settings.js";
import { act, edit, editor, render, request, scale, serverHost, settingsDialog, syncBrand, theme } from "../app/runtime";
import { desktop, mac, state } from "../model/state";
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

      const profiles = (state.status?.cloud_profiles ?? []).filter((p) => p.shares.length > 0 || p.forwards.length > 0);
      if (state.status?.member && profiles.length) {
        const list = el("div", "settings-profile-list", ...profiles.map((p) => {
          const osEl = osBadge(p.os, "device");
          const counts = `${p.shares.length}/${p.forwards.length}`;
          const impBtn = button(t("import_profile"), () => {
            void request({
              op: "import_profile",
              machine_id: p.machine_id,
              import_shares: true,
              import_forwards: true,
            }, false).then((next) => {
              state.status = next;
              render(true);
              notify(t("profile_imported"));
              if (settingsDialog.open) redraw();
            }).catch((err) => notify(failure(err), true));
          }, "button");
          const delBtn = tool("close", t("delete_backup"), () => {
            void request({ op: "delete_profile", machine_id: p.machine_id }, false)
              .then((next) => {
                state.status = next;
                render(true);
                if (settingsDialog.open) redraw();
              })
              .catch((err) => notify(failure(err), true));
          });
          return el(
            "div",
            "settings-profile-row",
            el(
              "div",
              "settings-profile-meta",
              ...(osEl ? [osEl] : []),
              el("span", "settings-device-name", p.name),
              el("span", "mono muted", `#${p.machine_id.slice(0, 6)} · ${counts}`),
            ),
            el("div", "settings-profile-btns", impBtn, delBtn),
          );
        }));
        nodes.push(el("div", "field-group", el("span", "settings-label", t("cloud_profiles")), list));
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
