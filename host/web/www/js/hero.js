// The top of the page: <zd-speaker> (js/zd-speaker.js), the speaker's name and its status line with the LED's lamp.
// connection.js sets the view and a job's progress, speaker.js what the running speaker does.
import { $ } from "./ui.js";

/// The view ("welcome", "dfu", "app"), a title, a status line; `led` and `look` from led.js, `vol` 0..1 (app only).
export function hero({ mode, title, status, led = null, look = null, vol = 0 }) {
  const a = $("art");
  if (a.getAttribute("mode") !== mode) a.setAttribute("mode", mode);
  for (const [k, v] of [
    ["led", led],
    ["look", look],
  ])
    if (v === null) a.removeAttribute(k);
    else if (a.getAttribute(k) !== v) a.setAttribute(k, v);
  a.setAttribute("vol", vol.toFixed(2));
  $("title").textContent = title;
  $("pill").textContent = status;
  $("status-lamp").style.setProperty("--lamp", led ? `var(--led-${led})` : "var(--grey-500)");
  $("status-lamp").classList.toggle("on", !!led);
}

/// A job's progress round the speaker's outline, 0..1; null: no job.
export function heroProgress(x) {
  if (x === null) $("art").removeAttribute("progress");
  else $("art").setAttribute("progress", String(x));
}

// The gate's press: the purple blink while its dialog is open.
new MutationObserver(() => $("art").toggleAttribute("press", $("press").open)).observe($("press"), {
  attributes: true,
  attributeFilter: ["open"],
});
