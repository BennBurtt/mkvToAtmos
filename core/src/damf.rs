//! Reader for the Dolby Atmos Master File set written by truehdd
//! (`.atmos` + `.atmos.metadata` + `.atmos.audio`).

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bed,
    Object,
}

/// One element of the master: a bed channel or a dynamic object.
#[derive(Debug, Clone)]
pub struct Element {
    pub id: u32,
    pub kind: Kind,
    /// Bed speaker label (L, R, C, LFE, Lss, ...); empty for objects.
    pub label: String,
    /// Channel index in the audio file.
    pub channel: usize,
}

/// State of an object from `sample_pos` on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Keyframe {
    pub sample_pos: u64,
    pub pos: [f64; 3],
    /// Linear gain (0 when inactive or -inf dB).
    pub gain: f64,
    pub size: f64,
    /// Samples taken to move from the previous state to this one.
    pub ramp: u32,
}

#[derive(Debug)]
pub struct Master {
    pub elements: Vec<Element>,
    /// Keyframes per element, same order as `elements`.
    pub keyframes: Vec<Vec<Keyframe>>,
    pub sample_rate: u32,
    pub audio: CafAudio,
}

#[derive(Debug)]
pub struct CafAudio {
    pub path: PathBuf,
    pub channels: usize,
    pub bits: u32,
    pub little_endian: bool,
    pub is_float: bool,
    pub sample_rate: f64,
    pub data_offset: u64,
    pub frames: u64,
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"')))
    {
        s[1..s.len() - 1].replace("''", "'")
    } else {
        s.to_string()
    }
}

pub fn load(atmos_path: &Path) -> io::Result<Master> {
    let text = std::fs::read_to_string(atmos_path)?;
    let dir = atmos_path.parent().unwrap_or(Path::new("."));

    let mut metadata_name = None;
    let mut audio_name = None;
    let mut section = "";
    let mut pending_label: Option<String> = None;
    let mut beds: Vec<(u32, String)> = Vec::new();
    let mut objects: Vec<u32> = Vec::new();

    for raw in text.lines() {
        let line = raw.trim();
        let line = line.strip_prefix("- ").unwrap_or(line).trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "metadata" if metadata_name.is_none() => metadata_name = Some(unquote(value)),
            "audio" if audio_name.is_none() => audio_name = Some(unquote(value)),
            "bedInstances" => section = "beds",
            "objects" => section = "objects",
            "channel" if section == "beds" => pending_label = Some(unquote(value)),
            "ID" => {
                let id: u32 = value.parse().map_err(|_| bad("ID invalide dans .atmos"))?;
                if section == "beds" {
                    beds.push((id, pending_label.take().unwrap_or_default()));
                } else if section == "objects" {
                    objects.push(id);
                }
            }
            _ => {}
        }
    }

    let metadata_path = dir.join(metadata_name.ok_or_else(|| bad("pas de fichier metadata"))?);
    let audio_path = dir.join(audio_name.ok_or_else(|| bad("pas de fichier audio"))?);

    let mut elements: Vec<Element> = beds
        .into_iter()
        .map(|(id, label)| Element {
            id,
            kind: Kind::Bed,
            label,
            channel: 0,
        })
        .chain(objects.into_iter().map(|id| Element {
            id,
            kind: Kind::Object,
            label: String::new(),
            channel: 0,
        }))
        .collect();
    elements.sort_by_key(|e| e.id);
    elements.dedup_by_key(|e| e.id);
    for (i, e) in elements.iter_mut().enumerate() {
        e.channel = i;
    }

    let audio = read_caf_header(&audio_path)?;
    if audio.channels != elements.len() {
        return Err(bad(&format!(
            "le fichier audio a {} canaux mais le master décrit {} éléments",
            audio.channels,
            elements.len()
        )));
    }

    let (sample_rate, keyframes) = read_metadata(&metadata_path, &elements)?;

    Ok(Master {
        elements,
        keyframes,
        sample_rate: sample_rate.unwrap_or(audio.sample_rate as u32),
        audio,
    })
}

