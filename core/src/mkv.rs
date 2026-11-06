//! Minimal Matroska reader: lists audio tracks and chapters, and extracts the raw
//! frames of one track (for TrueHD this gives a plain .thd/.mlp elementary stream).

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

const ID_EBML: u32 = 0x1A45DFA3;
const ID_SEGMENT: u32 = 0x18538067;
const ID_INFO: u32 = 0x1549A966;
const ID_TIMECODE_SCALE: u32 = 0x2AD7B1;
const ID_DURATION: u32 = 0x4489;
const ID_TRACKS: u32 = 0x1654AE6B;
const ID_TRACK_ENTRY: u32 = 0xAE;
const ID_TRACK_NUMBER: u32 = 0xD7;
const ID_TRACK_TYPE: u32 = 0x83;
const ID_CODEC_ID: u32 = 0x86;
const ID_NAME: u32 = 0x536E;
const ID_LANGUAGE: u32 = 0x22B59C;
const ID_FLAG_DEFAULT: u32 = 0x88;
const ID_CONTENT_ENCODINGS: u32 = 0x6D80;
const ID_CONTENT_ENCODING: u32 = 0x6240;
const ID_CONTENT_COMPRESSION: u32 = 0x5034;
const ID_CONTENT_COMP_ALGO: u32 = 0x4254;
const ID_CONTENT_COMP_SETTINGS: u32 = 0x4255;
const ID_CLUSTER: u32 = 0x1F43B675;
const ID_SIMPLE_BLOCK: u32 = 0xA3;
const ID_BLOCK_GROUP: u32 = 0xA0;
const ID_BLOCK: u32 = 0xA1;
const ID_CHAPTERS: u32 = 0x1043A770;
const ID_EDITION_ENTRY: u32 = 0x45B9;
const ID_CHAPTER_ATOM: u32 = 0xB6;
const ID_CHAPTER_TIME_START: u32 = 0x91;
const ID_CHAPTER_TIME_END: u32 = 0x92;
const ID_CHAPTER_DISPLAY: u32 = 0x80;
const ID_CHAP_STRING: u32 = 0x85;
const ID_CHAPTER_FLAG_HIDDEN: u32 = 0x98;

/// IDs that live directly under Segment; used to find the end of an unknown-size Cluster.
const SEGMENT_CHILDREN: [u32; 9] = [
    0x114D9B74, ID_INFO, ID_TRACKS, ID_CLUSTER, 0x1C53BB6B, ID_CHAPTERS, 0x1254C367, 0x1941A469,
    0xEC,
];

const UNKNOWN_SIZE: u64 = u64::MAX;

#[derive(Debug, Clone)]
pub struct Track {
    pub number: u64,
    pub track_type: u64,
    pub codec_id: String,
    pub name: String,
    pub language: String,
    pub is_default: bool,
    /// Bytes to prepend to every frame (Matroska "header stripping").
    pub strip_prefix: Vec<u8>,
}

impl Track {
    pub fn is_truehd(&self) -> bool {
        self.codec_id == "A_TRUEHD" || self.codec_id == "A_MLP"
    }
}

#[derive(Debug, Clone)]
pub struct Chapter {
    pub title: String,
    pub start_ns: u64,
    pub end_ns: Option<u64>,
}

#[derive(Debug, Default)]
pub struct MkvInfo {
    pub tracks: Vec<Track>,
    pub chapters: Vec<Chapter>,
    pub duration_ns: Option<u64>,
    /// Absolute file offset of the first Cluster and the end of the segment.
    clusters_start: Option<u64>,
    segment_end: u64,
}

struct Reader<R: Read + Seek> {
    r: R,
    pos: u64,
}

impl<R: Read + Seek> Reader<R> {
    fn byte(&mut self) -> io::Result<u8> {
        let mut b = [0u8; 1];
        self.r.read_exact(&mut b)?;
        self.pos += 1;
        Ok(b[0])
    }

