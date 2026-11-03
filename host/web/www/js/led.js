// The speaker's LED as the page draws it: Diag's `led` (mcu/src/led.h: source | act << 3 | kind << 6) and the
// receiver in front, to <zd-speaker>'s `look` and `led` (a colour name of tokens.css: --led-NAME), as led.c does.
// The choices on the page (sources, receivers) take the same colours for their lamps.
const KIND = ["boot", "standby", "error", "run"]; // led_kind_t
const ACT = ["playing", "ready", "asleep", "nonet", "starting", "choosing"]; // led_act_t
const SOURCE = ["red", "usb", "aux", "white", "ipod"]; // source_t: none, USB, AUX, Wi-Fi, iPod
const PLAYER = ["white", "white", "spotify", "snapcast", "white"]; // ZD1_PLAYER_*: none, AirPlay, Spotify, Snapcast, other

/// { look, led } for Diag's `led` and Info's `player`.
export function ledLook(code, player) {
  const source = code & 7,
    act = ACT[(code >> 3) & 7] ?? "playing",
    kind = KIND[(code >> 6) & 3];
  if (kind !== "run") return { look: kind, led: "red" };
  if (source === 3)
    return { look: act, led: act === "nonet" || act === "starting" ? "white" : (PLAYER[player] ?? "white") };
  return { look: act, led: SOURCE[source] ?? "red" };
}

/// The lamp colour of a choice on the page (a source or a receiver, by the names speaker.js uses).
export const lampOf = (choice) =>
  `var(--led-${{ usb: "usb", aux: "aux", wifi: "white", airplay: "white", spotify: "spotify", snapcast: "snapcast", standby: "red" }[choice] ?? "white"})`;
