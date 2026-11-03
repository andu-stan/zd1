//! What the MCU's replies say, for a person.

use zd1_host::spec::{Player, Slot, WifiSecurity};
use zd1_host::{msg, speaker};

fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

const RECEIVERS: [Player; 3] = [Player::Airplay, Player::Spotify, Player::Snapcast];

pub fn info(r: &msg::InfoReply) -> String {
    let up = r.uptime_ms / 1000;
    let mut l = vec![
        format!(
            "MCU      {}{}, up {}h{:02}m{:02}s, reset cause 0x{:02x}",
            r.fw,
            if r.built.is_empty() { String::new() } else { format!(" ({})", r.built) },
            up / 3600,
            up / 60 % 60,
            up % 60,
            r.rcause
        ),
        format!(
            "speaker  {}, source {}, volume {}/71, bass {}, amp {}",
            r.state,
            r.source,
            r.volume,
            r.bass,
            speaker::amp(r)
        ),
        format!(
            "module   {}, status 0x{:04x}{}{}",
            r.mod_state,
            r.mod_status,
            if r.console_open { ", console open over the host link" } else { "" },
            if r.link_proto != 0 && r.mod_proto != 0 && r.mod_proto != r.link_proto {
                format!("; its link protocol is {}, this firmware's {}", r.mod_proto, r.link_proto)
            } else {
                String::new()
            }
        ),
    ];
    if !r.mod_os.is_empty() {
        l.push(format!("         {}, debug services: {}", r.mod_os, r.debug));
    }
    if !r.mod_name.is_empty() || !r.mod_ip.is_empty() {
        l.push(format!(
            "         \"{}\"{}",
            r.mod_name,
            if r.mod_ip.is_empty() { ", no address".into() } else { format!(" at {}", r.mod_ip) }
        ));
    }
    if r.player != Player::None || r.players_on != 0 {
        let on: Vec<String> =
            RECEIVERS.iter().filter(|p| r.players_on >> p.value() & 1 == 1).map(|p| p.to_string()).collect();
        let front = if r.player == Player::None {
            "no receiver in front".into()
        } else {
            format!("{} {}", r.player, r.player_state)
        };
        l.push(format!(
            "         {front}; senders connected: {}",
            if on.is_empty() { "none".into() } else { on.join(", ") }
        ));
    }
    l.push(format!("prefs    a USB stream takes the speaker: {}", yn(r.auto_switch)));
    l.push(format!("gate     {}", speaker::gate(r)));
    l.join("\n")
}

/// A scanned network's security, and whether the speaker can join it.
pub fn security(s: WifiSecurity) -> &'static str {
    match s {
        WifiSecurity::Open => "open",
        WifiSecurity::Psk => "WPA2",
        WifiSecurity::Sae => "WPA3 only (can't be joined)",
        WifiSecurity::Other => "WEP or enterprise (can't be joined)",
        WifiSecurity::Unlisted(_) => "?",
    }
}

/// Diag.led: the look (mcu/src/led.h).
fn led(code: u8) -> String {
    let kind = ["boot", "standby", "error", "run"][(code >> 6) as usize];
    if kind != "run" {
        return kind.into();
    }
    let acts = ["playing", "ready", "asleep", "no network", "starting", "choosing", "?", "?"];
    format!("{} {}", zd1_host::spec::Source::from(code & 7), acts[(code >> 3 & 7) as usize])
}

fn dbfs(v: u32) -> f64 {
    20.0 * (v as f64 / 0x80_0000 as f64).log10()
}

