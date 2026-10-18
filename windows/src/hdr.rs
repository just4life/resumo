//! Finds out whether a video file is HDR (PQ / HLG) by reading its container
//! metadata, since libVLC 3 does not tell us. Understands Matroska/WebM
//! (`Colour` element), MP4/MOV (`colr`, `vpcC`, `mdcv`, `clli` boxes) and, when
//! the container says nothing, the colour description in the HEVC SPS.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Transfer {
    /// SMPTE ST 2084 (HDR10, HDR10+, Dolby Vision with an HDR10 base layer).
    Pq,
    /// ARIB STD-B67 (broadcast HDR, phones).
    Hlg,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct HdrInfo {
    pub transfer: Transfer,
    /// Brightest pixel the content was graded for, in cd/m².
    pub peak_nits: f32,
}

/// Peak assumed when the file does not say (typical HDR10 grading).
pub const DEFAULT_PEAK: f32 = 1000.0;

// H.273 transfer_characteristics codes.
const TC_PQ: u64 = 16;
const TC_HLG: u64 = 18;

/// What the metadata of one video track says.
#[derive(Default, Debug)]
struct Found {
    transfer: Option<u64>,
    max_cll: Option<f32>,
    mastering_max: Option<f32>,
}

impl Found {
    fn result(&self) -> Option<HdrInfo> {
        let transfer = match self.transfer? {
            TC_PQ => Transfer::Pq,
            TC_HLG => Transfer::Hlg,
            _ => return None,
        };
        let peak = match transfer {
            Transfer::Hlg => DEFAULT_PEAK,
            Transfer::Pq => self
                .max_cll
                .filter(|&v| v > 0.0)
                .or(self.mastering_max.filter(|&v| v > 0.0))
                .unwrap_or(DEFAULT_PEAK)
                .clamp(400.0, 10_000.0),
        };
        Some(HdrInfo { transfer, peak_nits: peak })
    }
}

/// Reads the metadata of a local file. `None` means SDR or unknown.
pub fn probe(path: &Path) -> Option<HdrInfo> {
    let mut f = File::open(path).ok()?;
    let mut head = [0u8; 12];
    f.read_exact(&mut head).ok()?;
    f.seek(SeekFrom::Start(0)).ok()?;
    let len = f.metadata().ok()?.len();
    let found = if head[..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        mkv::probe(&mut f, len)
    } else if &head[4..8] == b"ftyp" || &head[4..8] == b"moov" || &head[4..8] == b"mdat" || &head[4..8] == b"wide" {
        mp4::probe(&mut f, len)
    } else {
        None
    }?;
    found.result()
}

fn read_vec(f: &mut (impl Read + Seek), at: u64, len: usize) -> Option<Vec<u8>> {
    f.seek(SeekFrom::Start(at)).ok()?;
    let mut buf = vec![0u8; len];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn be(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0, |acc, &b| acc << 8 | b as u64)
}

mod mkv {
    use super::*;

    const SEGMENT: u64 = 0x1853_8067;
    const TRACKS: u64 = 0x1654_AE6B;
    const CLUSTER: u64 = 0x1F43_B675;
    const TRACK_ENTRY: u64 = 0xAE;
    const TRACK_TYPE: u64 = 0x83;
    const CODEC_ID: u64 = 0x86;
    const CODEC_PRIVATE: u64 = 0x63A2;
    const VIDEO: u64 = 0xE0;
    const COLOUR: u64 = 0x55B0;
    const TRANSFER: u64 = 0x55BA;
    const MAX_CLL: u64 = 0x55BC;
    const MASTERING: u64 = 0x55D0;
    const LUMINANCE_MAX: u64 = 0x55D9;

    /// Reads an EBML variable-length number. IDs keep their length marker.
    fn vint(f: &mut impl Read, keep_marker: bool) -> Option<(u64, bool)> {
        let mut first = [0u8; 1];
        f.read_exact(&mut first).ok()?;
        let len = first[0].leading_zeros() as usize + 1;
        if len > 8 {
            return None;
        }
        let mut value = if keep_marker { first[0] as u64 } else { (first[0] as u64) & (0xFF >> len) };
        let mut all_ones = value == (0xFF >> len) as u64;
        let mut rest = [0u8; 7];
        f.read_exact(&mut rest[..len - 1]).ok()?;
        for &b in &rest[..len - 1] {
            value = value << 8 | b as u64;
            all_ones &= b == 0xFF;
        }
        Some((value, !keep_marker && all_ones))
    }

