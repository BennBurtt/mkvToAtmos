//! atmosconv: MKV (Dolby TrueHD + Atmos) -> ADM BWF master and 7.1.4 WAV.
//!
//! Usage:
//!   atmosconv convert <file.mkv> [--out DIR] [--truehdd PATH] [--chapters]
//!                     [--no-adm] [--no-714] [--keep-damf] [--json]
//!   atmosconv tracks <file.mkv>
//!   atmosconv damf <file.atmos> [--out DIR] [--no-adm] [--no-714]   (skip the MKV step)

mod adm;
mod damf;
mod mkv;
mod render;
mod wav;

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

struct Opts {
    input: PathBuf,
    out: Option<PathBuf>,
    truehdd: Option<PathBuf>,
    chapters: bool,
    adm: bool,
    r714: bool,
    keep_damf: bool,
    json: bool,
}

struct Report {
    json: bool,
}

impl Report {
    fn progress(&self, phase: &str, label: &str, value: f64) {
        if self.json {
            println!(
                "{{\"type\":\"progress\",\"phase\":{},\"label\":{},\"value\":{:.4}}}",
                js(phase),
                js(label),
                value.clamp(0.0, 1.0)
            );
        } else {
            eprint!("\r{label} {:5.1}%   ", value * 100.0);
            if value >= 1.0 {
                eprintln!();
            }
        }
        let _ = io::stdout().flush();
    }

    fn info(&self, msg: &str) {
        if self.json {
            println!("{{\"type\":\"info\",\"message\":{}}}", js(msg));
        } else {
            eprintln!("{msg}");
        }
        let _ = io::stdout().flush();
    }

    fn file(&self, path: &Path) {
        if self.json {
            println!("{{\"type\":\"file\",\"path\":{}}}", js(&path.to_string_lossy()));
        } else {
            eprintln!("-> {}", path.display());
        }
        let _ = io::stdout().flush();
    }
}

fn js(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

fn parse_args(args: &[String]) -> io::Result<Opts> {
    let mut o = Opts {
        input: PathBuf::new(),
        out: None,
        truehdd: None,
        chapters: false,
        adm: true,
        r714: true,
        keep_damf: false,
        json: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => o.out = Some(it.next().ok_or_else(|| err("--out sans dossier"))?.into()),
            "--truehdd" => o.truehdd = Some(it.next().ok_or_else(|| err("--truehdd sans chemin"))?.into()),
            "--chapters" => o.chapters = true,
            "--no-adm" => o.adm = false,
            "--no-714" => o.r714 = false,
            "--keep-damf" => o.keep_damf = true,
            "--json" => o.json = true,
            s if s.starts_with("--") => return Err(err(format!("option inconnue : {s}"))),
            s => o.input = s.into(),
        }
    }
    if o.input.as_os_str().is_empty() {
        return Err(err("aucun fichier en entrée"));
    }
    Ok(o)
}

fn sanitize(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| if matches!(c, '/' | ':' | '\\' | '\0') { '-' } else { c })
        .collect();
    t.trim().trim_matches('.').to_string()
}

