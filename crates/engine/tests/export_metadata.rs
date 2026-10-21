//! What the export commands write of a camera photo's metadata, by the label of each option. The
//! fixture is a camera JPEG: EXIF with make, model, body serial number, capture time and a GPS IFD,
//! and an XMP packet with GPS, camera and location properties beside the File Info fields
//! (copyright, author, description).
//!
//! - File › Export › Quick Export, Export Preferences › Metadata: "none" writes no EXIF and no XMP,
//!   "copyright" only the copyright fields, "all" the whole packet. WebP went through the regular
//!   exporter with everything embedded, whatever the preference: the camera EXIF (GPS included)
//!   and the whole XMP.
//! - File › Export › Save for Web (Legacy), JPEG and PNG-24: no camera data or GPS unless the
//!   choice is "all". "allExceptCameraInfo" was an alias of "all" and embedded the camera's GPS,
//!   make and serial number from the XMP.

use photocraft_codecs::{ChannelLayout, EncodeOptions, Format, Image};
use photocraft_engine::Session;
use photocraft_engine::web_cmds::WebSettings;
use photocraft_raw::testgen::{TiffBuilder, Val};
use serde_json::json;

const MAKE: &str = "SecretCam Industries";
const SERIAL: &str = "SN-PRIVATE-424242";
const TAKEN: &str = "2026:10:01 09:30:00";
const CITY: &str = "Jerusalem";
const COPYRIGHT: &str = "Harbour Studio";
const AUTHOR: &str = "Dana Example";
const DESCRIPTION: &str = "Private family trip";

const CAMERA_XMP: &str = concat!(
    r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">"#,
    r#"<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:exif="http://ns.adobe.com/exif/1.0/""#,
    r#" xmlns:tiff="http://ns.adobe.com/tiff/1.0/" xmlns:aux="http://ns.adobe.com/exif/1.0/aux/" xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/""#,
    r#" exif:GPSLatitude="31,46.5N" exif:GPSLongitude="35,13.2E" tiff:Make="SecretCam Industries" aux:SerialNumber="SN-PRIVATE-424242""#,
    r#" photoshop:City="Jerusalem">"#,
    r#"<dc:rights><rdf:Alt><rdf:li xml:lang="x-default">© Harbour Studio</rdf:li></rdf:Alt></dc:rights>"#,
    r#"<dc:creator><rdf:Seq><rdf:li>Dana Example</rdf:li></rdf:Seq></dc:creator>"#,
    r#"<dc:description><rdf:Alt><rdf:li xml:lang="x-default">Private family trip</rdf:li></rdf:Alt></dc:description>"#,
    r#"</rdf:Description></rdf:RDF></x:xmpmeta>"#,
);

/// A camera EXIF block (TIFF structure): make, orientation 1, capture time, serial number and GPS.
fn camera_exif() -> Vec<u8> {
    let mut t = TiffBuilder::default();
    let gps = t.ifd(vec![(1, Val::Ascii("N".into())), (2, Val::Rational(vec![(31, 1), (46, 1), (3012, 100)])), (3, Val::Ascii("E".into()))]);
    let exif = t.ifd(vec![(0x9003, Val::Ascii(TAKEN.into())), (0xA431, Val::Ascii(SERIAL.into()))]);
    let ifd0 = t.ifd(vec![(271, Val::Ascii(MAKE.into())), (274, Val::Short(vec![1])), (34665, Val::Ifds(vec![exif])), (34853, Val::Ifds(vec![gps]))]);
    t.chain = vec![ifd0];
    t.build()
}

