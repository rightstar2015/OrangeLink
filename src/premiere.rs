//! Export items as Final Cut Pro 7 XML (xmeml), which Premiere Pro imports via
//! File > Import. Library folders become bins; files are referenced in place.
//! Optionally the items' clips are also laid out on a sequence, with a marker
//! at each, and a second 1080×1920 sequence uses each clip's 9:16 frame.

use crate::{
    library::Library,
    model::{Item, MediaKind},
};
use std::{collections::HashSet, fmt::Write};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// `file://localhost/F%3a/dir/a%20b.mp4`, the form Premiere writes itself.
fn path_url(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file://localhost/");
    for b in s.trim_start_matches('/').bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else if b == b':' {
            out.push_str("%3a");
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// Whether Premiere can use this item as media (images, video, audio).
pub fn is_exportable(lib: &Library, it: &Item) -> bool {
    it.kind != MediaKind::Other && !it.deleted && !lib.missing.contains(&it.id)
}

/// (timebase, ntsc) for a frame rate, e.g. 29.97 -> (30, true).
fn rate(fps: Option<f64>) -> (u32, bool) {
    let fps = fps.unwrap_or(30.0);
    let rounded = fps.round();
    let ntsc = (rounded - fps).abs() > 0.01 && (rounded * 1000.0 / 1001.0 - fps).abs() < 0.01;
    (rounded.max(1.0) as u32, ntsc)
}

fn rate_xml(tb: u32, ntsc: bool) -> String {
    format!("<rate><timebase>{tb}</timebase><ntsc>{}</ntsc></rate>", if ntsc { "TRUE" } else { "FALSE" })
}

fn clip_xml(out: &mut String, lib: &Library, it: &Item, n: usize) {
    let (tb, ntsc) = rate(it.fps);
    let frames = match (it.kind, it.duration) {
        (MediaKind::Video | MediaKind::Audio, Some(d)) => ((d * tb as f64).round() as u64).max(1),
        _ => 5 * tb as u64, // stills: 5 seconds
    };
    let r = rate_xml(tb, ntsc);
    let title = esc(&it.name);
    let is_audio = it.kind == MediaKind::Audio;
    let audio_chars = "<audio><samplecharacteristics><depth>16</depth><samplerate>48000</samplerate></samplecharacteristics>\
                       <channelcount>2</channelcount></audio>";
    let file_media = if is_audio {
        audio_chars.to_string()
    } else {
        format!(
            "<video><samplecharacteristics>{r}<width>{}</width><height>{}</height><anamorphic>FALSE</anamorphic>\
             <pixelaspectratio>square</pixelaspectratio><fielddominance>none</fielddominance></samplecharacteristics></video>{}",
            it.width,
            it.height,
            if it.kind == MediaKind::Video { audio_chars } else { "" }
        )
    };
    // Same shape as Premiere's own export: a master clip whose clipitem holds the file.
    let file = format!(
        "<file id=\"file-{n}\"><name>{}</name><pathurl>{}</pathurl>{r}<duration>{frames}</duration>\
         <timecode>{r}<string>00:00:00:00</string><frame>0</frame><displayformat>NDF</displayformat></timecode>\
         <media>{file_media}</media></file>",
        esc(&it.file_name),
        esc(&path_url(&lib.file_path(it))),
    );
    let track = if is_audio { "audio" } else { "video" };
    let alpha = if is_audio { "" } else { "<alphatype>none</alphatype>" };
    let _ = writeln!(
        out,
        "<clip id=\"masterclip-{n}\" explodedTracks=\"true\"><masterclipid>masterclip-{n}</masterclipid>\
         <ismasterclip>TRUE</ismasterclip><duration>{frames}</duration>{r}<name>{title}</name>\
         <media><{track}><track><clipitem id=\"clipitem-{n}\"><masterclipid>masterclip-{n}</masterclipid>\
         <name>{title}</name>{r}{alpha}{file}</clipitem></track></{track}></media></clip>",
    );
    // Marked clips: the same file with in/out points, referencing the file above.
    // `subclipinfo` (frames trimmed from each end) makes them subclips limited to
    // the marked range; an importer that ignores it still gets the in/out points.
    for (k, c) in it.clips.iter().enumerate() {
        let (i, o) = ((c.start * tb as f64).round() as u64, ((c.end * tb as f64).round() as u64).min(frames));
        if o <= i {
            continue;
        }
        let name = esc(&format!("{} - {}", it.name, c.name));
        let tail = frames - o;
        let _ = writeln!(
            out,
            "<clip id=\"masterclip-{n}-{k}\" explodedTracks=\"true\"><masterclipid>masterclip-{n}-{k}</masterclipid>\
             <ismasterclip>TRUE</ismasterclip><duration>{frames}</duration>{r}<in>{i}</in><out>{o}</out><name>{name}</name>\
             <subclipinfo><startoffset>{i}</startoffset><endoffset>{tail}</endoffset></subclipinfo>\
             <media><{track}><track><clipitem id=\"clipitem-{n}-{k}\"><masterclipid>masterclip-{n}-{k}</masterclipid>\
             <name>{name}</name>{r}<in>{i}</in><out>{o}</out>{alpha}<file id=\"file-{n}\"/></clipitem></track></{track}></media></clip>",
        );
    }
}

/// Sequences built alongside the bins.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sequences {
    /// The clips one after another on a timeline.
    pub sequence: bool,
    /// Seconds of gap between clips.
    pub gap: f64,
    /// A 1080×1920 sequence of the video clips, framed by their 9:16 crop.
    pub vertical: bool,
}

/// One clip placed on a sequence.
struct Placed<'a> {
    it: &'a Item,
    /// The `file-{n}` its master clip declared.
    file: usize,
    clip: &'a crate::model::Clip,
}

