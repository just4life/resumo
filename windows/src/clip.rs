//! Cutting a piece of the video into a file of its own.
//!
//! The piece is copied, not re-encoded: a second libVLC player reads the
//! source between the two marks and writes the same video and audio packets
//! into a new container (libVLC's stream output). That takes seconds and
//! loses no quality, but a copy can only start at a key frame, so the clip
//! may begin a moment before the start mark.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::snapshot;
use crate::vlc::{Instance, MediaPlayer, State};

/// Shortest piece worth saving, milliseconds.
pub const MIN_LENGTH: i64 = 500;

/// The piece being marked, in milliseconds of the video.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Marks {
    pub start: i64,
    /// `None` until set: the clip then ends wherever the video is now.
    pub end: Option<i64>,
}

impl Marks {
    pub fn new(start: i64) -> Self {
        Self { start: start.max(0), end: None }
    }

    /// Moves the start; an end that is no longer after it is dropped.
    pub fn set_start(&mut self, t: i64) {
        self.start = t.max(0);
        if self.end.is_some_and(|e| e < self.start + MIN_LENGTH) {
            self.end = None;
        }
    }

    /// Sets the end; false (and no change) if `t` is not after the start.
    pub fn set_end(&mut self, t: i64) -> bool {
        let ok = t >= self.start + MIN_LENGTH;
        if ok {
            self.end = Some(t);
        }
        ok
    }

    /// Start and end of the clip with the video at `now`, once there is one.
    pub fn range(&self, now: i64) -> Option<(i64, i64)> {
        let end = self.end.unwrap_or(now);
        (end >= self.start + MIN_LENGTH).then_some((self.start, end))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Mp4,
    Mkv,
}

impl Container {
    fn extension(self) -> &'static str {
        match self {
            Container::Mp4 => "mp4",
            Container::Mkv => "mkv",
        }
    }

    /// libVLC's own MP4 muxer writes repeated timestamps into cut pieces,
    /// so both go through FFmpeg's muxers (part of libVLC's avcodec plugin).
    /// `reset-ts` makes an MP4 start at zero whatever the source is; the
    /// Matroska muxer does that itself and gets it wrong when told to.
    fn muxer(self) -> &'static str {
        match self {
            Container::Mp4 => "avformat{mux=mp4,reset-ts}",
            Container::Mkv => "avformat{mux=matroska}",
        }
    }
}

/// Containers to try, best first, for codecs given as libVLC FourCCs:
/// MP4 opens everywhere but holds few codecs, Matroska holds them all.
pub fn containers(video: Option<&str>, audio: Option<&str>) -> Vec<Container> {
    let fits = |codec: Option<&str>, known: &[&str]| codec.is_none_or(|c| known.contains(&c.trim()));
    if fits(video, &["h264", "hevc"]) && fits(audio, &["mp4a", "a52", "eac3", "mpga", "mp3"]) {
        vec![Container::Mp4, Container::Mkv]
    } else {
        vec![Container::Mkv]
    }
}

/// Whether `source` is an MP4 / MOV file: by its first bytes for a local
/// file, by its extension for a URL.
pub fn is_mp4(source: &str) -> bool {
    if !source.contains("://") {
        use std::io::Read;
        let mut head = [0u8; 12];
        if std::fs::File::open(source).and_then(|mut f| f.read_exact(&mut head)).is_ok() {
            return matches!(&head[4..8], b"ftyp" | b"moov" | b"mdat" | b"free" | b"wide" | b"skip");
        }
    }
    let path = source.split(['?', '#']).next().unwrap_or(source).to_ascii_lowercase();
    ["mp4", "m4v", "mov", "m4a", "3gp"].iter().any(|ext| path.ends_with(&format!(".{ext}")))
}

/// `<Videos>/Resumo`, where saved clips go.
pub fn folder() -> PathBuf {
    snapshot::videos_dir().join("Resumo")
}

/// "Movie 00-01-40_00-01-55" for the piece of "Movie" from 1:40 to 1:55.
pub fn file_stem(title: &str, start: i64, end: i64) -> String {
    format!("{} {}_{}", snapshot::safe_name(title), snapshot::time_tag(start), snapshot::time_tag(end))
}

/// What to cut and where to put it.
pub struct Job {
    /// Local path or URL, as the player got it.
    pub source: String,
    /// The source is an MP4 or MOV file (see [`is_mp4`]).
    pub mp4_source: bool,
    pub start: i64,
    pub end: i64,
    /// libVLC id of the audio track to keep; negative for no sound.
    pub audio_track: i32,
    pub dir: PathBuf,
    /// File name without the extension, which the container decides.
    pub stem: String,
    pub containers: Vec<Container>,
}

