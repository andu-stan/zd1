// The network: the Wi-Fi sentence, the known networks, a scan, joining, the addresses.
import { wifiSentence } from "../pkg/zd1_web.js";
import { act } from "./connection.js";
import { drawReceivers } from "./receivers.js";
import { app } from "./shared.js";
import { $, ask, el, names, notice } from "./ui.js";

// ---- network ------------------------------------------------------------------------------------------------------

/// Signal bars from -dBm (57 = -57 dBm): four at -55 or better, one under -75.
export const bars = (dbm) => {
  const n = dbm <= 55 ? 4 : dbm <= 65 ? 3 : dbm <= 75 ? 2 : 1;
  const s = el("span", { className: "bars", title: `-${dbm} dBm` });
  s.setAttribute("aria-label", `signal ${n} of 4`);
  for (let k = 1; k <= 4; k++) s.append(el("i", { className: k <= n ? "on" : "" }));
  return s;
};

/// The Wi-Fi as a sentence (host/core's): [text, "ok" | "warn" | ""].
export function wifiStatus(st) {
  const w = wifiSentence(st, app.known);
  return [w.text, w.tone];
}

export function drawSettings() {
  const st = app.state;
  if (document.activeElement !== $("name")) $("name").value = st.name ?? "";
  const [text, kind] = wifiStatus(st);
  $("wifi-now").replaceChildren(
    el("span", { className: kind, textContent: text }),
    ...(st.wifi === app.S.WifiState.CONNECTED && st.signal ? [bars(st.signal)] : []),
  );
  const eth = names(app.S.NetMode)[st.eth_mode];
  $("eth-now").textContent =
    eth === "off"
      ? "Off."
      : !st.eth_link
        ? "No cable."
        : st.eth_ip
          ? `Cable connected, address ${st.eth_ip}.`
          : "Cable connected, waiting for an address.";
  $("wifi-toggle").textContent = names(app.S.NetMode)[st.wifi_mode] === "off" ? "Wi-Fi on" : "Wi-Fi off";
  drawKnown();
  drawAddresses();
  drawReceivers();
}

export function drawKnown() {
  const joined = app.state.wifi === app.S.WifiState.CONNECTED ? app.state.wifi_ssid : null;
  $("known").replaceChildren(
    ...(app.known.length
      ? app.known.map((n, k) => {
          const r = el(
            "div",
            { className: "row item" },
            el("span", { className: "num", textContent: `${k + 1}.` }),
            el("strong", { textContent: n.ssid }),
            el("span", { className: "tag", textContent: n.security === app.S.WifiSecurity.PSK ? "WPA2" : "open" }),
            n.ssid === joined ? el("span", { className: "tag ok", textContent: "joined" }) : null,
            el("span", { className: "grow" }),
          );
          if (k > 0) {
            const up = el("button", { className: "quiet", textContent: "Prefer" });
            up.title = "Put it first: the speaker joins it when it sees it";
            up.onclick = () => act(() => app.sp.setSetting("wifi.first", n.ssid).then(() => joinWatch(n.ssid)));
            r.append(up);
          }
          const f = el("button", { className: "quiet", textContent: "Forget" });
          f.onclick = async () => {
            if (
              await ask(
                `Forget ${n.ssid}?`,
                "The speaker no longer joins it; its passphrase is deleted on the speaker.",
                "Forget",
              )
            )
              act(() => app.sp.setSetting("wifi.forget", n.ssid));
          };
          r.append(f);
          return r;
        })
      : [el("p", { className: "small", textContent: "None yet." })]),
  );
}

