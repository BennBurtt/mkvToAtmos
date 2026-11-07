//! ADM (ITU-R BS.2076) metadata in the layout Dolby Atmos ADM BWF masters use:
//! beds as DirectSpeakers on the BS.2094 common definitions, objects as
//! AP_0003xxxx / AC_0003xxxx with cartesian positions.

use crate::damf::{Element, Keyframe, Kind};
use crate::wav::ChnaEntry;
use std::fmt::Write;

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
    /// `dbmd` chunk (Dolby metadata), for Dolby Atmos masters.
    pub dbmd: Option<Vec<u8>>,
}

/// The 7.1.2 bed Dolby Atmos masters carry, in Dolby order.
pub const BED_712: [&str; 10] = ["L", "R", "C", "LFE", "Lss", "Rss", "Lrs", "Rrs", "Lts", "Rts"];

/// Dolby "room centric" bed channels: name, speaker label, cartesian position.
const RC: [(&str, &str, [f64; 3]); 10] = [
    ("RoomCentricLeft", "RC_L", [-1.0, 1.0, 0.0]),
    ("RoomCentricRight", "RC_R", [1.0, 1.0, 0.0]),
    ("RoomCentricCenter", "RC_C", [0.0, 1.0, 0.0]),
    ("RoomCentricLFE", "RC_LFE", [-1.0, 1.0, -1.0]),
    ("RoomCentricLeftSideSurround", "RC_Lss", [-1.0, 0.0, 0.0]),
    ("RoomCentricRightSideSurround", "RC_Rss", [1.0, 0.0, 0.0]),
    ("RoomCentricLeftRearSurround", "RC_Lrs", [-1.0, -1.0, 0.0]),
    ("RoomCentricRightRearSurround", "RC_Rrs", [1.0, -1.0, 0.0]),
    ("RoomCentricLeftTopSurround", "RC_Lts", [-1.0, 0.0, 1.0]),
    ("RoomCentricRightTopSurround", "RC_Rts", [1.0, 0.0, 1.0]),
];

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

fn num(v: f64) -> String {
    format!("{:.10}", if v.abs() < 5e-11 { 0.0 } else { v })
}

/// Builds the `dbmd` chunk the way the Dolby Atmos Renderer writes it: fixed default
/// segments 0x07 and 0x09, then segment 0x0a with one byte per track.
pub fn build_dbmd(lfe: &[bool]) -> Vec<u8> {
    use crate::dbmd_template::{SEGMENTS_07_09, SEGMENT_0A_HEADER};
    let mut out = vec![0x06, 0x00, 0x00, 0x01];
    out.extend_from_slice(&SEGMENTS_07_09);
    let mut payload = SEGMENT_0A_HEADER.to_vec();
    payload[4] = lfe.len().min(255) as u8;
    for &l in lfe {
        payload.push(if l { 0x80 } else { 0x84 });
    }
    let size = payload.len() as u16;
    let sz = size.to_le_bytes();
    let sum = sz.iter().chain(payload.iter()).fold(0u8, |a, &b| a.wrapping_add(b));
    out.push(0x0a);
    out.extend_from_slice(&sz);
    out.extend_from_slice(&payload);
    out.push(sum.wrapping_neg());
    out.push(0x00); // end of segments
    if out.len() % 2 == 1 {
        out.push(0x00);
    }
    out
}

