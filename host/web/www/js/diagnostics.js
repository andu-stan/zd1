// Diagnostics: the internals at a glance, and the report for a person helping.
import { op } from "./connection.js";
import { app } from "./shared.js";
import { $, download, el, names, stamp } from "./ui.js";

// ---- diagnostics --------------------------------------------------------------------------------------------------

export function drawDiag(d) {
  const rows = [
    ["Temperature", `ADC5 0x${d.adc5.toString(16)}, ADC4 0x${d.adc4.toString(16)} (they fall as it warms)`],
    [
      "Amp supply",
      `ADC6 0x${d.adc6.toString(16)}${d.amp_unmuted ? ", unmuted" : ", muted"}${d.error ? `; protection ${d.error} tripped` : ""}`,
    ],
    ["DSP", `${d.dsp_running ? "running" : "off"}, input ${names(app.S.DspIn)[d.dsp_in]}`],
    [
      "USB audio",
      d.usb_streaming ? `streaming ${d.usb_rate} Hz, fill ${d.fill}/512, ${d.underruns} underruns` : "not streaming",
    ],
    ["Network", d.net ? "up" : "down"],
  ];
  $("diag").replaceChildren(...rows.flatMap(([k, v]) => [el("dt", { textContent: k }), el("dd", { textContent: v })]));
}

/// One file with everything a person helping would ask for: versions, the speaker's state, diagnostics, the slots, the
/// module's State record (no secrets: they read back as "(set)"), the MCU's log.
export async function report() {
  const parts = await op(async () => {
    const r = {
      hello: app.sp.hello(),
      info: await app.sp.call("Info", {}),
      diag: await app.sp.call("Diag", {}),
      boot: await app.sp.call("BootState", {}),
    };
    try {
      r.state = await app.sp.state();
    } catch (e) {
      r.state = String(e.message);
    }
    try {
      r.networks = (await app.sp.list("Networks")).map((n) => n.ssid);
    } catch {}
    return r;
  });
  if (!parts) return;
  const text =
    `zd1 report, ${new Date().toISOString()}\n\n` +
    Object.entries(parts)
      .map(([k, v]) => `== ${k}\n${JSON.stringify(v, (_, x) => (x instanceof Uint8Array ? [...x] : x), 2)}`)
      .join("\n\n") +
    `\n\n== the MCU's log (this page's copy)\n${$("log").textContent}\n`;
  download(`zd1-report-${stamp()}.txt`, new Blob([text], { type: "text/plain" }));
}