/// The clips of `items` in order: items as given, each item's clips by start.
fn placed<'a>(items: &[(&'a Item, usize)], video_only: bool) -> Vec<Placed<'a>> {
    let mut out = Vec::new();
    for &(it, file) in items {
        if video_only && it.kind != MediaKind::Video || !matches!(it.kind, MediaKind::Video | MediaKind::Audio) {
            continue;
        }
        let mut clips: Vec<&crate::model::Clip> = it.clips.iter().filter(|c| c.end > c.start).collect();
        clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        out.extend(clips.into_iter().map(|clip| Placed { it, file, clip }));
    }
    out
}

/// How many clips a sequence would hold, for the export dialog.
pub fn clip_count(lib: &Library, ids: &[String], video_only: bool) -> usize {
    let items: Vec<(&Item, usize)> = ids.iter().filter_map(|id| lib.items.get(id)).filter(|i| is_exportable(lib, i)).map(|i| (i, 0)).collect();
    placed(&items, video_only).len()
}

/// Basic Motion for a vertical sequence: scaled so the height fills 1920 and
/// moved so the clip's crop centre is in the middle. Center is in fractions of
/// the sequence frame from its middle.
fn motion(it: &Item, crop_x: f32) -> String {
    let (w, h) = (it.width.max(1) as f64, it.height.max(1) as f64);
    let scale = 1920.0 / h * 100.0;
    let scaled_w = w * 1920.0 / h;
    let horiz = (0.5 - crop_x as f64) * scaled_w / 1080.0;
    format!(
        "<filter><effect><name>Basic Motion</name><effectid>basic</effectid><effectcategory>motion</effectcategory>\
         <effecttype>motion</effecttype><mediatype>video</mediatype>\
         <parameter><parameterid>scale</parameterid><name>Scale</name><valuemin>0</valuemin><valuemax>1000</valuemax><value>{scale:.3}</value></parameter>\
         <parameter><parameterid>center</parameterid><name>Center</name><value><horiz>{horiz:.5}</horiz><vert>0</vert></value></parameter>\
         </effect></filter>"
    )
}

