// The page: what the speaker shows and the controls (docs/web.md: three views, by what USB shows). Every operation on
// the speaker is host/core's, through the WebAssembly in pkg/ (host/web); the modules in js/ ask the browser for the
// device, keep the backups and draw. This one starts it and wires the controls.

import "./js/zd-speaker.js";
import { act, connect, op, show } from "./js/connection.js";
import { consoleInit } from "./js/console.js";
import { report } from "./js/diagnostics.js";
import { joinForm, joinWatch, scan } from "./js/network.js";
import { app } from "./js/shared.js";
import { installMcu, installSystem, loadReleases, rollback, startFirmware } from "./js/software.js";
import { refresh } from "./js/speaker.js";
import { $, ask, download, fill, names, notice, readFile, stamp } from "./js/ui.js";
import init, { spec } from "./pkg/zd1_web.js";

// ---- wiring -------------------------------------------------------------------------------------------------------

async function main() {
  await init();
  app.S = spec();
  $("proto").textContent = app.S.proto;
  if (!navigator.usb) {
    $("nousb").hidden = false;
    $("connect").disabled = true;
    return;
  }
  $("connect").onclick = () => connect(true);
  $("disconnect").onclick = () => {
    app.sp?.free();
    app.bl?.free();
    app.sp = app.bl = null;
    show("welcome");
  };
  navigator.usb.addEventListener("connect", () => {
    if (!app.sp && !app.bl && !app.busy) connect(false);
  });
  navigator.usb.addEventListener("disconnect", (e) => {
    if (app.busy) return; // a flow restarts the device itself
    if (app.sp && e.device.productId === app.S.usb.pid) {
      app.sp = null;
      show("welcome");
      notice("The speaker left USB.");
    }
    if (app.bl && e.device.productId === app.S.usb.dfuPid) {
      app.bl = null;
      show("welcome");
      notice("The speaker's bootloader left USB.");
    }
  });
  $("dfu-start").onclick = startFirmware;
  $("grant-ok").onclick = async () => {
    const g = $("grant");
    try {
      await navigator.usb.requestDevice({ filters: [{ vendorId: +g.dataset.vid, productId: +g.dataset.pid }] });
    } catch {}
    g.close();
  };
  $("press-giveup").onclick = () => {
    $("press").close();
    notice("Gave up waiting for the press: the speaker refuses the change in a few seconds.");
  };
  $("job-cancel").onclick = () => {
    app.cancel = true;
  };
  $("job-close").onclick = () => {
    $("job").hidden = true;
  };
  for (const id of ["volume", "bass"]) {
    $(id).oninput = () => {
      $(`${id}-out`).textContent = $(id).value;
      fill($(id));
    };
    $(id).onchange = () =>
      act(() => app.sp.call("Volume", { volume: +$("volume").value, bass: +$("bass").value }), false);
  }
  $("name-form").onsubmit = (e) => {
    e.preventDefault();
    act(() => app.sp.setSetting("name", $("name").value.trim()));
  };
  $("scan").onclick = () => {
    $("join-form").hidden = true;
    op(scan);
  };
  $("hidden-net").onclick = () => {
    $("scan-list").hidden = true;
    joinForm(null, false);
  };
  $("join-cancel").onclick = () => {
    $("join-form").hidden = true;
  };
  $("open").onchange = () => {
    $("pass-row").hidden = $("open").checked;
    $("pass").required = !$("open").checked;
  };
  $("join-form").onsubmit = (e) => {
    e.preventDefault();
    const ssid = $("ssid").value,
      pass = $("open").checked ? undefined : $("pass").value;
    act(async () => {
      await app.sp.joinWifi(ssid, pass);
      $("pass").value = "";
      $("join-form").hidden = true;
      $("scan-list").hidden = true;
      notice(`${ssid} added: the speaker joins it now.`);
      joinWatch(ssid);
    });
  };
  $("wifi-toggle").onclick = () =>
    act(() =>
      names(app.S.NetMode)[app.state.wifi_mode] === "off" ? app.sp.setSetting("wifi.mode", "dhcp") : app.sp.wifiOff(),
    );
  $("auto-switch").onchange = () => act(() => app.sp.call("Prefs", { auto_switch: $("auto-switch").checked }), false);
  $("amp").onchange = () => act(() => app.sp.call("Amp", { allow: $("amp").value === "auto", keep: true }), false);
  $("amp-idle").onchange = () => act(() => app.sp.call("Prefs", { amp_idle_s: +$("amp-idle").value * 60 }), false);
  $("vol-cap").onchange = () => act(() => app.sp.call("Prefs", { volume_cap: +$("vol-cap").value }), false);
  $("export").onclick = () =>
    op(async () =>
      download(
        `zd1-settings-${stamp()}.txt`,
        new Blob(
          [
            `# zd1 settings, ${new Date().toISOString()} (the Wi-Fi networks' keys stay on the speaker)\n${await app.sp.exportSettings()}`,
          ],
          { type: "text/plain" },
        ),
      ),
    );
  $("import").onchange = async () => {
    const f = await readFile($("import"));
    if (f)
      act(async () =>
        notice(`${await app.sp.importSettings(new TextDecoder().decode(f.data))} settings written.`, "ok"),
      );
  };
  $("reset-settings").onclick = async () => {
    if (
      await ask(
        "Settings to their defaults?",
        "The name, the addresses, the receivers and their options go back to the defaults. The Wi-Fi networks and AirPlay's pairings stay.",
        "Reset",
      )
    )
      act(() =>
        app.sp.setSetting("reset", "settings").then(() => notice("The settings are back to their defaults.", "ok")),
      );
  };
  $("reset-all").onclick = async () => {
    if (
      await ask(
        "Everything as from the factory?",
        "Every setting, the known Wi-Fi networks and AirPlay's pairings are deleted: the speaker leaves its network. The software stays.",
        "Reset everything",
      )
    )
      act(() =>
        app.sp
          .setSetting("reset", "all")
          .then(() => notice("Everything is back to the defaults: add a network to put the speaker on one.", "ok")),
      );
  };
  for (const [id, f] of [
    ["system-file", installSystem],
    ["mcu-file", installMcu],
    ["dfu-file", installMcu],
  ]) {
    $(id).onchange = async () => {
      const x = await readFile($(id));
      if (x) f(x.name, x.data);
    };
  }
  const rbReady = () => {
    $("rb-go").disabled = !(app.rbBcd && app.rbMcu);
  };
  $("rb-bcd").onchange = async () => {
    app.rbBcd = await readFile($("rb-bcd"));
    $("rb-bcd-name").textContent = app.rbBcd?.name ?? "";
    rbReady();
  };
  $("rb-mcu").onchange = async () => {
    app.rbMcu = await readFile($("rb-mcu"));
    $("rb-mcu-name").textContent = app.rbMcu?.name ?? "";
    rbReady();
  };
  $("rb-go").onclick = rollback;
  $("report").onclick = report;
  $("log-clear").onclick = () => {
    $("log").textContent = "";
  };
  setInterval(() => {
    if (app.sp && !app.busy) app.sp.poll().catch(() => {});
  }, 500);
  setInterval(() => refresh(false), 2000);
  setInterval(() => refresh(true), 10000);
  consoleInit();
  loadReleases();
  await connect(false);
}

main();
