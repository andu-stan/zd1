# Settings

What the speaker's owner chooses (its name, the Wi-Fi networks, which receivers run and how), kept by our module OS
on the data partition (`storage.md`): `/data/zd1/settings`, one `KEY=VALUE` line each, and `/data/zd1/networks`, the
known Wi-Fi networks. Tags: `docs/README.md`.

## One schema: the State record
The speaker's settings are the writable fields of one typed record in the protocol spec, **`State`**
(`protocol/host/src/lib.rs`; the generated table: `docs/protocol/host.md`, The module's records): the module's
settings and its status together, encoded by the module with the host link's codec, pushed to the MCU (SPI reg 0xD6)
after every hello and whenever anything changes (once a second at most), kept by the MCU and read whole by the host
(`ModRecord State`, in pieces of 240 bytes; never gated). One declaration gives the module its keys, types, formats
and defaults (`zd1-settings` reads the spec), the host its types, and the page its form; the same record, as JSON, is
what an API on the module will serve.

- **A setting** is a field marked `set`: the host writes it one at a time (`SetSetting`), the key the field's name with
  dots for underscores (`wifi_mode` -> `wifi.mode`), the value in its string form (bools `on` / `off`, enums by name,
  numbers in decimal); the module checks it against the type, the maximum and the format (`ip`, `cidr`, `host`,
  `pin`, `bitrate`). Each interface has the same address settings, `eth.*` and `wifi.*`: `mode` (off, dhcp, static),
  `address` (ADDRESS/PREFIX), `gateway`, `dns`. Then the name, the three receivers and their options: `airplay.pin`
  (4 digits: senders pair with it and are remembered; empty: pairing per connection), `snapcast.server` (empty: found
  over mDNS), `snapcast.latency` (ms), `spotify.bitrate` (96, 160, 320).
- **An action** is a write with no state of its own, a field of the spec's `Actions` message, written like a setting
  and read back empty (or `!` and why it was refused): `wifi.add` (`wpa2 KEY SSID` or `open SSID`: the network added,
  or its key changed, and put first), `wifi.forget SSID`, `wifi.first SSID` (preferred), `airplay.forget` (any value:
  the paired senders forgotten, the accessory's identity kept), `reset settings` (every setting to its default; the
  networks and pairings stay) and `reset all` (those too). `zd1-linkd` logs a `wifi.add` as `wpa2 (key) SSID`.
- **The known networks** are a list, the preferred first, in `/data/zd1/networks` (a line each: the WPA2 key or `-`,
  the SSID in hex); the supplicant's configuration has them all with their priority by order, and it joins the
  preferred one it sees.
- **The status** is the rest: what it runs and its slots, the MCU's version, each interface's address and the wired
  link, the Wi-Fi (below), whether the module is the speaker's input, its volume, the receiver in front and the
  receivers with a sender.
- **The Wi-Fi's status** (`wifi`, `wifi_ssid` and `wifi_psk`: the network joined or being joined and whether it has a
  key, `bssid`, `signal`) is the supplicant's word: `zd1-linkd` talks to `wpa_supplicant`'s
  control socket (`zd1-net`'s `wpa`: a datagram socket, `STATUS` and `SCAN_RESULTS` once a second in a thread of its
  own, so the MCU link never waits for the supplicant; its events for the failures) and
  maps `wpa_state` to `WifiState`: SCANNING, the association and handshake CONNECTING, COMPLETED CONNECTED with the
  access point and its signal from the supplicant's BSS table (`BSS BSSID`, `level`: what the scan before the
  association saw; `SIGNAL_POLL` says nothing true under Wireless Extensions). A failure the events report stays
  shown while the supplicant retries: WRONGKEY (`CTRL-EVENT-SSID-TEMP-DISABLED ... reason=WRONG_KEY`, the 4-way
  handshake failed) until a connection completes, NOTFOUND (`CTRL-EVENT-NETWORK-NOT-FOUND`) until the network is
  being joined, FAILED for the rest. The supplicant runs whenever the radio is on (`wifi.mode` not off and `wlan0`
  there), with no network known too: IDLE then, and it scans when asked; OFF is no supplicant at all. `zd1
  wifi` prints it in a line ("Home (WPA2): connected to 02:00:00:00:00:0a at -57 dBm, 192.0.2.177").
