//! Export As's Metadata: None (`XmpEmbed::None`) embeds none of the document's metadata. It used
//! to drop only the XMP packet (#647): the EXIF block a camera JPEG opened with (make, model, body
//! serial number, capture time and the GPS position) was still written to PNG (`eXIf`), JPEG
//! (APP1) and WebP (`EXIF`), and a layered TIFF kept it, with the IPTC record, as Photoshop
//! resources. `XmpEmbed::All` (Save As, Export As's All) keeps all of it. Either way the file
//! opens upright: the pixels are stored as shown, and any EXIF written says Orientation 1.
//!
//! Checked per format that can carry EXIF (PNG, JPEG, lossless and lossy WebP), on the native
//! single-layer path and on the composite path, and for flat and layered TIFF.

use std::sync::Arc;

use photocraft_codecs::{ChannelLayout, EncodeOptions, Format, Image, decode, exif_orientation};
use photocraft_doc::{Document, Layer, LayerContent};
use photocraft_io::{ExportOptions, XmpEmbed, export, import};
use photocraft_raster::Surface;
use photocraft_raw::testgen::{TiffBuilder, Val};

const MAKE: &str = "SecretCam Industries";
const MODEL: &str = "Model X1 Private";
const SERIAL: &str = "SN-PRIVATE-424242";
const TAKEN: &str = "2026:10:01 09:30:00";
const CITY: &str = "Jerusalem";
/// EXIF IFD0's pointer to the GPS IFD.
const GPS_IFD: u16 = 0x8825;

/// A camera's XMP: GPS, camera and location properties beside the File Info fields.
const CAMERA_XMP: &str = concat!(
    r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">"#,
    r#"<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:exif="http://ns.adobe.com/exif/1.0/""#,
    r#" xmlns:tiff="http://ns.adobe.com/tiff/1.0/" xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/""#,
    r#" exif:GPSLatitude="31,46.5N" exif:GPSLongitude="35,13.2E" tiff:Make="SecretCam Industries" photoshop:City="Jerusalem">"#,
    r#"<dc:rights><rdf:Alt><rdf:li xml:lang="x-default">© Harbour Studio</rdf:li></rdf:Alt></dc:rights>"#,
    r#"</rdf:Description></rdf:RDF></x:xmpmeta>"#,
);

/// A camera EXIF block (TIFF structure, little-endian): IFD0 with make, model and `orientation`,
/// an EXIF IFD with the capture time and body serial number, and a GPS IFD.
fn camera_exif(orientation: u16) -> Vec<u8> {
    let mut t = TiffBuilder::default();
    let gps = t.ifd(vec![
        (0, Val::Byte(vec![2, 3, 0, 0])),
        (1, Val::Ascii("N".into())),
        (2, Val::Rational(vec![(31, 1), (46, 1), (3012, 100)])),
        (3, Val::Ascii("E".into())),
        (4, Val::Rational(vec![(35, 1), (13, 1), (1188, 100)])),
    ]);
    let exif = t.ifd(vec![(0x9003, Val::Ascii(TAKEN.into())), (0xA431, Val::Ascii(SERIAL.into()))]);
    let ifd0 = t.ifd(vec![
        (271, Val::Ascii(MAKE.into())),
        (272, Val::Ascii(MODEL.into())),
        (274, Val::Short(vec![orientation])),
        (34665, Val::Ifds(vec![exif])),
        (34853, Val::Ifds(vec![gps])),
    ]);
    t.chain = vec![ifd0];
    t.build()
}

