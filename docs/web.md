# The web page: installer and configurator

`host/web` is the speaker for anyone with a browser and a USB cable: install, update, restore, and the settings.
`zd1` (`host/cli`) is the same flows from a terminal; both are `host/core` (`docs/module/updater.md` has the flows'
mechanics and measurements). This page says what the page does now, what it is to become, and the rules that keep a
speaker in a state the tools understand. Tags: `docs/README.md`.

## Now
`host/core` compiled to WebAssembly over WebUSB (nusb's browser backend), and a static page (`www/`, plain JavaScript,
no framework). `nix build .#web` is the page as a directory. It is hosted at https://pawlowskialex.github.io/zd1/ with the
releases' images (below, Releases); `python3 tools/zeppelin web` serves it on localhost with this checkout's built
images offered under `images/` instead (`index.json` names them). The three views below are built:
- **Not connected**: Connect (the chooser filters on our firmware 03EB:2FE0 and the DFU bootloader 03EB:2FF8), and
  a triage by what the speaker does: our firmware by its light, B&W's software (never in the list: the key-hold into
  DFU), a stopped update, and a speaker that isn't listed (cable, browsers, Windows not yet, the Linux udev rule).
- **Bootloader**: the boot log and the module's state, and one primary action from them: no boot of ours logged,
  **Install** the latest release (`Bootloader.install`: host/core's `install`, the `.bcd` when the release has one);
  ours ran, **Start**; ours stopped early, **Repair** (the release's MCU firmware again). Under it the backups this
  browser kept (restore, download, delete), **Roll back to B&W's software** (`Bootloader.rollback`, the stock `.bcd`
  and MCU image as files), and Advanced (start, an MCU file).
- **Running**: what people reach for first. On top, under the speaker's drawing and name: Playing (the sources,
  volume, bass, the amp's sentence). A banner when a release is newer than what runs, with its notes and one
  **Update** that brings the boards that are behind to it, the system then the MCU; nothing when up to date. Then
  Network (the Wi-Fi and Ethernet as sentences from the State record with the plain reason when it failed, the name,
  the known networks in their order with Prefer and Forget, Add a network from a scan or by hand, the radio's limits,
  each interface's addresses), Receivers (a row each: on/off, its status, its options: AirPlay's PIN and pairings,
  Spotify's bitrate, Snapcast's server from the servers found or by hand, its latency, a link to the snapserver's
  web page), Behaviour (USB auto-switch, the amp's lock and idle time), Diagnostics (save a report: versions, state,
  diagnostics, the log in one file), Software (both versions, the slots and what each holds, the latest release and
  its verdict, earlier releases, back to the other slot when it holds a system, restore the MCU firmware from the
  last backup kept, files), Advanced (the debug services, the gate, settings export and import, reset; the module's
  console: open while unfolded, a line at a time and Ctrl-C, into BL2 through BL1's menu (`Module ESC`), the module
  restarted [HW 2026-11-02; `host/web/test/emu.mjs` opens it, sends a line, reads the answer]).
One job strip (sticky) carries every long flow: its phases, progress, a stop where the flow allows it, the outcome,
and on a lost connection what to do. Outcomes and errors are notices in the page, never `alert()`; the press dialog
can be given up. An MCU install keeps its backup twice (host/core's tar, as `zd1` writes it): a download, and a copy in the browser
(IndexedDB); the flash waits for both, and a browser that gives the page no storage asks whether to go on with the
download alone. The page's WebAssembly ran against the emulator, firmware and
bootloader [EMU 2026-11-01] (`host/web/test/emu.mjs`: the known networks, the actions, export and import, reset, the
amp's idle time); against the unit in Brave on the Mac every view works [HW 2026-11-02].

The look is a design system (below, Design system): black and white, a 2012 Apple product page made today, with
the speaker drawn on top (`<zd-speaker>`): its outline and the line down its middle, reflected on the floor as
product pictures had them; running, its LED's look
as a glow on the line, read from Diag's `led` with each refresh (`js/led.js`: the colours and animations of
`mcu/src/led.c`), sound off both ends while it plays (as far as the volume), the gate's purple blink while the press
dialog is open, a long job's progress round the outline. `design.html` shows the system without a speaker.

What it can't do yet: run on Windows (no WinUSB descriptors in the firmware), install without a release manifest beside it (the bootloader view's Install wants a release;
files go one board at a time).

## Goals
- **Hosted on GitHub Pages** from the public repo, with its releases: a visitor installs, updates and restores with the
  page alone. Best UX, technically excellent: one flow per job, every outcome shown, nothing to guess.
- **Our MCU firmware drives our module OS, and only that.** The stock module app has nothing our OS lacks; who wants it
  rolls both boards back. What stays is the way our OS reaches the stock escape hatches: the MCU's DFU bootloader (the
  relay) and BL2 (HCR 0x21 from the rescue, BL1's serial menu over the console).
- **Interlocks on every lever**: a speaker never lands in a pairing the tools don't understand, and every state has a
  named way out.

## Design system
Black and white; colour is the speaker's LED and nothing else. Controls come in three tiers: primary filled with
black ink (white; the source playing in the colour the LED shows for it, the one control in colour), secondary an outline with ink
of the same colour, tertiary a link; disabled fades the same drawing. Lamps (small lit LEDs) mark what is reported:
the status line, tags, notices, the banner. Words are Helvetica Neue, thin and light at size; monospace is for what the
speaker reports (versions, hashes, addresses, logs: `.data`). The one nostalgic touch is the 2012 product picture's
floor: the drawing stands on its reflection.
- **Tokens** (`www/css/tokens.css`): primitives (the greys, the LED's palette as a screen shows it, the type, space,
  radius and motion scales) and roles (surfaces, text, lines, states, controls, wells). Components use roles only.
- **Layers** (`www/style.css`): `tokens < base < layout < components < page`, so order decides, not specificity.
  `base.css` styles elements, `layout.css` the row/stack primitives, `components/` one file per component, `page.css`
  this page's parts (the bar, the hero, the job strip, the footer).
- **Components** and their markup, which the modules build (`js/ui.js` has the builders that need code: `segmented`,
  `fill` for a slider): buttons (`button` secondary, `.primary` the one action a view leads to, `.danger`, `.quiet`
  a link; `select` and the file picker drawn the same), the segmented control and the switch
  (`button.switch[aria-pressed]`), filled when chosen (the sources in `--lamp`, the LED's colour for
  each: `led.js` `lampOf`; switches white), the checkbox, the lamp (`.lamp`, and the tags' and notices' own), fields, the slider, the panel (`section.panel` > `h2` +
  `.panel-body`) and the facts table, lists and receivers, tags and signal bars, notices, the banner, progress, dialogs, terminal
  panes, disclosures and the triage. `<zd-speaker>` (`js/zd-speaker.js`, a custom element: `mode`, `led`, `look`,
  `vol`, `press`, `progress`) is the drawing.
- **The catalogue** (`www/design.html`, linked from the footer): every token and component in each state, the speaker
  in every mode and look. A change to the system is checked there first; a new component gets a row there.

## Pairings
The MCU runs the stock app, our firmware, or sits in its DFU bootloader; the module runs the stock app or our app
partition (the boot stub, the kernel, the rescue) with the system slots. Two pairings are supported; the rest are
transit states inside a flow, each detected and each with its exit.

| | Stock module app | Our app partition and slots |
|---|---|---|
| **Stock MCU app** | **stock speaker**: the rollback target, untouched by us | transit: the stub gets no answer on reg 0xC4 and boots the rescue, which speaks the BL2 subset of the SPI protocol (status booting, HCR 0x21 into BL2) and nothing more; no sound. Exit: DFU (key-hold) and the rollback or the install flow |
| **Our MCU firmware** | transit: the MCU reads the stock version string on reg 0x25 and reports state STOCK: the module is no source, the LED says attention, the page offers the two exits (install our module software; restore the MCU backup) | **the product** |
| **DFU bootloader** | the relay's target | the relay's target; the stub boots the rescue meanwhile |

BL1 talks on the UART only (its menu: `zd1 module esc`); BL2 answers on SPI (status 0x2300, HCR) and on the UART.
Our MCU firmware and our OS keep speaking what those two need, which is the stock framing and a handful of registers:
the base layer below.

**The base layer** is what every version of ours speaks for good, so that any pairing of our own versions, MCU and
OS, old and new, the rescue included, can still boot, tell what it is and update. Its meaning never changes; new
things get new registers. SPI: status 0x11 (0x2000 booting, 0x2100 ready, 0x2300 BL2), the version string 0x25, the
link protocol 0xD5, the hello 0xC3, the boot choice 0xC4, the update 0xC5 with its UART frames, shutdown 0xE2,
HCR. USB: the host link's append-only rule (`spec.lock`), the bulk pipe, the vendor's
DFU. Above the base layer the speaker's features are versioned by 0xD5's protocol number: a mismatch is a reported
state ("update pending", both versions named), never silence. So the order of the two boards' updates doesn't matter
and a slot rolled back to an older system still updates. The MCU's EEPROM block (our own, magic and CRC, beside
stock's) and the settings file (keys only ever added, `docs/module/settings.md`) already survive any version in either
direction.

## Releases
A release is a pair, built from one tag by CI (nix on Linux runners, with a binary cache: the MCU needs Microchip's
x86_64 toolchain, the OS image Linux): the MCU firmware (`zd1.hex`), the system image (`system.cpio.gz`), and the
`.bcd` (stub, kernel, rescue: changes rarely, so most releases point at an earlier one), with notes. Versions are
commits and don't order: the page compares by position in the list, never by value.

The Pages workflow (`.github/workflows/pages.yml`) attaches a tag's
three images and their `manifest.json` (`zd1 image`) to the GitHub release, then builds the page, copies every
release's images beside it and writes `images/index.json`: the releases newest
first, each with its version, date, notes, and per image the file, size and SHA-256. Same origin: no GitHub API, no
rate limit, no CORS, and the page fetches an image only when it installs it (today it downloads both to read their
versions). The manifest keeps older releases, which is "install a previous version" for free. Stock images for the
rollback are B&W's own update files; the page doesn't serve them (uploading them is not ours to decide: `AGENTS.md`):
it takes them as a file, or fetches them from the archive.org items `downloads/manifest.json` lists if those answer
cross-origin.

## The three views
**Not connected** is a triage, not a button: the states a speaker can be in by its light and its behaviour, the
key-hold into DFU, "Connect", and the requirements (Chrome, Edge, Brave, Opera, Android Chrome too; Windows once
the firmware has WinUSB descriptors; Linux's udev rule shown to copy). A stock speaker under its own firmware never
shows up on the chooser: the triage says so and sends it into DFU.

**Bootloader** is the installer. From the boot log and the manifest it offers one primary action:
- **Install** on an empty or stock speaker: one flow in `host/core`, `install`, ~5 minutes with named phases on one
  progress strip. The relay of the `.bcd` (85 s, from the bootloader: nothing of the MCU app is needed), the MCU
  firmware with its backup, start, our firmware answers, the module's first boot finds the slots empty and starts the
  rescue (within the MCU's 180 s), the system update into slot A (~90 s), the trial, ready. It lands on the network
  card: the user is at the computer and the speaker has no network yet.
- **Repair** when ours is there but didn't start: the same firmware again, the boot log shown.
- **Restore** from the backups this page kept: the tar as a download as now, and a copy in the browser's storage
  listed by date and the version it holds, so nothing has to be found again.
- **Roll back to stock**: the mirror of Install, module first: the stock `.bcd` through the relay (382 s), then the
  MCU backup (or the stock hex), start. The page then sees nothing, since the stock firmware has no host link, and
  says so beforehand.
- Advanced, folded: a file install for either image, "start the firmware".

**Running** puts what people reach for first: what it plays (source, volume, bass) with the speaker on top, then
**Network** and **Receivers** (below), **Behaviour** (the MCU's preferences: auto-switch to USB, the amp's policy and
idle time), **Diagnostics** folded, with a "save a report" button (versions, diagnostics, both logs in one file).
Software is upkeep: up to date, it waits near the end (versions of both boards, the latest release and the verdict,
"back to slot X" and "restore the previous MCU firmware"); behind a release, a banner under the speaker says so with
the notes and one Update button that brings both boards to the release (system, then MCU: either order works, this
one keeps the page connected longest). An **Advanced** fold: the debug services, settings export and import, reset
to defaults, the module's console (its way back from a broken image without a terminal program).

Throughout: one persistent job strip while anything runs, its phases named from the flow's events, stop where the
flow allows it; errors in the page, never `alert()`; the press dialog can be given up; a connection lost mid-job
says what state the speaker is in and what to do.

## Network and receivers
The page can only be as good as what the module tells it. Two mechanisms carry it, and nothing else is added
(`docs/module/settings.md` has their detail):
1. **One schema.** The module's state is one typed record in the protocol spec, `State` (`docs/module/settings.md`):
   its settings (the fields marked `set`, written one at a time with `SetSetting`) and its status, pushed to the
   MCU on change and read whole by the host (`ModRecord`), the same record a module API would serve as JSON. The
   Wi-Fi's state, access point and signal are status fields, `zd1-linkd` fills them in from the supplicant's control
   socket (its status once a second, its events for the failures; the supplicant runs whenever the radio is on, with
   no network configured too, so a fresh speaker can scan). Join is a result (connected, the key is wrong, not
   seen), and every status line on the page reads the same record.
2. **Other records**, made when asked (`Record`): a scan entry (SSID, signal, security), the snapservers found
   over mDNS, the known networks, AirPlay's pairings; each a message of the spec, a list a sequence of them (a u16
   length before each). The known networks become a list with a priority by order; the supplicant joins the one it
   sees. Init coalesces changes: a service restarts once after the last key of a burst settles, not once per key.
   Actions that aren't a value (a network added, forgotten or preferred, AirPlay's pairings forgotten, reset to
   defaults) are a write to a key with no state of its own: the spec's `Actions` message.

Then the network section is a flow: a status card (connected or not, which interface and network, how strong, the
address, the plain reason when it failed), the known networks with their order, forget and join-now, and "Add a
network" from the scan (signal bars, a lock, WPA3-only entries greyed with the reason) or by hand for hidden ones.
The limits are stated where they bite: the radio is 802.11b/g, 2.4 GHz only; WPA2 and WPA3 transition networks join,
WPA3-only can't (Wireless Extensions has no SAE). Receivers get a row each: switch, status from `status`, their one or
two options as settings (`airplay.pin`, `snapcast.latency`, `spotify.bitrate`), Snapcast's server from `servers` or by
hand. The snapserver itself isn't configurable from here (an HTTPS page can't reach a plain-HTTP server on the LAN):
a link to its own web UI. Each interface has the same address settings (off, DHCP, static with gateway and DNS), so
the wired port gets a fixed address too; the page shows both.