export async function scan() {
  const list = $("scan-list");
  list.hidden = false;
  list.replaceChildren(el("p", { className: "small", textContent: "Scanning…" }));
  let nets;
  try {
    nets = await app.sp.list("Scan");
    if (!nets.length) {
      await new Promise((r) => setTimeout(r, 4000));
      nets = await app.sp.list("Scan");
    }
  } catch (e) {
    list.replaceChildren(el("p", { className: "warn", textContent: e.message }));
    return;
  }
  const sec = names(app.S.WifiSecurity);
  list.replaceChildren(
    ...(nets.length
      ? nets.map((n) => {
          const s = sec[n.security];
          const can = s === "open" || s === "psk";
          const r = el(
            "button",
            { className: "row item net", disabled: !can },
            bars(n.signal),
            el("strong", { textContent: n.ssid }),
            el("span", {
              className: "tag",
              textContent:
                s === "psk" ? "WPA2" : s === "open" ? "open" : s === "sae" ? "WPA3 only" : "WEP or enterprise",
            }),
            app.known.some((k) => k.ssid === n.ssid) ? el("span", { className: "tag ok", textContent: "known" }) : null,
            el("span", { className: "grow" }),
            can
              ? null
              : el("span", {
                  className: "small",
                  textContent: s === "sae" ? "this radio can't do WPA3" : "not supported",
                }),
          );
          r.type = "button";
          r.onclick = () => joinForm(n.ssid, s === "open");
          return r;
        })
      : [el("p", { className: "small", textContent: "No network seen. The radio hears 2.4 GHz networks only." })]),
    el("button", { className: "quiet", textContent: "Scan again", onclick: scan }),
  );
}

export function joinForm(ssid, open) {
  $("join-form").hidden = false;
  $("ssid").value = ssid ?? "";
  $("ssid").readOnly = ssid !== null;
  $("open").checked = !!open;
  $("open-row").hidden = ssid !== null;
  $("pass-row").hidden = !!open;
  $("pass").value = "";
  $("pass").required = !open;
  (ssid !== null && !open ? $("pass") : $("ssid")).focus();
}

/// After a join: the State record until the speaker is on the network, or says why not (30 s at most).
export async function joinWatch(ssid) {
  for (let t = 0; t < 15; t++) {
    await new Promise((r) => setTimeout(r, 2000));
    if (!app.sp) return;
    let st;
    try {
      st = await app.sp.state();
    } catch {
      return;
    }
    app.state = st;
    drawSettings();
    const w = names(app.S.WifiState)[st.wifi];
    if (w === "connected" && st.wifi_ssid === ssid)
      return notice(`The speaker is on ${ssid}${st.wifi_ip ? ` at ${st.wifi_ip}` : ""}.`, "ok");
    if (w === "wrongkey" || w === "failed" || (w === "notfound" && t > 4)) return notice(wifiStatus(st)[0], "warn");
    if (w === "off") return;
  }
  notice(`No word from the speaker on ${ssid} yet: the Wi-Fi line above follows it.`);
}

export function drawAddresses() {
  const st = app.state;
  const modes = names(app.S.NetMode);
  $("addr").replaceChildren(
    ...[
      ["wifi", "Wi-Fi"],
      ["eth", "Ethernet"],
    ].map(([i, label]) => {
      const f = el("form", { className: "stack addr" });
      const mode = el(
        "select",
        {},
        ...["dhcp", "static", "off"].map((m) =>
          el("option", { value: m, textContent: m === "dhcp" ? "automatic (DHCP)" : m === "static" ? "fixed" : "off" }),
        ),
      );
      mode.value = modes[st[`${i}_mode`]];
      const input = (k, ph) => el("input", { value: st[`${i}_${k}`] ?? "", placeholder: ph, spellcheck: false });
      const [a, g, d] = [input("address", "192.0.2.20/24"), input("gateway", "192.0.2.1"), input("dns", "192.0.2.1")];
      const fixed = el(
        "div",
        { className: "stack" },
        el("div", { className: "row" }, el("label", { textContent: "Address" }), a),
        el("div", { className: "row" }, el("label", { textContent: "Gateway" }), g),
        el("div", { className: "row" }, el("label", { textContent: "DNS" }), d),
      );
      fixed.hidden = mode.value !== "static";
      mode.onchange = () => {
        fixed.hidden = mode.value !== "static";
      };
      f.append(
        el(
          "div",
          { className: "row" },
          el("label", { textContent: label }),
          mode,
          el("button", { textContent: "Save" }),
        ),
        fixed,
      );
      f.onsubmit = (e) => {
        e.preventDefault();
        act(async () => {
          if (mode.value === "static") {
            await app.sp.setSetting(`${i}.address`, a.value.trim());
            await app.sp.setSetting(`${i}.gateway`, g.value.trim());
            await app.sp.setSetting(`${i}.dns`, d.value.trim());
          }
          await app.sp.setSetting(`${i}.mode`, mode.value);
          notice(
            `${label}: ${mode.value === "static" ? `fixed at ${a.value.trim()}` : mode.value === "dhcp" ? "automatic" : "off"}.`,
            "ok",
          );
        });
      };
      return f;
    }),
  );
}