/// Whether IFD0 of an EXIF block (little-endian TIFF structure, optionally after `Exif\0\0`)
/// points to a GPS IFD.
fn has_gps_ifd(exif: &[u8]) -> bool {
    let tiff = exif.strip_prefix(b"Exif\0\0").unwrap_or(exif);
    assert!(tiff.starts_with(b"II*\0"), "little-endian EXIF");
    let at = u32::from_le_bytes([tiff[4], tiff[5], tiff[6], tiff[7]]) as usize;
    let n = u16::from_le_bytes([tiff[at], tiff[at + 1]]) as usize;
    (0..n).any(|i| {
        let e = at + 2 + 12 * i;
        u16::from_le_bytes([tiff[e], tiff[e + 1]]) == GPS_IFD
    })
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// Camera data, GPS or location anywhere in the file's bytes.
fn leaks(bytes: &[u8]) -> Vec<&'static str> {
    [MAKE, MODEL, SERIAL, TAKEN, CITY, "GPSLatitude"].into_iter().filter(|s| contains(bytes, s)).collect()
}

/// A 24×16 JPEG as a camera stores a portrait (Orientation 6), with the camera EXIF and XMP as
/// APP1 segments.
fn camera_jpeg() -> Vec<u8> {
    let (w, h) = (24u32, 16u32);
    let px: Vec<u8> = (0..w * h).flat_map(|i| [(i % 251) as u8, 120, (255 - i % 200) as u8]).collect();
    let img = Image::from_u8(w, h, ChannelLayout::Rgb, px).unwrap();
    let jpeg = photocraft_codecs::encode(&img, Format::Jpeg, &EncodeOptions { embed_metadata: false, ..Default::default() }).unwrap();
    let mut exif = b"Exif\0\0".to_vec();
    exif.extend_from_slice(&camera_exif(6));
    let mut xmp = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
    xmp.extend_from_slice(CAMERA_XMP.as_bytes());
    let mut out = jpeg[..2].to_vec();
    for seg in [exif, xmp] {
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((seg.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&seg);
    }
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// The camera JPEG opened: upright (16×24), its EXIF (GPS included) and XMP kept in the document.
fn camera_document() -> Document {
    let d = import("IMG_0042.JPG", &camera_jpeg()).unwrap().document;
    assert_eq!((d.size.width, d.size.height), (16, 24), "Orientation 6 is applied on open");
    let exif = d.metadata.exif.as_deref().expect("the document keeps the camera EXIF");
    assert!(has_gps_ifd(exif) && contains(exif, MAKE) && contains(exif, SERIAL), "fixture EXIF");
    assert!(d.metadata.xmp.as_deref().is_some_and(|x| x.contains("GPSLatitude")), "fixture XMP");
    d
}

/// `d` with a second, partly transparent layer on top: exports go through the composite.
fn layered(mut d: Document) -> Document {
    let mut s = Surface::new(d.pixel_format());
    s.fill_rect(photocraft_geom::Rect::new(2, 3, 9, 12), &[0.9, 0.1, 0.1, 1.0]);
    d.layers.push(Layer::new("Mark", LayerContent::Raster(s)));
    d
}

/// The formats that can carry EXIF, as (extension, encoder options).
fn exif_formats() -> Vec<(&'static str, EncodeOptions)> {
    vec![
        ("png", EncodeOptions::default()),
        ("jpg", EncodeOptions::default()),
        ("webp", EncodeOptions { webp_lossless: true, ..Default::default() }),
        ("webp", EncodeOptions { webp_lossless: false, webp_quality: 80, ..Default::default() }),
    ]
}

fn options(xmp: XmpEmbed, encode: &EncodeOptions) -> ExportOptions {
    ExportOptions { encode: encode.clone(), xmp, ..ExportOptions::default() }
}

fn assert_no_metadata(doc: &Document, what: &str) {
    for (ext, encode) in exif_formats() {
        let lossless = encode.webp_lossless;
        let r = export(doc, &format!("out.{ext}"), &options(XmpEmbed::None, &encode)).unwrap();
        let tag = format!("{what} {ext} (webp lossless {lossless})");
        assert!(leaks(&r.bytes).is_empty(), "{tag}: Metadata None wrote {:?}", leaks(&r.bytes));
        let back = decode(&r.bytes).unwrap();
        assert!(back.meta.exif.is_none(), "{tag}: Metadata None wrote an EXIF block");
        assert!(back.meta.xmp.is_none(), "{tag}: Metadata None wrote XMP");
        assert_eq!(back.dimensions(), (16, 24), "{tag}: the file opens upright without EXIF");
    }
}

#[test]
fn metadata_none_writes_no_exif_or_gps_in_any_format() {
    assert_no_metadata(&camera_document(), "single layer");
}

#[test]
fn metadata_none_writes_no_exif_or_gps_from_the_composite() {
    assert_no_metadata(&layered(camera_document()), "two layers");
}

#[test]
fn metadata_all_keeps_the_camera_exif_upright() {
    for doc in [camera_document(), layered(camera_document())] {
        for (ext, encode) in exif_formats() {
            let lossless = encode.webp_lossless;
            let r = export(&doc, &format!("out.{ext}"), &options(XmpEmbed::All, &encode)).unwrap();
            let tag = format!("{} layer(s) {ext} (webp lossless {lossless})", doc.layers.len());
            let back = decode(&r.bytes).unwrap();
            let exif = back.meta.exif.as_deref().unwrap_or_else(|| panic!("{tag}: All keeps the EXIF"));
            assert!(contains(exif, MAKE) && contains(exif, SERIAL) && has_gps_ifd(exif), "{tag}: All keeps the camera data and GPS");
            assert_eq!(exif_orientation(exif), 1, "{tag}: written upright");
            assert!(contains(&r.bytes, "GPSLatitude"), "{tag}: All keeps the whole XMP");
            assert_eq!(back.dimensions(), (16, 24), "{tag}");
        }
    }
}

/// An IPTC-NAA record: City (2:90) and By-line (2:80).
fn iptc() -> Vec<u8> {
    let mut v = vec![0x1C, 2, 90, 0, CITY.len() as u8];
    v.extend_from_slice(CITY.as_bytes());
    v.extend_from_slice(&[0x1C, 2, 80, 0, 12]);
    v.extend_from_slice(b"Dana Example");
    v
}

#[test]
fn layered_tiff_metadata_none_drops_the_exif_and_iptc_resources() {
    let mut d = layered(camera_document());
    d.metadata.psd_resources.push((1028, String::new(), Arc::new(iptc())));
    let none = ExportOptions { tiff_layers: true, xmp: XmpEmbed::None, ..ExportOptions::default() };
    let r = export(&d, "out.tif", &none).unwrap();
    assert!(import("out.tif", &r.bytes).unwrap().document.layers.len() >= 2, "written as a layered TIFF");
    assert!(leaks(&r.bytes).is_empty(), "layered TIFF, Metadata None wrote {:?}", leaks(&r.bytes));
    assert!(!contains(&r.bytes, "Dana Example"), "layered TIFF, Metadata None wrote the IPTC record");
    let all = ExportOptions { tiff_layers: true, xmp: XmpEmbed::All, ..ExportOptions::default() };
    let r = export(&d, "out.tif", &all).unwrap();
    assert!(contains(&r.bytes, MAKE) && contains(&r.bytes, SERIAL), "layered TIFF, All keeps the EXIF resource");
    assert!(contains(&r.bytes, "Dana Example"), "layered TIFF, All keeps the IPTC record");
}

#[test]
fn flat_tiff_metadata_none_writes_no_camera_data() {
    let d = camera_document();
    let none = ExportOptions { tiff_layers: false, xmp: XmpEmbed::None, ..ExportOptions::default() };
    let r = export(&d, "out.tif", &none).unwrap();
    assert!(leaks(&r.bytes).is_empty(), "flat TIFF, Metadata None wrote {:?}", leaks(&r.bytes));
    assert_eq!(decode(&r.bytes).unwrap().dimensions(), (16, 24));
}