## Interlocks
One authority: the MCU knows the module's state and the flows ask it; a flow never infers a state from timing or from
what it did last. The base layer makes every refusal a reported state with its exit.

| Lever | Check | Where |
|---|---|---|
| MCU firmware alone (not in Install or Roll back) | refused when it would leave our firmware over the stock module app; a stock hex (no identification) only inside Roll back | `host/core` |
| the module got ready | the stock version string: state STOCK; a link protocol the firmware doesn't know, older or newer: state "update pending" with both versions; the base layer works in both | MCU |
| Install, Roll back | module first, both ways; each phase's precondition read before it starts (DFU present, the module's state, the slot stamps) | `host/core` |
| `Update` | refused while the module is off, in BL2, booting, or an update runs; `Dfu` refused while an update streams | MCU |
| a slot trial | only slots with a stamp are offered; a slot the updater didn't finish holds no system and the trial falls back by itself | page, MCU |
| the gate | a press of ON/OFF, as now; unattended units open it for good | unchanged |

## Order of work
Done and seen on the unit: the stock dialect out, the module states, the State record, acknowledged V writes;
`install --no-bcd` (the whole flow three times); the schema (`State`, settings from the spec, addresses per
interface), the manifest and `zd1 image`; the Wi-Fi's status from the supplicant, the scan, the known networks taken
over, the slots' stamps; the snapservers and AirPlay's pairings as records, the receivers' options, the amp's idle
time, the page's views [2026-11-02]. Left on the unit (`units/<unit>/hw-checks.md`): the known networks' failure
cases (a wrong key, none in range, two networks). Next, in order:
1. The rest of the unit's checks; the page's MCU install on the unit (its backup kept in the browser).
2. `zd1 install` with the relay as one flow on the unit (`zd1 rollback` has run as one: 2026-11-03), then the
   page's Install and Roll back on it.
