//! Cuts a Dolby Atmos Master File set (.atmos + .atmos.metadata + .atmos.audio) into
//! several masters, one per time range (one per song).
//!
//! The metadata events are copied as they are (every field kept), rebased to the start of
//! each range. Each new master starts with one event per element holding its full state at
//! that point, so it plays correctly on its own.

use crate::damf::read_caf_header;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub struct Part {
    /// File name stem of the new master (without extension).
    pub name: String,
    pub start: u64,
    pub end: u64,
}

type Fields = Vec<(String, String)>;

fn set(fields: &mut Fields, key: &str, value: &str) {
    if let Some(f) = fields.iter_mut().find(|(k, _)| k == key) {
        f.1 = value.to_string();
    } else {
        fields.push((key.to_string(), value.to_string()));
    }
}

fn write_event(w: &mut impl Write, id: u32, sample_pos: u64, fields: &Fields) -> io::Result<()> {
    writeln!(w, "  - ID: {id}")?;
    writeln!(w, "    samplePos: {sample_pos}")?;
    for (k, v) in fields {
        writeln!(w, "    {k}: {v}")?;
    }
    Ok(())
}

struct Out {
    part_index: usize,
    w: BufWriter<File>,
    started: bool,
}

/// Writes one master per part into `out_dir`. Returns the paths of the new `.atmos` files.
pub fn split(atmos: &Path, parts: &[Part], out_dir: &Path, progress: &mut dyn FnMut(f64)) -> io::Result<Vec<PathBuf>> {
    let dir = atmos.parent().unwrap_or(Path::new("."));
    let text = fs::read_to_string(atmos)?;
    let field = |key: &str| -> Option<String> {
        text.lines()
            .map(|l| l.trim())
            .find_map(|l| l.strip_prefix(key).map(|v| v.trim().trim_matches('\'').trim_matches('"').to_string()))
    };
    let meta_path = dir.join(field("metadata:").ok_or_else(|| io::Error::other("pas de metadata dans .atmos"))?);
    let audio_path = dir.join(field("audio:").ok_or_else(|| io::Error::other("pas d'audio dans .atmos"))?);
    fs::create_dir_all(out_dir)?;

    // 1. .atmos files
    let mut outputs = Vec::new();
    for p in parts {
        let mut t = String::new();
        for line in text.lines() {
            let trimmed = line.trim_start();
            let indent = &line[..line.len() - trimmed.len()];
            if trimmed.starts_with("metadata:") {
                t.push_str(&format!("{indent}metadata: {}.atmos.metadata\n", p.name));
            } else if trimmed.starts_with("audio:") {
                t.push_str(&format!("{indent}audio: {}.atmos.audio\n", p.name));
            } else if trimmed.starts_with("offset:") {
                t.push_str(&format!("{indent}offset: 0.0\n"));
            } else {
                t.push_str(line);
                t.push('\n');
            }
        }
        let path = out_dir.join(format!("{}.atmos", p.name));
        fs::write(&path, t)?;
        outputs.push(path);
    }

    // 2. Audio: same CAF header, data cut to the range.
    let h = read_caf_header(&audio_path)?;
    let bpf = (h.channels * (h.bits as usize / 8)) as u64;
    let header_len = h.data_offset - 4 - 12; // up to the "data" chunk header
    let mut header = vec![0u8; header_len as usize];
    File::open(&audio_path)?.read_exact(&mut header)?;
    let total_bytes: u64 = parts.iter().map(|p| (p.end - p.start) * bpf).sum::<u64>().max(1);
    let mut copied = 0u64;
    for p in parts {
        let frames = (p.end.min(h.frames)).saturating_sub(p.start);
        let mut w = BufWriter::with_capacity(1 << 20, File::create(out_dir.join(format!("{}.atmos.audio", p.name)))?);
        w.write_all(&header)?;
        w.write_all(b"data")?;
        w.write_all(&((frames * bpf + 4) as i64).to_be_bytes())?;
        w.write_all(&0u32.to_be_bytes())?;
        let mut r = BufReader::with_capacity(1 << 20, File::open(&audio_path)?);
        r.seek(SeekFrom::Start(h.data_offset + p.start * bpf))?;
        let mut left = frames * bpf;
        let mut buf = vec![0u8; 4 << 20];
        while left > 0 {
            let n = (left as usize).min(buf.len());
            r.read_exact(&mut buf[..n])?;
            w.write_all(&buf[..n])?;
            left -= n as u64;
            copied += n as u64;
            progress(0.9 * copied as f64 / total_bytes as f64);
        }
        w.flush()?;
    }

    // 3. Metadata: one pass over the events, feeding every part.
    let mut outs: Vec<Out> = parts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            File::create(out_dir.join(format!("{}.atmos.metadata", p.name))).map(|f| Out {
                part_index: i,
                w: BufWriter::new(f),
                started: false,
            })
        })
        .collect::<io::Result<_>>()?;
    let mut sample_rate = String::from("48000");
    let mut state: BTreeMap<u32, Fields> = BTreeMap::new();
    let mut last_id: Option<u32> = None;
    let mut last_sp: u64 = 0;

    let mut handle = |id: Option<u32>, sp: Option<u64>, fields: Fields, state: &mut BTreeMap<u32, Fields>, outs: &mut Vec<Out>, sample_rate: &str| -> io::Result<()> {
        let id = match id.or(last_id) {
            Some(i) => i,
            None => return Ok(()),
        };
        let sp = sp.unwrap_or(last_sp);
        last_id = Some(id);
        last_sp = sp;
        // Parts whose start has been passed get their opening snapshot first.
        for o in outs.iter_mut() {
            let p = &parts[o.part_index];
            if !o.started && sp > p.start {
                start_part(o, state, sample_rate)?;
            }
        }
        let st = state.entry(id).or_default();
        for (k, v) in &fields {
            set(st, k, v);
        }
        for o in outs.iter_mut() {
            let p = &parts[o.part_index];
            if o.started && sp > p.start && sp < p.end {
                write_event(&mut o.w, id, sp - p.start, &fields)?;
            }
        }
        Ok(())
    };

    let reader = BufReader::with_capacity(1 << 20, File::open(&meta_path)?);
    let mut cur: Option<(Option<u32>, Option<u64>, Fields)> = None;
    for line in reader.lines() {
        let line = line?;
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let (new_event, t) = match t.strip_prefix("- ") {
            Some(r) => (true, r.trim()),
            None => (false, t),
        };
        let Some((k, v)) = t.split_once(':') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if k == "sampleRate" {
            sample_rate = v.to_string();
            continue;
        }
        if k == "events" {
            continue;
        }
        if new_event {
            if let Some((id, sp, f)) = cur.take() {
                handle(id, sp, f, &mut state, &mut outs, &sample_rate)?;
            }
            cur = Some((None, None, Vec::new()));
        }
        let Some(c) = cur.as_mut() else { continue };
        match k {
            "ID" => c.0 = v.parse().ok(),
            "samplePos" => c.1 = v.parse().ok(),
            _ => c.2.push((k.to_string(), v.to_string())),
        }
    }
    if let Some((id, sp, f)) = cur.take() {
        handle(id, sp, f, &mut state, &mut outs, &sample_rate)?;
    }
    for o in outs.iter_mut() {
        if !o.started {
            start_part(o, &state, &sample_rate)?;
        }
        o.w.flush()?;
    }
    progress(1.0);
    Ok(outputs)
}

fn start_part(o: &mut Out, state: &BTreeMap<u32, Fields>, sample_rate: &str) -> io::Result<()> {
    writeln!(o.w, "sampleRate: {sample_rate}")?;
    writeln!(o.w, "events:")?;
    for (id, fields) in state {
        let mut f = fields.clone();
        set(&mut f, "rampLength", "0");
        write_event(&mut o.w, *id, 0, &f)?;
    }
    o.started = true;
    Ok(())
}
