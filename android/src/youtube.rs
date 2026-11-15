//! YouTube links: turning the address of a video's page into the addresses
//! of its streams, which libVLC can play.
//!
//! YouTube's own apps ask its "player" API about a video and get back the
//! list of its formats with ready addresses; the same is asked here, as the
//! app for VR headsets does (its answers need no further deciphering). A
//! broadcast comes as one HLS playlist. A video comes as separate picture and
//! sound, each in several qualities: the best picture up to 1080p is played
//! with the best sound beside it.
//!
//! YouTube changes this API as it sees fit and refuses some requests (age
//! limits, some networks it takes for robots). Whatever it says is passed on
//! to the user in its own words.

use serde_json::{json, Value};

use crate::http::{self, Method};

const PLAYER_URL: &str = "https://www.youtube.com/youtubei/v1/player?prettyPrint=false";
/// Taller pictures are left alone: beyond this YouTube has only codecs that
/// many devices decode without their video chip.
const MAX_HEIGHT: i64 = 1080;
const BROWSER: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// An app of YouTube's whose requests are repeated.
struct Client {
    name: &'static str,
    /// The app's number in the `X-YouTube-Client-Name` header.
    id: &'static str,
    version: &'static str,
    user_agent: &'static str,
    /// The rest of what the app says of itself.
    device: fn() -> Value,
}

/// The apps to ask as, in turn, until one gets streams: the VR app first,
/// whose addresses work as they are; the iPhone app gives HLS playlists.
const CLIENTS: &[Client] = &[
    Client {
        name: "ANDROID_VR",
        id: "28",
        version: "1.62.27",
        user_agent: "com.google.android.apps.youtube.vr.oculus/1.62.27 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip",
        device: || json!({"deviceMake": "Oculus", "deviceModel": "Quest 3", "androidSdkVersion": 32, "osName": "Android", "osVersion": "12L"}),
    },
    Client {
        name: "IOS",
        id: "5",
        version: "20.03.02",
        user_agent: "com.google.ios.youtube/20.03.02 (iPhone16,2; U; CPU iOS 18_2_1 like Mac OS X;)",
        device: || json!({"deviceMake": "Apple", "deviceModel": "iPhone16,2", "osName": "iPhone", "osVersion": "18.2.1.22C161"}),
    },
];

/// What a YouTube address points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    /// A video or a broadcast, by its id.
    Video(String),
    /// A channel: the address of the page that shows its current broadcast.
    Channel(String),
}

/// The streams of a video.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stream {
    /// The picture, or picture and sound together.
    pub address: String,
    /// The sound, when it comes apart from the picture.
    pub audio: Option<String>,
    pub title: Option<String>,
    /// A broadcast going on now.
    pub live: bool,
}

/// Recognises YouTube addresses: `youtube.com/watch?v=…`, `youtu.be/…`,
/// `/live/…`, `/shorts/…`, `/embed/…`, and channels (`/@name`, `/@name/live`,
/// `/channel/…`), which stand for their current broadcast.
pub fn link(location: &str) -> Option<Link> {
    let rest = location.trim();
    let rest = rest.strip_prefix("https://").or_else(|| rest.strip_prefix("http://")).unwrap_or(rest);
    let (host, rest) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").or_else(|| host.strip_prefix("m.")).or_else(|| host.strip_prefix("music.")).unwrap_or(&host);
    let rest = rest.split('#').next().unwrap_or_default();
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let video = |id: &str| is_video_id(id).then(|| Link::Video(id.to_owned()));
    match (host, segments.as_slice()) {
        ("youtu.be", [id, ..]) => video(id),
        ("youtube.com" | "youtube-nocookie.com", ["watch", ..]) => {
            query.split('&').find_map(|pair| pair.strip_prefix("v=")).and_then(video)
        }
        ("youtube.com" | "youtube-nocookie.com", ["live" | "shorts" | "embed" | "v", id, ..]) => video(id),
        ("youtube.com", [name]) | ("youtube.com", [name, "live" | "streams" | "featured" | "videos"]) if is_handle(name) => {
            Some(Link::Channel(format!("https://www.youtube.com/{name}/live")))
        }
        ("youtube.com", [kind @ ("channel" | "c" | "user"), name]) | ("youtube.com", [kind @ ("channel" | "c" | "user"), name, "live"])
            if is_name(name) =>
        {
            Some(Link::Channel(format!("https://www.youtube.com/{kind}/{name}/live")))
        }
        _ => None,
    }
}

fn is_video_id(id: &str) -> bool {
    id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A channel's `@name`. Names may be in any alphabet, also percent-encoded.
fn is_handle(segment: &str) -> bool {
    segment.strip_prefix('@').is_some_and(is_name)
}

fn is_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 200 && name.chars().all(|c| c.is_alphanumeric() || "_-.%".contains(c))
}