#[derive(Default, Clone)]
struct RawState {
    pos: Option<[f64; 3]>,
    gain_db: Option<f64>,
    active: Option<bool>,
    size: Option<f64>,
    ramp: Option<u32>,
}

fn parse_gain(v: &str) -> Option<f64> {
    let v = unquote(v);
    if v == "-inf" {
        Some(f64::NEG_INFINITY)
    } else {
        v.parse().ok()
    }
}

fn parse_vec3(v: &str) -> Option<[f64; 3]> {
    let v = unquote(v);
    let inner = v.trim().strip_prefix('[')?.strip_suffix(']')?;
    let mut it = inner.split(',').map(|x| x.trim().parse::<f64>());
    let a = it.next()?.ok()?;
    let b = it.next()?.ok()?;
    let c = it.next()?.ok()?;
    Some([a, b, c])
}

fn read_metadata(path: &Path, elements: &[Element]) -> io::Result<(Option<u32>, Vec<Vec<Keyframe>>)> {
    let max_id = elements.iter().map(|e| e.id).max().unwrap_or(0) as usize;
    let mut index_of = vec![usize::MAX; max_id + 1];
    for (i, e) in elements.iter().enumerate() {
        index_of[e.id as usize] = i;
    }

    // Current (carried-forward) state per element, and keyframes.
    let mut state: Vec<RawState> = vec![
        RawState {
            pos: Some([0.0, 1.0, 0.0]),
            gain_db: Some(0.0),
            active: Some(true),
            size: Some(0.0),
            ramp: Some(0),
        };
        elements.len()
    ];
    let mut keyframes: Vec<Vec<Keyframe>> = vec![Vec::new(); elements.len()];
    let mut sample_rate = None;

    struct Ev {
        id: Option<u32>,
        sample_pos: Option<u64>,
        st: RawState,
    }
    let mut cur: Option<Ev> = None;

    let flush = |ev: Ev, state: &mut Vec<RawState>, keyframes: &mut Vec<Vec<Keyframe>>| {
        let (Some(id), Some(sp)) = (ev.id, ev.sample_pos) else {
            return;
        };
        let Some(&idx) = index_of.get(id as usize) else {
            return;
        };
        if idx == usize::MAX {
            return;
        }
        let s = &mut state[idx];
        if ev.st.pos.is_some() {
            s.pos = ev.st.pos;
        }
        if ev.st.gain_db.is_some() {
            s.gain_db = ev.st.gain_db;
        }
        if ev.st.active.is_some() {
            s.active = ev.st.active;
        }
        if ev.st.size.is_some() {
            s.size = ev.st.size;
        }
        // A ramp only applies to the event that carries it.
        let ramp = ev.st.ramp.unwrap_or(0);
        let gain_db = s.gain_db.unwrap_or(0.0);
        let gain = if s.active == Some(false) || gain_db == f64::NEG_INFINITY {
            0.0
        } else {
            10f64.powf(gain_db / 20.0)
        };
        let kf = Keyframe {
            sample_pos: sp,
            pos: s.pos.unwrap_or([0.0, 1.0, 0.0]),
            gain,
            size: s.size.unwrap_or(0.0),
            ramp,
        };
        let list = &mut keyframes[idx];
        if let Some(last) = list.last_mut() {
            if last.sample_pos == sp {
                *last = kf;
                return;
            }
            if last.pos == kf.pos && last.gain == kf.gain && last.size == kf.size {
                return;
            }
        }
        list.push(kf);
    };

    let reader = BufReader::with_capacity(1 << 20, File::open(path)?);
    for raw in reader.lines() {
        let raw = raw?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (is_new, line) = match trimmed.strip_prefix("- ") {
            Some(rest) => (true, rest.trim()),
            None => (false, trimmed),
        };
        if is_new {
            if let Some(ev) = cur.take() {
                flush(ev, &mut state, &mut keyframes);
            }
            cur = Some(Ev {
                id: None,
                sample_pos: None,
                st: RawState::default(),
            });
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key == "sampleRate" {
            sample_rate = value.parse().ok();
            continue;
        }
        let Some(ev) = cur.as_mut() else {
            continue;
        };
        match key {
            "ID" => ev.id = value.parse().ok(),
            "samplePos" => ev.sample_pos = value.parse().ok(),
            "pos" => ev.st.pos = parse_vec3(value),
            "gain" => ev.st.gain_db = parse_gain(value),
            "active" => ev.st.active = Some(value == "true"),
            "size" => ev.st.size = value.parse().ok(),
            "rampLength" => ev.st.ramp = value.parse().ok(),
            _ => {}
        }
    }
    if let Some(ev) = cur.take() {
        flush(ev, &mut state, &mut keyframes);
    }

    // Elements that never appear still need one keyframe.
    for list in keyframes.iter_mut() {
        if list.is_empty() || list[0].sample_pos > 0 {
            let first = list.first().copied();
            list.insert(
                0,
                Keyframe {
                    sample_pos: 0,
                    pos: first.map(|k| k.pos).unwrap_or([0.0, 1.0, 0.0]),
                    gain: first.map(|k| k.gain).unwrap_or(1.0),
                    size: first.map(|k| k.size).unwrap_or(0.0),
                    ramp: 0,
                },
            );
        }
    }
    Ok((sample_rate, keyframes))
}

pub fn read_caf_header(path: &Path) -> io::Result<CafAudio> {
    let file_len = std::fs::metadata(path)?.len();
    let mut f = BufReader::new(File::open(path)?);
    let mut hdr = [0u8; 8];
    f.read_exact(&mut hdr)?;
    if &hdr[0..4] != b"caff" {
        return Err(bad("fichier audio CAF invalide"));
    }
    let mut pos = 8u64;
    let mut desc: Option<(f64, u32, u32, u32)> = None; // rate, flags, channels, bits
    loop {
        let mut ch = [0u8; 12];
        if f.read_exact(&mut ch).is_err() {
            return Err(bad("pas de données dans le CAF"));
        }
        pos += 12;
        let kind = [ch[0], ch[1], ch[2], ch[3]];
        let size = i64::from_be_bytes(ch[4..12].try_into().unwrap());
        if &kind == b"desc" {
            let mut d = vec![0u8; size as usize];
            f.read_exact(&mut d)?;
            pos += size as u64;
            let rate = f64::from_be_bytes(d[0..8].try_into().unwrap());
            let flags = u32::from_be_bytes(d[12..16].try_into().unwrap());
            let channels = u32::from_be_bytes(d[24..28].try_into().unwrap());
            let bits = u32::from_be_bytes(d[28..32].try_into().unwrap());
            if &d[8..12] != b"lpcm" {
                return Err(bad("CAF non PCM"));
            }
            desc = Some((rate, flags, channels, bits));
        } else if &kind == b"data" {
            let (rate, flags, channels, bits) = desc.ok_or_else(|| bad("CAF sans desc"))?;
            let data_start = pos + 4; // edit count
            let data_len = if size < 0 { file_len - data_start } else { size as u64 - 4 };
            let data_len = data_len.min(file_len.saturating_sub(data_start));
            let frame_bytes = channels as u64 * (bits as u64 / 8);
            return Ok(CafAudio {
                path: path.to_path_buf(),
                channels: channels as usize,
                bits,
                little_endian: flags & 2 != 0,
                is_float: flags & 1 != 0,
                sample_rate: rate,
                data_offset: data_start,
                frames: data_len / frame_bytes,
            });
        } else {
            if size < 0 {
                return Err(bad("CAF invalide"));
            }
            f.seek(SeekFrom::Current(size))?;
            pos += size as u64;
        }
    }
}

/// Streams interleaved samples as f32 in [-1, 1).
pub struct CafReader {
    f: BufReader<File>,
    channels: usize,
    bytes: usize,
    little_endian: bool,
    is_float: bool,
    remaining: u64,
    buf: Vec<u8>,
}

impl CafReader {
    pub fn open(a: &CafAudio, start_frame: u64, frames: u64) -> io::Result<Self> {
        let mut f = BufReader::with_capacity(1 << 20, File::open(&a.path)?);
        let bytes = (a.bits / 8) as usize;
        f.seek(SeekFrom::Start(a.data_offset + start_frame * (a.channels * bytes) as u64))?;
        Ok(Self {
            f,
            channels: a.channels,
            bytes,
            little_endian: a.little_endian,
            is_float: a.is_float,
            remaining: frames.min(a.frames.saturating_sub(start_frame)),
            buf: Vec::new(),
        })
    }

    /// Reads up to `max_frames` frames into `out` (interleaved). Returns frames read.
    pub fn read(&mut self, max_frames: usize, out: &mut Vec<f32>) -> io::Result<usize> {
        let n = (max_frames as u64).min(self.remaining) as usize;
        out.clear();
        if n == 0 {
            return Ok(0);
        }
        let len = n * self.channels * self.bytes;
        self.buf.resize(len, 0);
        self.f.read_exact(&mut self.buf)?;
        self.remaining -= n as u64;
        out.reserve(n * self.channels);
        let b = &self.buf;
        match (self.is_float, self.bytes, self.little_endian) {
            (false, 3, false) => {
                for c in b.chunks_exact(3) {
                    let v = ((c[0] as i32) << 24 | (c[1] as i32) << 16 | (c[2] as i32) << 8) >> 8;
                    out.push(v as f32 / 8_388_608.0);
                }
            }
            (false, 3, true) => {
                for c in b.chunks_exact(3) {
                    let v = ((c[2] as i32) << 24 | (c[1] as i32) << 16 | (c[0] as i32) << 8) >> 8;
                    out.push(v as f32 / 8_388_608.0);
                }
            }
            (false, 2, le) => {
                for c in b.chunks_exact(2) {
                    let v = if le { i16::from_le_bytes([c[0], c[1]]) } else { i16::from_be_bytes([c[0], c[1]]) };
                    out.push(v as f32 / 32768.0);
                }
            }
            (false, 4, le) => {
                for c in b.chunks_exact(4) {
                    let a = [c[0], c[1], c[2], c[3]];
                    let v = if le { i32::from_le_bytes(a) } else { i32::from_be_bytes(a) };
                    out.push((v as f64 / 2_147_483_648.0) as f32);
                }
            }
            (true, 4, le) => {
                for c in b.chunks_exact(4) {
                    let a = [c[0], c[1], c[2], c[3]];
                    out.push(if le { f32::from_le_bytes(a) } else { f32::from_be_bytes(a) });
                }
            }
            _ => return Err(bad("format d'échantillon CAF non pris en charge")),
        }
        Ok(n)
    }

    /// Raw 24-bit little-endian copy (lossless path), interleaved.
    pub fn read_raw_i32(&mut self, max_frames: usize, out: &mut Vec<i32>) -> io::Result<usize> {
        let mut tmp = Vec::new();
        if !self.is_float && self.bytes == 3 {
            let n = (max_frames as u64).min(self.remaining) as usize;
            out.clear();
            if n == 0 {
                return Ok(0);
            }
            let len = n * self.channels * 3;
            self.buf.resize(len, 0);
            self.f.read_exact(&mut self.buf)?;
            self.remaining -= n as u64;
            for c in self.buf.chunks_exact(3) {
                let v = if self.little_endian {
                    ((c[2] as i32) << 24 | (c[1] as i32) << 16 | (c[0] as i32) << 8) >> 8
                } else {
                    ((c[0] as i32) << 24 | (c[1] as i32) << 16 | (c[2] as i32) << 8) >> 8
                };
                out.push(v);
            }
            return Ok(n);
        }
        let n = self.read(max_frames, &mut tmp)?;
        out.clear();
        out.extend(tmp.iter().map(|&x| (x * 8_388_608.0).round().clamp(-8_388_608.0, 8_388_607.0) as i32));
        Ok(n)
    }
}
