//! Sites whose links are pages, not streams: the player asks the site where
//! the stream is before playing.

use crate::{twitch, youtube};

pub use crate::youtube::Stream;

/// A link the player knows how to turn into a stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Site {
    /// A Twitch channel.
    Twitch(String),
    YouTube(youtube::Link),
}

impl Site {
    /// The site `location` is a page of, if it is one the player knows.
    pub fn of(location: &str) -> Option<Self> {
        twitch::channel(location).map(Site::Twitch).or_else(|| youtube::link(location).map(Site::YouTube))
    }

    pub fn name(&self) -> &'static str {
        match self {
            Site::Twitch(_) => "Twitch",
            Site::YouTube(_) => "YouTube",
        }
    }

    /// Whatever the link gives is a broadcast (YouTube says so only in its answer).
    pub fn always_live(&self) -> bool {
        matches!(self, Site::Twitch(_))
    }

    /// Asks the site for the stream. Blocks on the network: call it off the UI thread.
    pub fn resolve(&self) -> Result<Stream, String> {
        match self {
            Site::Twitch(channel) => {
                twitch::resolve(channel).map(|address| Stream { address, audio: None, title: None, live: true })
            }
            Site::YouTube(link) => youtube::resolve(link),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sites_are_told_apart() {
        assert_eq!(Site::of("https://www.twitch.tv/sadovov_58"), Some(Site::Twitch("sadovov_58".into())));
        let youtube = Site::of("https://youtu.be/dQw4w9WgXcQ").unwrap();
        assert_eq!((youtube.name(), youtube.always_live()), ("YouTube", false));
        assert_eq!(Site::of("https://example.org/video.mp4"), None);
        assert_eq!(Site::of("/home/user/video.mkv"), None);
    }
}