    fn read_id(&mut self) -> io::Result<u32> {
        let first = self.byte()?;
        let len = first.leading_zeros() as usize + 1;
        if len > 4 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad EBML id"));
        }
        let mut id = first as u32;
        for _ in 1..len {
            id = (id << 8) | self.byte()? as u32;
        }
        Ok(id)
    }

    fn read_size(&mut self) -> io::Result<u64> {
        let first = self.byte()?;
        let len = first.leading_zeros() as usize + 1;
        if len > 8 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad EBML size"));
        }
        let mask = if len == 8 { 0 } else { 0xFFu8 >> len };
        let mut value = (first & mask) as u64;
        let mut all_ones = (first & mask) == mask;
        for _ in 1..len {
            let b = self.byte()?;
            all_ones &= b == 0xFF;
            value = (value << 8) | b as u64;
        }
        Ok(if all_ones { UNKNOWN_SIZE } else { value })
    }

    fn bytes(&mut self, n: u64) -> io::Result<Vec<u8>> {
        if n > 512 * 1024 * 1024 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "element too large"));
        }
        let mut v = vec![0u8; n as usize];
        self.r.read_exact(&mut v)?;
        self.pos += n;
        Ok(v)
    }

    fn uint(&mut self, n: u64) -> io::Result<u64> {
        let b = self.bytes(n)?;
        Ok(b.iter().fold(0u64, |acc, &x| (acc << 8) | x as u64))
    }

    fn float(&mut self, n: u64) -> io::Result<f64> {
        let b = self.bytes(n)?;
        Ok(match n {
            4 => f32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64,
            8 => f64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
            _ => 0.0,
        })
    }

    fn string(&mut self, n: u64) -> io::Result<String> {
        let b = self.bytes(n)?;
        let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        Ok(String::from_utf8_lossy(&b[..end]).into_owned())
    }

    fn seek(&mut self, to: u64) -> io::Result<()> {
        self.r.seek(SeekFrom::Start(to))?;
        self.pos = to;
        Ok(())
    }
}

fn open(path: &Path) -> io::Result<Reader<BufReader<File>>> {
    let f = File::open(path)?;
    Ok(Reader {
        r: BufReader::with_capacity(1 << 20, f),
        pos: 0,
    })
}

pub fn probe(path: &Path) -> io::Result<MkvInfo> {
    let file_len = std::fs::metadata(path)?.len();
    let mut rd = open(path)?;
    let mut info = MkvInfo::default();

    // EBML header
    if rd.read_id()? != ID_EBML {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "pas un fichier Matroska/MKV"));
    }
    let sz = rd.read_size()?;
    rd.seek(rd.pos + sz)?;

    // Segment
    loop {
        let id = rd.read_id()?;
        let sz = rd.read_size()?;
        if id == ID_SEGMENT {
            info.segment_end = if sz == UNKNOWN_SIZE { file_len } else { (rd.pos + sz).min(file_len) };
            break;
        }
        if sz == UNKNOWN_SIZE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "segment introuvable"));
        }
        rd.seek(rd.pos + sz)?;
    }

    let mut timecode_scale: u64 = 1_000_000;
    let mut duration: Option<f64> = None;

    while rd.pos + 2 <= info.segment_end {
        let start = rd.pos;
        let id = match rd.read_id() {
            Ok(id) => id,
            Err(_) => break,
        };
        let sz = match rd.read_size() {
            Ok(s) => s,
            Err(_) => break,
        };
        let body = rd.pos;
        match id {
            ID_INFO => {
                let end = body + sz;
                while rd.pos < end {
                    let cid = rd.read_id()?;
                    let csz = rd.read_size()?;
                    match cid {
                        ID_TIMECODE_SCALE => timecode_scale = rd.uint(csz)?,
                        ID_DURATION => duration = Some(rd.float(csz)?),
                        _ => rd.seek(rd.pos + csz)?,
                    }
                }
            }
            ID_TRACKS => {
                let end = body + sz;
                while rd.pos < end {
                    let cid = rd.read_id()?;
                    let csz = rd.read_size()?;
                    if cid == ID_TRACK_ENTRY {
                        let end = rd.pos + csz;
                        info.tracks.push(parse_track(&mut rd, end)?);
                    } else {
                        rd.seek(rd.pos + csz)?;
                    }
                }
            }
            ID_CHAPTERS => {
                info.chapters = parse_chapters(&mut rd, body + sz)?;
            }
            ID_CLUSTER => {
                if info.clusters_start.is_none() {
                    info.clusters_start = Some(start);
                }
                if sz == UNKNOWN_SIZE {
                    // Cannot skip an unknown-size cluster cheaply: stop the header scan here.
                    // Chapters/tracks normally precede clusters.
                    break;
                }
                rd.seek(body + sz)?;
            }
            _ => {
                if sz == UNKNOWN_SIZE {
                    break;
                }
                rd.seek(body + sz)?;
            }
        }
    }

    info.duration_ns = duration.map(|d| (d * timecode_scale as f64) as u64);
    Ok(info)
}