fn find_truehdd(given: &Option<PathBuf>) -> io::Result<PathBuf> {
    if let Some(p) = given {
        return Ok(p.clone());
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("truehdd");
            if p.exists() {
                return Ok(p);
            }
        }
    }
    Ok(PathBuf::from("truehdd"))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let rep = Report { json };
    let result = match args.first().map(String::as_str) {
        Some("convert") => parse_args(&args[1..]).and_then(|o| convert(&o, &rep)),
        Some("damf") => parse_args(&args[1..]).and_then(|o| {
            let out = o.out.clone().unwrap_or_else(|| o.input.parent().unwrap_or(Path::new(".")).to_path_buf());
            let base = o.input.file_name().unwrap().to_string_lossy().trim_end_matches(".atmos").to_string();
            rep.info("Lecture du master Atmos");
            from_master(damf::load(&o.input)?, &out, &base, &[], &o, &rep)
        }),
        Some("tracks") => parse_args(&args[1..]).and_then(|o| list_tracks(&o.input)),
        Some("extract") => parse_args(&args[1..]).and_then(|o| {
            let info = mkv::probe(&o.input)?;
            let t = info.tracks.iter().find(|t| t.is_truehd()).ok_or_else(|| err("pas de piste TrueHD"))?;
            let out = o.out.clone().ok_or_else(|| err("--out requis"))?;
            let mut w = BufWriter::new(File::create(&out)?);
            mkv::extract_track(&o.input, &info, t, &mut w, &mut |_, _| {})?;
            w.flush()
        }),
        _ => {
            eprintln!("usage: atmosconv convert <fichier.mkv> [--out DOSSIER] [--chapters] [--no-adm] [--no-714] [--keep-damf] [--json]");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => {
            if json {
                println!("{{\"type\":\"done\"}}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            if json {
                println!("{{\"type\":\"error\",\"message\":{}}}", js(&e.to_string()));
            } else {
                eprintln!("Erreur : {e}");
            }
            ExitCode::from(1)
        }
    }
}

fn list_tracks(path: &Path) -> io::Result<()> {
    let info = mkv::probe(path)?;
    for t in &info.tracks {
        println!("piste {} : {} {} [{}]{}", t.number, t.codec_id, t.name, t.language, if t.is_truehd() { "  <- TrueHD" } else { "" });
    }
    for c in &info.chapters {
        println!("chapitre {:>8.2}s  {}", c.start_ns as f64 / 1e9, c.title);
    }
    Ok(())
}

fn convert(o: &Opts, rep: &Report) -> io::Result<()> {
    let info = mkv::probe(&o.input)?;
    let tracks: Vec<&mkv::Track> = info.tracks.iter().filter(|t| t.is_truehd()).collect();
    if tracks.is_empty() {
        return Err(err("ce MKV ne contient aucune piste Dolby TrueHD (donc pas d'Atmos lossless)"));
    }
    let stem = sanitize(&o.input.file_stem().unwrap_or_default().to_string_lossy());
    let out_dir = o
        .out
        .clone()
        .unwrap_or_else(|| o.input.parent().unwrap_or(Path::new(".")).join(format!("{stem} - Atmos")));
    fs::create_dir_all(&out_dir)?;
    let work = out_dir.join(".travail");
    fs::create_dir_all(&work)?;
    let truehdd = find_truehdd(&o.truehdd)?;

    rep.info(&format!("{} piste(s) TrueHD trouvée(s)", tracks.len()));
    let chapters: Vec<mkv::Chapter> = if o.chapters { info.chapters.clone() } else { Vec::new() };
    if o.chapters && chapters.is_empty() {
        rep.info("pas de chapitres dans ce MKV : le fichier sera converti en entier");
    }

    for (ti, t) in tracks.iter().enumerate() {
        let label = if tracks.len() == 1 {
            stem.clone()
        } else if !t.name.trim().is_empty() {
            format!("{stem} - {}", sanitize(&t.name))
        } else {
            format!("{stem} - piste {}", ti + 1)
        };
        let n = tracks.len();
        let tag = if n > 1 { format!(" ({}/{})", ti + 1, n) } else { String::new() };

        // 1. Extract
        let thd = work.join(format!("piste{}.thd", t.number));
        {
            let mut w = BufWriter::with_capacity(1 << 20, File::create(&thd)?);
            let bytes = mkv::extract_track(&o.input, &info, t, &mut w, &mut |a, b| {
                rep.progress("extract", &format!("Extraction TrueHD{tag}"), a as f64 / b.max(1) as f64)
            })?;
            w.flush()?;
            if bytes == 0 {
                return Err(err("la piste TrueHD est vide"));
            }
        }

        // 2. Decode with truehdd
        let dec_dir = work.join(format!("piste{}", t.number));
        let _ = fs::remove_dir_all(&dec_dir);
        fs::create_dir_all(&dec_dir)?;
        rep.progress("decode", &format!("Décodage Atmos{tag}"), 0.0);
        let status = Command::new(&truehdd)
            .arg("decode")
            .arg(&thd)
            .arg("--output-path")
            .arg(dec_dir.join("master"))
            .arg("--format")
            .arg("w64")
            .arg("--no-estimate-progress")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| err(format!("impossible de lancer truehdd ({}) : {e}", truehdd.display())))?;
        if !status.status.success() {
            let msg = String::from_utf8_lossy(&status.stderr);
            let last: Vec<&str> = msg.lines().rev().take(3).collect();
            return Err(err(format!("truehdd a échoué : {}", last.into_iter().rev().collect::<Vec<_>>().join(" / "))));
        }
        rep.progress("decode", &format!("Décodage Atmos{tag}"), 1.0);
        let _ = fs::remove_file(&thd);

        let mut atmos: Vec<PathBuf> = fs::read_dir(&dec_dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "atmos"))
            .collect();
        atmos.sort();

        if atmos.is_empty() {
            // No object metadata: channel-based Atmos (CBI) or plain 5.1/7.1 TrueHD.
            let mut files: Vec<PathBuf> = fs::read_dir(&dec_dir)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "caf" || x == "wav" || x == "audio"))
                .collect();
            files.sort();
            if files.is_empty() {
                return Err(err("truehdd n'a produit aucun fichier audio"));
            }
            for (si, f) in files.iter().enumerate() {
                let base = if files.len() > 1 { format!("{label} partie {}", si + 1) } else { label.clone() };
                let is_caf = std::fs::File::open(f)
                    .and_then(|mut h| {
                        let mut b = [0u8; 4];
                        std::io::Read::read_exact(&mut h, &mut b).map(|_| b)
                    })
                    .map(|b| &b == b"caff")
                    .unwrap_or(false);
                if is_caf {
                    let m = damf::channel_based(f)?;
                    let names: Vec<&str> = m.elements.iter().map(|e| e.label.as_str()).collect();
                    rep.info(&format!("piste sans objets ({} canaux : {}), rendu en 7.1.4", names.len(), names.join(" ")));
                    let ch = if files.len() == 1 { chapters.as_slice() } else { &[] };
                    from_master(m, &out_dir, &base, ch, o, rep)?;
                } else {
                    rep.info("cette piste TrueHD ne contient pas d'Atmos : export en multicanal classique");
                    let dst = out_dir.join(format!("{base} (TrueHD sans Atmos).w64.wav"));
                    fs::rename(f, &dst).or_else(|_| fs::copy(f, &dst).map(|_| ()))?;
                    rep.file(&dst);
                }
            }
            let _ = fs::remove_dir_all(&dec_dir);
            continue;
        }

        for (si, a) in atmos.iter().enumerate() {
            let base = if atmos.len() > 1 { format!("{label} partie {}", si + 1) } else { label.clone() };
            // Chapters only make sense on an unspliced stream.
            let ch = if atmos.len() == 1 { chapters.as_slice() } else { &[] };
            rep.info("Lecture du master Atmos");
            from_master(damf::load(a)?, &out_dir, &base, ch, o, rep)?;
            if o.keep_damf {
                let ddir = out_dir.join(format!("{base} (DAMF)"));
                fs::create_dir_all(&ddir)?;
                let stemname = a.file_name().unwrap().to_string_lossy().to_string();
                for ext in ["", ".metadata", ".audio"] {
                    let src = PathBuf::from(format!("{}{ext}", a.display()));
                    let dst = ddir.join(format!("{stemname}{ext}"));
                    fs::rename(&src, &dst).or_else(|_| fs::copy(&src, &dst).map(|_| ()))?;
                }
                rep.file(&ddir);
            }
        }
        let _ = fs::remove_dir_all(&dec_dir);
    }
    let _ = fs::remove_dir_all(&work);
    Ok(())
}

