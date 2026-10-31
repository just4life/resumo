//! Black bars drawn into the picture itself, for when the picture goes to the
//! video layer or a GPU texture, where the app never sees its pixels: a second
//! player, silent and unseen, opens the video again at a few moments, decodes
//! the first picture there in software and small, and [`find_bars`] looks at
//! it. It starts where the film is now, then looks across the rest of it.
//!
//! Each moment gets a player of its own: the first picture of a new playback
//! is always shown, while one after a jump is thrown away when libVLC thinks
//! it late (playing) or can't place it in time (paused).

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use egui::{pos2, Rect};

use crate::video::VideoSink;
use crate::vlc::{Instance, MediaPlayer};
use crate::zoom::find_bars;

/// Where in the video to look after where it is now, as shares of its
/// length. An episode's bars are the same throughout, so a few will do.
const SPOTS: [f32; 3] = [0.3, 0.6, 0.8];
/// How long one look may take: the file opens again (over the network, too)
/// and a 4K keyframe is decoded on the TV's processor.
const WAIT: Duration = Duration::from_secs(20);
/// How long the next look waits for the last one's player to be let go of:
/// over the network that can hang, and the next look shouldn't.
const CLOSE_WAIT: Duration = Duration::from_secs(3);
/// Fine enough to cut bars to a row of a 1080p picture or so, and cheap to
/// scale to and copy.
const WIDTH: usize = 640;

pub struct BarProbe {
    instance: Instance,
    location: String,
    options: Vec<String>,
    repaint: Arc<dyn Fn() + Send + Sync>,
    /// The look under way: its player, the moment (ms) and when it began.
    look: Option<(Inner, i64, Instant)>,
    /// The last look's player, being let go of, and since when.
    closing: Option<(JoinHandle<()>, Instant)>,
    /// The next of [`SPOTS`].
    next: usize,
    length: Option<i64>,
    seen: Vec<Rect>,
}

struct Inner {
    // Declared first, so dropped first: the player lets go of the sink.
    player: MediaPlayer,
    sink: Box<VideoSink>,
}

/// libVLC objects may be let go of on any thread.
struct Detached(Inner);
unsafe impl Send for Detached {}

impl BarProbe {
    /// Starts looking at `location`, opened with `options` like the video
    /// being watched, from `from` (ms). `repaint` is called on each picture.
    pub fn start(instance: &Instance, location: &str, options: &[String], from: i64, repaint: impl Fn() + Send + Sync + 'static) -> Option<Self> {
        let mut options = options.to_vec();
        options.extend(
            [
                // Software decoding: the TV's hardware decoder is the film's.
                ":codec=avcodec",
                // Keyframes only: the first picture is one.
                ":avcodec-skip-frame=3",
                // One thread: less memory for a 4K decoder, and cores left
                // for the film's sound.
                ":avcodec-threads=1",
                ":input-fast-seek",
                ":no-audio",
                ":no-spu",
                ":no-sub-autodetect-file",
                ":no-video-title-show",
            ]
            .map(String::from),
        );
        let mut probe = Self {
            instance: instance.clone(),
            location: location.to_owned(),
            options,
            repaint: Arc::new(repaint),
            look: None,
            closing: None,
            next: 0,
            length: None,
            seen: Vec::new(),
        };
        probe.look_at(from).then_some(probe)
    }

