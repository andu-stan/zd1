//! The module's receivers' toolkit (zd1-airplay, zd1-spotify, zd1-snapcast): the playback device (`device`: ALSA to
//! snd-sulu, AV port 2 -> the speaker's DSP, and when its frames play), the output (`output`: one stream at a time
//! from a queue, timed or not), the link to the speaker (`speaker`: zd1-linkd's socket: play state and volume to the
//! MCU, the speaker's keys and the other sources' claims back) and what each receiver's `main` starts with (`app`:
//! the command line, the log, the name).

pub mod app;
pub mod device;
pub mod output;
mod queue;
mod resample;
pub mod speaker;

pub use app::Args;
pub use output::{Output, Stream};
pub use queue::Sample;
pub use speaker::{Event, Speaker};
