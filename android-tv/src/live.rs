//! Live streams: telling them from files, and rewinding the little they allow.
//!
//! libVLC has no notion of "live". For a live HLS stream (Twitch, TV
//! channels) it reports the length of the playlist's window, about half a
//! minute, as the length of the media, while the time keeps growing past it:
//! "11:20 / 00:28". The position it reports is meaningless too.
//!
//! What can be trusted is the time itself: it runs with the stream, so
//! comparing it with a clock shows how far behind the broadcast playback is,
//! whatever got it there (a rewind, a stall; after a pause libVLC catches up
//! with the broadcast by itself). That is measured here
//! rather than assumed, because setting a position in such a stream lands
//! only roughly where asked: at one of a few places some seconds apart.
//! Measured on Twitch with libVLC 3: position 0.5 is about where playback
//! starts by itself, each 0.1 below it is some three seconds earlier, and
//! there is about half the window to go back.

/// Time past the reported length that gives a live stream away, milliseconds.
const PAST_END: i64 = 1500;
/// Closer to the broadcast than this counts as watching live, milliseconds.
const LIVE_SLACK: i64 = 1500;
/// The position at which libVLC starts a live stream by itself.
const START_POSITION: f32 = 0.5;
/// The position asked for to return to live: a little further than the
/// start, as the place it lands in may be earlier than asked.
const LIVE_POSITION: f32 = 0.55;
/// How long libVLC's time takes to show a seek, milliseconds.
const SEEK_SETTLES: i64 = 1200;
/// A fall of the time this big between two looks is not a stall, milliseconds.
const DROP: i64 = 2000;
/// A return to live counts as one if it lands no further behind than this.
const LIVE_LANDING: i64 = 4000;

/// A rewind or a return to live that libVLC's time doesn't show yet.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Seek {
    /// How far behind live it was meant to land.
    behind: i64,
    to_live: bool,
    /// Clock when it was asked for.
    since: i64,
}

/// A live stream being played.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Live {
    /// Length of the stream's window, milliseconds; 0 if it can't be rewound.
    window: i64,
    /// Time minus clock ("lead") when playback started by itself: the place
    /// that `START_POSITION` stands for.
    start: i64,
    /// Lead when watching live: the largest there was.
    edge: i64,
    /// Lead now.
    lead: i64,
    seek: Option<Seek>,
    /// Clock when the time fell back at once, if it is not believed yet.
    doubt: Option<i64>,
}

impl Live {
    fn new(window: i64, clock: i64, time: i64) -> Self {
        let lead = time - clock;
        Self { window, start: lead, edge: lead, lead, seek: None, doubt: None }
    }

    /// How much can be rewound in all, milliseconds.
    pub fn depth(&self) -> i64 {
        self.window / 2
    }

    pub fn can_rewind(&self) -> bool {
        self.depth() >= 2000
    }

    /// How far behind live playback is, milliseconds.
    pub fn behind(&self) -> i64 {
        match self.seek {
            _ if !self.can_rewind() => 0,
            Some(seek) => seek.behind,
            None => (self.edge - self.lead).max(0),
        }
    }

    pub fn is_at_edge(&self) -> bool {
        self.behind() <= LIVE_SLACK
    }