pub fn diag(r: &msg::DiagReply) -> String {
    let adc = |v: u16, lo: u16| format!("0x{v:03x}{}", if v < lo { " (LOW)" } else { "" });
    let trim = if r.nominal > 0 { (r.ratio as f64 / r.nominal as f64 - 1.0) * 1e6 } else { 0.0 };
    let mut l = vec![
        format!(
            "protect  ADC5 (temp) {}, ADC4 (temp 2) {}, ADC6 (amp supply) 0x{:03x}; PB04 fault {}, PB18 amp fault {}; \
                 under limits: temp {}/9, ADC4 {}/9, PB04 {}/3{}",
            adc(r.adc5, 0x1FF),
            adc(r.adc4, 0x1BC),
            r.adc6,
            yn(r.fault),
            yn(r.amp_fault),
            r.n_temp,
            r.n_adc4,
            r.n_fault,
            if r.error != 0 { format!(", ERROR {}", r.error) } else { String::new() }
        ),
        format!(
            "audio    DSP {}, input {}, volume index {}, S/PDIF {}; amp {}; AUX jack {}, USB VBUS {}; LED {}",
            if r.dsp_running { "running" } else { "off" },
            r.dsp_in,
            r.dsp_volume,
            if r.spdif_locked { "locked" } else { "no lock" },
            if r.amp_unmuted { "unmuted" } else { "muted" },
            yn(r.aux_jack),
            yn(r.usb_vbus),
            led(r.led)
        ),
        format!(
            "usb      host {}{}, {} packets, media keys {}{}",
            if r.usb_streaming { format!("streaming {} Hz", r.usb_rate) } else { "not streaming".into() },
            if r.usb_mute { ", muted" } else { "" },
            r.usb_packets,
            if r.hid { "up" } else { "down" },
            if r.usb_step > 0 { format!(", volume step {}", r.usb_step) } else { String::new() }
        ),
        format!(
            "         engine {}{}, fill {}/512 {}, under {} over {} dropped {}, {} f/s played, {} f/s in, trim {trim:+.0} ppm",
            if r.engine { "running" } else { "stopped" },
            if r.clock_ok || !r.engine { "" } else { " (NO bit clock)" },
            r.fill,
            if r.primed { "playing" } else { "priming" },
            r.underruns,
            r.overruns,
            r.dropped,
            r.play_fps,
            r.host_fps
        ),
        format!(
            "module   network {}, {}, a receiver's volume waiting: {}",
            if r.net { "up" } else { "down" },
            r.play,
            if r.pending_volume == 255 { "-".into() } else { format!("step {}", r.pending_volume) }
        ),
        format!("console  UART errors (overrun, framing) since boot: {}", r.modcon_overruns),
    ];
    let meter = if r.level > 0 {
        format!(
            "{:.1} dBFS (0x{:06x}){}",
            dbfs(r.level),
            r.level,
            if r.level_floor > 0 { format!(", its floor {:.1} dBFS", dbfs(r.level_floor)) } else { String::new() }
        )
    } else {
        "-".into()
    };
    l.push(format!("amp      on {} now, {} s since boot; heat cap {}; source silent {}; USB host sends sound: {}; input meter {meter}",
                   if r.amp_run_s > 0 { format!("{} s", r.amp_run_s) } else { "-".into() }, r.amp_on_s,
                   if r.heat_cap >= 71 { "none".into() } else { format!("step {}", r.heat_cap) },
                   if r.silent_s > 0 { format!("{} s", r.silent_s) } else { "-".into() }, yn(r.usb_audible)));
    l.push(format!(
        "remote   {}",
        if r.ir_frames > 0 {
            format!(
                "{} frames, the last: addr 0x{:02x} cmd 0x{:02x} toggle {} at {} ms",
                r.ir_frames, r.ir_addr, r.ir_cmd, r.ir_toggle as u8, r.ir_ms
            )
        } else {
            "no frames yet".into()
        }
    ));
    l.join("\n")
}

pub fn prefs(r: &msg::PrefsReply) -> String {
    format!(
        "a USB stream takes the speaker: {}; amp {}, off after {} min of silence; volume up to \
             {}/71",
        yn(r.auto_switch),
        if r.amp_auto { "allowed from every boot" } else { "locked at boot" },
        r.amp_idle_s / 60,
        r.volume_cap
    )
}

fn slot(s: Slot) -> &'static str {
    match s {
        Slot::None => "none",
        Slot::A => "A",
        Slot::B => "B",
        Slot::Rescue => "the rescue",
        Slot::Unlisted(_) => "?",
    }
}

pub fn boot(r: &msg::BootReply) -> String {
    let failed: Vec<&str> =
        [(1, "A"), (2, "B"), (4, "the rescue")].iter().filter(|(b, _)| r.failed & b != 0).map(|x| x.1).collect();
    format!(
        "good slot  {}\nnext boot  {}\nlast boot  {}{}\nfailed     {}",
        slot(r.good),
        if r.next != Slot::None { format!("{} (once)", slot(r.next)) } else { "the good one, unless it failed".into() },
        if r.last != Slot::None { slot(r.last) } else { "none asked since the MCU started" },
        if r.trial { ", not ready yet" } else { "" },
        if failed.is_empty() { "none since the last ready".into() } else { failed.join(", ") }
    )
}
