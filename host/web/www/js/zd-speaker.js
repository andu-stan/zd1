// <zd-speaker>: the speaker as a line drawing, its outline and the line down its middle, doing what the speaker does.
//   mode="welcome"  nothing connected: the outline in grey, a light running round it
//   mode="dfu"      the bootloader: the outline marching, waiting for firmware
//   mode="app"      running: the outline lit, its LED as a glow on the line (led, look), sound off both ends
//                   while it plays (vol, 0..1: how far and how strong)
//   led="usb"       the LED's colour, a name from tokens.css (--led-NAME)
//   look="playing"  the LED's animation: playing ready asleep standby boot error nonet starting choosing (led.js)
//   press           the gate's purple blink: a press of ON/OFF is wanted
//   progress="0.4"  a job's progress, drawn round the outline (absent: no job)
//   flat            without the reflection under it
// Its colours are the page's tokens (they inherit into the shadow root); design.html shows every state.
const SHAPE = "M40 118C40 82 170 32 340 32S640 82 640 118 510 204 340 204 40 154 40 118Z";
const WAVE_L = "M28 94Q14 118 28 142",
  WAVE_R = "M652 94Q666 118 652 142";

const template = document.createElement("template");
template.innerHTML = `
<style>
  :host { display: block; max-width: 620px; margin: 0 auto; --led: transparent; --vol: 0.4; }
  /* the floor it stands on: a reflection, as product pictures had them */
  svg { display: block; width: 100%; overflow: visible;
    -webkit-box-reflect: below 2px linear-gradient(transparent 55%, rgb(255 255 255 / 0.28)); }
  :host([flat]) svg { -webkit-box-reflect: unset; }
  .line, .centre { fill: none; stroke: var(--grey-500); stroke-width: 1.5; stroke-linecap: round;
    transition: stroke var(--slow) var(--ease); }
  .glow { fill: none; stroke: var(--led); stroke-width: 5; filter: blur(5px); opacity: 0;
    transition: opacity var(--slow) var(--ease), stroke var(--slow) var(--ease); }
  .run, .progress, .waves { fill: none; stroke-linecap: round; }
  .run { stroke: var(--grey-1000); stroke-width: 1.5; stroke-dasharray: 7 93; opacity: 0; }
  .progress { stroke: var(--grey-1000); stroke-width: 2.5; filter: drop-shadow(0 0 3px rgb(255 255 255 / 0.7));
    transition: stroke-dasharray var(--medium) var(--ease); }
  :host(:not([progress])) .progress { display: none; }
  .waves path { stroke: var(--grey-800); stroke-width: 1.5; opacity: 0; }

  /* nothing connected: the outline drawn in, then a light running round it */
  :host([mode="welcome"]) .line { stroke-dasharray: 100 100; animation: draw 2.2s var(--ease-out) both; }
  :host([mode="welcome"]) .run { opacity: 0.9; animation: run 7s linear 2.2s infinite; }
  @keyframes draw { from { stroke-dashoffset: 100; } }
  @keyframes run { from { stroke-dashoffset: 100; } to { stroke-dashoffset: 0; } }

  /* the bootloader: the outline marching */
  :host([mode="dfu"]) .line { stroke: var(--grey-700); stroke-dasharray: 0.6 0.9; animation: march 30s linear infinite; }
  @keyframes march { to { stroke-dashoffset: -100; } }

  /* running: the line lit, the LED's glow on it */
  :host([mode="app"]) .line, :host([mode="app"]) .centre { stroke: var(--grey-900); }
  :host([mode="app"]) .glow { opacity: 0.9; }
  :host([look="standby"]) .glow { opacity: 0.3; }
  :host([look="asleep"]) .glow { opacity: 0.4; }
  :host([look="ready"]) { --half: 2s; --lo: 0.15; --hi: 0.85; }
  :host([look="boot"]) { --half: 1.25s; --lo: 0.1; --hi: 1; }
  :host([look="nonet"]) { --half: 0.8s; --lo: 0; --hi: 0.8; }
  :host([look="starting"]) { --half: 1.2s; --lo: 0.1; --hi: 0.65; }
  :host([look="choosing"]) { --half: 0.25s; --lo: 0.45; --hi: 1; }
  :host(:is([look="ready"], [look="boot"], [look="nonet"], [look="starting"], [look="choosing"])) .glow {
    animation: breathe var(--half) ease-in-out infinite alternate; }
  @keyframes breathe { from { opacity: var(--lo); } to { opacity: var(--hi); } }
  :host([look="error"]) .glow, :host([press]) .glow { animation: blink 0.2s steps(1) infinite; }
  :host([press]) { --led: var(--led-purple) !important; }
  @keyframes blink { 50% { opacity: 0; } }

  /* playing: sound off both ends, as far and as strong as the volume */
  :host([look="playing"]) .waves path { animation: wave-l 1.8s var(--ease-out) infinite; }
  :host([look="playing"]) .waves .r path { animation-name: wave-r; }
  .waves path:nth-child(2) { animation-delay: 0.6s !important; }
  .waves path:nth-child(3) { animation-delay: 1.2s !important; }
  @keyframes wave-l {
    from { opacity: calc(0.15 + 0.7 * var(--vol)); transform: translateX(0); }
    to { opacity: 0; transform: translateX(calc(-12px - 44px * var(--vol))); } }
  @keyframes wave-r {
    from { opacity: calc(0.15 + 0.7 * var(--vol)); transform: translateX(0); }
    to { opacity: 0; transform: translateX(calc(12px + 44px * var(--vol))); } }
  @media (prefers-reduced-motion: reduce) { * { animation: none !important; } }
</style>
<svg viewBox="0 0 680 236" aria-hidden="true">
  <defs><path id="shape" pathLength="100" d="${SHAPE}"/></defs>
  <use class="glow" href="#shape"/>
  <g class="waves">
    <g><path d="${WAVE_L}"/><path d="${WAVE_L}"/><path d="${WAVE_L}"/></g>
    <g class="r"><path d="${WAVE_R}"/><path d="${WAVE_R}"/><path d="${WAVE_R}"/></g>
  </g>
  <use class="line" href="#shape"/>
  <path class="centre" d="M340 32V204"/>
  <use class="run" href="#shape"/>
  <use class="progress" href="#shape" stroke-dasharray="0 100"/>
</svg>`;

class ZdSpeaker extends HTMLElement {
  static observedAttributes = ["led", "vol", "progress"];
  constructor() {
    super();
    this.attachShadow({ mode: "open" }).append(template.content.cloneNode(true));
  }
  attributeChangedCallback(name, _old, v) {
    if (name === "led") this.style.setProperty("--led", v ? `var(--led-${v})` : "transparent");
    if (name === "vol") this.style.setProperty("--vol", v ?? "0");
    if (name === "progress")
      this.shadowRoot.querySelector(".progress").setAttribute("stroke-dasharray", `${(+v || 0) * 100} 100`);
  }
}
customElements.define("zd-speaker", ZdSpeaker);
