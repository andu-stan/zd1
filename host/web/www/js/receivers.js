// The receivers: AirPlay, Spotify Connect and Snapcast, each on or off with its options.
import { act, op } from "./connection.js";
import { app } from "./shared.js";
import { RECEIVERS } from "./speaker.js";
import { $, ask, el, notice } from "./ui.js";

// ---- receivers ----------------------------------------------------------------------------------------------------

export let servers = [],
  pairings = null;

export function drawReceivers() {
  const st = app.state;
  $("receivers").replaceChildren(
    ...RECEIVERS.map(([k, label]) => {
      const on = !!st[k];
      const who = app.S.Player[k.toUpperCase()];
      const status = !on
        ? "off"
        : st.front === who && st.front_state === app.S.PlayerState.PLAYING
          ? "playing"
          : (st.senders >> who) & 1
            ? "a sender is connected"
            : "waiting for a sender";
      const sw = el("button", { className: "switch", textContent: on ? "On" : "Off" });
      sw.setAttribute("aria-pressed", String(on));
      sw.onclick = () => act(() => app.sp.setSetting(k, on ? "off" : "on"));
      const head = el(
        "div",
        { className: "row" },
        el("strong", { textContent: label }),
        el("span", { className: `small ${status === "playing" ? "ok" : ""}`, textContent: status }),
        el("span", { className: "grow" }),
        sw,
      );
      const box = el("div", { className: "receiver" }, head);
      if (on) box.append(options(k));
      return box;
    }),
  );
}

export function options(k) {
  const st = app.state;
  const o = el("div", { className: "stack options" });
  if (k === "airplay") {
    const pin = el("input", {
      value: st.airplay_pin,
      placeholder: "none",
      maxLength: 4,
      inputMode: "numeric",
      pattern: "[0-9]{4}",
      className: "short",
    });
    const f = el(
      "form",
      { className: "row" },
      el("label", { textContent: "PIN" }),
      pin,
      el("button", { textContent: "Save" }),
      el("span", {
        className: "small",
        textContent: "with a PIN, senders pair once and are remembered; without, each connection pairs on its own",
      }),
    );
    f.onsubmit = (e) => {
      e.preventDefault();
      act(() => app.sp.setSetting("airplay.pin", pin.value.trim()));
    };
    o.append(f);
    const p = el("div", { className: "row" }, el("label", { textContent: "Paired" }));
    if (pairings === null) {
      const b = el("button", { className: "quiet", textContent: "Show" });
      b.onclick = () =>
        op(async () => {
          pairings = await app.sp.list("Pairings");
          drawReceivers();
        });
      p.append(b);
    } else {
      p.append(
        el("span", {
          textContent: pairings.length ? `${pairings.length} sender${pairings.length > 1 ? "s" : ""}` : "none",
        }),
      );
      if (pairings.length) {
        const b = el("button", { className: "quiet", textContent: "Forget them" });
        b.onclick = async () => {
          if (
            await ask(
              "Forget AirPlay's pairings?",
              "Every paired sender pairs again (with the PIN) next time.",
              "Forget",
            )
          )
            act(async () => {
              await app.sp.setSetting("airplay.forget", "yes");
              pairings = await app.sp.list("Pairings");
            });
        };
        p.append(b);
      }
    }
    o.append(p);
  }
  if (k === "spotify") {
    const s = el(
      "select",
      {},
      ...["96", "160", "320"].map((v) => el("option", { value: v, textContent: `${v} kb/s` })),
    );
    s.value = String(st.spotify_bitrate);
    s.onchange = () => act(() => app.sp.setSetting("spotify.bitrate", s.value));
    o.append(el("div", { className: "row" }, el("label", { textContent: "Quality" }), s));
  }
  if (k === "snapcast") {
    const cur = st.snapcast_server;
    const s = el(
      "select",
      {},
      el("option", { value: "", textContent: "found automatically (mDNS)" }),
      ...servers.map((x) =>
        el("option", { value: `${x.host}:${x.port}`, textContent: `${x.name} (${x.host}:${x.port})` }),
      ),
      el("option", { value: "?", textContent: "by hand…" }),
    );
    const hand = el("input", { value: cur, placeholder: "HOST or HOST:PORT", spellcheck: false });
    s.value = !cur ? "" : servers.some((x) => `${x.host}:${x.port}` === cur) ? cur : "?";
    hand.hidden = s.value !== "?";
    const find = el("button", { className: "quiet", textContent: "Find servers", type: "button" });
    find.onclick = () =>
      op(async () => {
        servers = await app.sp.list("Servers");
        if (!servers.length) {
          await new Promise((r) => setTimeout(r, 2000));
          servers = await app.sp.list("Servers");
        }
        if (!servers.length) notice("No snapserver answered on the speaker's networks.");
        drawReceivers();
      });
    const f = el(
      "form",
      { className: "row" },
      el("label", { textContent: "Server" }),
      s,
      hand,
      el("button", { textContent: "Save" }),
      find,
    );
    s.onchange = () => {
      hand.hidden = s.value !== "?";
    };
    f.onsubmit = (e) => {
      e.preventDefault();
      act(() => app.sp.setSetting("snapcast.server", s.value === "?" ? hand.value.trim() : s.value));
    };
    o.append(f);
    const lat = el("input", { type: "number", min: 0, max: 2000, value: st.snapcast_latency, className: "short" });
    const g = el(
      "form",
      { className: "row" },
      el("label", { textContent: "Latency" }),
      lat,
      el("span", { className: "small", textContent: "ms later than the server says (to line up with other speakers)" }),
      el("button", { textContent: "Save" }),
    );
    g.onsubmit = (e) => {
      e.preventDefault();
      act(() => app.sp.setSetting("snapcast.latency", String(+lat.value || 0)));
    };
    o.append(g);
    const web = cur ? cur.split(":")[0] : servers[0]?.host;
    if (web)
      o.append(
        el(
          "p",
          { className: "small" },
          "The server's own settings: ",
          el("a", {
            href: `http://${web}:1780`,
            target: "_blank",
            rel: "noopener",
            textContent: `its web page (${web}:1780)`,
          }),
          " (a page served over HTTPS can't reach it itself).",
        ),
      );
  }
  return o;
}
