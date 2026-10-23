//! The text of the info overlay (Tab): what is playing, in a few lines.

use crate::hdr::{HdrInfo, Transfer};
use crate::vlc::{MediaInfo, StreamInfo};

/// Short, familiar codec names instead of libVLC's long descriptions.
fn codec_name(s: &StreamInfo) -> String {
    let name = match s.fourcc.as_str() {
        "h264" => "H.264 (AVC)",
        "hevc" => match s.profile {
            2 => "H.265 (HEVC) Main 10",
            _ => "H.265 (HEVC)",
        },
        "av01" => "AV1",
        "VP90" => "VP9",
        "VP80" => "VP8",
        "mpgv" | "mp2v" => "MPEG-2",
        "mp1v" => "MPEG-1",
        "mp4v" | "DIVX" | "XVID" => "MPEG-4 Part 2",
        "WVC1" | "VC-1" => "VC-1",
        "WMV3" => "WMV 9",
        "theo" => "Theora",
        "MJPG" => "Motion JPEG",
        "mp4a" => "AAC",
        "a52 " | "a52" => "AC-3",
        "eac3" => "E-AC-3",
        "dts" => "DTS",
        "trhd" => "Dolby TrueHD",
        "mlp" => "MLP",
        "mpga" | "mp3" => "MP3",
        "opus" => "Opus",
        "vorb" => "Vorbis",
        "flac" => "FLAC",
        "alac" => "ALAC",
        "wma2" => "WMA",
        "araw" | "s16l" | "s16b" | "s24l" | "s24b" | "s32l" | "f32l" | "lpcm" => "PCM",
        _ => "",
    };
    if !name.is_empty() {
        return name.to_owned();
    }
    // "H264 - MPEG-4 AVC (part 10)" style; keep the part before the dash.
    let desc = s.description.split(" - ").next().unwrap_or("").trim();
    if desc.is_empty() {
        s.fourcc.clone()
    } else {
        desc.to_owned()
    }
}

fn channels(n: u32) -> String {
    match n {
        0 => String::new(),
        1 => "моно".into(),
        2 => "стерео".into(),
        3 => "2.1".into(),
        6 => "5.1".into(),
        8 => "7.1".into(),
        n => format!("{n} кан."),
    }
}

fn fps(v: f32) -> String {
    let rounded = (v * 1000.0).round() / 1000.0;
    if (rounded - rounded.round()).abs() < 0.001 {
        format!("{} кадр/с", rounded.round() as i64)
    } else {
        format!("{rounded} кадр/с")
    }
}

pub fn bitrate(bps: f32) -> String {
    if bps >= 1_000_000.0 {
        format!("{:.1} Мбит/с", bps / 1_000_000.0).replace('.', ",")
    } else {
        format!("{:.0} кбит/с", bps / 1000.0)
    }
}

/// `(label, value)` rows; streams libVLC hasn't described yet are left out.
pub fn rows(
    title: &str,
    info: &MediaInfo,
    picture: (f32, f32),
    hdr: Option<HdrInfo>,
    hdr_on: bool,
) -> Vec<(&'static str, String)> {
    let mut rows = vec![("Файл", title.to_owned())];
    if let Some(v) = &info.video {
        let (w, h) = if v.width > 0 && v.height > 0 { (v.width, v.height) } else { (picture.0 as u32, picture.1 as u32) };
        let mut parts = vec![codec_name(v)];
        if w > 0 && h > 0 {
            parts.push(format!("{w}×{h}"));
        }
        if v.fps > 0.0 {
            parts.push(fps(v.fps));
        }
        rows.push(("Видео", parts.join(" · ")));
    }
    if let Some(h) = hdr {
        let kind = match h.transfer {
            Transfer::Pq => "HDR10 (PQ)",
            Transfer::Hlg => "HLG",
        };
        let mode = if hdr_on { "→ SDR" } else { "без преобразования" };
        rows.push(("HDR", format!("{kind}, {mode}")));
    }
    if let Some(a) = &info.audio {
        let mut parts = vec![codec_name(a)];
        let ch = channels(a.channels);
        if !ch.is_empty() {
            parts.push(ch);
        }
        if a.rate > 0 {
            let khz = format!("{} кГц", a.rate as f32 / 1000.0).replace('.', ",");
            parts.push(khz);
        }
        if a.bitrate > 0 {
            parts.push(bitrate(a.bitrate as f32));
        }
        rows.push(("Аудио", parts.join(" · ")));
    }
    if info.bitrate > 0.0 {
        rows.push(("Битрейт", bitrate(info.bitrate)));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(fourcc: &str) -> StreamInfo {
        StreamInfo { fourcc: fourcc.into(), ..Default::default() }
    }

    #[test]
    fn rows_read_naturally() {
        let info = MediaInfo {
            video: Some(StreamInfo { width: 1920, height: 1080, fps: 23.976, ..stream("h264") }),
            audio: Some(StreamInfo { channels: 6, rate: 48_000, bitrate: 640_000, ..stream("a52 ") }),
            bitrate: 12_400_000.0,
        };
        let rows = rows("Movie.mkv", &info, (0.0, 0.0), None, true);
        assert_eq!(rows[1], ("Видео", "H.264 (AVC) · 1920×1080 · 23.976 кадр/с".to_owned()));
        assert_eq!(rows[2], ("Аудио", "AC-3 · 5.1 · 48 кГц · 640 кбит/с".to_owned()));
        assert_eq!(rows[3], ("Битрейт", "12,4 Мбит/с".to_owned()));
    }

    #[test]
    fn unknown_codecs_use_libvlc_names() {
        let s = StreamInfo { description: "Cinepak Video - something".into(), ..stream("cvid") };
        assert_eq!(codec_name(&s), "Cinepak Video");
        assert_eq!(codec_name(&stream("cvid")), "cvid");
        assert_eq!(fps(25.0), "25 кадр/с");
        assert_eq!(channels(2), "стерео");
        assert_eq!(bitrate(202_400.0), "202 кбит/с");
    }
}
