//! Twitch channels: turning `https://www.twitch.tv/<channel>` into the
//! address of the live stream itself, which libVLC can play.
//!
//! The same three steps as Twitch's own web player: ask its GraphQL API for
//! a playback token, get the channel's master playlist from the "usher"
//! service with that token, and pick the best variant from it. The variant
//! is played rather than the master playlist, because libVLC loses the sound
//! with Twitch's master playlists.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::http::{self, Method};

const CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";
const GQL_URL: &str = "https://gql.twitch.tv/gql";
const USHER_URL: &str = "https://usher.ttvnw.net/api/channel/hls";
const ACCESS_TOKEN_HASH: &str = "ed230aa1e33e07eebb8928504583da78a5173989fadfb1ac94be06a04f3cdbe9";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/157.0.0.0 Safari/537.36";

/// Path segments of twitch.tv that are pages, not channels.
const NOT_CHANNELS: &[&str] = &[
    "videos", "video", "clips", "clip", "directory", "category", "tags", "search", "settings", "subscriptions",
    "dashboard", "moderator", "popout", "downloads", "p",
];

/// The channel a location like `https://www.twitch.tv/name` points at
/// (also `twitch.tv/name`, `m.twitch.tv/name?x=1`); `None` for anything else,
/// including Twitch pages that are not a live channel (videos, clips).
pub fn channel(location: &str) -> Option<String> {
    let rest = location.trim();
    let rest = rest.strip_prefix("https://").or_else(|| rest.strip_prefix("http://")).unwrap_or(rest);
    let (host, path) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    if !matches!(host.as_str(), "twitch.tv" | "www.twitch.tv" | "m.twitch.tv" | "go.twitch.tv") {
        return None;
    }
    let name = path.split(['/', '?', '#']).next().unwrap_or_default().to_ascii_lowercase();
    let valid = (1..=40).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    (valid && !NOT_CHANNELS.contains(&name.as_str())).then_some(name)
}

/// Address of the best quality of the channel's live stream.
/// Asks Twitch over the network: call it off the UI thread.
pub fn resolve(channel: &str) -> Result<String, String> {
    let network = |e: String| format!("нет связи с Twitch ({e})");
    let headers = [("Client-Id", CLIENT_ID), ("User-Agent", USER_AGENT), ("Content-Type", "text/plain;charset=UTF-8")];
    let (status, body) = http::request(Method::Post, GQL_URL, &headers, Some(&token_request(channel))).map_err(network)?;
    if status != 200 {
        return Err(format!("Twitch ответил {status}"));
    }
    let (Some(token), Some(signature)) = (json_string(&body, "value"), json_string(&body, "signature")) else {
        return Err("канал не найден".to_owned());
    };

    let headers = [
        ("Client-Id", CLIENT_ID),
        ("User-Agent", USER_AGENT),
        ("Referer", "https://player.twitch.tv/"),
        ("Origin", "https://player.twitch.tv"),
    ];
    let (status, playlist) = http::request(Method::Get, &usher_url(channel, &token, &signature), &headers, None).map_err(network)?;
    match status {
        200 => best_variant(&playlist).ok_or_else(|| "в плейлисте Twitch нет потоков".to_owned()),
        // What usher answers for a channel that exists but is not streaming.
        404 => Err("канал сейчас не в эфире".to_owned()),
        _ => Err(format!("Twitch ответил {status}")),
    }
}

fn token_request(channel: &str) -> String {
    format!(
        r#"{{"operationName":"PlaybackAccessToken","variables":{{"isLive":true,"login":"{channel}","isVod":false,"vodID":"","playerType":"embed","platform":"web"}},"extensions":{{"persistedQuery":{{"version":1,"sha256Hash":"{ACCESS_TOKEN_HASH}"}}}}}}"#
    )
}

fn usher_url(channel: &str, token: &str, signature: &str) -> String {
    // Any number will do for `p`; the clock saves a dependency on a random source.
    let p = SystemTime::now().duration_since(UNIX_EPOCH).map_or(1_234_567, |d| d.subsec_nanos() % 10_000_000);
    format!(
        "{USHER_URL}/{channel}.m3u8?allow_source=true&allow_audio_only=true&fast_bread=true&p={p}\
         &play_session_id=00000000000000000000000000000000&player_backend=mediaplayer\
         &playlist_include_framerate=true&supported_codecs=av1%2Ch264&sig={}&token={}",
        percent_encode(signature),
        percent_encode(token)
    )
}