/// ADM for a full Atmos master segment, in the Dolby Atmos Master ADM profile layout
/// (as written by the Dolby Atmos Renderer): 7.1.2 beds as room-centric DirectSpeakers,
/// then objects, all in one audioContent, plus a `dbmd` chunk.
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
    let mut channel_map: Vec<Option<usize>> = Vec::new();
    let mut lfe_flags: Vec<bool> = Vec::new();
    let mut object_refs = String::new();
    let mut objects = String::new();
    let mut packs = String::new();
    let mut channels = String::new();
    let mut streams = String::new();
    let mut tracks = String::new();
    let mut uids = String::new();
    let mut object_n = 0x1001u32;
    let mut uid_n = 1u32;

    let n_beds = elements
        .iter()
        .filter(|e| e.kind == Kind::Bed && BED_712.contains(&e.label.as_str()))
        .map(|e| e.bed_instance + 1)
        .max()
        .unwrap_or(1);

    // Beds: each bed instance is a full 7.1.2 (missing channels are silence).
    for bi in 0..n_beds {
        let pack = format!("AP_0001{:04x}", 0x1001 + bi);
        let ao = format!("AO_{object_n:04x}");
        object_n += 1;
        let _ = writeln!(object_refs, "<audioObjectIDRef>{ao}</audioObjectIDRef>");
        let _ = writeln!(objects, r#"<audioObject audioObjectID="{ao}" audioObjectName="Atmos_Bed_{}" start="00:00:00.00000" duration="{end_tc}">"#, bi + 1);
        let _ = writeln!(objects, "<audioPackFormatIDRef>{pack}</audioPackFormatIDRef>");
        let _ = writeln!(packs, r#"<audioPackFormat audioPackFormatID="{pack}" audioPackFormatName="AtmosCustomPackFormat{}" typeDefinition="DirectSpeakers" typeLabel="0001">"#, bi + 1);
        for (i, (name, label, pos)) in RC.iter().enumerate() {
            let n = 0x1001 + bi as u32 * 10 + i as u32;
            let ac = format!("AC_0001{n:04x}");
            let as_ = format!("AS_0001{n:04x}");
            let at = format!("AT_0001{n:04x}_01");
            let uid = format!("ATU_{uid_n:08x}");
            uid_n += 1;
            let _ = writeln!(packs, "<audioChannelFormatIDRef>{ac}</audioChannelFormatIDRef>");
            let _ = writeln!(channels, r#"<audioChannelFormat audioChannelFormatID="{ac}" audioChannelFormatName="{name}" typeDefinition="DirectSpeakers" typeLabel="0001"><audioBlockFormat audioBlockFormatID="AB_0001{n:04x}_00000001"><speakerLabel>{label}</speakerLabel><cartesian>1</cartesian><position coordinate="X">{}</position><position coordinate="Y">{}</position><position coordinate="Z">{}</position></audioBlockFormat></audioChannelFormat>"#, num(pos[0]), num(pos[1]), num(pos[2]));
            let _ = writeln!(streams, r#"<audioStreamFormat audioStreamFormatID="{as_}" audioStreamFormatName="PCM_{name}" formatDefinition="PCM" formatLabel="0001"><audioChannelFormatIDRef>{ac}</audioChannelFormatIDRef><audioPackFormatIDRef>{pack}</audioPackFormatIDRef><audioTrackFormatIDRef>{at}</audioTrackFormatIDRef></audioStreamFormat>"#);
            let _ = writeln!(tracks, r#"<audioTrackFormat audioTrackFormatID="{at}" audioTrackFormatName="PCM_{name}" formatDefinition="PCM" formatLabel="0001"><audioStreamFormatIDRef>{as_}</audioStreamFormatIDRef></audioTrackFormat>"#);
            let _ = writeln!(uids, r#"<audioTrackUID UID="{uid}" bitDepth="24" sampleRate="{rate}"><audioTrackFormatIDRef>{at}</audioTrackFormatIDRef><audioPackFormatIDRef>{pack}</audioPackFormatIDRef></audioTrackUID>"#);
            let _ = writeln!(objects, "<audioTrackUIDRef>{uid}</audioTrackUIDRef>");
            let src = elements
                .iter()
                .find(|e| e.kind == Kind::Bed && e.bed_instance == bi && e.label == BED_712[i])
                .map(|e| e.channel);
            channel_map.push(src);
            lfe_flags.push(BED_712[i] == "LFE");
            chna.push(ChnaEntry { track_index: channel_map.len() as u16, uid, track_ref: at, pack_ref: pack.clone() });
        }
        packs.push_str("</audioPackFormat>\n");
        objects.push_str("</audioObject>\n");
    }

    // Objects (and bed channels outside 7.1.2, as static objects at their speaker).
    let mut obj_index = 0u32;
    for (e, kfs) in elements.iter().zip(keyframes) {
        if e.kind == Kind::Bed && BED_712.contains(&e.label.as_str()) {
            continue;
        }
        obj_index += 1;
        let n = 0x1000 + obj_index;
        let name = format!("Atmos_Obj_{obj_index}");
        let ap = format!("AP_0003{n:04x}");
        let ac = format!("AC_0003{n:04x}");
        let at = format!("AT_0003{n:04x}_01");
        let as_ = format!("AS_0003{n:04x}");
        let ao = format!("AO_{object_n:04x}");
        let uid = format!("ATU_{uid_n:08x}");
        object_n += 1;
        uid_n += 1;

        let _ = writeln!(object_refs, "<audioObjectIDRef>{ao}</audioObjectIDRef>");
        let _ = writeln!(objects, r#"<audioObject audioObjectID="{ao}" audioObjectName="{name}" start="00:00:00.00000" duration="{end_tc}"><audioPackFormatIDRef>{ap}</audioPackFormatIDRef><audioTrackUIDRef>{uid}</audioTrackUIDRef></audioObject>"#);
        let _ = writeln!(packs, r#"<audioPackFormat audioPackFormatID="{ap}" audioPackFormatName="{name}" typeDefinition="Objects" typeLabel="0003"><audioChannelFormatIDRef>{ac}</audioChannelFormatIDRef></audioPackFormat>"#);
        let _ = writeln!(channels, r#"<audioChannelFormat audioChannelFormatID="{ac}" audioChannelFormatName="{name}" typeDefinition="Objects" typeLabel="0003">"#);

        let seg = if e.kind == Kind::Bed {
            vec![Keyframe { sample_pos: 0, pos: label_position(&e.label), gain: 1.0, size: 0.0, ramp: 0 }]
        } else {
            segment_keyframes(kfs, 0, frames, opt)
        };
        let mut bi = 0u32;
        for (i, k) in seg.iter().enumerate() {
            let next = seg.get(i + 1).map(|x| x.sample_pos).unwrap_or(frames);
            let (u0, u1) = (units(k.sample_pos, rate), units(next, rate));
            if u1 <= u0 {
                continue;
            }
            bi += 1;
            let _ = write!(
                channels,
                r#"<audioBlockFormat audioBlockFormatID="AB_0003{n:04x}_{bi:08x}" rtime="{}" duration="{}"><cartesian>1</cartesian><position coordinate="X">{}</position><position coordinate="Y">{}</position>"#,
                tc_units(u0),
                tc_units(u1 - u0),
                num(k.pos[0].clamp(-1.0, 1.0)),
                num(k.pos[1].clamp(-1.0, 1.0)),
            );
            let z = k.pos[2].clamp(-1.0, 1.0);
            if z != 0.0 {
                let _ = write!(channels, r#"<position coordinate="Z">{}</position>"#, num(z));
            }
            let size = k.size.clamp(0.0, 1.0);
            if size > 0.0 {
                let _ = write!(channels, "<width>{0}</width><depth>{0}</depth><height>{0}</height>", num(size));
            }
            if (k.gain - 1.0).abs() > 1e-9 {
                let _ = write!(channels, "<gain>{}</gain>", num(k.gain));
            }
            let iu = units((k.ramp as u64).min(next - k.sample_pos), rate).min(u1 - u0);
            let _ = writeln!(
                channels,
                r#"<jumpPosition interpolationLength="{}.{:06}">1</jumpPosition></audioBlockFormat>"#,
                iu / 100_000,
                (iu % 100_000) * 10
            );
        }
        channels.push_str("</audioChannelFormat>\n");
        let _ = writeln!(streams, r#"<audioStreamFormat audioStreamFormatID="{as_}" audioStreamFormatName="PCM_{name}" formatDefinition="PCM" formatLabel="0001"><audioChannelFormatIDRef>{ac}</audioChannelFormatIDRef><audioPackFormatIDRef>{ap}</audioPackFormatIDRef><audioTrackFormatIDRef>{at}</audioTrackFormatIDRef></audioStreamFormat>"#);
        let _ = writeln!(tracks, r#"<audioTrackFormat audioTrackFormatID="{at}" audioTrackFormatName="PCM_{name}" formatDefinition="PCM" formatLabel="0001"><audioStreamFormatIDRef>{as_}</audioStreamFormatIDRef></audioTrackFormat>"#);
        let _ = writeln!(uids, r#"<audioTrackUID UID="{uid}" bitDepth="24" sampleRate="{rate}"><audioTrackFormatIDRef>{at}</audioTrackFormatIDRef><audioPackFormatIDRef>{ap}</audioPackFormatIDRef></audioTrackUID>"#);
        channel_map.push(Some(e.channel));
        lfe_flags.push(false);
        chna.push(ChnaEntry { track_index: channel_map.len() as u16, uid, track_ref: at, pack_ref: ap });
    }

    let mut x = String::with_capacity(channels.len() + objects.len() + 65536);
    x.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    x.push_str(r#"<ebuCoreMain xmlns="urn:ebu:metadata-schema:ebuCore_2016" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:schemaLocation="urn:ebu:metadata-schema:ebuCore_2016 ebucore.xsd" xml:lang="en">"#);
    x.push_str("\n<coreMetadata><format><audioFormatExtended>\n");
    let _ = writeln!(x, r#"<audioProgramme audioProgrammeID="APR_1001" audioProgrammeName="{}" start="00:00:00.00000" end="{end_tc}"><audioContentIDRef>ACO_1001</audioContentIDRef></audioProgramme>"#, esc(title));
    let _ = writeln!(x, r#"<audioContent audioContentID="ACO_1001" audioContentName="Atmos_Master_Content">"#);
    x.push_str(&object_refs);
    x.push_str("<dialogue mixedContentKind=\"0\">2</dialogue>\n</audioContent>\n");
    x.push_str(&objects);
    x.push_str(&packs);
    x.push_str(&channels);
    x.push_str(&streams);
    x.push_str(&tracks);
    x.push_str(&uids);
    x.push_str("</audioFormatExtended></format></coreMetadata></ebuCoreMain>\n");

    AdmDoc { axml: x, chna, channel_map, dbmd: Some(build_dbmd(&lfe_flags)) }
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
        dbmd: None,
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

#[cfg(test)]
mod tests {
    #[test]
    fn dbmd_matches_dolby_renderer() {
        // 7.1.2 bed + 15 objects, as in a Dolby Atmos Renderer ADM export.
        let mut lfe = vec![false; 25];
        lfe[3] = true;
        let ours = super::build_dbmd(&lfe);
        let dolby = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../tests/dolby_dbmd.bin")).unwrap();
        let first = ours.iter().zip(dolby.iter()).position(|(a, b)| a != b);
        assert_eq!(ours, dolby, "lens {} {} first diff {:?}", ours.len(), dolby.len(), first);
    }
}
