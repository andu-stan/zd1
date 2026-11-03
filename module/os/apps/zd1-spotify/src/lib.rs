//! zd1-spotify: the module's Spotify Connect receiver, on librespot (our fork, module/os/forks.md: the player
//! with a decoder plug-in and integer samples, the downloader with a bounded memory window). Spotify's Ogg
//! Vorbis goes through our fixed-point decoder (module/codecs/vorbis) on Warp0 (module/warp0: the Vorbis image) when it is
//! free, else on the ARM; the samples into ALSA through zd1-audio's output, the volume and play state to the speaker
//! through zd1-linkd, the speaker's keys to Spotify (docs/streaming.md).
//!
//!   zd1-spotify [--name NAME] [--device hw:0,0] [--bitrate 96|160|320] [--window-kb KB] [--no-warp0] [-v]
//!   zd1-spotify --play-file FILE.ogg [--seek-ms MS] [--device D] [--no-warp0]   (a test: the decoder and output
//!               path without Spotify; ZD1_PCM_HASH / ZD1_PCM_DUMP as for zd1-airplay)
//!
//! Name: "Zeppelin Air XXXXXX" from eth0's MAC, like the AirPlay receiver. Discovery: Spotify's zeroconf (mDNS
//! `_spotify-connect._tcp` and its HTTP endpoint): the phone sends its credentials when the speaker is picked;
//! nothing is stored (not yet on /data either), so after a reboot it is picked again.

mod decoder;
mod sink;

use futures_util::StreamExt;
use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::config::{DeviceType, SessionConfig};
use librespot_core::session::Session;
use librespot_playback::config::{Bitrate, PlayerConfig};
use librespot_playback::mixer::Mixer;
use librespot_playback::player::{Player, PlayerEvent};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zd1_audio::{Args, Output, Speaker, speaker::Event};

pub const SOURCE: &str = "spotify"; // our name to zd1-linkd

const USAGE: &str = "zd1-spotify [--name NAME] [--device hw:0,0] [--bitrate 96|160|320] [--window-kb KB] \
                     [--no-warp0] [-v] | --play-file FILE.ogg [--seek-ms MS] [--device D] [--no-warp0]";

/// The speaker's keys and yields -> the Connect session
fn on_speaker(spirc: &Mutex<Option<Spirc>>, ev: Event) {
    let guard = spirc.lock().unwrap();
    let Some(s) = guard.as_ref() else {
        tracing::info!(?ev, "no Spotify session to control");
        return;
    };
    let r = match ev {
        Event::Key { key: "play", .. } => s.play(),
        Event::Key { key: "pause", .. } | Event::Yield { .. } | Event::Release { .. } => s.pause(),
        Event::Key { key: "playpause", .. } => s.play_pause(),
        Event::Key { key: "next", .. } => s.next(),
        Event::Key { key: "prev", .. } => s.prev(),
        Event::Key { .. } | Event::Volume(_) => return,
    };
    match r {
        Ok(()) => tracing::info!(?ev, "-> Spotify"),
        Err(e) => tracing::warn!(?ev, %e, "Spotify refused"),
    }
}