3. WinUSB (MS OS 2.0) descriptors in the MCU firmware, for Windows.
4. Later: a small HTTP API on the module for home automation serving `State` as JSON (`docs/backlog.md`).

## Working notes for the next agent
- **The rules first** (`AGENTS.md`, and the local `CLAUDE.md` for the unit's own): a flash to the unit, a change of
  its settings and anything uploaded need the unit owner's OK each time; the amp stays short; per-unit data and the
  Wi-Fi passphrase never in a doc, a log or a commit (`zd1 state` prints the SSID: hide it when pasting).
- **Decisions already made** (`decisions.md`, the last five rows): our MCU firmware drives our OS only (the stock app
  is a reported state); a frozen base layer on the SPI link, and on the host link HELLO + `Dfu` across versions; one
  typed schema (`State`) for settings and status, settings its writable fields, keys = field names with dots; the same
  address settings per interface; there are **no units in the wild**, so backwards compatibility is never a reason
  (the host link breaks when a change wants it, protocol 3 now: bump `VERSION`, run `protocol/gen` with
  `--break`). "Pretty" means technically elegant: few mechanisms, each doing one job.
- **The unit**: what is installed on it is in `units/<unit>/status.md`. The known Wi-Fi networks are in
  `/data/zd1/networks`; `zd1 wifi --known` lists them.
- **The loop**: `nix develop` for every host tool (`zd1`, `zemu`, `cargo`: outside it `zd1` isn't on the PATH and
  `zemu --host` fails with exit 127). Nix builds the files git tracks: `git add` new files. Commit before a flash so
  the stamps name a commit. After a spec edit, both specs, run `protocol/gen` (`cd protocol/gen && cargo run`): it
  writes the MCU's two headers, the two C tests, `docs/protocol/{host,registers}.md` and `spec.lock`. The module's
  crates (`zd1-linkd`, `zd1-init`, `zd1-net`) don't compile on macOS (Linux ioctls): `nix build .#os-image`, on
  the Linux builder, is their check; `cargo test -p zd1-settings -p zd1-hostlink -p zd1-link -p zd1-host` runs here.
- **The emulator**: `zemu run --mcu -q --host 'zd1 state' ...` boots our OS with our MCU firmware and runs host
  commands (the gate open). Its dev boot has no slots: a slot trial (`zd1 update`'s last step, `install`) gets no
  verdict there; slot work needs `--nand --allow-panic` (`docs/module/updater.md`, Working notes), e.g. the install
  rehearsal: `zemu run --nand --mcu --allow-panic --timeout 1500 --until 'waiting for a system' --host 'zd1 bootloader'
  --host 'zd1 install --no-bcd --yes --unit $RUN'`. `zemu run stock --mcu` is the stock module app under our MCU
  (state STOCK). The emulated module is never starved, so the link's resend never fires there.
- **The link's lesson of 2026-10-31** (`docs/protocol/sequences.md`, Our OS, point 6): a V write to a busy module
  can arrive short and used to wedge the slave's parser for good; now the MCU waits for the write's echo and sends
  again, and `zd1-linkd` drops a half-received request after 100 ms. If reads ever answer nothing again, restart
  `zd1-linkd` on the module (`zd1 console run 'zd1-ctl restart zd1-linkd'`) and look for what wrote during its
  start.
- **Where the pieces are**: the schema `protocol/host/src/lib.rs` (`State`, `ScanEntry`, `KnownNetwork`,
  `Snapserver`, `Pairing`, `Actions`, `settings()`, `actions()`, `Field.key()`), its macros `macros.rs` (`set FORMAT = "DEFAULT"`, `[module]`), the codec's lists
  (`codec::decode_list`); the module's side `module/os/apps/zd1-settings` (validation from the spec,
  `record_values`, `wpa_conf`, the networks file and its old lines, `act_in`), `zd1-net/src/wpa.rs` (the supplicant's control socket: `Watch` for the status,
  `scan_results`, the `wpa_state` and event mapping with tests), `zd1-linkd` (`state_record`, `scan_record`, `networks_record`, `servers_record` (mDNS in a thread), `pairings_record`, `read_stamps`,
  `push_state`, records by number on the configuration channel), `zd1-init/src/system.rs` (`wifi`, `address()` for
  both interfaces), `zd1-init/src/supervisor.rs` (`SETTLE`, `restart_at`); the MCU `mcu/src/dm870.c` (`dm870_record`,
  the State cache, `write_v_once`'s echo), `hostif.c`; the host `host/core/src/settings.rs` (`state`, `record`,
  `list_of`, `scan`, `networks`, `join` (`wifi.add`), `export` / `import`, `shown`), `speaker.rs` (the sentences, the play choices), `backup.rs`, `install.rs`; the
  page `host/web/src/lib.rs` (`state()`, `list(record)`, `exportSettings`, `Bootloader.install` / `rollback`,
  `spec()`), `www/app.js` (the wiring) and its ES modules in `www/js/` (no build step): `connection.js` (`events`,
  `connect`, `job`), `backups.js` (IndexedDB), `network.js` (`drawSettings`, `wifiStatus`), `receivers.js`,
  `software.js` (`drawDfu`, `drawReleases`, `updateTo`), `speaker.js`, `hero.js` (the top: the drawing, the
  title, the status), `led.js`, `zd-speaker.js`, `diagnostics.js`, `ui.js`, `shared.js`, `design.js` (the catalogue);
  the local manifest
  `tools/zeppelin/web.py`; the workflow `.github/workflows/pages.yml`.
- **The supplicant's events, unverified**: the mapping of `CTRL-EVENT-SSID-TEMP-DISABLED reason=WRONG_KEY`,
  `CTRL-EVENT-NETWORK-NOT-FOUND` and the handshake-failed line is from wpa_supplicant 2.11's sources, not yet seen
  on the unit (the emulator has no radio): the wrong-key and unknown-network checks on the unit are the test.
  `zd1-net` builds on macOS (its Linux ioctls are `cfg`'d out) so `cargo test -p zd1-net` runs the parsers here.
- **The page without a device**: a stand-in for `pkg/zd1_web.js` (the real `spec()` and `imageInfo()`, a canned
  `Speaker` and `Bootloader`) served beside the built page draws every view in a headless Chromium (`--screenshot`,
  `--dump-dom`): how the views were checked. It isn't in the repo (it was a scratch tool); the emulator test is.
- **The State record after a write**: `zd1-linkd` pushes it right after a host's write, queued before the write's
  read-back, so a read after `SetSetting` sees the change (the once-a-second push alone left it a second stale).
- **Memory for an agent with the project's memory directory**: `module-state-schema`, `no-stock-app-compat`,
  `no-backwards-compat`, `amp-power-fragile`, `hardware-over-emulator`, `docs-current-state`.