pub enum Status {
    Running,
    Done(PathBuf),
    Failed(String),
}

/// A clip being written. Dropping it cancels the work.
pub struct Export {
    job: Job,
    /// Index into `job.containers` of the one being written.
    attempt: usize,
    player: MediaPlayer,
    /// Written under a plain temporary name and renamed when complete, so a
    /// half-written clip never looks like a finished one.
    temp: PathBuf,
}

impl Export {
    pub fn start(instance: &Instance, job: Job) -> Result<Self, String> {
        if job.containers.is_empty() {
            return Err("no container".to_owned());
        }
        std::fs::create_dir_all(&job.dir).map_err(|e| e.to_string())?;
        let (player, temp) = begin(instance, &job, job.containers[0])?;
        Ok(Self { job, attempt: 0, player, temp })
    }

    /// Share of the clip written so far, 0..1.
    pub fn progress(&self) -> f32 {
        let done = (self.player.time() - self.job.start) as f32 / (self.job.end - self.job.start).max(1) as f32;
        done.clamp(0.0, 1.0)
    }

    /// Call regularly (from the UI thread) until it stops returning `Running`.
    pub fn poll(&mut self, instance: &Instance) -> Status {
        match self.player.state() {
            State::Busy => Status::Running,
            state => {
                // Stopping closes the output, which is when the file gets its index.
                self.player.stop();
                let written = std::fs::metadata(&self.temp).is_ok_and(|m| m.len() > 0);
                if state == State::Finished && written {
                    let container = self.job.containers[self.attempt];
                    let path = snapshot::unique_path(&self.job.dir, &format!("{}.{}", self.job.stem, container.extension()));
                    return match std::fs::rename(&self.temp, &path) {
                        Ok(()) => Status::Done(path),
                        Err(e) => Status::Failed(e.to_string()),
                    };
                }
                let _ = std::fs::remove_file(&self.temp);
                self.attempt += 1;
                let Some(&next) = self.job.containers.get(self.attempt) else {
                    return Status::Failed("libVLC не смог записать файл".to_owned());
                };
                match begin(instance, &self.job, next) {
                    Ok((player, temp)) => {
                        self.player = player;
                        self.temp = temp;
                        Status::Running
                    }
                    Err(e) => Status::Failed(e),
                }
            }
        }
    }
}

impl Drop for Export {
    fn drop(&mut self) {
        self.player.stop();
        // Gone already if the clip was finished and renamed.
        let _ = std::fs::remove_file(&self.temp);
    }
}

fn begin(instance: &Instance, job: &Job, container: Container) -> Result<(MediaPlayer, PathBuf), String> {
    static SERIAL: AtomicU32 = AtomicU32::new(0);
    let n = SERIAL.fetch_add(1, Ordering::Relaxed);
    let temp = job.dir.join(format!("resumo-clip-{}-{n}.part", std::process::id()));
    let player = MediaPlayer::new(instance)?;
    player.set_media_with(&job.source, &options(job, container, &temp))?;
    if !player.play() {
        return Err("libVLC не смог начать запись".to_owned());
    }
    Ok((player, temp))
}

/// libVLC media options that copy the piece into `output`.
fn options(job: &Job, container: Container, output: &Path) -> Vec<String> {
    let seconds = |ms: i64| format!("{}.{:03}", ms / 1000, ms % 1000);
    let mut out = vec![
        format!(":start-time={}", seconds(job.start.max(0))),
        format!(":stop-time={}", seconds(job.end.max(0))),
        // Only the sound that is playing; subtitles are left out.
        ":no-sout-all".to_owned(),
        ":no-sout-spu".to_owned(),
    ];
    // libVLC's own MP4 reader hands out a little of the very beginning
    // before it jumps to the start time, and that often ends up in the clip,
    // with a long gap before the rest. FFmpeg's reader doesn't (but it cuts
    // Matroska wrongly, so it is used for MP4 only).
    if job.mp4_source {
        out.push(":demux=avformat".to_owned());
    }
    if job.audio_track >= 0 {
        out.push(format!(":audio-track-id={}", job.audio_track));
    } else {
        out.push(":no-sout-audio".to_owned());
    }
    out.push(format!(
        ":sout=#standard{{access=file,mux={},dst='{}'}}",
        container.muxer(),
        escape(&output.to_string_lossy())
    ));
    out
}