fn parse_track<R: Read + Seek>(rd: &mut Reader<R>, end: u64) -> io::Result<Track> {
    let mut t = Track {
        number: 0,
        track_type: 0,
        codec_id: String::new(),
        name: String::new(),
        language: String::from("und"),
        is_default: true,
        strip_prefix: Vec::new(),
    };
    while rd.pos < end {
        let id = rd.read_id()?;
        let sz = rd.read_size()?;
        match id {
            ID_TRACK_NUMBER => t.number = rd.uint(sz)?,
            ID_TRACK_TYPE => t.track_type = rd.uint(sz)?,
            ID_CODEC_ID => t.codec_id = rd.string(sz)?,
            ID_NAME => t.name = rd.string(sz)?,
            ID_LANGUAGE => t.language = rd.string(sz)?,
            ID_FLAG_DEFAULT => t.is_default = rd.uint(sz)? != 0,
            ID_CONTENT_ENCODINGS => {
                let e_end = rd.pos + sz;
                while rd.pos < e_end {
                    let eid = rd.read_id()?;
                    let esz = rd.read_size()?;
                    if eid == ID_CONTENT_ENCODING {
                        let c_end = rd.pos + esz;
                        while rd.pos < c_end {
                            let cid = rd.read_id()?;
                            let csz = rd.read_size()?;
                            if cid == ID_CONTENT_COMPRESSION {
                                let mut algo = 0;
                                let mut settings = Vec::new();
                                let k_end = rd.pos + csz;
                                while rd.pos < k_end {
                                    let kid = rd.read_id()?;
                                    let ksz = rd.read_size()?;
                                    match kid {
                                        ID_CONTENT_COMP_ALGO => algo = rd.uint(ksz)?,
                                        ID_CONTENT_COMP_SETTINGS => settings = rd.bytes(ksz)?,
                                        _ => rd.seek(rd.pos + ksz)?,
                                    }
                                }
                                if algo == 3 {
                                    t.strip_prefix = settings;
                                } else {
                                    return Err(io::Error::new(
                                        io::ErrorKind::Unsupported,
                                        "piste compressée (zlib) non prise en charge",
                                    ));
                                }
                            } else {
                                rd.seek(rd.pos + csz)?;
                            }
                        }
                    } else {
                        rd.seek(rd.pos + esz)?;
                    }
                }
            }
            _ => rd.seek(rd.pos + sz)?,
        }
    }
    rd.seek(end)?;
    Ok(t)
}

fn parse_chapters<R: Read + Seek>(rd: &mut Reader<R>, end: u64) -> io::Result<Vec<Chapter>> {
    let mut out = Vec::new();
    let mut first_edition_done = false;
    while rd.pos < end {
        let id = rd.read_id()?;
        let sz = rd.read_size()?;
        if id == ID_EDITION_ENTRY && !first_edition_done {
            first_edition_done = true;
            let e_end = rd.pos + sz;
            while rd.pos < e_end {
                let aid = rd.read_id()?;
                let asz = rd.read_size()?;
                if aid == ID_CHAPTER_ATOM {
                    let a_end = rd.pos + asz;
                    let mut ch = Chapter {
                        title: String::new(),
                        start_ns: 0,
                        end_ns: None,
                    };
                    let mut hidden = false;
                    while rd.pos < a_end {
                        let cid = rd.read_id()?;
                        let csz = rd.read_size()?;
                        match cid {
                            ID_CHAPTER_TIME_START => ch.start_ns = rd.uint(csz)?,
                            ID_CHAPTER_TIME_END => ch.end_ns = Some(rd.uint(csz)?),
                            ID_CHAPTER_FLAG_HIDDEN => hidden = rd.uint(csz)? != 0,
                            ID_CHAPTER_DISPLAY => {
                                let d_end = rd.pos + csz;
                                while rd.pos < d_end {
                                    let did = rd.read_id()?;
                                    let dsz = rd.read_size()?;
                                    if did == ID_CHAP_STRING && ch.title.is_empty() {
                                        ch.title = rd.string(dsz)?;
                                    } else {
                                        rd.seek(rd.pos + dsz)?;
                                    }
                                }
                            }
                            _ => rd.seek(rd.pos + csz)?,
                        }
                    }
                    if !hidden {
                        out.push(ch);
                    }
                } else {
                    rd.seek(rd.pos + asz)?;
                }
            }
        } else {
            rd.seek(rd.pos + sz)?;
        }
    }
    rd.seek(end)?;
    out.sort_by_key(|c| c.start_ns);
    Ok(out)
}

