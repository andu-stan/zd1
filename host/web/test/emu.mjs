// The page's WebAssembly against the emulator, in node: a WebUSB stand-in whose one device is the emulator's USB bridge
// (ZD1DFU_EMU=host:port, emu/renode/avr32/UC3Usb.cs: EP0 only), under nusb's browser backend. The page's code from
// connect on, without a browser:
//   zemu run --mcu -q --host 'node host/web/test/emu.mjs "$(nix build --no-link --print-out-paths .#web)"'
// After `zd1 bootloader` (another --host step before it) the device is the bootloader, and this checks the page's
// side of DFU instead.
import fs from "node:fs";
import net from "node:net";
import path from "node:path";

if (!process.argv[2]) {
  console.error("usage: node emu.mjs PAGE_DIR (nix build --no-link --print-out-paths .#web)");
  process.exit(2);
}
const dir = path.resolve(process.argv[2]);
const [host, port] = (process.env.ZD1DFU_EMU ?? "").split(":");
if (!port) {
  console.error("ZD1DFU_EMU=host:port (zemu run --mcu --host ...)");
  process.exit(2);
}

// ---- the bridge: 'R' bus reset | 'S' setup[8] + OUT data | 'Z' us32; answers 'A', 'D' len16 data, 'X' (stall) ----
const sock = net.createConnection({ host, port: +port });
await new Promise((res, rej) => {
  sock.once("connect", res);
  sock.once("error", rej);
});
let buf = Buffer.alloc(0),
  waiting = null;
sock.on("data", (d) => {
  buf = Buffer.concat([buf, d]);
  waiting?.();
});
async function take(n) {
  while (buf.length < n)
    await new Promise((res) => {
      waiting = res;
    });
  const r = buf.subarray(0, n);
  buf = buf.subarray(n);
  return r;
}
let queue = Promise.resolve();
function bridge(msg) {
  // one exchange at a time
  const p = queue.then(async () => {
    sock.write(msg);
    const k = (await take(1))[0];
    if (k === 0x41) return { ok: true, data: Buffer.alloc(0) };
    if (k === 0x44) return { ok: true, data: Buffer.from(await take((await take(2)).readUInt16LE(0))) };
    return { ok: false };
  });
  queue = p.catch(() => {});
  return p;
}
const TYPE = { standard: 0, class: 1, vendor: 2 },
  RCPT = { device: 0, interface: 1, endpoint: 2, other: 3 };
function setup(s, dirIn, len, data = Buffer.alloc(0)) {
  const h = Buffer.alloc(9);
  h[0] = 0x53;
  h[1] = (dirIn ? 0x80 : 0) | (TYPE[s.requestType] << 5) | RCPT[s.recipient];
  h[2] = s.request;
  h.writeUInt16LE(s.value, 3);
  h.writeUInt16LE(s.index, 5);
  h.writeUInt16LE(len, 7);
  return bridge(Buffer.concat([h, data]));
}

// ---- the device, as WebUSB shows one --------------------------------------------------------------------------------
await bridge(Buffer.from("R"));
const desc = async (type, len) =>
  (await setup({ requestType: "standard", recipient: "device", request: 6, value: type << 8, index: 0 }, true, len))
    .data;
const dd = await desc(1, 18);
const cfg = await desc(2, (await desc(2, 9)).readUInt16LE(2));
const vid = dd.readUInt16LE(8),
  pid = dd.readUInt16LE(10);
// our firmware wants an address and a configuration before interface requests (ASF), as a host OS gives it
for (const [req, val] of [
  [5, 1],
  [9, 1],
])
  await setup({ requestType: "standard", recipient: "device", request: req, value: val, index: 0 }, false, 0);
const interfaces = [];
for (let i = 0; i < cfg.length; i += cfg[i]) {
  if (cfg[i + 1] === 4 && cfg[i + 3] === 0) {
    interfaces.push({
      interfaceNumber: cfg[i + 2],
      claimed: false,
      alternates: [
        {
          alternateSetting: 0,
          interfaceClass: cfg[i + 5],
          interfaceSubclass: cfg[i + 6],
          interfaceProtocol: cfg[i + 7],
          interfaceName: null,
          endpoints: [],
        },
      ],
    });
  }
}
const configuration = { configurationValue: 1, configurationName: null, interfaces };
const device = {
  vendorId: vid,
  productId: pid,
  deviceClass: dd[4],
  deviceSubclass: dd[5],
  deviceProtocol: dd[6],
  deviceVersionMajor: dd[13],
  deviceVersionMinor: dd[12] >> 4,
  deviceVersionSubminor: dd[12] & 15,
  usbVersionMajor: dd[3],
  usbVersionMinor: dd[2] >> 4,
  usbVersionSubminor: dd[2] & 15,
  manufacturerName: null,
  productName: null,
  serialNumber: null,
  opened: false,
  configuration,
  configurations: [configuration],
  async open() {
    this.opened = true;
  },
  async close() {
    this.opened = false;
  },
  async selectConfiguration() {},
  async reset() {},
  async claimInterface(n) {
    interfaces.find((i) => i.interfaceNumber === n).claimed = true;
  },
  async releaseInterface(n) {
    interfaces.find((i) => i.interfaceNumber === n).claimed = false;
  },
  async controlTransferIn(s, len) {
    const r = await setup(s, true, len);
    if (!r.ok) return { status: "stall" };
    const ab = r.data.buffer.slice(r.data.byteOffset, r.data.byteOffset + r.data.length);
    return { status: "ok", data: new DataView(ab) };
  },
  async controlTransferOut(s, data) {
    const d = Buffer.from(data ? new Uint8Array(data.buffer ?? data, data.byteOffset ?? 0, data.byteLength) : []);
    const r = await setup(s, false, d.length, d);
    return r.ok ? { status: "ok", bytesWritten: d.length } : { status: "stall", bytesWritten: 0 };
  },
  async transferOut() {
    throw new DOMException("the emulator's bridge carries EP0 only", "NotSupportedError");
  },
};