/// A session holding the camera JPEG, opened as the editor opens it.
fn camera_session() -> Session {
    let (w, h) = (32u32, 24u32);
    let px: Vec<u8> = (0..w * h).flat_map(|i| [(i % 251) as u8, 140, (255 - i % 200) as u8]).collect();
    let img = Image::from_u8(w, h, ChannelLayout::Rgb, px).unwrap();
    let jpeg = photocraft_codecs::encode(&img, Format::Jpeg, &EncodeOptions { embed_metadata: false, ..Default::default() }).unwrap();
    let mut exif = b"Exif\0\0".to_vec();
    exif.extend_from_slice(&camera_exif());
    let mut xmp = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
    xmp.extend_from_slice(CAMERA_XMP.as_bytes());
    let mut bytes = jpeg[..2].to_vec();
    for seg in [exif, xmp] {
        bytes.extend_from_slice(&[0xFF, 0xE1]);
        bytes.extend_from_slice(&((seg.len() + 2) as u16).to_be_bytes());
        bytes.extend_from_slice(&seg);
    }
    bytes.extend_from_slice(&jpeg[2..]);
    let doc = photocraft_io::import("IMG_0042.JPG", &bytes).unwrap().document;
    assert!(doc.metadata.exif.as_deref().is_some_and(|e| contains(e, SERIAL)), "fixture EXIF");
    assert!(doc.metadata.xmp.as_deref().is_some_and(|x| x.contains("GPSLatitude")), "fixture XMP");
    let mut s = Session::new();
    s.add_document(doc, None);
    s
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("photocraft-export-metadata-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A written file: its bytes, its decoded EXIF and its decoded XMP.
struct Written {
    bytes: Vec<u8>,
    exif: Option<Vec<u8>>,
    xmp: String,
}

fn read(path: &std::path::Path) -> Written {
    let bytes = std::fs::read(path).unwrap();
    let meta = photocraft_codecs::decode(&bytes).unwrap().meta;
    Written { exif: meta.exif, xmp: meta.xmp.unwrap_or_default(), bytes }
}

/// Which of `needles` the file carries, in its bytes or its decoded XMP.
fn carried(w: &Written, needles: &[&'static str]) -> Vec<&'static str> {
    needles.iter().copied().filter(|n| contains(&w.bytes, n) || w.xmp.contains(n)).collect()
}

/// Camera data, GPS and location: written only when the option is "all".
const CAMERA: [&str; 6] = [MAKE, SERIAL, TAKEN, CITY, "GPSLatitude", "GPSLongitude"];

#[test]
fn quick_export_writes_only_what_export_preferences_metadata_says() {
    let dir = tmp("quick");
    let mut s = camera_session();
    for format in ["png", "jpg", "webp"] {
        for metadata in ["none", "copyright", "all"] {
            s.execute("file.export.exportPreferences", json!({"quickExportFormat": format, "metadata": metadata})).unwrap();
            let path = dir.join(format!("{metadata}.{format}"));
            s.execute("file.export.quickExport", json!({"path": path.to_string_lossy()})).unwrap();
            let w = read(&path);
            let tag = format!("Quick Export as {format}, Metadata {metadata}");
            match metadata {
                "none" => {
                    assert!(w.exif.is_none(), "{tag}: wrote the camera EXIF");
                    assert!(w.xmp.is_empty(), "{tag}: wrote XMP");
                    let found = carried(&w, &[MAKE, SERIAL, TAKEN, CITY, "GPSLatitude", COPYRIGHT, AUTHOR, DESCRIPTION]);
                    assert!(found.is_empty(), "{tag}: wrote {found:?}");
                }
                "copyright" => {
                    assert!(w.exif.is_none(), "{tag}: wrote the camera EXIF");
                    assert!(w.xmp.contains(COPYRIGHT), "{tag}: the copyright is kept");
                    let found = carried(&w, &[MAKE, SERIAL, TAKEN, CITY, "GPSLatitude", AUTHOR, DESCRIPTION]);
                    assert!(found.is_empty(), "{tag}: wrote {found:?}");
                }
                _ => assert!(w.xmp.contains(DESCRIPTION) && w.xmp.contains(COPYRIGHT), "{tag}: All keeps the whole packet"),
            }
        }
    }
    s.execute("prefs.reset", json!({"path": "export"})).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn save_for_web_writes_camera_data_and_gps_only_for_all() {
    let dir = tmp("sfw");
    let mut s = camera_session();
    for format in ["jpeg", "png24"] {
        for metadata in ["none", "copyright", "copyrightAndContact", "allExceptCameraInfo", "all"] {
            let path = dir.join(format!("{metadata}-{format}.img"));
            s.execute("file.export.saveForWebLegacy", json!({"format": format, "metadata": metadata, "path": path.to_string_lossy()})).unwrap();
            let w = read(&path);
            let tag = format!("Save for Web {format}, Metadata {metadata}");
            assert!(w.exif.is_none(), "{tag}: wrote an EXIF block");
            if metadata != "all" {
                let found = carried(&w, &CAMERA);
                assert!(found.is_empty(), "{tag}: wrote camera data or GPS {found:?}");
                assert!(!w.xmp.contains("exif:") && !w.xmp.contains("tiff:") && !w.xmp.contains("aux:"), "{tag}: camera properties in the XMP");
            }
            let expected: &[&str] = match metadata {
                "none" => &[],
                "copyright" => &[COPYRIGHT],
                "copyrightAndContact" => &[COPYRIGHT, AUTHOR],
                _ => &[COPYRIGHT, AUTHOR, DESCRIPTION],
            };
            for kept in [COPYRIGHT, AUTHOR, DESCRIPTION] {
                assert_eq!(w.xmp.contains(kept), expected.contains(&kept), "{tag}: {kept}");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn all_except_camera_info_is_its_own_choice() {
    let st = WebSettings::from_params(&json!({"metadata": "allExceptCameraInfo"}), "file.export.saveForWebLegacy").unwrap();
    assert_eq!(st.to_params()["metadata"], "allExceptCameraInfo");
    let all = WebSettings::from_params(&json!({"metadata": "all"}), "file.export.saveForWebLegacy").unwrap();
    assert_eq!(all.to_params()["metadata"], "all");
    assert_ne!(st, all);
}