    /// Opens the video again at `at` (ms).
    fn look_at(&mut self, at: i64) -> bool {
        let player = match MediaPlayer::new(&self.instance) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("bar probe: {e}");
                return false;
            }
        };
        let repaint = self.repaint.clone();
        let sink = Box::new(VideoSink::new(move || repaint()).shrunk_to(WIDTH));
        // SAFETY: the sink is dropped after the player (see `Inner`).
        unsafe { player.set_video_callbacks(&sink.callbacks()) };
        let inner = Inner { player, sink };
        let mut options = self.options.clone();
        if at > 0 {
            options.push(format!(":start-time={:.3}", at as f64 / 1000.0));
        }
        if let Err(e) = inner.player.set_media(&self.location, &options) {
            log::warn!("bar probe: {e}");
            return false;
        }
        if !inner.player.play() {
            log::warn!("bar probe: can't play");
            return false;
        }
        self.look = Some((inner, at, Instant::now()));
        true
    }

    /// Lets go of the look's player, on a thread: stopping waits for
    /// libVLC's threads, which may be reading over the network.
    fn close(&mut self) {
        if let Some((inner, ..)) = self.look.take() {
            let inner = Detached(inner);
            let repaint = self.repaint.clone();
            self.closing = std::thread::Builder::new()
                .name("bar-probe-stop".into())
                .spawn(move || {
                    drop(inner);
                    repaint();
                })
                .ok()
                .map(|h| (h, Instant::now()));
        }
    }

    /// The bars two pictures already agree on, before the rest are in.
    pub fn so_far(&self) -> Option<Rect> {
        settle(&self.seen).filter(|c| c.min.x > 0.0 || c.min.y > 0.0)
    }

    /// Looks done or under way, and how many there are in all.
    pub fn progress(&self) -> (usize, usize) {
        (self.next + 1, SPOTS.len() + 1)
    }

    /// Looks at what came since last time. Once it is done: the part of the
    /// picture without bars, or `None` when too few pictures came to tell.
    pub fn poll(&mut self) -> Option<Option<Rect>> {
        if let Some((inner, at, began)) = &self.look {
            let found = inner.sink.with_frame(|f| (f.width > 0).then(|| find_bars(&f.pixels, f.width, f.height)));
            match found {
                Some(found) => {
                    log::info!("bar probe: picture at {at} ms: {found:?}");
                    self.seen.extend(found);
                }
                None if began.elapsed() < WAIT => return None,
                None => log::warn!("bar probe: no picture at {at} ms"),
            }
            self.length = self.length.or(inner.player.length().filter(|_| inner.player.is_seekable()));
            self.close();
        }
        // One player at a time, as a rule: two 4K decoders are a lot for a TV.
        if self.closing.as_ref().is_some_and(|(h, since)| !h.is_finished() && since.elapsed() < CLOSE_WAIT) {
            return None;
        }
        self.closing = None;
        while let Some(len) = self.length.filter(|_| self.next < SPOTS.len()) {
            let at = (len as f32 * SPOTS[self.next]) as i64;
            self.next += 1;
            if self.look_at(at) {
                return None;
            }
        }
        let content = settle(&self.seen);
        match content {
            Some(c) => log::info!("bar probe: {} pictures, bars {:.3} / {:.3}", self.seen.len(), c.min.x, c.min.y),
            None => log::warn!("bar probe: {} pictures, can't tell", self.seen.len()),
        }
        Some(content)
    }
}

impl Drop for BarProbe {
    fn drop(&mut self) {
        self.close();
    }
}

/// The bars all the pictures have: per side, the thinnest seen at least
/// twice. Dark scenes only ever look like thicker bars, and one picture
/// with a long subtitle line in the bar doesn't undo the rest. `None` from
/// fewer than two pictures.
fn settle(seen: &[Rect]) -> Option<Rect> {
    if seen.len() < 2 {
        return None;
    }
    let side = |f: fn(&Rect) -> f32| {
        let mut v: Vec<f32> = seen.iter().map(f).collect();
        v.sort_by(f32::total_cmp);
        v.windows(2).find(|w| w[1] - w[0] < 0.01).map_or(0.0, |w| w[0])
    };
    let h = side(|r| r.min.x);
    let v = side(|r| r.min.y);
    Some(Rect::from_min_max(pos2(h, v), pos2(1.0 - h, 1.0 - v)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(h: f32, v: f32) -> Rect {
        Rect::from_min_max(pos2(h, v), pos2(1.0 - h, 1.0 - v))
    }

    #[test]
    fn thinnest_seen_twice() {
        // Letterboxed throughout, with a dark scene that looked like pillarbox too.
        let r = settle(&[bars(0.0, 0.06), bars(0.13, 0.06), bars(0.0, 0.06), bars(0.0, 0.2)]).unwrap();
        assert_eq!((r.min.x, r.min.y), (0.0, 0.06));
        // A long subtitle line in the lower bar, once.
        let r = settle(&[bars(0.0, 0.06), bars(0.0, 0.0), bars(0.0, 0.065)]).unwrap();
        assert_eq!(r.min.y, 0.06);
        // Two pictures that don't agree: no bars.
        assert_eq!(settle(&[bars(0.0, 0.1), bars(0.0, 0.2)]).unwrap().min.y, 0.0);
        // One picture can't tell.
        assert_eq!(settle(&[bars(0.0, 0.1)]), None);
        assert_eq!(settle(&[]), None);
    }
}
