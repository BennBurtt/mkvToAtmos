//! ADM (ITU-R BS.2076) metadata in the layout Dolby Atmos ADM BWF masters use:
//! beds as DirectSpeakers on the BS.2094 common definitions, objects as
//! AP_0003xxxx / AC_0003xxxx with cartesian positions.

use crate::damf::{Element, Keyframe, Kind};
use crate::wav::ChnaEntry;
use std::fmt::Write;

/// BS.2094 common channel format for a DAMF bed label.
pub fn common_channel(label: &str) -> Option<u32> {
    Some(match label {
        "L" => 0x01,
        "R" => 0x02,
        "C" => 0x03,
        "LFE" => 0x04,
        "Lss" => 0x0a,
        "Rss" => 0x0b,
        "Lrs" => 0x1c,
        "Rrs" => 0x1d,
        "Lts" => 0x13,
        "Rts" => 0x14,
        "Lfh" => 0x22,
        "Rfh" => 0x23,
        "Lrh" => 0x1e,
        "Rrh" => 0x1f,
        "Lw" => 0x18,
        "Rw" => 0x19,
        "LFE2" => 0x21,
        _ => return None,
    })
}

/// Time in 10 µs units, the resolution of the ADM time format used here.
pub fn units(samples: u64, rate: u32) -> u64 {
    (samples as u128 * 100_000 / rate as u128 + if (samples as u128 * 100_000) % rate as u128 * 2 >= rate as u128 { 1 } else { 0 }) as u64
}

pub fn timecode(samples: u64, rate: u32) -> String {
    tc_units(units(samples, rate))
}