/// Writes every frame of `track` to `out`, in file order. Returns the number of bytes written.
/// `progress` is called with (bytes read, total bytes).
pub fn extract_track(
    path: &Path,
    info: &MkvInfo,
    track: &Track,
    out: &mut dyn Write,
    progress: &mut dyn FnMut(u64, u64),
) -> io::Result<u64> {
    let Some(start) = info.clusters_start else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "aucune donnée audio dans le MKV"));
    };
    let mut rd = open(path)?;
    rd.seek(start)?;
    let end = info.segment_end;
    let mut written = 0u64;
    let mut last_report = 0u64;

    while rd.pos + 2 <= end {
        let id = match rd.read_id() {
            Ok(id) => id,
            Err(_) => break,
        };
        let sz = match rd.read_size() {
            Ok(s) => s,
            Err(_) => break,
        };
        if id == ID_CLUSTER {
            let c_end = if sz == UNKNOWN_SIZE { end } else { rd.pos + sz };
            while rd.pos + 2 <= c_end {
                let here = rd.pos;
                let cid = match rd.read_id() {
                    Ok(id) => id,
                    Err(_) => break,
                };
                if sz == UNKNOWN_SIZE && SEGMENT_CHILDREN.contains(&cid) && cid != 0xEC {
                    // Next top-level element: the unknown-size cluster has ended.
                    rd.seek(here)?;
                    break;
                }
                let csz = rd.read_size()?;
                match cid {
                    ID_SIMPLE_BLOCK => {
                        let data = rd.bytes(csz)?;
                        written += write_block(&data, track, out)?;
                    }
                    ID_BLOCK_GROUP => {
                        let g_end = rd.pos + csz;
                        while rd.pos < g_end {
                            let gid = rd.read_id()?;
                            let gsz = rd.read_size()?;
                            if gid == ID_BLOCK {
                                let data = rd.bytes(gsz)?;
                                written += write_block(&data, track, out)?;
                            } else {
                                rd.seek(rd.pos + gsz)?;
                            }
                        }
                    }
                    _ => {
                        if csz == UNKNOWN_SIZE {
                            break;
                        }
                        rd.seek(rd.pos + csz)?;
                    }
                }
                if rd.pos - last_report > 8 * 1024 * 1024 {
                    last_report = rd.pos;
                    progress(rd.pos, end);
                }
            }
        } else {
            if sz == UNKNOWN_SIZE {
                break;
            }
            rd.seek(rd.pos + sz)?;
        }
    }
    progress(end, end);
    Ok(written)
}

fn read_vint(data: &[u8], pos: &mut usize) -> io::Result<u64> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "bloc MKV invalide");
    let first = *data.get(*pos).ok_or_else(bad)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 || *pos + len > data.len() {
        return Err(bad());
    }
    let mask = if len == 8 { 0 } else { 0xFFu8 >> len };
    let mut v = (first & mask) as u64;
    for i in 1..len {
        v = (v << 8) | data[*pos + i] as u64;
    }
    *pos += len;
    Ok(v)
}

fn write_block(data: &[u8], track: &Track, out: &mut dyn Write) -> io::Result<u64> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "bloc MKV invalide");
    let mut p = 0usize;
    let num = read_vint(data, &mut p)?;
    if num != track.number {
        return Ok(0);
    }
    if p + 3 > data.len() {
        return Err(bad());
    }
    p += 2; // relative timecode
    let flags = data[p];
    p += 1;
    let lacing = (flags >> 1) & 0x03;

    let mut frames: Vec<&[u8]> = Vec::new();
    if lacing == 0 {
        frames.push(&data[p..]);
    } else {
        let count = *data.get(p).ok_or_else(bad)? as usize + 1;
        p += 1;
        let mut sizes: Vec<usize> = Vec::with_capacity(count);
        match lacing {
            1 => {
                // Xiph
                for _ in 0..count - 1 {
                    let mut s = 0usize;
                    loop {
                        let b = *data.get(p).ok_or_else(bad)?;
                        p += 1;
                        s += b as usize;
                        if b != 255 {
                            break;
                        }
                    }
                    sizes.push(s);
                }
            }
            3 => {
                // EBML
                let first = read_vint(data, &mut p)? as i64;
                sizes.push(first as usize);
                let mut prev = first;
                for _ in 1..count - 1 {
                    let start = p;
                    let raw = read_vint(data, &mut p)? as i64;
                    let len = (p - start) as u32;
                    let bias = (1i64 << (7 * len - 1)) - 1;
                    let s = prev + (raw - bias);
                    if s < 0 {
                        return Err(bad());
                    }
                    sizes.push(s as usize);
                    prev = s;
                }
            }
            _ => {
                // Fixed
                let total = data.len() - p;
                for _ in 0..count - 1 {
                    sizes.push(total / count);
                }
            }
        }
        let used: usize = sizes.iter().sum();
        if p + used > data.len() {
            return Err(bad());
        }
        sizes.push(data.len() - p - used);
        for s in sizes {
            frames.push(&data[p..p + s]);
            p += s;
        }
    }

    let mut n = 0u64;
    for f in frames {
        if !track.strip_prefix.is_empty() {
            out.write_all(&track.strip_prefix)?;
            n += track.strip_prefix.len() as u64;
        }
        out.write_all(f)?;
        n += f.len() as u64;
    }
    Ok(n)
}