#[tokio::main(flavor = "current_thread")]
pub async fn main() {
    zd1_compat::init();
    let options = ["--name", "--device", "--bitrate", "--window-kb", "--play-file", "--seek-ms"];
    let args = Args::parse(USAGE, &options, &["--no-warp0", "-v"]);
    args.log();
    let warp0 = !args.flag("--no-warp0");
    if let Some(path) = args.get("--play-file") {
        let seek = args.get("--seek-ms").and_then(|v| v.parse().ok());
        play_file(path, &args.device(), seek, warp0);
        return;
    }
    let mac = zd1_audio::app::mac();
    let name = args.name();
    let bitrate = match args.get("--bitrate") {
        Some("96") => Bitrate::Bitrate96,
        Some("160") => Bitrate::Bitrate160,
        _ => Bitrate::Bitrate320,
    };
    // each track streams through a window of this much memory (librespot keeps the whole file otherwise: 4-12 MB,
    // twice around a track change, in a module with ~17 MB free)
    let window_kb: usize = args.get_or("--window-kb", 1536);
    let _ = librespot_audio::AudioFetchParams::set(librespot_audio::AudioFetchParams {
        memory_limit: Some(window_kb * 1024),
        ..Default::default()
    });

    let session_config = SessionConfig {
        device_id: format!("{:0<40}", mac.iter().map(|b| format!("{b:02x}")).collect::<String>()),
        ..SessionConfig::default()
    };
    let connect_config = ConnectConfig { name: name.clone(), device_type: DeviceType::Speaker, ..Default::default() };

    let spirc: Arc<Mutex<Option<Spirc>>> = Arc::new(Mutex::new(None));
    let sp = spirc.clone();
    let speaker = Speaker::start(SOURCE, move |ev| on_speaker(&sp, ev));
    let out = Output::start(args.device(), speaker.clone());
    let mixer: Arc<dyn Mixer> = Arc::new(sink::SpeakerVolume::new(speaker.clone(), connect_config.initial_volume));

    let player_config = PlayerConfig {
        bitrate,
        decoder_plugin: Some(Arc::new(decoder::VorbisPlugin { warp0 })),
        ditherer: None,
        ..PlayerConfig::default()
    };
    let mut session = Session::new(session_config.clone(), None);
    let (o2, s2) = (out.clone(), speaker.clone());
    let player = Player::new(player_config, session.clone(), mixer.get_soft_volume(), move || {
        Box::new(sink::OutputSink::new(o2, s2))
    });
    let mut events = player.get_player_event_channel();

    let mut discovery = loop {
        match librespot_discovery::Discovery::builder(
            session_config.device_id.clone(),
            session_config.client_id.clone(),
        )
        .name(name.clone())
        .device_type(DeviceType::Speaker)
        .launch()
        {
            Ok(d) => break d,
            Err(e) => {
                // at boot the network may not be up yet
                tracing::warn!(%e, "discovery: trying again in 10 s");
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
    };
    tracing::info!(name, "receiver up");

    let mut spirc_task: Option<std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>> = None;
    // Spotify refusing a track's audio key stops the player on it (our librespot fork; the app shows it paused, play
    // tries again): said once per session, since for some accounts it is every track (docs/streaming.md)
    let mut key_refused = false;
    loop {
        tokio::select! {
            creds = discovery.next() => {
                let Some(creds) = creds else {
                    tracing::error!("discovery stopped");
                    break;
                };
                tracing::info!(user = creds.username.as_deref().unwrap_or("?"), "picked in a Spotify app: connecting");
                speaker.intent();
                if let Some(s) = spirc.lock().unwrap().take() {
                    let _ = s.shutdown();
                }
                if let Some(t) = spirc_task.take() {
                    // the old session's end mustn't hold up the new one
                    if tokio::time::timeout(Duration::from_secs(5), t).await.is_err() {
                        tracing::warn!("the previous Spotify session didn't end in 5 s: dropped");
                    }
                }
                key_refused = false;
                if !session.is_invalid() {
                    session.shutdown();
                }
                session = Session::new(session_config.clone(), None);
                player.set_session(session.clone());
                match Spirc::new(connect_config.clone(), session.clone(), creds, player.clone(), mixer.clone()).await {
                    Ok((s, task)) => {
                        *spirc.lock().unwrap() = Some(s);
                        spirc_task = Some(Box::pin(task));
                        speaker.session(true);
                    }
                    Err(e) => tracing::error!(%e, "could not connect to Spotify"),
                }
            }
            _ = async { spirc_task.as_mut().unwrap().await }, if spirc_task.is_some() => {
                tracing::warn!("Spotify session ended");
                spirc_task = None;
                spirc.lock().unwrap().take();
                out.stop();
                speaker.session(false);
            }
            ev = events.recv() => match ev {
                Some(PlayerEvent::Playing { track_id, position_ms, .. }) => {
                    tracing::info!(%track_id, position_ms, "playing");
                }
                Some(PlayerEvent::Paused { position_ms, .. }) => tracing::info!(position_ms, "paused"),
                Some(PlayerEvent::Unavailable { track_id, .. }) => tracing::warn!(%track_id, "unavailable: skipped"),
                Some(PlayerEvent::AudioKeyUnavailable { track_id, .. }) => {
                    if !key_refused {
                        tracing::error!(%track_id, "Spotify refused the track's audio key: stopped on it. Spotify \
                                        gives some accounts no keys through librespot (librespot-org/librespot#1649)");
                    } else {
                        tracing::warn!(%track_id, "audio key refused again");
                    }
                    key_refused = true;
                }
                Some(PlayerEvent::EndOfTrack { track_id, .. }) => tracing::debug!(%track_id, "end of track"),
                // the app picked this speaker / another device: the speaker lists Spotify as a source while it's ours
                Some(PlayerEvent::SessionConnected { .. }) => speaker.session(true),
                Some(PlayerEvent::SessionDisconnected { .. }) => speaker.session(false),
                Some(_) => {}
                None => break,
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    if let Some(s) = spirc.lock().unwrap().take() {
        let _ = s.shutdown();
    }
    if let Some(t) = spirc_task.take() {
        t.await;
    }
    out.stop();
}

/// `--play-file`: an Ogg Vorbis file through the plugin and the sink as the player would drive them.
fn play_file(path: &str, device: &str, seek_ms: Option<u32>, warp0: bool) {
    use librespot_playback::audio_backend::Sink;
    use librespot_playback::decoder::DecoderPlugin;
    let file = std::fs::File::open(path).expect("open");
    let plugin = decoder::VorbisPlugin { warp0 };
    let mut dec =
        plugin.open(Box::new(file), librespot_metadata::audio::AudioFileFormat::OGG_VORBIS_320).expect("decoder");
    if let Some(ms) = seek_ms {
        tracing::info!(got = dec.seek(ms).expect("seek"), ms, "seek");
    }
    let speaker = Speaker::start(SOURCE, |_| {});
    let out = Output::start(device.to_string(), speaker.clone());
    let mut sink = sink::OutputSink::new(out.clone(), speaker);
    let mut converter = librespot_playback::convert::Converter::new(None);
    sink.start().unwrap();
    let (t0, mut packets, mut samples, mut first) = (std::time::Instant::now(), 0u64, 0u64, None);
    let mut last_pos = 0;
    while let Some((pos, pkt)) = dec.next_packet().expect("decode") {
        if let librespot_playback::decoder::AudioPacket::S16(s) = &pkt {
            samples += s.len() as u64 / 2;
        }
        first.get_or_insert(pos.position_ms);
        last_pos = pos.position_ms;
        packets += 1;
        sink.write(pkt, &mut converter).unwrap();
    }
    sink.stop().unwrap();
    out.stop();
    tracing::info!(
        packets,
        samples,
        first_ms = first.unwrap_or(0),
        last_ms = last_pos,
        seconds = t0.elapsed().as_secs_f64(),
        "played"
    );
}
