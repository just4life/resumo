//! Reads a local video ahead of libVLC. Android 11 passes USB drives and the
//! shared storage through a slow layer (FUSE) that can stall a read for a
//! moment; libVLC reads the file on the same thread that feeds the decoder,
//! so with a 4K film every stall is a freeze. Here a thread of its own reads
//! the file in big blocks into a large buffer, and libVLC takes from that
//! buffer (as a callbacks media, see [`crate::vlc::OpenStream`]).

use std::collections::VecDeque;
use std::fs::File;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::playlist;
use crate::vlc::{self, OpenStream, Stream};

/// One read from the file.
const BLOCK: usize = 4 << 20;
/// How far ahead of libVLC the file is read: about ten seconds of a 4K
/// Blu-ray remux.
const AHEAD: usize = 96 << 20;

/// Opens `path` for libVLC, read ahead.
pub fn open(path: PathBuf) -> OpenStream {
    Box::new(move || {
        let (file, used) = open_direct(&path)?;
        let size = file.metadata().ok()?.len();
        log::info!("reading {} ahead", used.display());
        let shared = Arc::new(Shared { state: Mutex::new(State::default()), cond: Condvar::new() });
        let reader = shared.clone();
        std::thread::Builder::new().name("read-ahead".into()).spawn(move || fill(file, &reader)).ok()?;
        Some((Box::new(ReadAhead { shared }) as Box<dyn Stream>, size))
    })
}

/// The drive itself rather than its FUSE view, where the system lets the
/// app read it (`/storage/1234-ABCD/…` is `/mnt/media_rw/1234-ABCD/…`).
fn open_direct(path: &Path) -> Option<(File, PathBuf)> {
    let direct = path.strip_prefix("/storage").ok().filter(|rest| !rest.starts_with("emulated") && !rest.starts_with("self"));
    if let Some(direct) = direct.map(|rest| Path::new("/mnt/media_rw").join(rest)) {
        if let Ok(f) = File::open(&direct) {
            return Some((f, direct));
        }
    }
    Some((File::open(path).ok()?, path.to_owned()))
}

/// Subtitle files next to `path` named like it, which libVLC can't look for
/// itself in a callbacks media.
pub fn subtitles_beside(path: &Path) -> Vec<String> {
    let (Some(dir), Some(stem)) = (path.parent(), path.file_stem().and_then(|s| s.to_str())) else { return Vec::new() };
    let stem = stem.to_lowercase();
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let ext = p.extension().and_then(|e| e.to_str()).map(str::to_lowercase);
            let name = p.file_name().and_then(|n| n.to_str()).map(str::to_lowercase);
            ext.is_some_and(|e| playlist::SUBTITLE_EXTENSIONS.contains(&e.as_str()))
                && name.is_some_and(|n| n.starts_with(&stem))
        })
        .collect();
    found.sort();
    found.iter().map(|p| vlc::path_to_uri(p)).collect()
}

struct Shared {
    state: Mutex<State>,
    cond: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Default)]
struct State {
    /// Where in the file `buf` begins: libVLC's position.
    start: u64,
    buf: VecDeque<u8>,
    /// A position for the reading thread to go to.
    seek: Option<u64>,
    /// Bumped on every seek, so that a block read before it is dropped.
    generation: u64,
    end: bool,
    failed: bool,
    closed: bool,
}

/// The reading thread.
fn fill(mut file: File, shared: &Shared) {
    let mut block = vec![0u8; BLOCK];
    loop {
        let generation = {
            let mut s = shared.lock();
            loop {
                if s.closed {
                    return;
                }
                if let Some(to) = s.seek.take() {
                    s.failed = file.seek(SeekFrom::Start(to)).is_err();
                    s.end = false;
                }
                if !s.end && !s.failed && s.buf.len() < AHEAD {
                    break s.generation;
                }
                s = shared.cond.wait(s).unwrap_or_else(|e| e.into_inner());
            }
        };
        let read = loop {
            match file.read(&mut block) {
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                r => break r,
            }
        };
        let mut s = shared.lock();
        if s.generation != generation {
            continue;
        }
        match read {
            Ok(0) => s.end = true,
            Ok(n) => s.buf.extend(&block[..n]),
            Err(e) => {
                log::warn!("read-ahead: {e}");
                s.failed = true;
            }
        }
        shared.cond.notify_all();
    }
}

struct ReadAhead {
    shared: Arc<Shared>,
}

impl Stream for ReadAhead {
    fn read(&mut self, out: &mut [u8]) -> Option<usize> {
        let mut s = self.shared.lock();
        loop {
            if !s.buf.is_empty() {
                let n = out.len().min(s.buf.len());
                let (a, b) = s.buf.as_slices();
                let from_a = n.min(a.len());
                out[..from_a].copy_from_slice(&a[..from_a]);
                out[from_a..n].copy_from_slice(&b[..n - from_a]);
                s.buf.drain(..n);
                s.start += n as u64;
                self.shared.cond.notify_all();
                return Some(n);
            }
            if s.end {
                return Some(0);
            }
            if s.failed {
                return None;
            }
            s = self.shared.cond.wait(s).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn seek(&mut self, to: u64) -> bool {
        let mut s = self.shared.lock();
        let have = s.start..=s.start + s.buf.len() as u64;
        if have.contains(&to) {
            // Forward into what is read already (the demuxer skipping).
            let skip = (to - s.start) as usize;
            s.buf.drain(..skip);
        } else {
            s.buf.clear();
            s.seek = Some(to);
            s.generation += 1;
            s.end = false;
            s.failed = false;
        }
        s.start = to;
        self.shared.cond.notify_all();
        true
    }
}

impl Drop for ReadAhead {
    fn drop(&mut self) {
        // The thread ends on its own once a read in progress comes back.
        self.shared.lock().closed = true;
        self.shared.cond.notify_all();
    }
}