fn sequence_xml(out: &mut String, name: &str, id: &str, clips: &[Placed], gap: f64, size: (u32, u32), vertical: bool) {
    let Some(first) = clips.first() else { return };
    let (tb, ntsc) = rate(first.it.fps);
    let r = rate_xml(tb, ntsc);
    let frames = |secs: f64| (secs * tb as f64).round() as u64;
    let mut video = String::new();
    let mut audio = String::new();
    let mut markers = String::new();
    let mut at = 0u64;
    for (k, p) in clips.iter().enumerate() {
        let (ctb, cntsc) = rate(p.it.fps);
        let cr = rate_xml(ctb, cntsc);
        let src = |secs: f64| (secs * ctb as f64).round() as u64;
        let total = p.it.duration.map_or(src(p.clip.end), src);
        let (i, o) = (src(p.clip.start), src(p.clip.end).min(total.max(1)));
        let len = frames(p.clip.end - p.clip.start).max(1);
        let (start, end) = (at, at + len);
        let name = esc(&p.clip.name);
        let file = p.file;
        if p.it.kind == MediaKind::Video {
            let fx = if vertical { motion(p.it, p.clip.crop_x.unwrap_or(0.5)) } else { String::new() };
            let _ = writeln!(
                video,
                "<clipitem id=\"{id}-v{k}\"><name>{name}</name><duration>{total}</duration>{cr}<start>{start}</start><end>{end}</end>\
                 <in>{i}</in><out>{o}</out><file id=\"file-{file}\"/>{fx}</clipitem>"
            );
        }
        let _ = writeln!(
            audio,
            "<clipitem id=\"{id}-a{k}\"><name>{name}</name><duration>{total}</duration>{cr}<start>{start}</start><end>{end}</end>\
             <in>{i}</in><out>{o}</out><file id=\"file-{file}\"/><sourcetrack><mediatype>audio</mediatype><trackindex>1</trackindex></sourcetrack></clipitem>"
        );
        let comment = p.clip.note.as_deref().map(esc).unwrap_or_default();
        let _ = write!(markers, "<marker><name>{name}</name><comment>{comment}</comment><in>{start}</in><out>-1</out></marker>");
        at = end + frames(gap.max(0.0));
    }
    let duration = at.saturating_sub(frames(gap.max(0.0)));
    let (w, h) = size;
    let _ = writeln!(
        out,
        "<sequence id=\"{id}\"><name>{}</name><duration>{duration}</duration>{r}\
         <timecode>{r}<string>00:00:00:00</string><frame>0</frame><displayformat>NDF</displayformat></timecode>\
         <media><video><format><samplecharacteristics>{r}<width>{w}</width><height>{h}</height><anamorphic>FALSE</anamorphic>\
         <pixelaspectratio>square</pixelaspectratio><fielddominance>none</fielddominance></samplecharacteristics></format>\
         <track>\n{video}</track></video><audio><track>\n{audio}</track></audio></media>{markers}</sequence>",
        esc(name)
    );
}