/// Escapes a value for a quoted place in a libVLC stream output chain.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\\' | '\'' | '"') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cuts a real file: `RESUMO_TEST_VIDEO=film.mkv cargo test -- --ignored cuts`.
    /// The clips are left in the system's temporary folder to look at.
    #[test]
    #[ignore = "needs a video file and libVLC's plugins"]
    fn cuts_a_real_file() {
        let source = std::env::var("RESUMO_TEST_VIDEO").expect("set RESUMO_TEST_VIDEO");
        let audio_track = std::env::var("RESUMO_TEST_AUDIO").map_or(-1, |v| v.parse().unwrap());
        let dir = std::env::temp_dir().join("resumo clip's test");
        let instance = Instance::new(&["--quiet"]).unwrap();
        for containers in [vec![Container::Mp4], vec![Container::Mkv]] {
            let job = Job {
                source: source.clone(),
                mp4_source: is_mp4(&source),
                start: 100_000,
                end: 115_500,
                audio_track,
                dir: dir.clone(),
                stem: "clip".into(),
                containers,
            };
            let mut export = Export::start(&instance, job).unwrap();
            let path = loop {
                match export.poll(&instance) {
                    Status::Running => std::thread::sleep(std::time::Duration::from_millis(20)),
                    Status::Done(path) => break path,
                    Status::Failed(e) => panic!("{e}"),
                }
            };
            assert!(std::fs::metadata(&path).unwrap().len() > 10_000);
            assert!(!export.temp.exists());
            println!("{}", path.display());
        }
    }

    #[test]
    fn marks_keep_the_end_after_the_start() {
        let mut m = Marks::new(10_000);
        // No end yet: the clip runs to where the video is.
        assert_eq!(m.range(10_200), None);
        assert_eq!(m.range(14_000), Some((10_000, 14_000)));
        assert!(!m.set_end(10_100));
        assert!(m.set_end(20_000));
        assert_eq!(m.range(99_000), Some((10_000, 20_000)));
        m.set_start(15_000);
        assert_eq!(m.range(0), Some((15_000, 20_000)));
        // A start after the end starts the clip over.
        m.set_start(30_000);
        assert_eq!(m, Marks { start: 30_000, end: None });
    }

    #[test]
    fn container_follows_the_codecs() {
        assert_eq!(containers(Some("h264"), Some("mp4a")), vec![Container::Mp4, Container::Mkv]);
        assert_eq!(containers(Some("hevc"), Some("a52 ")), vec![Container::Mp4, Container::Mkv]);
        assert_eq!(containers(Some("h264"), None), vec![Container::Mp4, Container::Mkv]);
        assert_eq!(containers(Some("VP90"), Some("Opus")), vec![Container::Mkv]);
        assert_eq!(containers(Some("h264"), Some("dts")), vec![Container::Mkv]);
    }

    #[test]
    fn names_and_options() {
        assert_eq!(file_stem("Kung Fu: Soccer", 100_000, 3_715_500), "Kung Fu_ Soccer 00-01-40_01-01-55");
        let job = Job {
            source: "a.mkv".into(),
            mp4_source: false,
            start: 100_040,
            end: 115_500,
            audio_track: 2,
            dir: PathBuf::new(),
            stem: String::new(),
            containers: vec![Container::Mp4],
        };
        let opts = options(&job, Container::Mp4, Path::new(r"C:\Users\O'Neil\x.part"));
        assert_eq!(opts[0], ":start-time=100.040");
        assert_eq!(opts[1], ":stop-time=115.500");
        assert!(opts.contains(&":audio-track-id=2".to_owned()));
        assert_eq!(
            opts.last().unwrap(),
            r":sout=#standard{access=file,mux=avformat{mux=mp4,reset-ts},dst='C:\\Users\\O\'Neil\\x.part'}"
        );
        assert!(!opts.contains(&":demux=avformat".to_owned()));
        let silent = options(&Job { audio_track: -1, mp4_source: true, ..job }, Container::Mkv, Path::new("x"));
        assert!(silent.contains(&":no-sout-audio".to_owned()));
        assert!(silent.contains(&":demux=avformat".to_owned()));
        assert!(is_mp4("https://example.com/a/Film.MP4?x=1"));
        assert!(!is_mp4("https://example.com/film.mkv"));
    }
}
