// The catalogue (design.html): swatches from the token names, the sources' segmented control, the sliders' fill.
import { lampOf } from "./led.js";
import { fill, segmented } from "./ui.js";
import "./zd-speaker.js";

for (const box of document.querySelectorAll(".swatches")) {
  const lamps = box.classList.contains("lamps");
  for (const t of box.dataset.tokens.split(" ")) {
    const i = document.createElement("i");
    if (lamps) {
      i.className = "lamp on";
      i.style.setProperty("--lamp", `var(--${t})`);
    } else i.style.background = `var(--${t})`;
    const s = document.createElement("span");
    s.className = "swatch";
    s.append(i, `--${t}`);
    box.append(s);
  }
}
const sources = [
  ["usb", "USB"],
  ["aux", "AUX"],
  ["wifi", "Network"],
  ["airplay", "AirPlay"],
  ["spotify", "Spotify Connect"],
  ["snapcast", "Snapcast"],
  ["standby", "Standby"],
];
const pick = (cur) =>
  segmented(
    document.getElementById("demo-sources"),
    sources.map(([k, l]) => [k, l, lampOf(k)]),
    cur,
    pick,
  );
pick("spotify");
for (const r of document.querySelectorAll('input[type="range"]')) {
  fill(r);
  r.addEventListener("input", () => {
    fill(r);
    r.nextElementSibling.textContent = r.value;
  });
}
