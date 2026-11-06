//! 24-bit PCM writer for RIFF/WAVE and BW64 (EBU Tech 3285 / ITU-R BS.2088),
//! with optional `chna` and `axml` chunks for ADM.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

pub struct ChnaEntry {
    /// 1-based track index in the file.
    pub track_index: u16,
    pub uid: String,
    pub track_ref: String,
    pub pack_ref: String,
}

pub struct WavWriter {
    w: BufWriter<File>,
    channels: usize,
    expected_frames: u64,
    written_frames: u64,
    buf: Vec<u8>,
}

fn fixed(s: &str, n: usize) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.resize(n, 0);
    v
}

fn chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
}

impl WavWriter {
    /// Creates the file and writes every header chunk. `frames` must be the exact number of
    /// frames that will be written.
    pub fn create(
        path: &Path,
        sample_rate: u32,
        channels: usize,
        frames: u64,
        chna: &[ChnaEntry],
        axml: Option<&str>,
    ) -> io::Result<Self> {
        let block_align = channels * 3;
        let data_size = frames * block_align as u64;

        let mut chunks = Vec::new();
        // fmt (plain PCM, as Dolby ADM masters use)
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&1u16.to_le_bytes());
        fmt.extend_from_slice(&(channels as u16).to_le_bytes());
        fmt.extend_from_slice(&sample_rate.to_le_bytes());
        fmt.extend_from_slice(&(sample_rate * block_align as u32).to_le_bytes());
        fmt.extend_from_slice(&(block_align as u16).to_le_bytes());
        fmt.extend_from_slice(&24u16.to_le_bytes());
        chunk(&mut chunks, b"fmt ", &fmt);

        if !chna.is_empty() {
            let mut c = Vec::new();
            let uids = chna.len() as u16;
            c.extend_from_slice(&(channels as u16).to_le_bytes());
            c.extend_from_slice(&uids.to_le_bytes());
            for e in chna {
                c.extend_from_slice(&e.track_index.to_le_bytes());
                c.extend(fixed(&e.uid, 12));
                c.extend(fixed(&e.track_ref, 14));
                c.extend(fixed(&e.pack_ref, 11));
                c.push(0);
            }
            chunk(&mut chunks, b"chna", &c);
        }
        if let Some(x) = axml {
            chunk(&mut chunks, b"axml", x.as_bytes());
        }

        // 4 ("WAVE") + 36 (ds64/JUNK) + chunks + 8 (data header) + data
        let riff_size = 4 + 36 + chunks.len() as u64 + 8 + data_size + (data_size % 2);
        let big = riff_size > u32::MAX as u64 - 1024 || std::env::var_os("ATMOSCONV_FORCE_BW64").is_some();

        let mut head = Vec::new();
        if big {
            head.extend_from_slice(b"BW64");
            head.extend_from_slice(&u32::MAX.to_le_bytes());
            head.extend_from_slice(b"WAVE");
            let mut ds = Vec::new();
            ds.extend_from_slice(&riff_size.to_le_bytes());
            ds.extend_from_slice(&data_size.to_le_bytes());
            ds.extend_from_slice(&frames.to_le_bytes());
            ds.extend_from_slice(&0u32.to_le_bytes());
            chunk(&mut head, b"ds64", &ds);
        } else {
            head.extend_from_slice(b"RIFF");
            head.extend_from_slice(&(riff_size as u32).to_le_bytes());
            head.extend_from_slice(b"WAVE");
            chunk(&mut head, b"JUNK", &[0u8; 28]);
        }
        head.extend_from_slice(&chunks);
        head.extend_from_slice(b"data");
        head.extend_from_slice(&(if big { u32::MAX } else { data_size as u32 }).to_le_bytes());

        let mut w = BufWriter::with_capacity(1 << 20, File::create(path)?);
        w.write_all(&head)?;
        Ok(Self {
            w,
            channels,
            expected_frames: frames,
            written_frames: 0,
            buf: Vec::new(),
        })
    }

    /// Interleaved 24-bit samples held in i32.
    pub fn write_i32(&mut self, samples: &[i32]) -> io::Result<()> {
        self.buf.clear();
        self.buf.reserve(samples.len() * 3);
        for &s in samples {
            let b = s.to_le_bytes();
            self.buf.extend_from_slice(&b[0..3]);
        }
        self.w.write_all(&self.buf)?;
        self.written_frames += (samples.len() / self.channels) as u64;
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        if self.written_frames != self.expected_frames {
            // Pad with silence so the header stays truthful.
            let missing = self.expected_frames.saturating_sub(self.written_frames);
            let zeros = vec![0i32; self.channels * 4096];
            let mut left = missing;
            while left > 0 {
                let n = left.min(4096) as usize;
                self.write_i32(&zeros[..n * self.channels])?;
                left -= n as u64;
            }
        }
        let data_size = self.expected_frames * (self.channels * 3) as u64;
        if data_size % 2 == 1 {
            self.w.write_all(&[0])?;
        }
        self.w.flush()
    }
}

/// Float -> 24-bit with clipping. Returns the number of clipped samples.
pub fn to_i24(src: &[f32], dst: &mut Vec<i32>) -> u64 {
    dst.clear();
    dst.reserve(src.len());
    let mut clipped = 0;
    for &x in src {
        let v = (x as f64 * 8_388_608.0).round();
        if !(-8_388_608.0..=8_388_607.0).contains(&v) {
            clipped += 1;
        }
        dst.push(v.clamp(-8_388_608.0, 8_388_607.0) as i32);
    }
    clipped
}