/// Asks YouTube for the streams behind `link`.
/// Blocks on the network: call it off the UI thread.
pub fn resolve(link: &Link) -> Result<Stream, String> {
    let id = match link {
        Link::Video(id) => id.clone(),
        Link::Channel(page) => current_broadcast(page)?,
    };
    let mut first_error = None;
    for client in CLIENTS {
        match ask(client, &id).and_then(|answer| pick(&answer)) {
            Ok(stream) => return Ok(stream),
            Err(e) => first_error = first_error.or(Some(e)),
        }
    }
    Err(first_error.unwrap_or_else(|| "видео недоступно".to_owned()))
}

/// The video a channel is broadcasting now: its "live" page names it as the
/// page's canonical address (and names the channel itself when it is not).
fn current_broadcast(page: &str) -> Result<String, String> {
    let headers = [("User-Agent", BROWSER), ("Accept-Language", "ru,en;q=0.8")];
    let (status, html) = http::request(Method::Get, page, &headers, None).map_err(|e| format!("нет связи с YouTube ({e})"))?;
    match status {
        200 => canonical_video(&html).ok_or_else(|| "канал сейчас не ведёт трансляцию".to_owned()),
        404 => Err("канал не найден".to_owned()),
        _ => Err(format!("YouTube ответил {status}")),
    }
}

fn canonical_video(html: &str) -> Option<String> {
    let marker = "<link rel=\"canonical\" href=\"";
    let address = html.split(marker).nth(1)?.split('"').next()?;
    match link(address)? {
        Link::Video(id) => Some(id),
        Link::Channel(_) => None,
    }
}

fn ask(client: &Client, id: &str) -> Result<Value, String> {
    let mut about = (client.device)();
    about["clientName"] = client.name.into();
    about["clientVersion"] = client.version.into();
    about["userAgent"] = client.user_agent.into();
    // The language YouTube explains refusals in.
    about["hl"] = "ru".into();
    let body = json!({"videoId": id, "context": {"client": about}, "contentCheckOk": true, "racyCheckOk": true});
    let headers = [
        ("Content-Type", "application/json"),
        ("User-Agent", client.user_agent),
        ("X-YouTube-Client-Name", client.id),
        ("X-YouTube-Client-Version", client.version),
    ];
    let (status, text) =
        http::request(Method::Post, PLAYER_URL, &headers, Some(&body.to_string())).map_err(|e| format!("нет связи с YouTube ({e})"))?;
    match status {
        200 => {}
        // What YouTube answers to networks it takes for robots.
        403 | 429 => return Err(format!("YouTube отказал в ответе этой сети ({status})")),
        _ => return Err(format!("YouTube ответил {status}")),
    }
    serde_json::from_str(&text).map_err(|_| "YouTube ответил непонятно".to_owned())
}