- **The slots' stamps** (`stamp_a`, `stamp_b`: what each system slot holds, empty for none) are read by `zd1-linkd`
  in a thread after it starts and after an update (the slot that doesn't run: what `zd1-slot install` recorded in
  `/data/zd1/stamp.SLOT`), so the page offers "back to slot X" only for a slot with a system.
- **Other records** (`Record`) are made when asked: the MCU asks through the configuration channel (`zd1/@N`, N the
  record's number) and hands them out in pieces; an unknown number comes back as `!no record N`, one the module
  can't make as `!` and why. A list record is entries one after another, each a u16 length and a message
  (`codec::decode_list`). **`Scan`** (1) is the networks around the speaker as the supplicant's last scan found
  them, a `ScanEntry` each (the SSID, the signal in -dBm, what it takes to join: open, a WPA2 key, WPA3-only or
  WEP / enterprise, which this radio can't), the strongest first, one per SSID, hidden ones left out, as many as fit
  a notification (1024 bytes: ~26); each read also starts a new scan, so a fresh radio's first answer is empty and
  one a few seconds later has the networks (`zd1 wifi --scan` waits and reads again). Without a supplicant:
  `!no Wi-Fi: the radio is off`. **`Networks`** (2): the known networks, a `KnownNetwork` each (the SSID, open or
  WPA2: the key stays on the module), the preferred first. **`Servers`** (3): the snapservers on the speaker's
  networks, a `Snapserver` each (its name, address, port); `zd1-linkd` answers from its last look and starts another in
  a thread (`zd1-mdns`, 1.5 s on each interface with an address), so the first read may be empty; without an address,
  `!no network`. **`Pairings`** (4): the controllers paired with AirPlay, a `Pairing` each (its identifier, admin),
  from `zd1-airplay`'s store.

From a computer on the speaker's USB port, through our MCU firmware (`zd1 settings --help`):
```
zd1 state                             the whole record, every field
zd1 settings                          the settings and their values (one read of the record)
zd1 name "Kitchen"                    the name (AirPlay, Spotify Connect, Snapcast, the MCU)
zd1 wifi                              the Wi-Fi in a line: the network, whether it is joined, the access point and signal
zd1 wifi --scan                       the networks the speaker's radio sees
zd1 wifi --known                      the networks it knows, the preferred first
zd1 wifi MyNetwork                    asks for the passphrase (the key is derived here, PBKDF2 as wpa_passphrase), adds it first
zd1 wifi MyNetwork --first | --forget
zd1 ip 192.0.2.20/24 --gateway 192.0.2.1 --dns 192.0.2.1     Wi-Fi; --eth the wired interface; off, dhcp
zd1 receiver spotify off
zd1 settings airplay.pin 3939         a receiver's option (spotify.bitrate, snapcast.latency, snapcast.server)
zd1 pairings [--forget]               AirPlay's paired senders
zd1 servers                           the snapservers found
zd1 settings --export > FILE          the settings that differ from the defaults (no secrets); --import FILE
zd1 reset [--all] --yes               back to the defaults
```
On the module: `zd1-settings get|set|list|keys|networks|pairings`. Only these two ways exist: the USB port (the host link's
`SetSetting`, then the configuration channel, SPI reg 0x45, names `zd1/KEY`) or a shell on the module. Nothing on the
network changes a setting yet (a module API with its own authentication is planned: `docs/backlog.md`), and over USB a
change needs the gate open: a press of ON/OFF on the speaker (`zd1 settings` asks for it). The values are kept in
`/data/zd1/settings` on the data partition (`storage.md`), one `KEY=VALUE` line each.

## How a change takes effect
`zd1-settings` checks the value, rewrites the file (a new file renamed over the old: a power cut leaves one or the
other) and tells init (`changed KEY...` on its socket, as `zd1-ctl changed` sends it), which restarts the services that list the key once the changes have
settled (1.5 s after the last one: a burst of keys, one restart): the receivers for `name` (each restarts under the
new name; `zd1-linkd` tells the MCU the new device name), `wifi` for `wifi.*` (the actions on the networks too), a
receiver for its switch and its options (`airplay.*`, `spotify.*`, `snapcast.*`: AirPlay for `airplay.forget`, so it
loads its store again), everything for `reset`. `zd1-linkd` pushes the State record right after the write, so the
host's read-back and its next read of the record see it; through the MCU the host gets the value back as the module
now has it, or the reason it refused (`!...`).

## Another version's settings
The data partition stays when an image is installed, so a newer or an older OS finds the file the other one wrote.
The file stays `KEY=VALUE` lines for good: a newer OS may add keys and values but never changes what an existing key
or value means (a new meaning gets a new key). An older OS reads the keys it knows with values it accepts and takes
the default for the rest (a key it doesn't know, a value it can't check: `zd1-settings list` shows those as
`(default)`); a change rewrites only that key's line, the newer OS's lines stay for after the next upgrade
(`module/os/apps/zd1-settings`, test `a_newer_os_wrote_it`).

## Secrets
The Wi-Fi passphrase never leaves the computer: `zd1 wifi` derives the WPA2 key there. The key goes over
USB, the MCU and SPI to the module, which keeps it in the settings file (and hands it to `wpa_supplicant` in a
root-only file under `/var/run`). It is never logged or read back: a secret is shown and answered as `(set)` (`zd1-settings`, `zd1-linkd`), and
`zd1-linkd` logs a configuration request by its name only.

## On the unit [HW 2026-10-26]
- `zd1 wifi NETWORK` from the Mac, through the MCU: init restarted `wifi` (then for each key; coalesced since), and
  it joined from the settings within ~3 s. After a power cycle of the speaker an image without a network of its own
  joined from them 5 s after `[os] up`; `/data` kept.
- **The Wi-Fi's status and the records** [HW 2026-11-01, system 4f2215a in slot B]: after the update the stored
  network was taken over from the settings file's old lines as the one known network and joined as before (the
  same address); `zd1 wifi` read "connected to 02:00:00:00:00:0a at -60 dBm" with the address; `zd1 wifi --scan`
  listed ten networks around the unit, nine WPA2 and one WEP or enterprise marked unjoinable; the slots' stamps read
  (A 327238f, B 4f2215a); `zd1 servers` answered empty (no snapserver running), `zd1 pairings` none (no PIN set).
- **The records, the receivers' options and a PIN** [HW 2026-11-02]: `zd1 servers` with a snapserver running,
  `zd1 pairings` and `--forget` (the iPhone asked for the PIN again), `spotify.bitrate` and `snapcast.latency` (each
  receiver restarted once with its option), `airplay.pin` set and cleared.
- Not on the unit yet: the supplicant idling without a network, a wrong key, two known networks. In the emulator, which has no radio
  [EMU 2026-11-01, `zemu run --mcu`, `host/web/test/emu.mjs`]: networks added, preferred and forgotten (the old lines
  following the first), an unknown one refused, the scan refused without a radio, the receivers started with
  `--pin` and `--latency` and without after a reset, export, import, `reset settings` (the networks kept) and
  `reset all`, one restart per burst.
- `zd1 name`: the MCU's device name followed, init restarted Snapcast, AirPlay and Spotify, and AirPlay
  (`_raop`, `_airplay`) and Spotify came back under the new name.

## Other state on the data partition
- `/data/zd1/airplay`: the AirPlay accessory's identity (its Ed25519 seed, so senders see the same speaker across
  restarts) and up to 16 paired controllers (`zd1-airplay`'s store).
- Spotify Connect keeps nothing: after a reboot the speaker is picked again in the app (`backlog.md`).