// what nusb's browser backend looks for: a Window whose navigator has usb
globalThis.Window = class Window {};
Object.setPrototypeOf(globalThis, globalThis.Window.prototype);
Object.defineProperty(globalThis.navigator, "usb", {
  value: {
    async getDevices() {
      return [device];
    },
    addEventListener() {},
  },
});

// ---- the page's side ------------------------------------------------------------------------------------------------
const wasm = await import(path.join(dir, "pkg", "zd1_web.js"));
wasm.initSync({ module: fs.readFileSync(path.join(dir, "pkg", "zd1_web_bg.wasm")) });
const log = [];
const events = {
  text: (s) => log.push(s),
  prompt: (s) => console.log("prompt:", s),
  step: (s) => console.log("step:", s),
  phase: (n, of, w) => console.log(`phase ${n}/${of}:`, w),
  progress() {},
  cancelled: () => false,
  wantsDevice: (_v, _p, w) => console.log("wants", w),
  backup() {},
  console: (b) => consoleOut.push(...b),
};
const consoleOut = [];
let fails = 0;
const check = (ok, what) => {
  console.log(ok ? "ok  " : "FAIL", what);
  if (!ok) fails++;
};
const S = wasm.spec();
if (pid === S.usb.dfuPid) {
  const bl = await wasm.Bootloader.connect(events);
  check(!!bl && !(await wasm.Speaker.connect(events)), "the bootloader, and no firmware");
  const st = await bl.state();
  check(
    typeof st.module === "string" && st.boot.length > 0,
    `state: the module ${st.module}, the firmware's last boot reached ${st.boot.at(-1)?.stage}`,
  );
  sock.end();
  process.exit(fails ? 1 : 0);
}
const sp = await wasm.Speaker.connect(events);
check(!!sp, `connected to ${vid.toString(16)}:${pid.toString(16)}`);
const h = sp.hello();
check(h.proto === S.proto && h.board === "zd1-emu", `hello: ${h.fw} ${h.board}, protocol ${h.proto}`);
const info = await sp.call("Info", {});
check(
  info.msg === "InfoReply" && typeof info.mod_os === "string",
  `info: state ${info.state}, module ${info.mod_os || info.mod_state}`,
);
const st = await sp.state();
check(
  st.msg === "State" && typeof st.name === "string" && typeof st.airplay === "boolean",
  `state: ${st.os}, slot ${st.slot}, name "${st.name}"`,
);
const keys = S.settings.map((f) => f.key);
check(keys.includes("name") && keys.includes("eth.mode") && "eth_mode" in st, `settings: ${keys.join(" ")}`);
const name = `web test ${Date.now() % 1000}`;
check((await sp.setSetting("name", name)) === name, "set the name");
check((await sp.call("GetSetting", { key: "name" })).value === name, "read it back");
await sp.setSetting("name", "");
const v = await sp.call("Volume", { volume: 12, bass: 2 });
const i2 = await sp.call("Info", {});
check(v.msg === "Ok" && i2.volume === 12 && i2.bass === 2, "volume and bass");
try {
  await sp.call("Select", { source: "NOPE" });
  check(false, "a bad enum name refused");
} catch (e) {
  check(/Select.source/.test(e.message), "a bad enum name refused");
}
// the Wi-Fi networks, the actions, the records
await sp.joinWifi("Web Home", "a passphrase");
await sp.joinWifi("Web Cafe", undefined);
let nets = await sp.list("Networks");
check(
  nets.map((n) => n.ssid).join() === "Web Cafe,Web Home" && nets[1].security === S.WifiSecurity.PSK,
  `known networks: ${nets.map((n) => n.ssid).join(", ")}`,
);
await sp.setSetting("wifi.first", "Web Home");
await sp.setSetting("wifi.forget", "Web Cafe");
nets = await sp.list("Networks");
check(nets.length === 1 && nets[0].ssid === "Web Home", "prefer one, forget the other");
try {
  await sp.setSetting("wifi.forget", "Nowhere");
  check(false, "forgetting an unknown network refused");
} catch (e) {
  check(/no known network/.test(e.message), `forgetting an unknown network refused: ${e.message}`);
}
try {
  await sp.list("Scan");
  check(false, "a scan without a radio refused");
} catch (e) {
  check(/radio/.test(e.message), `a scan without a radio refused: ${e.message}`);
}
check(
  Array.isArray(await sp.list("Pairings")) && Array.isArray(await sp.list("Servers").catch(() => [])),
  "pairings and servers read",
);
await sp.setSetting("airplay.pin", "3939");
await sp.setSetting("spotify.bitrate", "160");
const exported = await sp.exportSettings();
check(
  /airplay\.pin=3939/.test(exported) && /spotify\.bitrate=160/.test(exported) && !/wifi\.psk|Web Home/.test(exported),
  `export: ${exported.trim().split("\n").join(" ")}`,
);
await sp.setSetting("reset", "settings");
check(
  (await sp.state()).airplay_pin === "" && (await sp.list("Networks")).length === 1,
  "reset settings: the options gone, the network kept",
);
check(
  (await sp.importSettings(exported)) === exported.trim().split("\n").length &&
    (await sp.state()).airplay_pin === "3939",
  "import",
);
await sp.setSetting("reset", "all");
check((await sp.list("Networks")).length === 0 && (await sp.state()).airplay_pin === "", "reset all");
const p = await sp.call("Prefs", { amp_idle_s: 600 });
check(p.amp_idle_s === 600 && (await sp.call("Info", {})).amp_idle_s === 600, "the amp's idle time, 10 min");
try {
  await sp.call("Prefs", { amp_idle_s: 3600 });
  check(false, "an hour refused");
} catch (e) {
  check(e.code === "ARG", `an hour refused: ${e.message}`);
}
await sp.call("Prefs", { amp_idle_s: 300 });
const vc = await sp.call("Prefs", { volume_cap: 55 });
check(vc.volume_cap === 55 && (await sp.call("Info", {})).volume_cap === 55, "the volume cap, 55");
try {
  await sp.call("Prefs", { volume_cap: 72 });
  check(false, "a cap of 72 refused");
} catch (e) {
  check(e.code === "ARG", `a cap of 72 refused: ${e.message}`);
}
await sp.call("Prefs", { volume_cap: 40 });
await sp.call("Volume", { volume: 50, bass: 0 });
check((await sp.call("Info", {})).volume === 40, "the volume cap holds the host link's Volume too");
await sp.call("Prefs", { volume_cap: 30 });
check((await sp.call("Info", {})).volume === 30, "a lower cap takes the volume down to it");
await sp.call("Prefs", { volume_cap: 40 });
check(typeof (await sp.state()).stamp_a === "string", "the slots' stamps in the State record");
const boot = await sp.call("BootState", {});
check(boot.msg === "BootReply", `boot state: good ${boot.good}, last ${boot.last}`);
check(S.Source.WIFI === 3 && S.usb.dfuPid === 0x2ff8, "the spec's enums");
// the module's console: open, a line, its output as the page polls, closed
const co = await sp.consoleOpen(true);
check(co.open && (await sp.call("Info", {})).console_open, "the console open");
await sp.consoleWrite(new TextEncoder().encode("echo web-$((6*7))\n"));
for (let t = 0; t < 100 && !new TextDecoder().decode(new Uint8Array(consoleOut)).includes("web-42"); t++) {
  await sp.poll();
  await new Promise((r) => setTimeout(r, 50));
}
check(
  new TextDecoder().decode(new Uint8Array(consoleOut)).includes("web-42"),
  "a line on the console, its answer back",
);
const cc = await sp.consoleOpen(false);
check(!cc.open && cc.lost === 0 && !(await sp.call("Info", {})).console_open, "the console closed, nothing lost");
await sp.play("standby");
check((await sp.call("Info", {})).state === S.SysState.STANDBY, "play standby");
try {
  await sp.play("aux");
  check(false, "AUX without a plug refused");
} catch (e) {
  check(e.code === "ARG", `AUX without a plug refused: ${e.message}`);
}
const ws = wasm.wifiSentence(await sp.state(), await sp.list("Networks"));
const i3 = await sp.call("Info", {});
check(
  typeof ws.text === "string" &&
    ws.tone !== undefined &&
    wasm.ampSentence(i3) &&
    /open for good/.test(wasm.gateSentence(i3)),
  `sentences: ${ws.text} / the amp ${wasm.ampSentence(i3)} / the gate ${wasm.gateSentence(i3)}`,
);
const sys = path.join(process.cwd(), "emu/img/os/system.cpio.gz");
if (fs.existsSync(sys)) {
  const ii = wasm.imageInfo("system.cpio.gz", fs.readFileSync(sys));
  check(ii.kind === "system" && ii.stamp.startsWith(ii.version), `image info: ${ii.stamp}`);
}
console.log(`MCU log: ${log.join("").length} bytes`);
sock.end();
process.exit(fails ? 1 : 0);