/// Chooses what to play from the player API's answer.
fn pick(answer: &Value) -> Result<Stream, String> {
    let playability = &answer["playabilityStatus"];
    let status = playability["status"].as_str().unwrap_or_default();
    if status != "OK" {
        let reason = playability["reason"].as_str().or_else(|| playability["messages"][0].as_str());
        return Err(match (status, reason) {
            (_, Some(reason)) => reason.trim_end_matches('.').to_owned(),
            ("LIVE_STREAM_OFFLINE", None) => "трансляция ещё не началась".to_owned(),
            _ => "видео недоступно".to_owned(),
        });
    }
    let details = &answer["videoDetails"];
    let title = details["title"].as_str().map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned);
    let live = details["isLive"].as_bool().unwrap_or(false);
    let data = &answer["streamingData"];
    let hls = data["hlsManifestUrl"].as_str().map(str::to_owned);
    let stream = |address: String, audio: Option<String>| Stream { address, audio, title: title.clone(), live };

    // A broadcast is its playlist: the separate formats are only its latest seconds.
    if let (true, Some(hls)) = (live, &hls) {
        return Ok(stream(hls.clone(), None));
    }
    let formats = |list: &str| data[list].as_array().map(Vec::as_slice).unwrap_or_default().iter().filter(|f| f["url"].is_string());
    let kind = |f: &Value, kind: &str| f["mimeType"].as_str().is_some_and(|m| m.starts_with(kind));
    let address = |f: &Value| f["url"].as_str().unwrap_or_default().to_owned();
    let number = |f: &Value, key: &str| f[key].as_i64().unwrap_or(0);

    let picture = formats("adaptiveFormats").filter(|f| kind(f, "video/")).max_by_key(|f| {
        let height = number(f, "height");
        // H.264 first at the same size: every device decodes it in hardware.
        let h264 = f["mimeType"].as_str().is_some_and(|m| m.contains("avc1"));
        (height <= MAX_HEIGHT, if height <= MAX_HEIGHT { height } else { -height }, h264, number(f, "fps"), number(f, "bitrate"))
    });
    let sound = formats("adaptiveFormats")
        .filter(|f| kind(f, "audio/"))
        // Not a dubbing into another language, nor the flattened-loudness copy.
        .filter(|f| f["audioTrack"]["audioIsDefault"].as_bool().unwrap_or(true) && !f["isDrc"].as_bool().unwrap_or(false))
        .max_by_key(|f| number(f, "bitrate"));
    if let (Some(picture), Some(sound)) = (picture, sound) {
        return Ok(stream(address(picture), Some(address(sound))));
    }
    if let Some(hls) = hls {
        return Ok(stream(hls, None));
    }
    // Picture and sound in one file: small, but better than nothing.
    if let Some(whole) = formats("formats").max_by_key(|f| (number(f, "height"), number(f, "bitrate"))) {
        return Ok(stream(address(whole), None));
    }
    Err("YouTube не дал адресов потоков для этого видео".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_recognised() {
        let video = |id: &str| Some(Link::Video(id.to_owned()));
        assert_eq!(link("https://www.youtube.com/watch?v=dQw4w9WgXcQ"), video("dQw4w9WgXcQ"));
        assert_eq!(link("https://youtube.com/watch?feature=share&v=dQw4w9WgXcQ&t=42s#x"), video("dQw4w9WgXcQ"));
        assert_eq!(link("https://youtu.be/dQw4w9WgXcQ?si=abc"), video("dQw4w9WgXcQ"));
        assert_eq!(link("m.youtube.com/shorts/aqz-KE-bpKQ"), video("aqz-KE-bpKQ"));
        assert_eq!(link("https://www.youtube.com/live/aqz-KE-bpKQ?feature=shared"), video("aqz-KE-bpKQ"));
        assert_eq!(link("https://www.youtube-nocookie.com/embed/aqz-KE-bpKQ"), video("aqz-KE-bpKQ"));
        assert_eq!(link("https://music.youtube.com/watch?v=aqz-KE-bpKQ&list=RDAMVM"), video("aqz-KE-bpKQ"));

        let channel = |page: &str| Some(Link::Channel(page.to_owned()));
        assert_eq!(link("https://www.youtube.com/@NASA"), channel("https://www.youtube.com/@NASA/live"));
        assert_eq!(link("https://www.youtube.com/@NASA/live"), channel("https://www.youtube.com/@NASA/live"));
        assert_eq!(link("https://www.youtube.com/@NASA/streams"), channel("https://www.youtube.com/@NASA/live"));
        assert_eq!(
            link("https://www.youtube.com/channel/UCLA_DiR1FfKNvjuUpBHmylQ/live"),
            channel("https://www.youtube.com/channel/UCLA_DiR1FfKNvjuUpBHmylQ/live")
        );

        for other in [
            "https://www.youtube.com/",
            "https://www.youtube.com/watch?v=short",
            "https://www.youtube.com/playlist?list=PL123",
            "https://www.youtube.com/feed/subscriptions",
            "https://youtu.be/",
            "https://example.org/watch?v=dQw4w9WgXcQ",
            "https://www.twitch.tv/youtube",
            "C:\\video\\youtube.com\\watch.mp4",
        ] {
            assert_eq!(link(other), None, "{other}");
        }
    }

    #[test]
    fn the_broadcast_of_a_channel_is_found_in_its_page() {
        let live = r#"<head><link rel="canonical" href="https://www.youtube.com/watch?v=aqz-KE-bpKQ"><meta x>"#;
        assert_eq!(canonical_video(live).as_deref(), Some("aqz-KE-bpKQ"));
        let idle = r#"<link rel="canonical" href="https://www.youtube.com/channel/UCLA_DiR1FfKNvjuUpBHmylQ">"#;
        assert_eq!(canonical_video(idle), None);
        assert_eq!(canonical_video("<html></html>"), None);
    }

    fn video(itag: i64, mime: &str, height: i64, bitrate: i64) -> Value {
        json!({"itag": itag, "url": format!("https://v/{itag}"), "mimeType": mime, "height": height, "fps": 30, "bitrate": bitrate})
    }

    fn audio(itag: i64, mime: &str, bitrate: i64) -> Value {
        json!({"itag": itag, "url": format!("https://a/{itag}"), "mimeType": mime, "bitrate": bitrate})
    }

    #[test]
    fn a_video_is_its_best_picture_and_sound() {
        let mut dubbed = audio(999, "audio/webm; codecs=\"opus\"", 900_000);
        dubbed["audioTrack"] = json!({"displayName": "English (dubbed)", "audioIsDefault": false});
        let mut original = audio(251, "audio/webm; codecs=\"opus\"", 160_000);
        original["audioTrack"] = json!({"displayName": "Русский (оригинал)", "audioIsDefault": true});
        let mut flattened = audio(998, "audio/webm; codecs=\"opus\"", 800_000);
        flattened["isDrc"] = true.into();
        let answer = json!({
            "playabilityStatus": {"status": "OK"},
            "videoDetails": {"title": " Кот играет на пианино ", "isLive": false},
            "streamingData": {
                "formats": [{"itag": 18, "url": "https://whole/18", "mimeType": "video/mp4", "height": 360, "bitrate": 500_000}],
                "adaptiveFormats": [
                    video(313, "video/webm; codecs=\"vp9\"", 2160, 18_000_000),
                    video(248, "video/webm; codecs=\"vp9\"", 1080, 2_600_000),
                    video(137, "video/mp4; codecs=\"avc1.640028\"", 1080, 4_000_000),
                    video(136, "video/mp4; codecs=\"avc1.4d401f\"", 720, 2_000_000),
                    audio(140, "audio/mp4; codecs=\"mp4a.40.2\"", 130_000),
                    dubbed,
                    original,
                    flattened,
                    // Enciphered: no address to use.
                    {"itag": 400, "signatureCipher": "s=...", "mimeType": "video/mp4", "height": 1080, "bitrate": 9_000_000},
                ],
            },
        });
        let stream = pick(&answer).unwrap();
        assert_eq!(stream.address, "https://v/137");
        assert_eq!(stream.audio.as_deref(), Some("https://a/251"));
        assert_eq!(stream.title.as_deref(), Some("Кот играет на пианино"));
        assert!(!stream.live);
    }

    #[test]
    fn other_answers() {
        // Only pictures taller than the limit: the smallest of them.
        let answer = json!({
            "playabilityStatus": {"status": "OK"},
            "streamingData": {"adaptiveFormats": [
                video(1, "video/webm", 4320, 1), video(2, "video/webm", 1440, 1), audio(3, "audio/mp4", 1),
            ]},
        });
        assert_eq!(pick(&answer).unwrap().address, "https://v/2");

        // A broadcast: its playlist, whatever else is listed.
        let answer = json!({
            "playabilityStatus": {"status": "OK"},
            "videoDetails": {"title": "Эфир", "isLive": true},
            "streamingData": {
                "hlsManifestUrl": "https://manifest/live.m3u8",
                "adaptiveFormats": [video(137, "video/mp4", 1080, 1), audio(140, "audio/mp4", 1)],
            },
        });
        let stream = pick(&answer).unwrap();
        assert_eq!((stream.address.as_str(), stream.audio, stream.live), ("https://manifest/live.m3u8", None, true));

        // The iPhone app's answer for a video: a playlist and nothing else usable.
        let answer = json!({"playabilityStatus": {"status": "OK"}, "streamingData": {"hlsManifestUrl": "https://manifest/vod.m3u8"}});
        assert_eq!(pick(&answer).unwrap().address, "https://manifest/vod.m3u8");

        // Only the small whole file.
        let answer = json!({
            "playabilityStatus": {"status": "OK"},
            "streamingData": {"formats": [{"itag": 18, "url": "https://whole/18", "mimeType": "video/mp4", "height": 360}]},
        });
        assert_eq!(pick(&answer).unwrap(), Stream { address: "https://whole/18".into(), audio: None, title: None, live: false });

        // Refusals, in YouTube's words.
        let refused = json!({"playabilityStatus": {"status": "LOGIN_REQUIRED", "reason": "Войдите, чтобы подтвердить свой возраст."}});
        assert_eq!(pick(&refused).unwrap_err(), "Войдите, чтобы подтвердить свой возраст");
        assert_eq!(pick(&json!({"playabilityStatus": {"status": "LIVE_STREAM_OFFLINE"}})).unwrap_err(), "трансляция ещё не началась");
        assert_eq!(pick(&json!({})).unwrap_err(), "видео недоступно");
        let empty = json!({"playabilityStatus": {"status": "OK"}, "streamingData": {"adaptiveFormats": [{"signatureCipher": "s=1"}]}});
        assert!(pick(&empty).is_err());
    }

    /// `RESUMO_TEST_YOUTUBE=<address> cargo test -- --ignored resolves_a_youtube --nocapture`
    #[test]
    #[ignore = "needs the network"]
    fn resolves_a_youtube_address() {
        let address = std::env::var("RESUMO_TEST_YOUTUBE").expect("RESUMO_TEST_YOUTUBE");
        let stream = resolve(&link(&address).expect("a YouTube address")).unwrap();
        println!("{stream:#?}");
    }
}