    /// Where playback is between the oldest moment (0) and live (1), for the seek bar.
    pub fn fraction(&self) -> f32 {
        if self.can_rewind() {
            (1.0 - self.behind() as f32 / self.depth() as f32).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    /// libVLC's time at `clock`, while the stream plays.
    fn observe(&mut self, clock: i64, time: i64) {
        if time <= 0 {
            return;
        }
        let lead = time - clock;
        if let Some(seek) = self.seek {
            if clock - seek.since < SEEK_SETTLES {
                return;
            }
            self.seek = None;
            self.lead = lead;
            // Where a return to live landed is live from now on, unless it
            // fell short of it by more than such a seek is off by.
            let short = self.edge - lead;
            if seek.to_live && short <= LIVE_LANDING {
                self.edge = lead;
            } else if seek.to_live && short > self.depth() + LIVE_LANDING {
                // Not a place in the stream: its timestamps had started anew.
                self.shift(-short);
            }
        }
        // A stall falls behind little by little. A drop at once is the time
        // not having caught up yet after a pause, unless it stays.
        if self.lead - lead > DROP && clock - *self.doubt.get_or_insert(clock) < SEEK_SETTLES {
            return;
        }
        self.doubt = None;
        self.lead = lead;
        let ahead = lead - self.edge;
        let over = -ahead - self.window;
        if ahead > self.depth().max(LIVE_LANDING) {
            // Further ahead than playback gets: the timestamps jumped.
            self.shift(ahead);
        } else if ahead > 0 {
            // Closer to the broadcast than before.
            self.edge = lead;
        } else if self.window > 0 && over > 0 {
            // Further back than the stream keeps: they started anew.
            self.shift(-over);
        }
    }

    /// Moves what counts as live by `ms`, and the place playback started at with it.
    fn shift(&mut self, ms: i64) {
        self.edge += ms;
        self.start += ms;
    }

    /// Moves to `fraction` of the rewindable part (1 = live). Returns the
    /// position to give libVLC, or `None` if the stream can't be rewound.
    pub fn seek_to(&mut self, fraction: f32, clock: i64) -> Option<f32> {
        if !self.can_rewind() {
            return None;
        }
        let behind = ((1.0 - fraction.clamp(0.0, 1.0)) * self.depth() as f32) as i64;
        let to_live = behind < LIVE_SLACK;
        let position = if to_live {
            LIVE_POSITION
        } else {
            // How far before the place playback started at.
            let before_start = (self.start - (self.edge - behind)) as f32;
            (START_POSITION - before_start / self.window as f32).clamp(0.0, LIVE_POSITION)
        };
        self.seek = Some(Seek { behind: if to_live { 0 } else { behind }, to_live, since: clock });
        Some(position)
    }

    /// Moves by `delta` milliseconds (negative: back); see [`seek_to`](Self::seek_to).
    /// `None` also when there is nowhere to move: already live, or as far
    /// back as it goes.
    pub fn seek_by(&mut self, delta: i64, clock: i64) -> Option<f32> {
        if !self.can_rewind() {
            return None;
        }
        let now = self.behind();
        let behind = (now - delta).clamp(0, self.depth());
        let nowhere = if delta < 0 { behind <= now } else { behind >= now || self.is_at_edge() };
        if nowhere {
            return None;
        }
        self.seek_to(1.0 - behind as f32 / self.depth() as f32, clock)
    }
}

/// Watches what libVLC reports about the current media and decides whether
/// it is a live stream.
#[derive(Clone, Copy, Debug, Default)]
pub struct Detector {
    /// The source is known to be live (a Twitch channel).
    known: bool,
    live: Option<Live>,
}

impl Detector {
    /// For a media that is just starting.
    pub fn new(known_live: bool) -> Self {
        Self { known: known_live, live: None }
    }

    pub fn live(&self) -> Option<&Live> {
        self.live.as_ref()
    }

    pub fn live_mut(&mut self) -> Option<&mut Live> {
        self.live.as_mut()
    }

    /// The source was known to be live from the start.
    pub fn expects_live(&self) -> bool {
        self.known
    }

    /// Call every frame while the media plays, with a clock that never stops
    /// and libVLC's time and length, all in milliseconds; `network` says the
    /// media is a URL (files are never live).
    pub fn update(&mut self, network: bool, clock: i64, time: i64, length: Option<i64>) {
        if !(network || self.known) || time <= 0 {
            return;
        }
        let length = length.filter(|&len| len > 0);
        match (&mut self.live, length) {
            // The length arrived after playback began.
            (Some(live), Some(len)) if live.window == 0 => live.window = len,
            (Some(live), _) => live.observe(clock, time),
            // No length at all: a plain live stream (rtsp, udp, radio).
            (None, None) => {
                if self.known {
                    self.live = Some(Live::new(0, clock, time));
                }
            }
            (None, Some(len)) => {
                if self.known || time > len + PAST_END {
                    self.live = Some(Live::new(len, clock, time));
                }
            }
        }
    }
}

/// "−12 с" / "−1:05": how far behind live, for the time label.
pub fn format_behind(ms: i64) -> String {
    let s = (ms.max(0) + 500) / 1000;
    if s < 60 {
        format!("−{s} с")
    } else {
        format!("−{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream playing steadily from `clock` for `ms`, its time `lead` ahead of the clock.
    fn play(d: &mut Detector, clock: &mut i64, ms: i64, lead: i64) {
        for _ in 0..ms / 250 {
            *clock += 250;
            d.update(true, *clock, *clock + lead, Some(28_000));
        }
    }

    #[test]
    fn files_and_videos_are_not_live() {
        let mut d = Detector::new(false);
        // A local file, even with a wrong length in its header.
        d.update(false, 1_000, 700_000, Some(600_000));
        assert!(d.live().is_none());
        // A video on the network, playing normally.
        d.update(true, 2_000, 30_000, Some(600_000));
        d.update(true, 3_000, 600_500, Some(600_000));
        assert!(d.live().is_none());
    }

    #[test]
    fn hls_window_is_detected_by_time_passing_its_end() {
        let mut d = Detector::new(false);
        d.update(true, 3_000, 11_000, Some(28_000));
        assert!(d.live().is_none());
        d.update(true, 21_000, 29_000, Some(28_000));
        assert!(d.live().is_none());
        d.update(true, 22_000, 30_000, Some(28_000));
        let live = d.live().unwrap();
        assert!(live.can_rewind() && live.is_at_edge());
        assert_eq!(live.depth(), 14_000);
        assert_eq!(live.fraction(), 1.0);
    }

    #[test]
    fn rewinding_shows_what_was_measured() {
        let mut d = Detector::new(true);
        let mut clock = 0;
        play(&mut d, &mut clock, 5_000, 8_000);
        assert!(d.live().unwrap().is_at_edge());

        // Seven seconds back: a quarter of the window before the start.
        let p = d.live_mut().unwrap().seek_by(-7_000, clock).unwrap();
        assert!((p - 0.25).abs() < 0.001);
        assert_eq!(d.live().unwrap().behind(), 7_000);
        // It landed 6.5 s back in fact, which shows once the time settles.
        play(&mut d, &mut clock, 500, 1_500);
        assert_eq!(d.live().unwrap().behind(), 7_000);
        play(&mut d, &mut clock, 1_000, 1_500);
        assert_eq!(d.live().unwrap().behind(), 6_500);
        assert!((d.live().unwrap().fraction() - (1.0 - 6.5 / 14.0)).abs() < 0.001);

        // No further back than there is.
        let p = d.live_mut().unwrap().seek_by(-60_000, clock).unwrap();
        assert_eq!(p, 0.0);
        assert_eq!(d.live().unwrap().behind(), 14_000);
        play(&mut d, &mut clock, 2_000, -6_800);
        assert_eq!(d.live().unwrap().behind(), 14_800);
        assert_eq!(d.live_mut().unwrap().seek_by(-5_000, clock), None);

        // Back to live: it lands a little ahead of where it started, and
        // that is live from now on.
        assert_eq!(d.live_mut().unwrap().seek_to(1.0, clock), Some(LIVE_POSITION));
        assert!(d.live().unwrap().is_at_edge());
        play(&mut d, &mut clock, 2_000, 11_000);
        assert_eq!(d.live().unwrap().behind(), 0);
        assert_eq!(d.live_mut().unwrap().seek_by(5_000, clock), None);
        // Three seconds back from there is the place it started at.
        let p = d.live_mut().unwrap().seek_by(-3_000, clock).unwrap();
        assert!((p - 0.5).abs() < 0.001);
    }

    #[test]
    fn stalls_fall_behind_and_live_is_a_click_away() {
        let mut d = Detector::new(true);
        let mut clock = 0;
        play(&mut d, &mut clock, 3_000, 8_000);
        // The network choked for six seconds.
        for stalled in 1..=24 {
            play(&mut d, &mut clock, 250, 8_000 - stalled * 250);
        }
        let live = d.live().unwrap();
        assert_eq!(live.behind(), 6_000);
        assert!(!live.is_at_edge());
        // A pause: the time stands still, then catches up with the broadcast.
        clock += 10_000;
        play(&mut d, &mut clock, 500, -8_000);
        assert_eq!(d.live().unwrap().behind(), 6_000);
        play(&mut d, &mut clock, 1_000, 1_500);
        assert_eq!(d.live().unwrap().behind(), 6_500);
        // The return to live lands two seconds short: close enough.
        d.live_mut().unwrap().seek_to(1.0, clock).unwrap();
        play(&mut d, &mut clock, 2_000, 6_000);
        assert_eq!(d.live().unwrap().behind(), 0);
    }

    #[test]
    fn timestamp_jumps_do_not_last() {
        let mut d = Detector::new(true);
        let mut clock = 1_000_000;
        play(&mut d, &mut clock, 3_000, 8_000);
        // Forward: nothing to see.
        play(&mut d, &mut clock, 3_000, 90_000);
        assert!(d.live().unwrap().is_at_edge());
        // Back: looks like the oldest there is, until the next return to live.
        play(&mut d, &mut clock, 3_000, -500_000);
        assert_eq!(d.live().unwrap().behind(), 28_000);
        assert_eq!(d.live().unwrap().fraction(), 0.0);
        d.live_mut().unwrap().seek_to(1.0, clock).unwrap();
        play(&mut d, &mut clock, 2_000, -500_000);
        assert!(d.live().unwrap().is_at_edge());
        // And rewinding goes on from there as before.
        let p = d.live_mut().unwrap().seek_by(-7_000, clock).unwrap();
        assert!((p - 0.25).abs() < 0.001);
    }

    #[test]
    fn plain_live_streams_cannot_be_rewound() {
        let mut d = Detector::new(true);
        d.update(true, 100, 0, None);
        assert!(d.live().is_none());
        d.update(true, 600, 500, None);
        let live = d.live_mut().unwrap();
        assert!(!live.can_rewind() && live.is_at_edge());
        assert_eq!(live.seek_by(-5_000, 700), None);
        assert_eq!(live.fraction(), 1.0);
        // The length may come a moment after playback starts.
        d.update(true, 900, 800, Some(28_000));
        assert_eq!(d.live().unwrap().depth(), 14_000);
    }

    #[test]
    fn behind_is_formatted() {
        assert_eq!(format_behind(4_600), "−5 с");
        assert_eq!(format_behind(65_000), "−1:05");
    }
}