    /// (id, payload start, payload size or None when unknown)
    fn header(f: &mut (impl Read + Seek)) -> Option<(u64, u64, Option<u64>)> {
        let (id, _) = vint(f, true)?;
        let (size, unknown) = vint(f, false)?;
        let start = f.stream_position().ok()?;
        Some((id, start, (!unknown).then_some(size)))
    }

    pub fn probe(f: &mut (impl Read + Seek), len: u64) -> Option<Found> {
        // EBML header, then the Segment.
        let (_, start, size) = header(f)?;
        f.seek(SeekFrom::Start(start + size?)).ok()?;
        let (id, seg_start, seg_size) = header(f)?;
        if id != SEGMENT {
            return None;
        }
        let seg_end = seg_size.map_or(len, |s| (seg_start + s).min(len));
        let mut pos = seg_start;
        while pos < seg_end {
            f.seek(SeekFrom::Start(pos)).ok()?;
            let (id, start, size) = header(f)?;
            match id {
                TRACKS => {
                    let size = size.unwrap_or(seg_end - start).min(4 << 20);
                    let data = read_vec(f, start, size as usize)?;
                    return tracks(&data);
                }
                // Tracks always come before the first cluster in practice.
                CLUSTER => return None,
                _ => pos = start + size?,
            }
        }
        None
    }

    /// Children of an in-memory element: (id, payload).
    fn children(mut data: &[u8]) -> Vec<(u64, &[u8])> {
        let mut out = Vec::new();
        while !data.is_empty() {
            let mut cur = std::io::Cursor::new(data);
            let Some((id, _)) = vint(&mut cur, true) else { break };
            let Some((size, _)) = vint(&mut cur, false) else { break };
            let start = cur.position() as usize;
            let end = start.saturating_add(size as usize).min(data.len());
            out.push((id, &data[start..end]));
            data = &data[end..];
        }
        out
    }

    fn float(data: &[u8]) -> Option<f32> {
        match data.len() {
            4 => Some(f32::from_bits(be(data) as u32)),
            8 => Some(f64::from_bits(be(data)) as f32),
            _ => None,
        }
    }