struct Range {
    title: String,
    start: u64,
    end: u64,
}

fn from_master(m: damf::Master, out_dir: &Path, base: &str, chapters: &[mkv::Chapter], o: &Opts, rep: &Report) -> io::Result<()> {
    let rate = m.sample_rate;
    let total = m.audio.frames;
    let nb_obj = m.elements.iter().filter(|e| e.kind == damf::Kind::Object).count();
    let beds: Vec<&str> = m.elements.iter().filter(|e| e.kind == damf::Kind::Bed).map(|e| e.label.as_str()).collect();
    rep.info(&format!(
        "{} objets, bed : {}, {:.1} min à {} Hz",
        nb_obj,
        if beds.is_empty() { "aucun".to_string() } else { beds.join(" ") },
        total as f64 / rate as f64 / 60.0,
        rate
    ));
    fs::create_dir_all(out_dir)?;

    let mut ranges = Vec::new();
    if chapters.is_empty() {
        ranges.push(Range { title: base.to_string(), start: 0, end: total });
    } else {
        for (i, c) in chapters.iter().enumerate() {
            let s = (c.start_ns as u128 * rate as u128 / 1_000_000_000) as u64;
            let e = chapters
                .get(i + 1)
                .map(|n| (n.start_ns as u128 * rate as u128 / 1_000_000_000) as u64)
                .or(c.end_ns.map(|e| (e as u128 * rate as u128 / 1_000_000_000) as u64))
                .unwrap_or(total)
                .min(total);
            if e <= s + rate as u64 / 10 {
                continue;
            }
            let name = if c.title.trim().is_empty() { format!("Chapitre {}", i + 1) } else { sanitize(&c.title) };
            ranges.push(Range { title: format!("{base} - {:02} {name}", i + 1), start: s, end: e });
        }
        if ranges.is_empty() {
            ranges.push(Range { title: base.to_string(), start: 0, end: total });
        }
    }

    let nr = ranges.len();
    for (ri, r) in ranges.iter().enumerate() {
        let frames = r.end - r.start;
        // Keep the ADM XML to a size every reader copes with: about 600k blocks at most.
        let mut opt = adm::BlockOptions::default();
        opt.min_gap = opt.min_gap.max(frames * nb_obj.max(1) as u64 / 600_000);
        let tag = if nr > 1 { format!(" {}/{}", ri + 1, nr) } else { String::new() };

        if o.adm {
            let kfs: Vec<Vec<damf::Keyframe>> = m
                .keyframes
                .iter()
                .map(|k| adm::segment_keyframes(k, r.start, r.end, &adm::BlockOptions { min_gap: 0, pos_eps: -1.0, gain_eps: -1.0 }))
                .collect();
            let doc = adm::build_master(&r.title, &m.elements, &kfs, frames, rate, &opt);
            let path = out_dir.join(format!("{} - ADM.wav", r.title));
            let nout = doc.channel_map.len();
            let nin = m.elements.len();
            let mut w = wav::WavWriter::create(&path, rate, nout, frames, &doc.chna, Some(&doc.axml))?;
            let mut rd = damf::CafReader::open(&m.audio, r.start, frames)?;
            let (mut buf, mut ob) = (Vec::new(), Vec::new());
            let mut done = 0u64;
            loop {
                let n = rd.read_raw_i32(48000, &mut buf)?;
                if n == 0 {
                    break;
                }
                ob.clear();
                ob.reserve(n * nout);
                for f in 0..n {
                    for src in &doc.channel_map {
                        ob.push(src.map(|c| buf[f * nin + c]).unwrap_or(0));
                    }
                }
                w.write_i32(&ob)?;
                done += n as u64;
                if done % (48000 * 20) < 48000 {
                    rep.progress("adm", &format!("Écriture ADM{tag}"), done as f64 / frames as f64);
                }
            }
            w.finish()?;
            rep.progress("adm", &format!("Écriture ADM{tag}"), 1.0);
            rep.file(&path);
        }

        if o.r714 {
            let path = out_dir.join(format!("{} - 7.1.4.wav", r.title));
            let doc = adm::build_714(&r.title, frames, rate);
            let mut w = wav::WavWriter::create(&path, rate, render::CHANNELS, frames, &doc.chna, Some(&doc.axml))?;
            let mut rd = damf::CafReader::open(&m.audio, r.start, frames)?;
            let mut ren = render::Renderer::new(&m.elements, &m.keyframes);
            let (mut inb, mut outb, mut ib) = (Vec::new(), Vec::new(), Vec::new());
            let mut done = 0u64;
            let mut clipped = 0u64;
            let mut peak = 0f32;
            loop {
                let n = rd.read(48000, &mut inb)?;
                if n == 0 {
                    break;
                }
                ren.process(&inb, n, r.start + done, &mut outb);
                for &x in &outb {
                    peak = peak.max(x.abs());
                }
                clipped += wav::to_i24(&outb, &mut ib);
                w.write_i32(&ib)?;
                done += n as u64;
                if done % (48000 * 10) < 48000 {
                    rep.progress("render", &format!("Rendu 7.1.4{tag}"), done as f64 / frames as f64);
                }
            }
            w.finish()?;
            rep.progress("render", &format!("Rendu 7.1.4{tag}"), 1.0);
            if clipped > 0 {
                rep.info(&format!(
                    "attention : {clipped} échantillons saturés dans le rendu 7.1.4 (crête {:+.1} dBFS)",
                    20.0 * (peak as f64).log10()
                ));
            }
            rep.file(&path);
        }
    }
    Ok(())
}
