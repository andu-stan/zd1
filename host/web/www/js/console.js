// The module's console over the host link (Advanced): its bytes as they come while the pane is unfolded, a line at a
// time to it, Ctrl-C, BL1's escape into BL2 (the module's way back from a broken image: BL2's web updater).
import { job, op } from "./connection.js";
import { app } from "./shared.js";
import { $, ask } from "./ui.js";

const utf8 = new TextDecoder();
let poller = 0;

/// What the console said: terminal escapes and carriage returns out, the pane kept to its last 60000 characters.
export function consoleText(bytes) {
  const p = $("console");
  const end = p.scrollTop + p.clientHeight >= p.scrollHeight - 4;
  const s = utf8
    .decode(bytes, { stream: true })
    // biome-ignore lint/suspicious/noControlCharactersInRegex: the terminal's escapes are what it matches
    .replace(/\x1b\[[0-9;?]*[A-Za-z]/g, "")
    .replace(/\r/g, "");
  p.textContent = (p.textContent + s).slice(-60000);
  if (end) p.scrollTop = p.scrollHeight;
}

/// Open while the pane is unfolded: polled often, so typing feels like a terminal; closed when folded (or by the MCU
/// once the page stops polling).
async function openIt(open) {
  clearInterval(poller);
  if (!app.sp) return;
  await op(() => app.sp.consoleOpen(open));
  if (open)
    poller = setInterval(() => {
      if (app.sp && !app.busy) app.sp.poll().catch(() => {});
    }, 50);
}

const send = (s) => op(() => app.sp.consoleWrite(new TextEncoder().encode(s)));

export function consoleInit() {
  $("console-pane").ontoggle = () => openIt($("console-pane").open);
  $("console-line").onkeydown = (e) => {
    if (e.key !== "Enter") return;
    send(`${e.target.value}\n`);
    e.target.value = "";
  };
  $("console-ctrlc").onclick = () => send("\x03");
  $("console-esc").onclick = async () => {
    if (
      !(await ask(
        "Into BL2",
        "The module restarts into its bootloader's menu, whatever its image, and from there into BL2 (its web updater " +
          "on 169.254.1.1). Whatever plays stops. Back to the image: Restart the module.",
      ))
    )
      return;
    await job("Into BL2", false, async () =>
      (await app.sp.call("Module", { op: "ESC", timeout_s: 0 })).ok
        ? "BL1 started BL2: its web updater is on 169.254.1.1 in ~11 s."
        : "No BL1 menu: the module boots as usual. Try again.",
    );
  };
  $("console-restart").onclick = () => op(() => app.sp.call("Module", { op: "RESTART", timeout_s: 0 }));
}