    pub(super) fn tracks(data: &[u8]) -> Option<Found> {
        for (id, entry) in children(data) {
            if id != TRACK_ENTRY {
                continue;
            }
            let fields = children(entry);
            if !fields.iter().any(|&(id, v)| id == TRACK_TYPE && be(v) == 1) {
                continue;
            }
            let mut found = Found::default();
            for &(id, v) in &fields {
                if id != VIDEO {
                    continue;
                }
                for (id, colour) in children(v) {
                    if id != COLOUR {
                        continue;
                    }
                    for (id, v) in children(colour) {
                        match id {
                            TRANSFER => found.transfer = Some(be(v)),
                            MAX_CLL => found.max_cll = Some(be(v) as f32),
                            MASTERING => {
                                for (id, v) in children(v) {
                                    if id == LUMINANCE_MAX {
                                        found.mastering_max = float(v);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            if found.transfer.is_none() {
                let codec = fields.iter().find(|&&(id, _)| id == CODEC_ID).map(|&(_, v)| v);
                let private = fields.iter().find(|&&(id, _)| id == CODEC_PRIVATE).map(|&(_, v)| v);
                if let (Some(b"V_MPEGH/ISO/HEVC"), Some(p)) = (codec, private) {
                    found.transfer = super::hevc::transfer_from_hvcc(p);
                }
            }
            return Some(found);
        }
        None
    }
}

mod mp4 {
    use super::*;

    const CONTAINERS: [&[u8; 4]; 6] = [b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd"];
    const MAX_MOOV: u64 = 64 << 20;

    pub fn probe(f: &mut (impl Read + Seek), len: u64) -> Option<Found> {
        let mut pos = 0;
        while pos + 8 <= len {
            let h = read_vec(f, pos, 16.min((len - pos) as usize))?;
            let (size, header) = match be(&h[..4]) {
                0 => (len - pos, 8),
                1 if h.len() >= 16 => (be(&h[8..16]), 16),
                s => (s, 8),
            };
            if size < header {
                return None;
            }
            if &h[4..8] == b"moov" {
                let data = read_vec(f, pos + header, (size - header).min(MAX_MOOV) as usize)?;
                return Some(walk(&data, &mut Found::default()).unwrap_or_default());
            }
            pos += size;
        }
        None
    }

    /// Boxes of an in-memory buffer: (type, payload).
    fn boxes(mut data: &[u8]) -> Vec<([u8; 4], &[u8])> {
        let mut out = Vec::new();
        while data.len() >= 8 {
            let (size, header) = match be(&data[..4]) {
                0 => (data.len() as u64, 8),
                1 if data.len() >= 16 => (be(&data[8..16]), 16),
                s => (s, 8),
            };
            if size < header || size > data.len() as u64 {
                break;
            }
            let kind = [data[4], data[5], data[6], data[7]];
            out.push((kind, &data[header as usize..size as usize]));
            data = &data[size as usize..];
        }
        out
    }

    /// Walks moov looking for the first video sample entry that says anything.
    fn walk(data: &[u8], found: &mut Found) -> Option<Found> {
        for (kind, payload) in boxes(data) {
            if kind == *b"stsd" {
                // FullBox header + entry count, then sample entries.
                let entries = payload.get(8..)?;
                for (_, entry) in boxes(entries) {
                    // VisualSampleEntry has 78 bytes of fixed fields.
                    if let Some(f) = entry.get(78..).and_then(sample_entry) {
                        return Some(f);
                    }
                }
            } else if CONTAINERS.contains(&&kind) {
                if let Some(f) = walk(payload, found) {
                    return Some(f);
                }
            }
        }
        None
    }

    pub(super) fn sample_entry(children: &[u8]) -> Option<Found> {
        let mut found = Found::default();
        let mut hvcc = None;
        for (kind, v) in boxes(children) {
            match &kind {
                b"colr" if v.len() >= 10 && (&v[..4] == b"nclx" || &v[..4] == b"nclc") => {
                    found.transfer = Some(be(&v[6..8]));
                }
                // VP9 codec configuration carries the colour description itself.
                b"vpcC" if v.len() >= 10 && found.transfer.is_none() => found.transfer = Some(v[8] as u64),
                b"clli" if v.len() >= 2 => found.max_cll = Some(be(&v[..2]) as f32),
                b"mdcv" if v.len() >= 24 => found.mastering_max = Some(be(&v[16..20]) as f32 / 10_000.0),
                b"hvcC" => hvcc = Some(v),
                _ => {}
            }
        }
        if found.transfer.is_none() {
            found.transfer = hvcc.and_then(super::hevc::transfer_from_hvcc);
        }
        found.transfer.is_some().then_some(found)
    }
}

mod hevc {
    /// Bit reader over an RBSP (emulation prevention bytes already removed).
    struct Bits<'a> {
        data: &'a [u8],
        pos: usize,
    }

    impl Bits<'_> {
        fn bit(&mut self) -> Option<u32> {
            let byte = *self.data.get(self.pos / 8)?;
            let b = (byte >> (7 - self.pos % 8)) & 1;
            self.pos += 1;
            Some(b as u32)
        }
        fn bits(&mut self, n: u32) -> Option<u32> {
            (0..n).try_fold(0, |acc, _| Some(acc << 1 | self.bit()?))
        }
        fn skip(&mut self, n: usize) -> Option<()> {
            self.pos += n;
            (self.pos <= self.data.len() * 8).then_some(())
        }
        fn flag(&mut self) -> Option<bool> {
            Some(self.bit()? == 1)
        }
        fn ue(&mut self) -> Option<u32> {
            let mut zeros = 0;
            while self.bit()? == 0 {
                zeros += 1;
                if zeros > 31 {
                    return None;
                }
            }
            Some((1u32 << zeros) - 1 + self.bits(zeros)?)
        }
        fn se(&mut self) -> Option<i32> {
            let v = self.ue()? as i64;
            Some(if v % 2 == 1 { (v + 1) / 2 } else { -(v / 2) } as i32)
        }
    }

    fn rbsp(nal: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(nal.len());
        let mut zeros = 0;
        for &b in nal {
            if zeros >= 2 && b == 3 {
                zeros = 0;
                continue;
            }
            zeros = if b == 0 { zeros + 1 } else { 0 };
            out.push(b);
        }
        out
    }

    /// transfer_characteristics from the first SPS in an HEVCDecoderConfigurationRecord.
    pub fn transfer_from_hvcc(hvcc: &[u8]) -> Option<u64> {
        let arrays = *hvcc.get(22)? as usize;
        let mut p = 23;
        for _ in 0..arrays {
            let kind = hvcc.get(p)? & 0x3F;
            let count = super::be(hvcc.get(p + 1..p + 3)?) as usize;
            p += 3;
            for _ in 0..count {
                let len = super::be(hvcc.get(p..p + 2)?) as usize;
                let nal = hvcc.get(p + 2..p + 2 + len)?;
                p += 2 + len;
                if kind == 33 {
                    return sps_transfer(nal.get(2..)?);
                }
            }
        }
        None
    }

    fn sps_transfer(nal_payload: &[u8]) -> Option<u64> {
        let data = rbsp(nal_payload);
        let r = &mut Bits { data: &data, pos: 0 };
        r.skip(4)?; // sps_video_parameter_set_id
        let max_sub_layers = r.bits(3)? as usize;
        r.skip(1)?;
        // profile_tier_level
        r.skip(2 + 1 + 5 + 32 + 4 + 43 + 1 + 8)?;
        let mut sub = Vec::new();
        for _ in 0..max_sub_layers {
            sub.push((r.flag()?, r.flag()?));
        }
        if max_sub_layers > 0 {
            r.skip(2 * (8 - max_sub_layers))?;
        }
        for (profile, level) in sub {
            if profile {
                r.skip(88)?;
            }
            if level {
                r.skip(8)?;
            }
        }
        r.ue()?; // sps_seq_parameter_set_id
        if r.ue()? == 3 {
            r.skip(1)?;
        }
        r.ue()?;
        r.ue()?; // picture size
        if r.flag()? {
            for _ in 0..4 {
                r.ue()?;
            }
        }
        r.ue()?;
        r.ue()?; // bit depths
        let poc_lsb_bits = r.ue()? + 4;
        let all_layers = r.flag()?;
        for _ in if all_layers { 0 } else { max_sub_layers }..=max_sub_layers {
            r.ue()?;
            r.ue()?;
            r.ue()?;
        }
        for _ in 0..6 {
            r.ue()?; // block sizes and transform hierarchy depths
        }
        if r.flag()? && r.flag()? {
            scaling_list_data(r)?;
        }
        r.skip(2)?; // amp, sao
        if r.flag()? {
            r.skip(8)?;
            r.ue()?;
            r.ue()?;
            r.skip(1)?;
        }
        let sets = r.ue()? as usize;
        let mut delta_pocs: Vec<usize> = Vec::with_capacity(sets);
        for i in 0..sets {
            let inter = i != 0 && r.flag()?;
            if inter {
                r.skip(1)?;
                r.ue()?;
                let mut n = 0;
                for _ in 0..=delta_pocs[i - 1] {
                    let used = r.flag()?;
                    if used || r.flag()? {
                        n += 1;
                    }
                }
                delta_pocs.push(n);
            } else {
                let neg = r.ue()? as usize;
                let pos = r.ue()? as usize;
                for _ in 0..neg + pos {
                    r.ue()?;
                    r.skip(1)?;
                }
                delta_pocs.push(neg + pos);
            }
        }
        if r.flag()? {
            for _ in 0..r.ue()? {
                r.skip(poc_lsb_bits as usize + 1)?;
            }
        }
        r.skip(2)?; // temporal mvp, strong intra smoothing
        if !r.flag()? {
            return None; // no VUI
        }
        if r.flag()? && r.bits(8)? == 255 {
            r.skip(32)?;
        }
        if r.flag()? {
            r.skip(1)?;
        }
        if !r.flag()? {
            return None; // no video_signal_type
        }
        r.skip(4)?;
        if !r.flag()? {
            return None; // no colour description
        }
        r.skip(8)?;
        Some(r.bits(8)? as u64)
    }

    fn scaling_list_data(r: &mut Bits) -> Option<()> {
        for size_id in 0..4 {
            let mut matrix = 0;
            while matrix < 6 {
                if !r.flag()? {
                    r.ue()?;
                } else {
                    let coefs = 64.min(1 << (4 + (size_id << 1)));
                    if size_id > 1 {
                        r.se()?;
                    }
                    for _ in 0..coefs {
                        r.se()?;
                    }
                }
                matrix += if size_id == 3 { 3 } else { 1 };
            }
        }
        Some(())
    }

    #[cfg(test)]
    pub(super) fn sps_transfer_for_test(nal_payload: &[u8]) -> Option<u64> {
        sps_transfer(nal_payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// hvcC from an x265 HDR10 encode, cut down to its SPS.
    const HVCC: &str = "0102200000009000000000005df000fcfdfafa00000f01a10001002d42010102200000030090000003000003005da00280802d136595952930b9a84880482000000300200000030301";

    #[test]
    fn hevc_sps_colour_description() {
        assert_eq!(hevc::transfer_from_hvcc(&hex(HVCC)), Some(TC_PQ));
        // SPS without VUI: nothing to report.
        assert_eq!(hevc::sps_transfer_for_test(&[0x01, 0x01]), None);
    }

    fn ebml(id: u64, payload: &[u8]) -> Vec<u8> {
        let mut out: Vec<u8> = id.to_be_bytes().iter().copied().skip_while(|&b| b == 0).collect();
        out.extend_from_slice(&(payload.len() as u64 | 0x01 << 56).to_be_bytes()); // 8-byte size
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn matroska_colour_element() {
        let mastering = ebml(0x55D9, &4000f32.to_bits().to_be_bytes());
        let colour = [ebml(0x55BA, &[16]), ebml(0x55BC, &[0x03, 0xE8]), ebml(0x55D0, &mastering)].concat();
        let video = ebml(0xE0, &ebml(0x55B0, &colour));
        let audio = ebml(0xAE, &ebml(0x83, &[2]));
        let entry = ebml(0xAE, &[ebml(0x83, &[1]), video].concat());
        let found = mkv::tracks(&[audio, entry].concat()).unwrap();
        assert_eq!(found.result(), Some(HdrInfo { transfer: Transfer::Pq, peak_nits: 1000.0 }));

        // No Colour element: fall back to the HEVC SPS in CodecPrivate.
        let fields = [ebml(0x83, &[1]), ebml(0x86, b"V_MPEGH/ISO/HEVC"), ebml(0x63A2, &hex(HVCC))].concat();
        let found = mkv::tracks(&ebml(0xAE, &fields)).unwrap();
        assert_eq!(found.result(), Some(HdrInfo { transfer: Transfer::Pq, peak_nits: DEFAULT_PEAK }));

        // SDR (BT.709 transfer).
        let sdr = ebml(0xAE, &[ebml(0x83, &[1]), ebml(0xE0, &ebml(0x55B0, &ebml(0x55BA, &[1])))].concat());
        assert_eq!(mkv::tracks(&sdr).unwrap().result(), None);
    }

    fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        [&((payload.len() + 8) as u32).to_be_bytes()[..], kind, payload].concat()
    }

    #[test]
    fn mp4_colr_and_light_levels() {
        let colr = mp4_box(b"colr", &[b"nclx".as_slice(), &[0, 9, 0, 18, 0, 9, 0]].concat());
        assert_eq!(mp4::sample_entry(&colr).unwrap().result().map(|i| i.transfer), Some(Transfer::Hlg));

        let colr = mp4_box(b"colr", &[b"nclx".as_slice(), &[0, 9, 0, 16, 0, 9, 0]].concat());
        let mut mdcv = vec![0u8; 24];
        mdcv[16..20].copy_from_slice(&(4000u32 * 10_000).to_be_bytes());
        let found = mp4::sample_entry(&[colr.clone(), mp4_box(b"mdcv", &mdcv)].concat()).unwrap();
        assert_eq!(found.result(), Some(HdrInfo { transfer: Transfer::Pq, peak_nits: 4000.0 }));
        let clli = mp4_box(b"clli", &[0x02, 0x58, 0, 0]);
        let found = mp4::sample_entry(&[colr, mp4_box(b"mdcv", &mdcv), clli].concat()).unwrap();
        assert_eq!(found.result().unwrap().peak_nits, 600.0);
    }
}
