// The speaker at a glance: the state line, the sources, volume and bass, Behaviour and Advanced.
import { ampSentence, gateSentence } from "../pkg/zd1_web.js";
import { act, show } from "./connection.js";
import { drawDiag } from "./diagnostics.js";
import { hero } from "./hero.js";
import { lampOf, ledLook } from "./led.js";
import { drawSettings } from "./network.js";
import { app } from "./shared.js";
import { drawnKey, drawReleases, drawSoftware } from "./software.js";
import { $, el, fill, names, segmented } from "./ui.js";

// ---- the state ----------------------------------------------------------------------------------------------------

export const SRC = () => names(app.S.Source),
  PLAYER = () => names(app.S.Player);
/// What the status line calls a source or a receiver.
const NAME = {
  usb: "USB",
  aux: "AUX",
  wifi: "The network",
  ipod: "The dock",
  airplay: "AirPlay",
  spotify: "Spotify Connect",
  snapcast: "Snapcast",
  other: "An app",
};
export const RECEIVERS = [
  ["airplay", "AirPlay"],
  ["spotify", "Spotify Connect"],
  ["snapcast", "Snapcast"],
];

export async function refresh(all) {
  if (!app.sp || app.busy) return;
  try {
    app.info = await app.sp.call("Info", {});
    const diag = await app.sp.call("Diag", {});
    if (all) {
      app.state = await app.sp.state();
      app.known = await app.sp.list("Networks").catch(() => []);
      drawSettings();
      drawSoftware(await app.sp.call("BootState", {}));
    }
    drawDiag(diag);
    app.led = diag.led;
    draw();
  } catch (e) {
    if (e?.code === "GONE") {
      app.sp = null;
      show("welcome");
    }
  }
}

export function draw() {
  const i = app.info,
    src = SRC()[i.source],
    player = PLAYER()[i.player];
  const playing = i.state === app.S.SysState.RUN;
  const what = player !== "none" ? player : src;
  hero({
    mode: "app",
    title: app.state?.name || i.mod_name || "Zeppelin Air",
    status:
      i.state === app.S.SysState.STANDBY
        ? "Standby"
        : i.state === app.S.SysState.ERROR
          ? "A protection tripped"
          : `${NAME[what] ?? what} · volume ${i.volume}`,
    ...ledLook(app.led ?? 0, i.player),
    vol: i.volume / 71,
  });
  const choices = [
    ["usb", "USB"],
    ["aux", "AUX"],
    ["wifi", "Network"],
  ];
  for (const [k, label] of RECEIVERS) if ((i.players_on >> app.S.Player[k.toUpperCase()]) & 1) choices.push([k, label]);
  choices.push(["standby", "Standby"]);
  const cur = !playing ? "standby" : src === "wifi" && player !== "none" ? player : src;
  segmented(
    $("sources"),
    choices.map(([k, label]) => [k, label, lampOf(k)]),
    cur,
    (k) => act(() => app.sp.play(k), false),
  );
  $("volume").max = i.volume_cap || 71; // the one limit: the slider stops there
  if (document.activeElement !== $("volume")) $("volume").value = i.volume;
  if (document.activeElement !== $("bass")) $("bass").value = i.bass;
  fill($("volume"));
  fill($("bass"));
  $("volume-out").textContent = i.volume;
  $("bass-out").textContent = i.bass;
  $("now").textContent = `The amp is ${ampSentence(i)}.`;
  drawBehaviour();
  drawAdvanced();
  $("fw").textContent = `${i.fw}${i.built ? `, built ${i.built}` : ""}`;
  const ms = names(app.S.ModState)[i.mod_state];
  $("os").textContent =
    ms === "stock"
      ? "B&W's software (not ours): the module is no source. The banner on top installs ours, or restore the main board's backup."
      : ms === "mismatch"
        ? `${i.mod_os}: its link protocol is ${i.mod_proto}, this firmware's ${i.link_proto}: update one of them`
        : i.mod_os || `not running our OS (${ms})`;
  if (app.drawnFor !== drawnKey()) drawReleases();
}
// ---- behaviour and advanced ---------------------------------------------------------------------------------------

export const IDLE = [1, 2, 5, 10, 15, 20, 30];

export function drawBehaviour() {
  const i = app.info;
  if (document.activeElement !== $("auto-switch")) $("auto-switch").checked = i.auto_switch;
  $("amp").value = i.amp_auto ? "auto" : "deny";
  $("amp-now").textContent = i.amp_allowed ? "" : ampSentence(i);
  const min = Math.round(i.amp_idle_s / 60) || 5;
  if ($("amp-idle").options.length === 0 || ![...$("amp-idle").options].some((o) => +o.value === min)) {
    $("amp-idle").replaceChildren(
      ...[...new Set([...IDLE, min])]
        .sort((a, b) => a - b)
        .map((m) => el("option", { value: m, textContent: `${m} min` })),
    );
  }
  if (document.activeElement !== $("amp-idle")) $("amp-idle").value = min;
  if ($("vol-cap").options.length === 0) {
    $("vol-cap").replaceChildren(
      ...Array.from({ length: 71 }, (_, k) =>
        el("option", { value: k + 1, textContent: `${k + 1} of 71${k + 1 === 40 ? " (the default)" : ""}` }),
      ),
    );
  }
  if (document.activeElement !== $("vol-cap")) $("vol-cap").value = i.volume_cap || 40;
}

export function drawAdvanced() {
  const i = app.info;
  const dbg = names(app.S.Debug);
  segmented(
    $("debug"),
    [
      ["default", "The image's"],
      ["on", "On"],
      ["off", "Off"],
    ],
    dbg[i.debug],
    (k) => act(() => app.sp.call("Debug", { mode: k.toUpperCase() }), false),
  );
  const g = names(app.S.Gate)[i.gate];
  $("gate-now").textContent = gateSentence(i);
  $("gate-btn").textContent = g === "open" ? "Ask for a press again" : "Open for good";
  $("gate-btn").onclick = () => act(() => app.sp.call("Lock", { open: g !== "open" }), false);
}