/// The variant to play from a master playlist: the source quality if it is
/// named, else the one with the highest bitrate.
fn best_variant(playlist: &str) -> Option<String> {
    let mut best: Option<(bool, u64, &str)> = None;
    let mut current: Option<(bool, u64)> = None;
    for line in playlist.lines().map(str::trim) {
        if let Some(attrs) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let mut source = false;
            let mut bandwidth = 0;
            for part in attrs.split(',') {
                if let Some(v) = part.strip_prefix("BANDWIDTH=") {
                    bandwidth = v.parse().unwrap_or(0);
                } else if let Some(v) =
                    part.strip_prefix("VIDEO=").or_else(|| part.strip_prefix("NAME=")).or_else(|| part.strip_prefix("STABLE-VARIANT-ID="))
                {
                    let name = v.trim_matches('"');
                    source |= name.eq_ignore_ascii_case("source") || name.eq_ignore_ascii_case("chunked");
                }
            }
            current = Some((source, bandwidth));
        } else if line.starts_with("http") {
            if let Some((source, bandwidth)) = current.take() {
                if best.is_none_or(|(s, b, _)| (source, bandwidth) > (s, b)) {
                    best = Some((source, bandwidth, line));
                }
            }
        }
    }
    best.map(|(_, _, url)| url.to_owned())
}

/// The string value of the first `"key":"..."` in a JSON text, unescaped.
/// Enough for the two fields read from Twitch's answer, without a JSON crate.
fn json_string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let after = &json[json.find(&needle)? + needle.len()..];
    let mut chars = after.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?.chars();
    let mut out = String::new();
    loop {
        match chars.next()? {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?).unwrap_or('\u{fffd}'));
                }
                c => out.push(c),
            },
            c => out.push(c),
        }
    }
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len() * 3);
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asks Twitch for real: `RESUMO_TEST_TWITCH=channel cargo test -- --ignored resolves --nocapture`.
    #[test]
    #[ignore = "needs the network and a channel that is live"]
    fn resolves_a_live_channel() {
        let name = std::env::var("RESUMO_TEST_TWITCH").expect("set RESUMO_TEST_TWITCH");
        let url = resolve(&name).unwrap();
        println!("{url}");
        assert!(url.starts_with("https://") && url.contains(".m3u8"));
    }

    #[test]
    fn channels_are_recognised() {
        assert_eq!(channel("https://www.twitch.tv/sadovov_58").as_deref(), Some("sadovov_58"));
        assert_eq!(channel(" twitch.tv/Some_Channel?referrer=raid ").as_deref(), Some("some_channel"));
        assert_eq!(channel("https://m.twitch.tv/name/").as_deref(), Some("name"));
        assert_eq!(channel("https://www.twitch.tv/videos/123456"), None);
        assert_eq!(channel("https://www.twitch.tv/directory/all"), None);
        assert_eq!(channel("https://www.twitch.tv/"), None);
        assert_eq!(channel("https://example.com/twitch.tv/name"), None);
        assert_eq!(channel("https://usher.ttvnw.net/api/channel/hls/name.m3u8"), None);
        assert_eq!(channel("C:/Videos/twitch.tv/film.mkv"), None);
    }

    #[test]
    fn best_variant_prefers_source() {
        let playlist = "#EXTM3U\n\
            #EXT-X-STREAM-INF:BANDWIDTH=9000000,RESOLUTION=1280x720,VIDEO=\"720p60\"\nhttps://a/720.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=8000000,RESOLUTION=1920x1080,CODECS=\"avc1.64002A,mp4a.40.2\",VIDEO=\"chunked\"\nhttps://a/source.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=160000,VIDEO=\"audio_only\"\nhttps://a/audio.m3u8\n";
        assert_eq!(best_variant(playlist).as_deref(), Some("https://a/source.m3u8"));
        let unnamed = "#EXT-X-STREAM-INF:BANDWIDTH=100\nhttps://a/low\n#EXT-X-STREAM-INF:BANDWIDTH=300\nhttps://a/high\n";
        assert_eq!(best_variant(unnamed).as_deref(), Some("https://a/high"));
        assert_eq!(best_variant("#EXTM3U\n"), None);
    }

    #[test]
    fn token_is_read_and_encoded() {
        let json = r#"{"data":{"streamPlaybackAccessToken":{"value":"{\"channel\":\"a\",\"x\":\"\u0041\"}","signature":"0af3","__typename":"PlaybackAccessToken"}}}"#;
        assert_eq!(json_string(json, "value").as_deref(), Some(r#"{"channel":"a","x":"A"}"#));
        assert_eq!(json_string(json, "signature").as_deref(), Some("0af3"));
        assert_eq!(json_string(r#"{"data":{"streamPlaybackAccessToken":null}}"#, "value"), None);
        let url = usher_url("name", r#"{"a":1}"#, "0af3");
        assert!(url.starts_with("https://usher.ttvnw.net/api/channel/hls/name.m3u8?allow_source=true&"));
        assert!(url.ends_with("&sig=0af3&token=%7B%22a%22%3A1%7D"));
        assert!(token_request("name").contains(r#""login":"name""#));
    }
}