/// Builds the XML for `ids`, mirroring the library's folder tree as bins.
/// Items Premiere can't use (subtitles, documents, missing files) are left out.
pub fn build_xml(lib: &Library, ids: &[String], project: &str, seq: Sequences) -> String {
    let wanted: HashSet<&String> = ids.iter().collect();
    let items: Vec<&Item> = lib.items.values().filter(|i| wanted.contains(&i.id) && is_exportable(lib, i)).collect();
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE xmeml>\n<xmeml version=\"4\"><project>");
    let _ = writeln!(out, "<name>{}</name><children>", esc(project));
    let mut n = 0;

    // The file number each item's master clip declares, for sequences to refer to.
    let mut files: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();

    fn bin<'a>(out: &mut String, lib: &Library, items: &[&'a Item], parent: Option<&str>, n: &mut usize, files: &mut std::collections::HashMap<&'a str, usize>) {
        for f in lib.child_folders(parent) {
            let mut inner = String::new();
            for it in items.iter().filter(|i| i.folders.contains(&f.id)) {
                if files.contains_key(it.id.as_str()) {
                    continue; // in an earlier folder: one master clip per file
                }
                *n += 1;
                files.insert(&it.id, *n);
                clip_xml(&mut inner, lib, it, *n);
            }
            bin(&mut inner, lib, items, Some(&f.id), n, files);
            if !inner.is_empty() {
                let _ = write!(out, "<bin><name>{}</name><children>\n{inner}</children></bin>\n", esc(&f.name));
            }
        }
    }
    bin(&mut out, lib, &items, None, &mut n, &mut files);
    let loose: Vec<&Item> = items.iter().copied().filter(|i| !files.contains_key(i.id.as_str())).collect();
    for it in loose {
        n += 1;
        files.insert(&it.id, n);
        clip_xml(&mut out, lib, it, n);
    }
    // Sequences: in the order asked for, falling back to when items were added.
    let mut ordered: Vec<(&Item, usize)> = ids.iter().filter_map(|id| items.iter().find(|i| i.id == *id).map(|i| (*i, files[i.id.as_str()]))).collect();
    ordered.dedup_by_key(|(i, _)| i.id.clone());
    if seq.sequence {
        sequence_xml(&mut out, &format!("{project} 片段"), "sequence-1", &placed(&ordered, false), seq.gap, (1920, 1080), false);
    }
    if seq.vertical {
        sequence_xml(&mut out, &format!("{project} 直式"), "sequence-2", &placed(&ordered, true), seq.gap, (1080, 1920), true);
    }
    out.push_str("</children></project></xmeml>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_non_media_and_nests_bins() {
        let root = std::env::temp_dir().join(format!("orangelink-xml-{}", crate::model::new_id()));
        let mut lib = Library::create(&root).unwrap();
        let folder = lib.add_folder("素材", None);
        let mk = |name: &str, ext: &str, kind: MediaKind, folders: Vec<String>| Item {
            id: crate::model::new_id(),
            name: name.into(),
            file_name: format!("{name}.{ext}"),
            ext: ext.into(),
            kind,
            source: Some(root.join(format!("{name}.{ext}"))),
            duration: Some(2.0),
            size: 1,
            width: 1920,
            height: 1080,
            tags: vec![],
            folders,
            annotation: String::new(),
            url: String::new(),
            rating: 0,
            palette: vec![],
            added_at: 0,
            modified_at: 0,
            deleted: false,
            hash: String::new(),
            origin: None,
            fps: Some(29.97),
            codec: None,
            text: None,
            use_count: 0,
            last_used: None,
            clips: Vec::new(),
            fields: Default::default(),
            subtitle: None,
            links: Vec::new(),
        };
        let mut video = mk("clip", "mp4", MediaKind::Video, vec![folder]);
        video.clips.push(crate::model::Clip { id: "c".into(), name: "開場".into(), start: 0.5, end: 1.5, crop_x: None, ..Default::default() });
        let xml_file = mk("project", "xml", MediaKind::Other, vec![]);
        let ids = vec![video.id.clone(), xml_file.id.clone()];
        lib.insert_item(video);
        lib.insert_item(xml_file);

        let xml = build_xml(&lib, &ids, "Test", Sequences::default());
        assert!(xml.contains("<bin><name>素材</name>"));
        assert!(xml.contains("<pathurl>file://localhost/"));
        assert!(xml.contains("clip.mp4") && !xml.contains("project.xml"));
        assert!(xml.contains("<duration>60</duration>") && xml.contains("<ntsc>TRUE</ntsc>"));
        assert_eq!(xml.matches("<clip ").count(), xml.matches("</clip>").count());
        // 0.5 s .. 1.5 s at 30 fps, sharing the master clip's file.
        assert!(xml.contains("<name>clip - 開場</name>") && xml.contains("<in>15</in><out>45</out>"));
        assert!(xml.contains("<file id=\"file-1\"/>"));
        assert!(xml.contains("<subclipinfo><startoffset>15</startoffset><endoffset>15</endoffset></subclipinfo>"));
        assert!(!xml.contains("<sequence"));

        // With sequences: two clips 1 s apart, a marker each, and a vertical one.
        let vid = ids[0].clone();
        lib.items.get_mut(&vid).unwrap().clips.push(crate::model::Clip { id: "d".into(), name: "結尾".into(), start: 0.0, end: 0.4, note: Some("聊天 ×3.2".into()), crop_x: Some(0.25), ..Default::default() });
        assert_eq!(clip_count(&lib, &ids, true), 2);
        let xml = build_xml(&lib, &ids, "Test", Sequences { sequence: true, gap: 1.0, vertical: true });
        assert_eq!(xml.matches("<sequence ").count(), 2);
        assert_eq!(xml.matches("<clip ").count(), xml.matches("</clip>").count());
        assert_eq!(xml.matches("<sequence ").count(), xml.matches("</sequence>").count());
        // 結尾 (0..0.4 s) first: 12 frames, a 30-frame gap, then 開場 at 42.
        assert!(xml.contains("<start>0</start><end>12</end><in>0</in><out>12</out>"), "{xml}");
        assert!(xml.contains("<start>42</start><end>72</end><in>15</in><out>45</out>"), "{xml}");
        assert!(xml.contains("<marker><name>結尾</name><comment>聊天 ×3.2</comment><in>0</in><out>-1</out></marker>"));
        assert!(xml.contains("<width>1080</width><height>1920</height>"));
        // 1920×1080 scaled to 1920 high is 3413 wide; a quarter in is 853 px left of centre.
        assert!(xml.contains("<value>177.778</value>") && xml.contains("<horiz>0.79012</horiz>"), "{xml}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn urls_and_rates() {
        assert_eq!(path_url(std::path::Path::new(r"F:\素材\a b.mp4")), "file://localhost/F%3a/%E7%B4%A0%E6%9D%90/a%20b.mp4");
        assert_eq!(rate(Some(29.97)), (30, true));
        assert_eq!(rate(Some(25.0)), (25, false));
        assert_eq!(rate(None), (30, false));
    }
}