pub fn tc_units(mut units: u64) -> String {
    let frac = units % 100_000;
    units /= 100_000;
    let s = units % 60;
    let m = (units / 60) % 60;
    let h = units / 3600;
    format!("{h:02}:{m:02}:{s:02}.{frac:05}")
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Options that control how dense the object automation is.
pub struct BlockOptions {
    /// Minimum distance between two blocks, in samples.
    pub min_gap: u64,
    /// Position change below which an update is ignored (DAMF units, -1..1).
    pub pos_eps: f64,
    /// Relative gain change below which an update is ignored.
    pub gain_eps: f64,
}

impl Default for BlockOptions {
    fn default() -> Self {
        Self {
            min_gap: 960, // 20 ms at 48 kHz
            pos_eps: 0.002,
            gain_eps: 0.005,
        }
    }
}

/// The keyframes of one element inside [start, end), rebased to start, thinned out.
pub fn segment_keyframes(kfs: &[Keyframe], start: u64, end: u64, opt: &BlockOptions) -> Vec<Keyframe> {
    let mut out: Vec<Keyframe> = Vec::new();
    // State in force at `start`.
    let idx = kfs.partition_point(|k| k.sample_pos <= start);
    let init = if idx > 0 { kfs[idx - 1] } else { kfs[0] };
    out.push(Keyframe {
        sample_pos: 0,
        ramp: 0,
        ..init
    });

    let mut pending: Option<Keyframe> = None;
    for k in &kfs[idx..] {
        if k.sample_pos >= end {
            break;
        }
        let rel = Keyframe {
            sample_pos: k.sample_pos - start,
            ..*k
        };
        let last = *out.last().unwrap();
        let moved = (0..3).any(|i| (rel.pos[i] - last.pos[i]).abs() > opt.pos_eps)
            || (rel.gain - last.gain).abs() > opt.gain_eps * last.gain.max(rel.gain).max(1e-6)
            || (rel.gain == 0.0) != (last.gain == 0.0)
            || (rel.size - last.size).abs() > 0.01;
        if !moved {
            continue;
        }
        if rel.sample_pos - last.sample_pos >= opt.min_gap {
            let mut kf = rel;
            if pending.take().is_some() {
                // Skipped updates in between: glide over the whole gap.
                kf.ramp = kf.ramp.max((rel.sample_pos - last.sample_pos).min(u32::MAX as u64) as u32);
            }
            out.push(kf);
        } else {
            pending = Some(rel);
        }
    }
    if let Some(p) = pending {
        let last = *out.last().unwrap();
        let at = (last.sample_pos + opt.min_gap).min(end - start - 1).max(last.sample_pos + 1);
        if at < end - start {
            out.push(Keyframe {
                sample_pos: at,
                ramp: (at - last.sample_pos) as u32,
                ..p
            });
        }
    }
    out
}

pub struct AdmDoc {
    pub axml: String,
    pub chna: Vec<ChnaEntry>,
    /// For each track of the file: the source channel it carries, or None for silence.
    pub channel_map: Vec<Option<usize>>,
}

/// The 7.1.2 bed Dolby Atmos masters carry, in Dolby order.
pub const BED_712: [&str; 10] = ["L", "R", "C", "LFE", "Lss", "Rss", "Lrs", "Rrs", "Lts", "Rts"];

/// Position of a speaker label that is not part of the 7.1.2 bed, used to turn it into a
/// static object (DAMF/ADM cartesian coordinates).
pub fn label_position(label: &str) -> [f64; 3] {
    match label {
        "Lfh" => [-1.0, 1.0, 1.0],
        "Rfh" => [1.0, 1.0, 1.0],
        "Lrh" => [-1.0, -1.0, 1.0],
        "Rrh" => [1.0, -1.0, 1.0],
        "Lw" => [-1.0, 0.5, 0.0],
        "Rw" => [1.0, 0.5, 0.0],
        "Lc" => [-0.5, 1.0, 0.0],
        "Rc" => [0.5, 1.0, 0.0],
        "Cs" => [0.0, -1.0, 0.0],
        "Tc" => [0.0, 0.0, 1.0],
        "Tfc" => [0.0, 1.0, 1.0],
        "Tbc" => [0.0, -1.0, 1.0],
        _ => [0.0, 1.0, 0.0],
    }
}

/// ADM for a full Atmos master segment: beds + objects.
pub fn build_master(
    title: &str,
    elements: &[Element],
    keyframes: &[Vec<Keyframe>],
    frames: u64,
    rate: u32,
    opt: &BlockOptions,
) -> AdmDoc {
    let end_tc = timecode(frames, rate);
    let mut chna = Vec::new();
    let mut programme_refs = String::new();
    let mut contents = String::new();
    let mut objects = String::new();
    let mut defs = String::new();
    let mut uids = String::new();

    // Beds: each bed instance is written as a full 7.1.2 (the Dolby Renderer accepts no
    // other layout, and an LFE-only bed is what TrueHD usually carries). Missing bed channels
    // are written as silence.
    let mut content_n = 0x1001u32;
    let mut object_n = 0x1001u32;
    let mut uid_n = 1u32;
    let mut channel_map: Vec<Option<usize>> = Vec::new();
    let n_beds = elements
        .iter()
        .filter(|e| e.kind == Kind::Bed && BED_712.contains(&e.label.as_str()))
        .map(|e| e.bed_instance + 1)
        .max()
        .unwrap_or(1);
    let pack = "AP_00011001";
    let _ = writeln!(defs, r#"<audioPackFormat audioPackFormatID="{pack}" audioPackFormatName="AtmosCustomPackFormat1" typeLabel="0001" typeDefinition="DirectSpeakers">"#);
    for l in BED_712 {
        let _ = writeln!(defs, "<audioChannelFormatIDRef>AC_000100{:02x}</audioChannelFormatIDRef>", common_channel(l).unwrap());
    }
    defs.push_str("</audioPackFormat>\n");
    for bi in 0..n_beds {
        let name = format!("Bed {}", bi + 1);
        let aco = format!("ACO_{content_n:04x}");
        let ao = format!("AO_{object_n:04x}");
        content_n += 1;
        object_n += 1;
        let _ = writeln!(programme_refs, "<audioContentIDRef>{aco}</audioContentIDRef>");
        let _ = writeln!(contents, r#"<audioContent audioContentID="{aco}" audioContentName="{name}"><audioObjectIDRef>{ao}</audioObjectIDRef></audioContent>"#);
        let _ = writeln!(objects, r#"<audioObject audioObjectID="{ao}" audioObjectName="{name}" start="00:00:00.00000" duration="{end_tc}">"#);
        let _ = writeln!(objects, "<audioPackFormatIDRef>{pack}</audioPackFormatIDRef>");
        for l in BED_712 {
            let uid = format!("ATU_{uid_n:08x}");
            let at = format!("AT_000100{:02x}_01", common_channel(l).unwrap());
            let _ = writeln!(objects, "<audioTrackUIDRef>{uid}</audioTrackUIDRef>");
            let _ = writeln!(uids, r#"<audioTrackUID UID="{uid}" sampleRate="{rate}" bitDepth="24"><audioTrackFormatIDRef>{at}</audioTrackFormatIDRef><audioPackFormatIDRef>{pack}</audioPackFormatIDRef></audioTrackUID>"#);
            let src = elements
                .iter()
                .find(|e| e.kind == Kind::Bed && e.bed_instance == bi && e.label == l)
                .map(|e| e.channel);
            channel_map.push(src);
            chna.push(ChnaEntry {
                track_index: channel_map.len() as u16,
                uid,
                track_ref: at,
                pack_ref: pack.to_string(),
            });
            uid_n += 1;
        }
        objects.push_str("</audioObject>\n");
    }

    // Objects (and bed channels with an unknown label, rendered as static objects).
    let mut obj_index = 0u32;
    for (e, kfs) in elements.iter().zip(keyframes) {
        if e.kind == Kind::Bed && BED_712.contains(&e.label.as_str()) {
            continue;
        }
        obj_index += 1;
        let n = 0x1000 + obj_index;
        let ap = format!("AP_0003{n:04x}");
        let ac = format!("AC_0003{n:04x}");
        let at = format!("AT_0003{n:04x}_01");
        let as_ = format!("AS_0003{n:04x}");
        let aco = format!("ACO_{content_n:04x}");
        let ao = format!("AO_{object_n:04x}");
        let uid = format!("ATU_{uid_n:08x}");
        content_n += 1;
        object_n += 1;
        uid_n += 1;
        let name = format!("Atmos_Obj_{obj_index}");

        let _ = writeln!(programme_refs, "<audioContentIDRef>{aco}</audioContentIDRef>");
        let _ = writeln!(contents, r#"<audioContent audioContentID="{aco}" audioContentName="{name}"><audioObjectIDRef>{ao}</audioObjectIDRef></audioContent>"#);
        let _ = writeln!(objects, r#"<audioObject audioObjectID="{ao}" audioObjectName="{name}" start="00:00:00.00000" duration="{end_tc}"><audioPackFormatIDRef>{ap}</audioPackFormatIDRef><audioTrackUIDRef>{uid}</audioTrackUIDRef></audioObject>"#);
        let _ = writeln!(defs, r#"<audioPackFormat audioPackFormatID="{ap}" audioPackFormatName="AtmosCustomPackFormat{n}" typeLabel="0003" typeDefinition="Objects"><audioChannelFormatIDRef>{ac}</audioChannelFormatIDRef></audioPackFormat>"#);
        let _ = writeln!(defs, r#"<audioChannelFormat audioChannelFormatID="{ac}" audioChannelFormatName="{name}" typeLabel="0003" typeDefinition="Objects">"#);

        let seg = if e.kind == Kind::Bed {
            vec![Keyframe { sample_pos: 0, pos: label_position(&e.label), gain: 1.0, size: 0.0, ramp: 0 }]
        } else {
            segment_keyframes(kfs, 0, frames, opt)
        };
        for (i, k) in seg.iter().enumerate() {
            let next = seg.get(i + 1).map(|x| x.sample_pos).unwrap_or(frames);
            let dur = next.saturating_sub(k.sample_pos).max(1);
            let interp = (k.ramp as u64).min(dur);
            // Durations are taken from the rounded start times so blocks tile exactly.
            let (u0, u1) = (units(k.sample_pos, rate), units(next, rate));
            if u1 <= u0 {
                continue;
            }
            let size = k.size.clamp(0.0, 1.0);
            let _ = write!(
                defs,
                r#"<audioBlockFormat audioBlockFormatID="AB_0003{n:04x}_{bi:08x}" rtime="{rt}" duration="{du}"><cartesian>1</cartesian><position coordinate="X">{x:.5}</position><position coordinate="Y">{y:.5}</position><position coordinate="Z">{z:.5}</position>"#,
                bi = i + 1,
                rt = tc_units(u0),
                du = tc_units(u1 - u0),
                x = k.pos[0].clamp(-1.0, 1.0),
                y = k.pos[1].clamp(-1.0, 1.0),
                z = k.pos[2].clamp(-1.0, 1.0),
            );
            if size > 0.0 {
                let _ = write!(defs, "<width>{size:.4}</width><depth>{size:.4}</depth><height>{size:.4}</height>");
            }
            let _ = writeln!(
                defs,
                r#"<gain>{g:.6}</gain><jumpPosition interpolationLength="{il}">1</jumpPosition></audioBlockFormat>"#,
                g = k.gain,
                il = {
                    let iu = units(interp, rate).min(u1 - u0);
                    format!("{}.{:05}", iu / 100_000, iu % 100_000)
                },
            );
        }
        defs.push_str("</audioChannelFormat>\n");
        let _ = writeln!(defs, r#"<audioStreamFormat audioStreamFormatID="{as_}" audioStreamFormatName="PCM_{name}" formatLabel="0001" formatDefinition="PCM"><audioChannelFormatIDRef>{ac}</audioChannelFormatIDRef><audioTrackFormatIDRef>{at}</audioTrackFormatIDRef></audioStreamFormat>"#);
        let _ = writeln!(defs, r#"<audioTrackFormat audioTrackFormatID="{at}" audioTrackFormatName="PCM_{name}" formatLabel="0001" formatDefinition="PCM"><audioStreamFormatIDRef>{as_}</audioStreamFormatIDRef></audioTrackFormat>"#);
        let _ = writeln!(uids, r#"<audioTrackUID UID="{uid}" sampleRate="{rate}" bitDepth="24"><audioTrackFormatIDRef>{at}</audioTrackFormatIDRef><audioPackFormatIDRef>{ap}</audioPackFormatIDRef></audioTrackUID>"#);
        channel_map.push(Some(e.channel));
        chna.push(ChnaEntry {
            track_index: channel_map.len() as u16,
            uid,
            track_ref: at,
            pack_ref: ap,
        });
    }

    let axml = wrap(title, &end_tc, &programme_refs, &contents, &objects, &defs, &uids);
    AdmDoc { axml, chna, channel_map }
}

/// ADM for a 7.1.4 channel-based file (BS.2094 pack AP_00010017, Dolby channel order).
pub fn build_714(title: &str, frames: u64, rate: u32) -> AdmDoc {
    let end_tc = timecode(frames, rate);
    let chans = [0x01, 0x02, 0x03, 0x04, 0x0a, 0x0b, 0x1c, 0x1d, 0x22, 0x23, 0x1e, 0x1f];
    let pack = "AP_00010017";
    let mut objects = String::new();
    let mut uids = String::new();
    let mut chna = Vec::new();
    let _ = writeln!(objects, r#"<audioObject audioObjectID="AO_1001" audioObjectName="7.1.4" start="00:00:00.00000" duration="{end_tc}"><audioPackFormatIDRef>{pack}</audioPackFormatIDRef>"#);
    for (i, c) in chans.iter().enumerate() {
        let uid = format!("ATU_{:08x}", i + 1);
        let at = format!("AT_000100{c:02x}_01");
        let _ = writeln!(objects, "<audioTrackUIDRef>{uid}</audioTrackUIDRef>");
        let _ = writeln!(uids, r#"<audioTrackUID UID="{uid}" sampleRate="{rate}" bitDepth="24"><audioTrackFormatIDRef>{at}</audioTrackFormatIDRef><audioPackFormatIDRef>{pack}</audioPackFormatIDRef></audioTrackUID>"#);
        chna.push(ChnaEntry { track_index: (i + 1) as u16, uid, track_ref: at, pack_ref: pack.into() });
    }
    objects.push_str("</audioObject>\n");
    let refs = "<audioContentIDRef>ACO_1001</audioContentIDRef>\n";
    let contents = r#"<audioContent audioContentID="ACO_1001" audioContentName="7.1.4"><audioObjectIDRef>AO_1001</audioObjectIDRef></audioContent>"#;
    AdmDoc {
        axml: wrap(title, &end_tc, refs, contents, &objects, "", &uids),
        chna,
        channel_map: (0..12).map(Some).collect(),
    }
}

fn wrap(title: &str, end_tc: &str, refs: &str, contents: &str, objects: &str, defs: &str, uids: &str) -> String {
    let mut x = String::with_capacity(defs.len() + 4096);
    x.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    x.push('\n');
    x.push_str(r#"<ebuCoreMain xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns="urn:ebu:metadata-schema:ebuCore_2014" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" schema="EBU_CORE_20140201.xsd" xml:lang="en">"#);
    x.push_str("\n<coreMetadata><format><audioFormatExtended version=\"ITU-R_BS.2076-2\">\n");
    let _ = writeln!(x, r#"<audioProgramme audioProgrammeID="APR_1001" audioProgrammeName="{}" start="00:00:00.00000" end="{end_tc}">"#, esc(title));
    x.push_str(refs);
    x.push_str("</audioProgramme>\n");
    x.push_str(contents);
    x.push('\n');
    x.push_str(objects);
    x.push_str(defs);
    x.push_str(uids);
    x.push_str("</audioFormatExtended></format></coreMetadata></ebuCoreMain>\n");
    x
}
