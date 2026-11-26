import { failure, t } from "../i18n";
import { createIconButton } from "../../../../packages/ui/icons.js";
import { singleSpan } from "../../../../packages/ui/host-card.js";
import { editor, render, request, switchForwardPort } from "../app/runtime";
import { toInt, toIp } from "../model/net";
import { forwardsOfService, labelFor, state } from "../model/state";
import type { Label, Offering } from "../model/types";
import { button, dialogHeader, el } from "../ui/dom";

export function customize(
  service: Offering,
  label: Label | undefined,
  routing: boolean,
  focusPort = false,
  onDone?: () => void,
) {
  const existingFwds = forwardsOfService(service.id, state.status);
  const firstFwd = existingFwds[0];
  const single = singleSpan(service.rules.ips) && singleSpan(service.rules.ports);
  let remarkVal = label?.remark ?? "";
  let targetIpVal = firstFwd?.target_ip ?? service.rules.ips[0]?.start ?? "";
  let targetPortVal = String(firstFwd?.target_port ?? service.rules.ports[0]?.start ?? "");
  let localPortVal = firstFwd ? String(firstFwd.local_port) : "";
  let aliasVal = label?.alias ?? "";
  let aliasIpVal = label?.ip ?? service.rules.ips[0]?.start ?? "";
  let conflictPort: number | null = firstFwd?.error === "conflict" ? firstFwd.local_port : null;
  let suggestedPort: number | null = firstFwd?.suggested_port ?? null;
  let localError = "";
  let dialogOpen = true;
  editor.addEventListener("close", () => { dialogOpen = false; }, { once: true });

  const clearPortErr = () => {
    localError = "";
    conflictPort = null;
  };

  async function suggestFree(preferred?: number) {
    try {
      const reply = await request({ op: "suggest_local_port", preferred }, false);
      if (reply.suggested_port) {
        localPortVal = String(reply.suggested_port);
        suggestedPort = reply.suggested_port;
        clearPortErr();
        if (dialogOpen && editor.open) draw();
      }
    } catch {}
  }

  function draw() {
    const header = dialogHeader(t("access_settings"), () => { if (!state.busy) editor.close(); });
    const body = el("div", "dialog-body");
    if (focusPort && !routing) {
      body.append(el("p", "muted", t("need_local_forward")));
    }

    const remarkInput = el("input");
    Object.assign(remarkInput, { name: "remark", value: remarkVal, maxLength: 64, spellcheck: false });
    remarkInput.addEventListener("input", () => { remarkVal = remarkInput.value; });
    body.append(el("label", "", t("remark"), remarkInput));

    let targetIpInput: HTMLInputElement | null = null;
    let targetPortInput: HTMLInputElement | null = null;
    if (!single && existingFwds.length > 0) {
      const chipList = el("div", "fwd-chip-list", ...existingFwds.map((f) => {
        const rm = createIconButton({
          icon: "close",
          title: t("local_port_clear"),
          label: `${t("local_port_clear")} 127.0.0.1:${f.local_port}`,
          variant: "ghost",
          size: "sm",
          onClick: () => {
            void switchForwardPort(f.service, f.target_ip, f.target_port, 0).then(() => {
              if (dialogOpen && editor.open) {
                customize(service, labelFor(service.id, state.status), routing, focusPort, onDone);
              }
            });
          },
        });
        return el(
          "div",
          "fwd-chip",
          el("span", "mono", `127.0.0.1:${f.local_port} ← ${f.target_ip}:${f.target_port}`),
          rm,
        );
      }));
      body.append(el("div", "field-group", el("span", "field-label", t("active_forwards")), chipList));
    }

    if (!single) {
      targetIpInput = el("input");
      Object.assign(targetIpInput, { name: "target_ip", value: targetIpVal, maxLength: 15, spellcheck: false });
      targetIpInput.addEventListener("input", () => { targetIpVal = targetIpInput!.value; clearPortErr(); });
      targetPortInput = el("input");
      Object.assign(targetPortInput, { name: "target_port", value: targetPortVal, type: "number", min: "1", max: "65535" });
      targetPortInput.addEventListener("input", () => { targetPortVal = targetPortInput!.value; clearPortErr(); });
      body.append(el(
        "div",
        "field-pair",
        el("label", "", t("local_target_ip"), targetIpInput),
        el("label", "", t("local_target_port"), targetPortInput),
      ));
    }

    const localPortInput = el("input");
    Object.assign(localPortInput, {
      name: "local_port",
      value: localPortVal,
      type: "number",
      min: "1",
      max: "65535",
      placeholder: t("local_port_placeholder"),
    });
    localPortInput.addEventListener("input", () => {
      localPortVal = localPortInput.value;
      clearPortErr();
    });
    const suggestBtn = button(t("local_port_suggest"), () => {
      const p = Number(localPortVal) || Number(targetPortVal) || undefined;
      void suggestFree(p);
    }, "button");
    const portRow = el("div", "host-ip-row", localPortInput, suggestBtn);
    if (firstFwd) {
      const clearBtn = button(t("local_port_clear"), () => {
        localPortVal = "";
        clearPortErr();
        void saveAll(true);
      }, "button");
      portRow.append(clearBtn);
    }
    body.append(el("div", "field-group", el("span", "field-label", t("local_port_label")), portRow));

    let aliasInput: HTMLInputElement | null = null;
    let aliasIpInput: HTMLInputElement | null = null;
    if (routing) {
      aliasInput = el("input");
      Object.assign(aliasInput, { name: "alias", value: aliasVal, placeholder: "app.fesen", maxLength: 72, spellcheck: false });
      aliasInput.addEventListener("input", () => { aliasVal = aliasInput!.value; });
      aliasIpInput = el("input");
      Object.assign(aliasIpInput, { name: "ip", value: aliasIpVal, maxLength: 15, spellcheck: false });
      aliasIpInput.addEventListener("input", () => { aliasIpVal = aliasIpInput!.value; });
      body.append(
        el("label", "", t("alias"), aliasInput),
        el("label", "", t("alias_ip"), aliasIpInput),
      );
    }

    const errText = localError || (conflictPort ? t("local_port_occupied").replace("{port}", String(conflictPort)) : "");
    if (errText) {
      const box = el("div", "feedback error inline-port-alert", el("span", "", errText));
      box.setAttribute("role", "alert");
      if (suggestedPort && suggestedPort !== conflictPort) {
        const useBtn = button(t("switch_to_port").replace("{port}", String(suggestedPort)), () => {
          localPortVal = String(suggestedPort);
          clearPortErr();
          void saveAll();
        }, "button primary");
        box.append(useBtn);
      }
      body.append(box);
    }

    const saveBtn = button(t("save"), () => void saveAll(), "button primary");
    saveBtn.disabled = state.busy;
    const cancelBtn = button(t("cancel"), () => { if (!state.busy) editor.close(); }, "button");
    cancelBtn.disabled = state.busy;
    const footer = el("div", "dialog-footer", cancelBtn, saveBtn);

    const formEl = el("form", "", header, body, footer);
    formEl.className = "";
    formEl.addEventListener("submit", (e) => {
      e.preventDefault();
      void saveAll();
    });

    editor.tabIndex = -1;
    editor.replaceChildren(formEl);
    if (!editor.open) {
      editor.showModal();
      (focusPort ? localPortInput : remarkInput).focus();
    }
  }

  async function saveAll(forceClearForward = false) {
    if (state.busy) return;
    localError = "";
    const tIp = targetIpVal.trim();
    const tPort = Number(targetPortVal);
    const lPortText = forceClearForward ? "" : localPortVal.trim();
    const lPort = lPortText === "" ? 0 : Number(lPortText);

    if (lPortText !== "" && (!Number.isInteger(lPort) || lPort < 1 || lPort > 65535)) {
      localError = t("local_port_invalid");
      draw();
      return;
    }
    if (lPort > 0 || forceClearForward || firstFwd) {
      if (
        !/^\d{1,3}(\.\d{1,3}){3}$/.test(tIp)
        || toIp(toInt(tIp)) !== tIp
        || !service.rules.ips.some((r) => toInt(r.start) <= toInt(tIp) && toInt(tIp) <= toInt(r.end))
        || !Number.isInteger(tPort)
        || !service.rules.ports.some((r) => r.start <= tPort && tPort <= r.end)
      ) {
        localError = t("invalid_target");
        draw();
        return;
      }
    }

    state.busy = true;
    draw();
    try {
      const needForwardChange = forceClearForward
        || (lPort > 0 && (!firstFwd || firstFwd.local_port !== lPort || firstFwd.target_ip !== tIp || firstFwd.target_port !== tPort))
        || (lPort === 0 && !!firstFwd);

      if (needForwardChange) {
        try {
          if (firstFwd && (firstFwd.target_ip !== tIp || firstFwd.target_port !== tPort)) {
            state.status = await request({
              op: "set_local_forward",
              service: service.id,
              target_ip: firstFwd.target_ip,
              target_port: firstFwd.target_port,
              local_port: 0,
            }, false);
          }
          state.status = await request({
            op: "set_local_forward",
            service: service.id,
            target_ip: tIp,
            target_port: tPort,
            local_port: lPort,
          }, false);
        } catch (err) {
          if (err === "conflict" && lPort > 0) {
            conflictPort = lPort;
            try {
              const s = await request({ op: "suggest_local_port", preferred: lPort }, false);
              suggestedPort = s.suggested_port ?? null;
            } catch {}
            return;
          }
          localError = failure(typeof err === "string" ? err : "internal");
          return;
        }
      }

      const alias = routing ? aliasVal.trim() : (label?.alias ?? "");
      const ip = routing ? aliasIpVal.trim() : (label?.ip ?? null);
      state.status = await request({
        op: "customize",
        id: service.id,
        remark: remarkVal.trim(),
        alias,
        ip: alias ? ip : null,
      }, false);

      editor.close();
      render(true);
      onDone?.();
    } catch (err) {
      localError = failure(typeof err === "string" ? err : "internal");
    } finally {
      state.busy = false;
      if (dialogOpen && editor.open) draw();
    }
  }

  draw();
  if (focusPort && !localPortVal) {
    void suggestFree(Number(targetPortVal) || undefined);
  }
}
