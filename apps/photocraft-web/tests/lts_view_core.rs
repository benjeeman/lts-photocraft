//! FP1 tests of the Photo agent view core, T-PH-02..T-PH-12 (implementation plan §1 FP1; final
//! design Appendix A.2, §6.3, §7.1-§7.5; INV-4, INV-12, INV-13, INV-14, INV-18, INV-28, INV-34,
//! INV-35, INV-41, INV-58), and the R0 amendment (master plan v2 §3.1 with its critic corrections):
//! T-PH-12 extended, T-PH-EXIF, T-PH-13, T-PH-13b and T-PH-TXT.
//!
//! `src/web/lts_view_core.rs` is the pure core behind `window.photocraftView`. It uses external
//! crates only, so this test includes it by path and builds it natively; the wasm glue
//! (`src/web/lts_view.rs`) and the other owner's modules are not compiled here. No browser,
//! network, model call or file output.
//!
//! The core's API as these tests use it:
//! - Reads over `&Session` that change no document, UI state, journal or session setting:
//!   `describe`, `render`, `sample` and `metadata` (`(&Session, &Value) -> Result<_, ViewError>`),
//!   `watermark(&Session, busy, families) -> Value`, and `fonts(&mut TextEngine, &Value)`, which
//!   lists and probes without registering anything. `sha256_hex` and `fontset_digest` are the
//!   hand-written digest helpers (no new crate).
//! - A document is `{"index": n}` or `{"id": "<DocId>"}` (both must agree; absent: the active one);
//!   revisions are `p:<DocState::revision>`; DocIds are decimal strings.
//! - `render` returns `Rendered { bytes, info }`: PNG (JPEG or WebP with `format`) without the
//!   document's EXIF or XMP. `info` has `target`, `mime`, `documentId`, `rev`, `width`, `height` and,
//!   per target, `layerId`, `bounds` (`[x, y, width, height]` in document pixels), `mask`,
//!   `historyIndex`, `label` and `current`. Read failures are `ViewError { code: &'static str,
//!   message: String }` with registry codes: `budget_exceeded` (decided from the document header
//!   before any pixel is allocated), `read_failed`, `unsupported_read`. `Rendered` and `ViewError`
//!   derive `Debug`, `Clone` and `PartialEq`.
//! - `execute(&mut PhotocraftApp, &Env, command, &params, &options, run)` returns the
//!   `photocraft.typed-execute.v1` envelope; `run` is `&mut dyn FnMut(&mut PhotocraftApp, &str,
//!   Value) -> Result<Value, String>`. Every `not_applied` code is decided before `run` is called;
//!   once `run` was called the outcome is `applied` or `unknown`. The glue passes
//!   `|app, id, params| app.run(id, params)`; these tests pass spies. During `run` the session gate
//!   is `authorize_step` (`fn(&str, &Value) -> photocraft_engine::Result<()>` over the same
//!   `POLICY`, whose entries have `id`, `keys` and `tier: u8`), script events are off and
//!   background jobs are off; all three are restored afterwards.
//! - `Env<'a>` is what the glue knows at drain time: `hidden`, `expired`, `busy`
//!   (`agent_context::busy`), the registered font `families: &'a [String]`,
//!   `validate: fn(&str, &Value, &Session) -> Result<(), String>` (`agent_context::validate`,
//!   called rather than copied), and `runtime` (`Runtime::Browser` for the browser hook, which
//!   these tests use unless they say otherwise; `Runtime::Native` for a native worker).
//!
//! R0 (master plan v2 §3.1: the newsroom families and safety fixes pre-declared before the one
//! Photo WASM build) adds, and these tests use:
//! - Tier B policy entries (`R0_POLICY` below), each a subset of the keys the engine's own command
//!   documentation (`CommandSpec::params`) names; the never list (`NEVER`) stays out of the table.
//!   Every entry has `runtimes: Runtimes::Both`; work too long for the page is refused in the
//!   browser by value as `native_only` (a Content-Aware Fill hole over the browser's cap).
//! - `export(&Session, &Value) -> Result<Rendered, ViewError>`: the agent export (`EXPORT_KEYS`:
//!   `document`, `format` png|jpeg|webp, `quality`, `maxBytes`, `metadata` none|copyright|newsroom,
//!   `removeLocation` (true or absent), `fields`) through the FP1 encoder. No EXIF, GPS, camera,
//!   location data or resolution ever; `copyright` (OC-13, the default) writes only creator and
//!   copyright, `newsroom` (on request only) adds the IPTC credit line and caption
//!   (`EXPORT_FIELDS`), from the document's File Info or the caller's `fields`; `none` writes
//!   nothing (FP1's render bytes). Over `maxBytes`: `budget_exceeded`. `info` adds
//!   `target:"export"`, `metadata`, `removeLocation` and the written `fields`.
//! - `place_bytes(&mut PhotocraftApp, &Env, Vec<u8>, &params, &options, place)`: the `placeBytes`
//!   hook (`PLACE_BYTES`: id `placeBytes`, keys `name`, `scale`, `center`, `region`, tier 1), with
//!   `place: &mut dyn FnMut(&mut PhotocraftApp, &str, Vec<u8>, Value) -> Result<Value, String>`;
//!   the glue passes `photocraft_engine::file_cmds::place_bytes` and `sync_views`. Same envelope and
//!   discipline as `execute` (every `not_applied` code before `place` is called; input codes
//!   `input_rejected`, `decode_failed`, `budget_exceeded`). `scale` and `center` are required; a
//!   restore `region` pins them to 100 % on its centre and must match the image; `place` gets the
//!   input re-encoded as a metadata-free PNG named `<stem>.png`; both runtimes take 1 byte to
//!   `MAX_INPUT_BYTES` (Photo's input cap, 16 MiB in R0 and 64 MiB from R1: the bytes arrive by
//!   handle, never in the broker's 1 MiB command), and CMYK is refused. The browser hook's queue
//!   holds at most `MAX_PENDING_PLACE_BYTES` of waiting input: `place_queue_refusal(waiting, length)
//!   -> Option<Value>` is the typed `queue_full` refusal the glue returns before it copies an input.
//! - `text_check(&mut TextEngine, &Session, &Value) -> Result<Value, ViewError>` (`TEXT_CHECK_KEYS`:
//!   `document`, `layerId`): a type layer's text against the lines `type.info` lays out: `lines`
//!   (`start`, `end` in characters), `shownChars`, `overflow`, `hiddenText`, `glyphs`, `tofu`,
//!   `missingFont`.
//!
//! R1 (master plan v3 W3, widen-1: M6, M9, M18, RD-12, RD-17, IN-8) adds, and the `r1_*` tests and
//! `jpeg_import_4096` use:
//! - The `R1_POLICY` entries (Layer › Create and Release Clipping Mask, Adjustment Properties for the
//!   six tone kinds, Select › Modify › Expand and Contract) and the `R1_KEYS` (`direction` on
//!   `type.create`, the fill opacity `fill` on `layer.setProps`). No key takes a weight (RD-12).
//!   `type.create` text with a right-to-left letter needs a `direction` (RD-12). A fill below 1 and
//!   an outside or centre stroke on any layer but a shape layer are refused together, in either order
//!   (OC-18: the engine lays such a stroke under the layer's whole shape; the ignored `oc18_*` test
//!   holds what Photoshop shows until the engine fix is GREEN).
//! - RD-17 sizes: documents, crops, resizes, placements and parameter coordinates up to 4096 pixels
//!   a side, and every read of such a document; `MAX_INPUT_BYTES` and `MAX_PENDING_PLACE_BYTES` are
//!   64 MiB.
//! - The import, a `placeBytes` mode: `newDocument: true` (no `scale`, `center` or `region`, no
//!   document preconditions). The core re-encodes the input as a PNG of its pixels, its colour profile
//!   and its positive File Info fields only, and calls the place function with the caller's file
//!   name; the glue then opens it as a new document (`file_cmds::open_bytes_as`, decoded as PNG), whose
//!   colour policy keeps an embedded profile and the pixels as they are.
//! - IN-8 in `describe`: per layer `effects.items`, `blendIf`, `channels` and `linkGroup` as the
//!   engine's own `inspect::layer` reports them, and a type layer's paragraph `direction` and `align`;
//!   per document `channels` (`channel_cmds::channels_json`), `guides` and `profile`.
#![cfg(not(target_arch = "wasm32"))]

#[path = "../src/web/lts_view_core.rs"]
mod lts_view_core;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use lts_view_core::{Env, Rendered, Runtime, Runtimes, ViewError};
use photocraft_codecs::ChannelLayout;
use photocraft_engine::Session;
use photocraft_engine::actions_cmds::Action;
use photocraft_engine::channel_cmds::ChannelTarget;
use photocraft_engine::doc::{
    BlendMode, ColorMode, DocId, Document, Effect, Layer, LayerContent, LayerId, PixelFormat, Rect, SampleType, Size, SmartSource, Surface,
};
use photocraft_text::TextEngine;
use photocraft_ui_egui::{PhotocraftApp, UiState};
use serde_json::{Value, json};

/// The first document's filled selection and pixels: x, y, width, height (as in F0).
const FILLED: [i32; 4] = [8, 6, 20, 12];
/// `#336699`, opaque.
const FILL: [u8; 4] = [0x33, 0x66, 0x99, 0xff];
const WHITE: [u8; 4] = [0xff, 0xff, 0xff, 0xff];
/// The layer mask and selection of T-PH-05: x, y, width, height.
const MASKED: [i32; 4] = [16, 8, 24, 20];
/// The typed execute envelope (design Appendix A.2).
const TYPED: &str = "photocraft.typed-execute.v1";
/// An XMP packet with File Info fields (title, author) and a GPS property.
const XMP: &str = concat!(
    r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">"#,
    r#"<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:exif="http://ns.adobe.com/exif/1.0/" exif:GPSLatitude="31,46.5N">"#,
    r#"<dc:title><rdf:Alt><rdf:li xml:lang="x-default">Fixture title</rdf:li></rdf:Alt></dc:title>"#,
    r#"<dc:creator><rdf:Seq><rdf:li>LTS fixture</rdf:li></rdf:Seq></dc:creator>"#,
    r#"</rdf:Description></rdf:RDF></x:xmpmeta>"#,
);

/// The typed policy table: the legacy 13 commands plus the newsroom set (plan §1 FP1), with exact
/// keys and tiers (1: undoable or view state; 2: destructive, checkpointed by the broker).
const EXPECTED_POLICY: [(&str, u8, &[&str]); 25] = [
    ("document.activate", 1, &["document"]),
    ("document.pixel", 1, &["x", "y"]),
    ("edit.fill", 1, &["color", "target"]),
    ("edit.redo", 1, &[]),
    ("edit.undo", 1, &[]),
    ("file.new", 1, &["width", "height", "background", "name"]),
    ("filter.blur.gaussianBlur", 2, &["radius"]),
    ("filter.pixelate.mosaic", 2, &["cellSize"]),
    ("image.crop", 2, &["x", "y", "width", "height"]),
    ("image.imageSize", 2, &["width", "height"]),
    ("layer.duplicate", 1, &["layer"]),
    ("layer.layerMask.hideAll", 1, &["layer"]),
    ("layer.layerMask.hideSelection", 1, &["layer"]),
    ("layer.layerMask.revealAll", 1, &["layer"]),
    ("layer.layerMask.revealSelection", 1, &["layer"]),
    ("layer.new.layer", 1, &["name"]),
    ("layer.newFillLayer.solidColor", 1, &["color"]),
    ("layer.select", 1, &["layer"]),
    ("layer.setProps", 1, &["layer", "name", "visible", "opacity", "blend"]),
    ("layer.translate", 1, &["layer", "dx", "dy"]),
    ("select.all", 1, &[]),
    ("select.deselect", 1, &[]),
    ("select.inverse", 1, &[]),
    ("select.rect", 1, &["x", "y", "width", "height", "ellipse", "mode", "feather"]),
    ("type.create", 1, &["text", "x", "y", "size", "font", "color", "name", "align", "box"]),
];

/// Real engine commands outside the typed table: ambient file, script, preference, plug-in and
/// profile paths (tier 3, INV-13) and edits no typed tool maps to.
const OUTSIDE: [&str; 16] = [
    "file.close",
    "file.closeAll",
    "file.revert",
    "file.placeLinked",
    "file.fileInfo",
    "file.automate.batch",
    "file.scripts.imageProcessor",
    "file.export.quickExport",
    "actions.play",
    "prefs.set",
    "plugin.install",
    "edit.assignProfile",
    "edit.purge.all",
    "document.move",
    "edit.paste",
    "layer.delete",
];

/// R0 tier B (master plan v2 §3.1, best effort): selections, Select and Mask, Content-Aware Fill
/// on a new layer, a layer-style subset, type depth, a tone and enhance subset, and text and
/// visibility templates, each with a subset of the keys its own params doc names. Image ›
/// Variables Define and Data Sets are tier 2: they change the document without a history step.
const R0_POLICY: &[(&str, u8, &[&str])] = &[
    ("edit.contentAwareFill", 1, &["sampling", "margin", "colorAdaptation", "rotationAdaptation", "scale", "mirror", "output", "seed"]),
    (
        "image.adjustments.shadowsHighlights",
        2,
        &["shadowAmount", "shadowTone", "shadowRadius", "highlightAmount", "highlightTone", "highlightRadius", "color", "midtone"],
    ),
    ("image.applyDataSet", 1, &["name", "index"]),
    ("image.autoColor", 2, &[]),
    ("image.autoContrast", 2, &[]),
    ("image.autoTone", 2, &[]),
    ("image.variables.dataSets", 2, &["dataSets", "append"]),
    ("image.variables.define", 2, &["defs"]),
    ("layer.layerStyle.clear", 1, &["layer"]),
    ("layer.layerStyle.dropShadow", 1, &["layer", "color", "opacity", "angle", "useGlobalLight", "distance", "spread", "size"]),
    ("layer.layerStyle.stroke", 1, &["size", "position", "color", "opacity"]),
    ("layer.newAdjustmentLayer.brightnessContrast", 1, &["brightness", "contrast"]),
    ("layer.newAdjustmentLayer.curves", 1, &["points"]),
    ("layer.newAdjustmentLayer.exposure", 1, &["exposure", "offset", "gamma"]),
    ("layer.newAdjustmentLayer.hueSaturation", 1, &["hue", "saturation", "lightness"]),
    ("layer.newAdjustmentLayer.levels", 1, &["inBlack", "gamma", "inWhite", "outBlack", "outWhite"]),
    ("layer.newAdjustmentLayer.vibrance", 1, &["vibrance", "saturation"]),
    ("select.colorRange", 1, &["select", "color", "points", "fuzziness", "invert", "sampleAllLayers", "mode"]),
    ("select.object", 1, &["rect", "mode", "sampleAllLayers"]),
    ("select.quick", 1, &["points", "size", "mode", "sampleAllLayers", "enhanceEdge"]),
    ("select.refineEdge", 1, &["radius", "smartRadius", "smooth", "feather", "contrast", "shiftEdge", "decontaminate", "amount", "output", "sampleAllLayers"]),
    ("select.subject", 1, &["sampleAllLayers", "mode"]),
    ("type.edit", 1, &["layer", "text", "replace", "box", "point", "move", "name"]),
    ("type.setStyle", 1, &["layer", "range", "font", "size", "color", "tracking", "leading", "fauxBold", "align", "direction"]),
];

/// Never in the table (master plan v2 §3.1): Content-Aware Scale, path commands, and File › Place
/// Embedded with a path (an agent places bytes through the `placeBytes` hook instead).
const NEVER: [&str; 8] =
    ["edit.contentAwareScale", "path.set", "path.fill", "path.stroke", "path.toSelection", "select.toWorkPath", "type.createWorkPath", "file.placeEmbedded"];

/// T-PH-EXIF's camera XMP: GPS, camera and location properties beside the creator, copyright,
/// credit and caption that OC-13 and OD-14 keep.
const CAMERA_XMP: &str = concat!(
    r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">"#,
    r#"<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:exif="http://ns.adobe.com/exif/1.0/""#,
    r#" xmlns:tiff="http://ns.adobe.com/tiff/1.0/" xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/""#,
    r#" xmlns:xmpRights="http://ns.adobe.com/xap/1.0/rights/" exif:GPSLatitude="31,46.5N" exif:GPSLongitude="35,13.2E""#,
    r#" tiff:Make="LTS Camera" tiff:Model="Fixture X1" photoshop:City="Jerusalem" photoshop:Credit="LTS Agency">"#,
    r#"<dc:creator><rdf:Seq><rdf:li>LTS Photographer</rdf:li></rdf:Seq></dc:creator>"#,
    r#"<dc:rights><rdf:Alt><rdf:li xml:lang="x-default">© 2026 LTS Agency</rdf:li></rdf:Alt></dc:rights>"#,
    r#"<dc:description><rdf:Alt><rdf:li xml:lang="x-default">A fixture caption</rdf:li></rdf:Alt></dc:description>"#,
    r#"<xmpRights:Marked>True</xmpRights:Marked>"#,
    r#"</rdf:Description></rdf:RDF></x:xmpmeta>"#,
);

/// What no agent export may carry: GPS, camera and location data of the camera fixture.
const PRIVATE: [&str; 8] = ["GPS", "exif:", "tiff:", "photoshop:City", "Jerusalem", "LTS Camera", "Fixture X1", "31,46.5N"];

/// T-PH-13: the edit the agent keeps (over the fill, the background and the caption) and the
/// protected region inside it that is restored from the checkpoint: x, y, width, height.
const EDITED: [i32; 4] = [4, 4, 40, 30];
const PROTECTED: [i32; 4] = [12, 10, 16, 12];
/// T-PH-13: an odd-sized region away from the document's centre (32, 24), over the edge of the
/// fill and the white background, restored by a patch of the checkpoint: x, y, width, height.
const PATCH: [i32; 4] = [21, 9, 15, 11];
/// `#cc2200`, opaque: the edit's colour.
const RED: [u8; 4] = [0xcc, 0x22, 0x00, 0xff];

/// Two documents as an editing session leaves them (the F0 fixture). The first (64×48) has a type
/// layer (active) over a pixel layer filled inside a live selection, over its white background.
/// The second (32×24) has an empty layer. The first is active again.
fn fixture() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(Session::new(), Default::default());
    let [x, y, width, height] = FILLED;
    for (id, params) in [
        ("file.new", json!({"width": 64, "height": 48, "background": "white", "name": "first"})),
        ("layer.new.layer", json!({"name": "Filled"})),
        ("select.rect", json!({"x": x, "y": y, "width": width, "height": height})),
        ("edit.fill", json!({"color": "#336699"})),
        ("type.create", json!({"text": "LTS", "x": 30, "y": 40, "size": 12, "color": "#204060", "name": "Caption"})),
        ("file.new", json!({"width": 32, "height": 24, "background": "white", "name": "second"})),
        ("layer.new.layer", json!({"name": "Empty"})),
    ] {
        run(&mut app, id, params);
    }
    assert!(app.session.set_active(0), "the first document exists");
    app
}

/// Runs a command as the fixture's own driver (not the path under test).
fn run(app: &mut PhotocraftApp, id: &str, params: Value) -> Value {
    match app.run(id, params.clone()) {
        Ok(value) => value,
        Err(error) => panic!("fixture {id} {params}: {error}"),
    }
}

/// Adds a document a human could open but the view refuses: `layers` empty pixel layers. Returns
/// its DocId and its first layer's id.
fn add_document(app: &mut PhotocraftApp, name: &str, width: u32, height: u32, layers: usize) -> (String, u64) {
    let mut doc = Document::new(name, Size::new(width, height), ColorMode::Rgb, SampleType::U8);
    let format = doc.pixel_format();
    doc.layers.extend((0..layers).map(|n| Layer::raster(format!("Layer {n}"), format)));
    let first = doc.layers.first().map(|layer| layer.id.0).expect("at least one layer");
    let index = app.session.add_document(doc, None);
    (app.session.documents()[index].doc.id.0.to_string(), first)
}

fn doc_id(app: &PhotocraftApp, index: usize) -> String {
    app.session.documents()[index].doc.id.0.to_string()
}

fn rev(app: &PhotocraftApp, index: usize) -> String {
    format!("p:{}", app.session.documents()[index].revision)
}

/// The id of the top-level layer `name`.
fn layer_id(doc: &Document, name: &str) -> LayerId {
    doc.layers.iter().find(|layer| layer.name == name).map(|layer| layer.id).unwrap_or_else(|| panic!("no layer {name}"))
}

/// Preconditions that hold for document `index` as it is now: its DocId, revision, active layer
/// and history head (`length` entries, the newest `label`).
fn options_for(app: &PhotocraftApp, index: usize) -> Value {
    let state = &app.session.documents()[index];
    let entries = state.history.entries();
    let head = json!({"length": entries.len(), "label": entries.last()});
    json!({"document": {"id": doc_id(app, index)}, "rev": rev(app, index), "activeLayer": state.active_layer.map(|layer| layer.0), "historyHead": head})
}

/// The listed fields of an object (a missing field reads as `null`).
fn pick(value: &Value, keys: &[&str]) -> Value {
    Value::Object(keys.iter().map(|key| ((*key).to_string(), value[*key].clone())).collect())
}

/// `value` with `key` set to `field`.
fn with(mut value: Value, key: &str, field: Value) -> Value {
    value[key] = field;
    value
}

/// `value` without `key`.
fn without(mut value: Value, key: &str) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.remove(key);
    }
    value
}

/// Whether pixel (x, y) lies in the `[x, y, width, height]` region.
fn inside([x0, y0, width, height]: [i32; 4], x: i32, y: i32) -> bool {
    (x0..x0 + width).contains(&x) && (y0..y0 + height).contains(&y)
}

/// The bundled families (Inter, JetBrains Mono): what a page has before any font import.
fn bundled_families() -> Vec<String> {
    let mut engine = TextEngine::new();
    engine.fonts.families()
}

/// Nothing hidden, expired or busy; the bundled families; a validator that accepts everything; the
/// browser hook's runtime (what the one Photo WASM build decides).
fn env(families: &[String]) -> Env<'_> {
    Env { hidden: false, expired: false, busy: false, families, validate: |_, _, _| Ok(()), runtime: Runtime::Browser }
}

fn no_xmp() -> photocraft_io::ExportOptions {
    photocraft_io::ExportOptions { xmp: photocraft_io::XmpEmbed::None, ..Default::default() }
}

/// The successful result of a read.
fn ok<T>(result: Result<T, ViewError>, situation: &str) -> T {
    result.unwrap_or_else(|error| panic!("{situation}: {error:?}"))
}

/// The code of a read that must fail.
fn code<T: std::fmt::Debug>(result: Result<T, ViewError>, situation: &str) -> &'static str {
    match result {
        Ok(value) => panic!("{situation}: expected a typed failure, got {value:?}"),
        Err(error) => error.code,
    }
}

/// Decodes an 8-bit PNG, JPEG or WebP to straight RGBA (gray expanded, opaque images at alpha 255).
fn rgba(bytes: &[u8]) -> (u32, u32, Vec<[u8; 4]>) {
    let image = photocraft_codecs::decode(bytes).expect("a decodable image");
    assert_eq!(image.sample_type(), photocraft_codecs::SampleType::U8, "8-bit fixtures");
    let data = image.data();
    let pixels: Vec<[u8; 4]> = match image.layout() {
        ChannelLayout::Gray => data.iter().map(|&v| [v, v, v, 0xff]).collect(),
        ChannelLayout::GrayA => data.chunks_exact(2).map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        ChannelLayout::Rgb => data.chunks_exact(3).map(|p| [p[0], p[1], p[2], 0xff]).collect(),
        ChannelLayout::Rgba => data.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect(),
        other => panic!("unexpected layout {other:?}"),
    };
    (image.width(), image.height(), pixels)
}

/// Decodes an 8-bit gray PNG (masks and selections).
fn gray(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let image = photocraft_codecs::decode(bytes).expect("a decodable image");
    assert_eq!((image.layout(), image.sample_type()), (ChannelLayout::Gray, photocraft_codecs::SampleType::U8), "an 8-bit gray image");
    (image.width(), image.height(), image.data().to_vec())
}

/// The 64×48 gray image is 255 inside `region` and 0 elsewhere.
fn assert_gray(bytes: &[u8], region: [i32; 4], what: &str) {
    let (width, height, values) = gray(bytes);
    assert_eq!((width, height), (64, 48), "{what}: the document's size");
    for (i, value) in values.iter().enumerate() {
        let (x, y) = (i as i32 % 64, i as i32 / 64);
        let expected = if inside(region, x, y) { 255 } else { 0 };
        assert_eq!(*value, expected, "{what} at ({x}, {y})");
    }
}

/// The `[x, y, width, height]` region of a decoded image.
fn crop(image: &(u32, u32, Vec<[u8; 4]>), [x, y, w, h]: [i32; 4]) -> (u32, u32, Vec<[u8; 4]>) {
    let (width, pixels) = (image.0 as i32, &image.2);
    let region: Vec<[u8; 4]> = (y..y + h).flat_map(|row| (x..x + w).map(move |col| pixels[(row * width + col) as usize])).collect();
    (w as u32, h as u32, region)
}

/// The `[x, y, width, height]` box of the pixels whose alpha is not zero.
fn alpha_box(image: &(u32, u32, Vec<[u8; 4]>)) -> [i32; 4] {
    let width = image.0 as i32;
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for (i, pixel) in image.2.iter().enumerate() {
        if pixel[3] > 0 {
            let (x, y) = (i as i32 % width, i as i32 / width);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
    }
    assert!(x0 <= x1, "some pixel is visible");
    [x0, y0, x1 - x0 + 1, y1 - y0 + 1]
}

/// Layer `id` rendered alone (visible, Normal, unclipped, no selection) over a transparent canvas
/// of the document's size, through the exported composite: an oracle independent of the core.
fn alone(doc: &Document, id: LayerId) -> (u32, u32, Vec<[u8; 4]>) {
    let mut layer = doc.layer(id).expect("the layer exists").clone();
    layer.visible = true;
    layer.blend = BlendMode::Normal;
    layer.clipped = false;
    let mut solo = doc.clone();
    solo.layers = vec![layer];
    solo.selection = None;
    rgba(&photocraft_io::export(&solo, "png", &no_xmp()).expect("the solo composite").bytes)
}

/// A minimal EXIF block (TIFF structure, no `Exif\0\0` prefix) whose IFD0 holds one entry: the GPS
/// IFD pointer (0x8825, to an empty GPS IFD) when `gps`, else Orientation (0x0112).
fn exif(gps: bool, big_endian: bool) -> Vec<u8> {
    let u16b = |v: u16| if big_endian { v.to_be_bytes() } else { v.to_le_bytes() };
    let u32b = |v: u32| if big_endian { v.to_be_bytes() } else { v.to_le_bytes() };
    let mut bytes = if big_endian { b"MM\0*".to_vec() } else { b"II*\0".to_vec() };
    bytes.extend(u32b(8));
    bytes.extend(u16b(1));
    if gps {
        // GPSInfo, LONG, one value: the GPS IFD right after IFD0 (8 + 2 + 12 + 4 = 26).
        bytes.extend(u16b(0x8825));
        bytes.extend(u16b(4));
        bytes.extend(u32b(1));
        bytes.extend(u32b(26));
    } else {
        // Orientation, SHORT, one value (1), left-justified in the value field.
        bytes.extend(u16b(0x0112));
        bytes.extend(u16b(3));
        bytes.extend(u32b(1));
        bytes.extend(u16b(1));
        bytes.extend([0, 0]);
    }
    bytes.extend(u32b(0));
    if gps {
        bytes.extend(u16b(0));
        bytes.extend(u32b(0));
    }
    bytes
}

/// One document's identity and editing state.
#[derive(Debug, PartialEq)]
struct DocumentState {
    id: DocId,
    revision: u64,
    saved_revision: u64,
    path: Option<String>,
    dirty: bool,
    selection: Option<Rect>,
    active_layer: Option<LayerId>,
    selected_layers: Vec<LayerId>,
    history: Vec<String>,
    can_redo: bool,
    last_damage: Option<Rect>,
}

/// What a read or a refusal must leave alone (T-PH-01's list plus the session settings a typed
/// call toggles).
#[derive(Debug, PartialEq)]
struct Observed {
    documents: Vec<DocumentState>,
    active: Option<usize>,
    status: String,
    status_error: bool,
    last_command: String,
    journal: usize,
    frame: u64,
    gate_installed: bool,
    script_events: bool,
    background_jobs: bool,
    foreground: [f32; 4],
}

fn observe(app: &PhotocraftApp) -> Observed {
    let documents = app
        .session
        .documents()
        .iter()
        .map(|d| DocumentState {
            id: d.doc.id,
            revision: d.revision,
            saved_revision: d.saved_revision,
            path: d.path.clone(),
            dirty: d.is_dirty(),
            selection: d.doc.selection.as_ref().map(|s| s.content_bounds()),
            active_layer: d.active_layer,
            selected_layers: d.selected_layers.clone(),
            history: d.history.entries(),
            can_redo: d.history.can_redo(),
            last_damage: d.last_damage,
        })
        .collect();
    Observed {
        documents,
        active: app.session.active_index(),
        status: app.ui.status.clone(),
        status_error: app.ui.status_error,
        last_command: app.perf.last_command.clone(),
        journal: app.session.journal.len(),
        frame: app.frame,
        gate_installed: app.session.authorize.is_some(),
        script_events: app.session.prefs().script_events.enabled,
        background_jobs: app.background_jobs,
        foreground: app.session.tools.foreground,
    }
}

/// Everything observable plus the document snapshots themselves (`Arc` identity).
struct Snapshot {
    observed: Observed,
    ui: UiState,
    perf: Value,
    documents: Vec<Arc<Document>>,
}

fn snapshot(app: &PhotocraftApp) -> Snapshot {
    let documents = app.session.documents().iter().map(|d| Arc::clone(&d.doc)).collect();
    Snapshot { observed: observe(app), ui: app.ui.clone(), perf: json!(app.perf), documents }
}

fn assert_unchanged(app: &PhotocraftApp, before: &Snapshot, situation: &str) {
    assert_eq!(observe(app), before.observed, "{situation}: a document, the selection, the status, the journal or a session setting changed");
    assert_eq!(app.ui, before.ui, "{situation}: the UI state changed");
    assert_eq!(json!(app.perf), before.perf, "{situation}: the timings changed");
    let documents = app.session.documents();
    let same = documents.len() == before.documents.len() && documents.iter().zip(&before.documents).all(|(d, s)| Arc::ptr_eq(&d.doc, s));
    assert!(same, "{situation}: a document snapshot was replaced");
}

/// T-PH-01's discipline for every new read (INV-18): three calls on the same state return equal
/// results and leave everything observable unchanged.
fn assert_pure<T: PartialEq + std::fmt::Debug>(app: &PhotocraftApp, situation: &str, read: impl Fn(&PhotocraftApp) -> T) -> T {
    let before = snapshot(app);
    let first = read(app);
    for call in 2..=3 {
        assert_eq!(read(app), first, "{situation}: read {call} differs from the first");
    }
    assert_unchanged(app, &before, situation);
    first
}

/// A typed request whose run function only counts its calls and never dispatches.
fn decide(app: &mut PhotocraftApp, env: &Env<'_>, command: &str, params: &Value, options: &Value) -> (Value, usize) {
    let mut ran = 0;
    let envelope = lts_view_core::execute(app, env, command, params, options, &mut |_: &mut PhotocraftApp, _: &str, _: Value| -> Result<Value, String> {
        ran += 1;
        Err("the counting run function never dispatches".into())
    });
    (envelope, ran)
}

/// `code` is decided before the run function: zero calls, a typed `not_applied` envelope, nothing
/// changed.
fn assert_refused(app: &mut PhotocraftApp, env: &Env<'_>, command: &str, params: Value, options: Value, code: &str) -> Value {
    let case = format!("{command} {params} {options}");
    let before = snapshot(app);
    let (envelope, ran) = decide(app, env, command, &params, &options);
    assert_eq!(ran, 0, "{case}: reached the run function although `{code}` must be decided before dispatch: {envelope}");
    let expected = json!({"resultVersion": TYPED, "outcome": "not_applied", "code": code});
    assert_eq!(pick(&envelope, &["resultVersion", "outcome", "code"]), expected, "{case}");
    assert_unchanged(app, &before, &case);
    envelope
}

/// A typed request run with the real `PhotocraftApp::run`: the envelope and every dispatch.
fn typed(app: &mut PhotocraftApp, env: &Env<'_>, command: &str, params: Value, options: Value) -> (Value, Vec<(String, Value)>) {
    let mut calls = Vec::new();
    let envelope = lts_view_core::execute(app, env, command, &params, &options, &mut |app: &mut PhotocraftApp, id: &str, p: Value| {
        calls.push((id.to_string(), p.clone()));
        app.run(id, p)
    });
    (envelope, calls)
}

/// A `placeBytes` request placed as the glue places it: the engine's own
/// `file_cmds::place_bytes`, then the views follow the documents. Returns the envelope and the
/// number of placements.
fn place(app: &mut PhotocraftApp, env: &Env<'_>, bytes: Vec<u8>, params: &Value, options: &Value) -> (Value, usize) {
    let mut calls = 0;
    let envelope = lts_view_core::place_bytes(app, env, bytes, params, options, &mut |app: &mut PhotocraftApp, name: &str, bytes: Vec<u8>, p: Value| {
        calls += 1;
        let placed = photocraft_engine::file_cmds::place_bytes(&mut app.session, name, bytes, None, &p).map_err(|error| error.to_string());
        app.sync_views();
        placed
    });
    (envelope, calls)
}

/// `code` is decided before the place function: zero placements, a typed `not_applied` envelope,
/// nothing changed. Returns the envelope.
fn assert_place_refused(app: &mut PhotocraftApp, env: &Env<'_>, bytes: Vec<u8>, params: Value, options: Value, code: &str) -> Value {
    let case = format!("placeBytes {params} {options} ({} bytes)", bytes.len());
    let before = snapshot(app);
    let mut calls = 0;
    let envelope =
        lts_view_core::place_bytes(app, env, bytes, &params, &options, &mut |_: &mut PhotocraftApp, _: &str, _: Vec<u8>, _: Value| -> Result<Value, String> {
            calls += 1;
            Err("the counting place function never places".into())
        });
    assert_eq!(calls, 0, "{case}: reached the place function although `{code}` must be decided before dispatch: {envelope}");
    let expected = json!({"resultVersion": TYPED, "outcome": "not_applied", "code": code});
    assert_eq!(pick(&envelope, &["resultVersion", "outcome", "code"]), expected, "{case}");
    assert_unchanged(app, &before, &case);
    envelope
}

/// Whether an engine params doc names `key`: quoted (`"key"`) or as a bare word (`{key:`, `|key}`).
fn names(doc: &str, key: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    doc.match_indices(key).any(|(at, _)| !word(doc[..at].chars().next_back()) && !word(doc[at + key.len()..].chars().next()))
}

/// Document `index` as an agent export delivers it (PNG, no metadata), decoded.
fn delivered(app: &PhotocraftApp, index: usize) -> (u32, u32, Vec<[u8; 4]>) {
    let params = json!({"document": {"index": index}, "format": "png", "metadata": "none"});
    rgba(&ok(lts_view_core::export(&app.session, &params), "a lossless export").bytes)
}

/// Two pixels within one level per channel (an 8-bit round trip through the compositor).
fn close(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 1)
}

/// A 48×32 camera JPEG: EXIF with a GPS IFD (T-PH-07's block) and `CAMERA_XMP`.
fn camera_jpeg() -> Vec<u8> {
    let pixels: Vec<u8> = (0..48 * 32).flat_map(|i: u32| [(i % 48 * 5) as u8, (i / 48 * 7) as u8, 128]).collect();
    let image = photocraft_codecs::Image::from_u8(48, 32, ChannelLayout::Rgb, pixels).expect("an RGB image");
    let meta = photocraft_codecs::Metadata { exif: Some(exif(true, false)), xmp: Some(CAMERA_XMP.to_string()), ..Default::default() };
    photocraft_codecs::encode(&image.with_meta(meta), photocraft_codecs::Format::Jpeg, &photocraft_codecs::EncodeOptions::default()).expect("a camera JPEG")
}

/// A 16×16 CMYK JPEG of one ink mix, as Photo's own encoder writes CMYK (Adobe APP14, 4:4:4).
fn cmyk_jpeg() -> Vec<u8> {
    let image = photocraft_codecs::Image::from_u8(16, 16, ChannelLayout::Cmyk, [10_u8, 100, 200, 50].repeat(256)).expect("a CMYK image");
    photocraft_codecs::encode(&image, photocraft_codecs::Format::Jpeg, &photocraft_codecs::EncodeOptions::default()).expect("a CMYK JPEG")
}

/// `textCheck` with the engine `type.info` lays out with (the process-wide one), released at once.
fn shared_check(app: &PhotocraftApp, params: &Value) -> Result<Value, ViewError> {
    let mut engine = photocraft_text::shared().lock().unwrap_or_else(|error| error.into_inner());
    lts_view_core::text_check(&mut engine, &app.session, params)
}

/// Straight RGBA pixels encoded as an 8-bit PNG.
fn png((width, height, pixels): &(u32, u32, Vec<[u8; 4]>)) -> Vec<u8> {
    let data: Vec<u8> = pixels.iter().flatten().copied().collect();
    let image = photocraft_codecs::Image::from_u8(*width, *height, ChannelLayout::Rgba, data).expect("an RGBA image");
    photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &photocraft_codecs::EncodeOptions::default()).expect("a PNG")
}

/// A `width`×`height` PNG of one opaque colour.
fn solid_png(width: u32, height: u32) -> Vec<u8> {
    png(&(width, height, vec![[0x80, 0x40, 0x20, 0xff]; (width * height) as usize]))
}

/// A `width`×`height` PNG of xorshift noise, which deflate cannot shrink.
fn noise_png(width: u32, height: u32) -> Vec<u8> {
    let mut state = 0x9e37_79b9_u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        (state >> 24) as u8
    };
    let pixels = (0..width * height).map(|_| [next(), next(), next(), 0xff]).collect();
    png(&(width, height, pixels))
}

/// The chunk types of a PNG file, in order.
fn png_chunks(bytes: &[u8]) -> Vec<[u8; 4]> {
    let mut chunks = Vec::new();
    let mut at = 8;
    while at + 8 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[at..at + 4].try_into().expect("a chunk length")) as usize;
        chunks.push(bytes[at + 4..at + 8].try_into().expect("a chunk type"));
        at += 12 + length;
    }
    chunks
}

/// `placeBytes` parameters that restore `region` from bytes of exactly its size: 100 % on the
/// region's centre.
fn restore(name: &str, [x, y, width, height]: [i32; 4]) -> Value {
    let center = [f64::from(x) + f64::from(width) / 2.0, f64::from(y) + f64::from(height) / 2.0];
    json!({"name": name, "scale": 100, "center": center, "region": [x, y, width, height]})
}

/// Layer `id` of document 0 rendered alone and trimmed (`render` with `target: layer`): its
/// reported bounds and its pixels.
fn layer_render(app: &PhotocraftApp, id: LayerId) -> (Value, (u32, u32, Vec<[u8; 4]>)) {
    let params = json!({"document": {"index": 0}, "target": "layer", "layerId": id.0});
    let rendered = ok(lts_view_core::render(&app.session, &params), "a layer render");
    (rendered.info["bounds"].clone(), rgba(&rendered.bytes))
}

/// T-PH-02: a composite render of any document (active or not, by index or DocId) has the exported
/// composite's pixels, none of the document's EXIF or XMP, and changes nothing; samples are read
/// from those pixels; JPEG and WebP outputs are the same pure read.
#[test]
fn render_composite_is_pure() {
    let mut app = fixture();
    {
        let doc = Arc::make_mut(&mut app.session.active_mut().expect("the first document").doc);
        doc.metadata.exif = Some(Arc::new(exif(true, false)));
        doc.metadata.xmp = Some(XMP.to_string());
    }
    let (first, second) = (doc_id(&app, 0), doc_id(&app, 1));
    for (index, document) in [(0, json!({"index": 0})), (0, json!({"id": first})), (1, json!({"index": 1})), (1, json!({"id": second}))] {
        let params = json!({"document": document, "target": "composite"});
        let rendered = ok(assert_pure(&app, "composite render", |app| lts_view_core::render(&app.session, &params)), "composite render");
        let state = &app.session.documents()[index];
        let exported = photocraft_io::export(&state.doc, "png", &no_xmp()).expect("the exported composite").bytes;
        assert_eq!(rgba(&rendered.bytes), rgba(&exported), "{params}: the pixels of the exported composite");
        assert!(rendered.bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "{params}: a PNG");
        let (id, revision) = (state.doc.id.0.to_string(), format!("p:{}", state.revision));
        let (width, height) = (state.doc.size.width, state.doc.size.height);
        let expected = json!({"target": "composite", "mime": "image/png", "documentId": id, "rev": revision, "width": width, "height": height});
        assert_eq!(pick(&rendered.info, &["target", "mime", "documentId", "rev", "width", "height"]), expected, "{params}");
        let decoded = photocraft_codecs::decode(&rendered.bytes).expect("a decodable PNG");
        assert!(decoded.meta.exif.is_none() && decoded.meta.xmp.is_none(), "{params}: no EXIF or XMP of the document in a render");
    }
    let active = ok(lts_view_core::render(&app.session, &json!({"target": "composite"})), "the active document's composite");
    assert_eq!(active.info["documentId"], first, "no document key: the active one");
    let unknown = lts_view_core::render(&app.session, &json!({"document": {"index": 0}, "target": "thumbnail"}));
    assert_eq!(code(unknown, "an unknown target"), "unsupported_read");

    // Samples come from the composite's pixels, never from `document.pixel` (which runs a command).
    let composite = rgba(&ok(lts_view_core::render(&app.session, &json!({"document": {"index": 0}, "target": "composite"})), "composite").bytes);
    let points: [(u32, u32); 4] = [(10, 8), (0, 0), (63, 47), (31, 33)];
    let mut requested: Vec<Value> = points.iter().map(|(x, y)| json!({"x": x, "y": y})).collect();
    requested.push(json!({"x": 64, "y": 0}));
    let params = json!({"document": {"id": first}, "points": requested});
    let sampled = ok(assert_pure(&app, "sample", |app| lts_view_core::sample(&app.session, &params)), "samples");
    let samples = sampled["samples"].as_array().expect("samples");
    assert_eq!(samples.len(), points.len() + 1);
    for (sample, &(x, y)) in samples.iter().zip(&points) {
        let pixel = composite.2[(y * composite.0 + x) as usize];
        let hex = format!("#{:02x}{:02x}{:02x}", pixel[0], pixel[1], pixel[2]);
        assert_eq!(pick(sample, &["x", "y", "rgba", "hex"]), json!({"x": x, "y": y, "rgba": pixel, "hex": hex}), "sample at ({x}, {y})");
    }
    assert_eq!(samples[0]["hex"], "#336699", "inside the fill");
    assert_eq!(samples[1]["hex"], "#ffffff", "the white background");
    assert_eq!(pick(&samples[4], &["x", "y", "rgba", "hex"]), json!({"x": 64, "y": 0, "rgba": null, "hex": null}), "outside the document");
    let current = rev(&app, 0);
    assert_eq!(pick(&sampled, &["documentId", "rev"]), json!({"documentId": first, "rev": current}));

    for (format, mime, magic) in [("jpeg", "image/jpeg", &b"\xff\xd8\xff"[..]), ("webp", "image/webp", &b"RIFF"[..])] {
        let params = json!({"document": {"index": 0}, "target": "composite", "format": format, "quality": 90});
        let rendered = ok(assert_pure(&app, format, |app| lts_view_core::render(&app.session, &params)), format);
        assert_eq!(rendered.info["mime"], mime);
        assert!(rendered.bytes.starts_with(magic), "{format}: {mime}");
        let decoded = photocraft_codecs::decode(&rendered.bytes).expect("a decodable image");
        assert_eq!((decoded.width(), decoded.height()), (64, 48), "{format}");
        assert!(decoded.meta.exif.is_none() && decoded.meta.xmp.is_none(), "{format}: no EXIF or XMP of the document");
    }
}

/// T-PH-03: a layer renders alone (visible, Normal, unclipped), trimmed to its visible pixels, and
/// `bounds` places the image in document pixels. The trim is the box of the pixels whose alpha is
/// not zero in the delivered image (Layer › Export Layer trims at the compositor's float alpha, so
/// its document is never smaller).
#[test]
fn layer_render_reports_trimmed_bounds() {
    let app = fixture();
    let doc = Arc::clone(&app.session.documents()[0].doc);
    let first = doc_id(&app, 0);
    let filled = layer_id(&doc, "Filled");
    let params = json!({"document": {"id": first}, "target": "layer", "layerId": filled.0});
    let rendered = ok(assert_pure(&app, "pixel layer render", |app| lts_view_core::render(&app.session, &params)), "pixel layer render");
    let expected = json!({"target": "layer", "mime": "image/png", "documentId": first, "layerId": filled.0, "bounds": FILLED, "width": 20, "height": 12});
    assert_eq!(pick(&rendered.info, &["target", "mime", "documentId", "layerId", "bounds", "width", "height"]), expected);
    let (width, height, pixels) = rgba(&rendered.bytes);
    assert_eq!((width, height), (20, 12));
    assert!(pixels.iter().all(|pixel| *pixel == FILL), "the trimmed pixel layer holds only its fill");

    let caption = layer_id(&doc, "Caption");
    let params = json!({"document": {"index": 0}, "target": "layer", "layerId": caption.0});
    let rendered = ok(assert_pure(&app, "type layer render", |app| lts_view_core::render(&app.session, &params)), "type layer render");
    let solo = alone(&doc, caption);
    let bounds = alpha_box(&solo);
    assert_eq!(rendered.info["bounds"], json!(bounds), "the box of the caption's visible pixels");
    assert_eq!(rgba(&rendered.bytes), crop(&solo, bounds), "the caption's pixels, unchanged");
    let [x, y, w, h] = bounds;
    assert!(x >= 28 && y < 40 && x + w <= 64 && y + h <= 48, "document pixels near the anchor (30, 40): {bounds:?}");
    let exported = photocraft_engine::layer_menu_cmds::layer_document(&doc, caption).expect("Layer › Export Layer's document");
    assert!((w as u32) <= exported.size.width && (h as u32) <= exported.size.height, "never larger than Export Layer's trim: {bounds:?}");

    let empty = layer_id(&app.session.documents()[1].doc, "Empty");
    for (document, layer, situation) in [
        (json!({"index": 1}), empty.0, "an empty layer"),
        (json!({"index": 0}), empty.0, "a layer of another document"),
        (json!({"index": 0}), 999_999_999, "an unknown layer"),
    ] {
        let params = json!({"document": document, "target": "layer", "layerId": layer});
        assert_eq!(code(assert_pure(&app, situation, |app| lts_view_core::render(&app.session, &params)), situation), "read_failed", "{params}");
    }
}

/// T-PH-04: renders are bounded before allocation (INV-28). A document over the bound on a side
/// (4097 pixels: over R1's 4096 and R0's 2048) or with more than 64 layers is refused from its header
/// for every target and for samples, even one whose composite would need about 1.4 TB; a 2048-pixel
/// side renders (R1's 4096: `r1_documents_crops_and_resizes_reach_4096_and_stay_observable`). Reads
/// of no pixels still work.
#[test]
fn render_budget_refused_before_allocation() {
    let mut app = fixture();
    let (wide, wide_layer) = add_document(&mut app, "wide", 4097, 1, 1);
    let (tall, tall_layer) = add_document(&mut app, "tall", 1, 4097, 1);
    let (huge, _) = add_document(&mut app, "huge", 300_000, 300_000, 1);
    let (stacked, _) = add_document(&mut app, "stacked", 64, 48, 65);
    let (edge, _) = add_document(&mut app, "edge", 2048, 1, 1);
    let (full, _) = add_document(&mut app, "full stack", 64, 48, 64);
    assert!(app.session.set_active(0), "the first document exists");
    for (id, layer) in [(&wide, wide_layer), (&tall, tall_layer)] {
        for target in [
            json!({"target": "composite"}),
            json!({"target": "composite", "format": "jpeg", "quality": 90}),
            json!({"target": "layer", "layerId": layer}),
            json!({"target": "mask", "layerId": layer}),
            json!({"target": "selection"}),
            json!({"target": "history", "historyIndex": 0}),
        ] {
            let params = with(target, "document", json!({"id": id}));
            let refused = assert_pure(&app, "an oversized render", |app| lts_view_core::render(&app.session, &params));
            assert_eq!(code(refused, "an oversized render"), "budget_exceeded", "{params}");
        }
    }
    for id in [&wide, &tall, &huge] {
        let params = json!({"document": {"id": id}, "points": [{"x": 0, "y": 0}]});
        let refused = assert_pure(&app, "an oversized sample", |app| lts_view_core::sample(&app.session, &params));
        assert_eq!(code(refused, "an oversized sample"), "budget_exceeded", "{params}");
    }
    for id in [&huge, &stacked] {
        let params = json!({"document": {"id": id}, "target": "composite"});
        let refused = assert_pure(&app, "an oversized composite", |app| lts_view_core::render(&app.session, &params));
        assert_eq!(code(refused, "an oversized composite"), "budget_exceeded", "{params}");
    }
    for (id, width, height) in [(&edge, 2048, 1), (&full, 64, 48)] {
        let params = json!({"document": {"id": id}, "target": "composite"});
        let rendered = ok(lts_view_core::render(&app.session, &params), "a composite at the bound");
        assert_eq!(pick(&rendered.info, &["width", "height"]), json!({"width": width, "height": height}), "{params}");
    }
    let described = ok(lts_view_core::describe(&app.session, &json!({"document": {"id": huge}})), "describe reads no pixels");
    assert_eq!(pick(&described["document"], &["width", "height"]), json!({"width": 300_000, "height": 300_000}));
    let metadata = ok(lts_view_core::metadata(&app.session, &json!({"document": {"id": huge}})), "metadata reads no pixels");
    assert_eq!(metadata["documentId"], huge);
}

/// T-PH-05: a layer mask and the selection render as 8-bit gray PNGs of the document's size (0
/// hidden or unselected, 255 revealed or selected), with the mask's properties; describe reports
/// the same properties. A missing mask or selection is a typed read failure.
#[test]
fn mask_render_gray_png_with_properties() {
    let mut app = fixture();
    let filled = layer_id(&app.session.documents()[0].doc, "Filled");
    let [x, y, width, height] = MASKED;
    run(&mut app, "layer.select", json!({"layer": filled.0}));
    run(&mut app, "select.rect", json!({"x": x, "y": y, "width": width, "height": height}));
    run(&mut app, "layer.layerMask.revealSelection", json!({}));
    let first = doc_id(&app, 0);
    let params = json!({"document": {"id": first}, "target": "mask", "layerId": filled.0});
    let mask = ok(assert_pure(&app, "mask render", |app| lts_view_core::render(&app.session, &params)), "mask render");
    let properties = json!({"enabled": true, "linked": true, "density": 1.0, "feather": 0.0, "bounds": MASKED});
    let expected = json!({"target": "mask", "mime": "image/png", "layerId": filled.0, "width": 64, "height": 48, "mask": properties});
    assert_eq!(pick(&mask.info, &["target", "mime", "layerId", "width", "height", "mask"]), expected);
    assert_gray(&mask.bytes, MASKED, "mask");

    let params = json!({"document": {"id": first}, "target": "selection"});
    let selection = ok(assert_pure(&app, "selection render", |app| lts_view_core::render(&app.session, &params)), "selection render");
    let expected = json!({"target": "selection", "mime": "image/png", "width": 64, "height": 48, "bounds": MASKED});
    assert_eq!(pick(&selection.info, &["target", "mime", "width", "height", "bounds"]), expected);
    assert_gray(&selection.bytes, MASKED, "selection");

    let params = json!({"document": {"id": first}, "layerId": filled.0});
    let described = ok(assert_pure(&app, "describe a masked layer", |app| lts_view_core::describe(&app.session, &params)), "describe");
    assert_eq!(described["layer"]["mask"], properties, "describe reports the same mask properties");

    let caption = layer_id(&app.session.documents()[0].doc, "Caption");
    for (params, situation) in [
        (json!({"document": {"id": first}, "target": "mask", "layerId": caption.0}), "a layer without a mask"),
        (json!({"document": {"index": 1}, "target": "selection"}), "a document without a selection"),
    ] {
        assert_eq!(code(assert_pure(&app, situation, |app| lts_view_core::render(&app.session, &params)), situation), "read_failed");
    }
}

/// T-PH-06: any history entry renders as the composite of that state, the current entry as the
/// composite itself, and nothing in the history moves (no undo, no redo, no truncation).
#[test]
fn history_state_render() {
    let app = fixture();
    let entries = app.session.documents()[0].history.entries();
    assert_eq!(entries.len(), 5, "{entries:?}");
    let first = doc_id(&app, 0);
    let state = |index: usize| -> Rendered {
        let params = json!({"document": {"id": first}, "target": "history", "historyIndex": index});
        ok(assert_pure(&app, "history render", |app| lts_view_core::render(&app.session, &params)), "history render")
    };
    let opened = state(0);
    let expected = json!({"target": "history", "historyIndex": 0, "label": entries[0], "current": false, "width": 64, "height": 48});
    assert_eq!(pick(&opened.info, &["target", "historyIndex", "label", "current", "width", "height"]), expected);
    assert!(rgba(&opened.bytes).2.iter().all(|pixel| *pixel == WHITE), "the new document is white");

    let filled = state(3);
    assert_eq!(pick(&filled.info, &["historyIndex", "label", "current"]), json!({"historyIndex": 3, "label": "Fill", "current": false}));
    let (width, _, pixels) = rgba(&filled.bytes);
    for (i, pixel) in pixels.iter().enumerate() {
        let (x, y) = ((i as u32 % width) as i32, (i as u32 / width) as i32);
        let expected = if inside(FILLED, x, y) { FILL } else { WHITE };
        assert_eq!(*pixel, expected, "after Fill, before the caption, at ({x}, {y})");
    }

    let current = state(4);
    assert_eq!(pick(&current.info, &["historyIndex", "label", "current"]), json!({"historyIndex": 4, "label": entries[4], "current": true}));
    let composite = ok(lts_view_core::render(&app.session, &json!({"document": {"id": first}, "target": "composite"})), "composite");
    assert_eq!(rgba(&current.bytes), rgba(&composite.bytes), "the current entry is the composite");
    assert_ne!(rgba(&current.bytes), rgba(&filled.bytes), "the caption exists only after its step");

    let params = json!({"document": {"id": first}, "target": "history", "historyIndex": 5});
    let past = assert_pure(&app, "past the current entry", |app| lts_view_core::render(&app.session, &params));
    assert_eq!(code(past, "past the current entry"), "read_failed");
    let params = json!({"document": {"index": 1}, "target": "history", "historyIndex": 0});
    let other = ok(assert_pure(&app, "an inactive document's history", |app| lts_view_core::render(&app.session, &params)), "inactive history");
    let (width, height, pixels) = rgba(&other.bytes);
    assert_eq!((width, height), (32, 24));
    assert!(pixels.iter().all(|pixel| *pixel == WHITE), "the second document as opened");
}

/// T-PH-07: the metadata read reports the ICC profile, the XMP File Info fields and EXIF presence
/// with GPS flagged, reading fields only (never `file.fileInfo` or `edit.assignProfile`, which write
/// when given parameters; INV-14): no packet or profile appears, and present ones stay byte-identical.
#[test]
fn metadata_read_never_writes_xmp_or_profile() {
    let mut app = fixture();
    let second = doc_id(&app, 1);
    let params = json!({"document": {"index": 1}});
    let bare = ok(assert_pure(&app, "metadata of a bare document", |app| lts_view_core::metadata(&app.session, &params)), "bare metadata");
    assert_eq!(bare["documentId"], second);
    for key in ["icc", "xmp", "exif"] {
        assert_eq!(bare[key]["present"], false, "{key}: {bare}");
    }
    let doc = &app.session.documents()[1].doc;
    assert!(doc.icc_profile.is_none() && doc.metadata.xmp.is_none() && doc.metadata.exif.is_none(), "reading created nothing");

    run(&mut app, "edit.assignProfile", json!({"profile": "display-p3"}));
    {
        let doc = Arc::make_mut(&mut app.session.active_mut().expect("the first document").doc);
        doc.metadata.xmp = Some(XMP.to_string());
        doc.metadata.exif = Some(Arc::new(exif(true, false)));
    }
    let first = doc_id(&app, 0);
    let icc = app.session.documents()[0].doc.icc_profile.clone().expect("Display P3 assigned");
    let params = json!({"document": {"id": first}});
    let rich = ok(assert_pure(&app, "metadata", |app| lts_view_core::metadata(&app.session, &params)), "metadata");
    assert_eq!(rich["documentId"], first);
    let profile = json!({"present": true, "bytes": icc.len(), "description": "Display P3", "colorSpace": "Rgb"});
    assert_eq!(pick(&rich["icc"], &["present", "bytes", "description", "colorSpace"]), profile);
    assert_eq!(pick(&rich["xmp"], &["present", "bytes", "gps"]), json!({"present": true, "bytes": XMP.len(), "gps": true}));
    assert_eq!(rich["xmp"]["fields"], photocraft_engine::file_cmds::read_file_info(Some(XMP)), "File Info fields, as the engine reads them");
    assert_eq!(rich["xmp"]["fields"]["title"], "Fixture title");
    assert_eq!(pick(&rich["exif"], &["present", "bytes", "gps"]), json!({"present": true, "bytes": exif(true, false).len(), "gps": true}));

    for (block, gps) in [(exif(false, false), false), (exif(true, true), true), (exif(false, true), false)] {
        Arc::make_mut(&mut app.session.active_mut().expect("the first document").doc).metadata.exif = Some(Arc::new(block.clone()));
        let read = ok(assert_pure(&app, "EXIF", |app| lts_view_core::metadata(&app.session, &params)), "EXIF");
        assert_eq!(pick(&read["exif"], &["present", "bytes", "gps"]), json!({"present": true, "bytes": block.len(), "gps": gps}), "{block:02x?}");
    }
    let malformed = b"II*\0\xff\xff\xff\x7f".to_vec();
    Arc::make_mut(&mut app.session.active_mut().expect("the first document").doc).metadata.exif = Some(Arc::new(malformed));
    let read = ok(assert_pure(&app, "malformed EXIF", |app| lts_view_core::metadata(&app.session, &params)), "malformed EXIF is still read");
    assert_eq!(read["exif"]["present"], true);
}

/// T-PH-08: the font list and its digest, and a glyph probe that counts tofu (.notdef) through the
/// type engine's own layout, falling back as type layers do; nothing is registered or removed.
#[test]
fn font_probe_counts_tofu() {
    for (input, digest) in [
        ("", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        ("abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        ("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq", "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"),
    ] {
        assert_eq!(lts_view_core::sha256_hex(input.as_bytes()), digest, "sha256 of {input:?}");
    }
    let million = "a".repeat(1_000_000);
    assert_eq!(lts_view_core::sha256_hex(million.as_bytes()), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");

    let mut engine = TextEngine::new();
    let families = engine.fonts.families();
    assert!(families.iter().any(|family| family == "Inter"), "{families:?}");
    let digest = lts_view_core::fontset_digest(&families);
    let listing = serde_json::to_string(&families).expect("a JSON list");
    assert_eq!(digest, format!("sha256:{}", lts_view_core::sha256_hex(listing.as_bytes())), "the digest of the JSON family list");
    let listed = ok(lts_view_core::fonts(&mut engine, &json!({})), "the font list");
    assert_eq!(pick(&listed, &["families", "fontsetDigest"]), json!({"families": families, "fontsetDigest": digest}));
    assert!(listed["probe"].is_null(), "no probe without a probe request");

    for (text, family, glyphs, tofu, missing) in [
        ("Hello", "Inter", 5, 0, false),
        ("שלום", "Inter", 4, 4, false),
        ("Hi שלום", "Inter", 7, 4, false),
        ("Hello", "LTS Missing Family 20261014", 5, 0, true),
    ] {
        let probed = ok(lts_view_core::fonts(&mut engine, &json!({"probe": {"text": text, "family": family, "sizePt": 24}})), text);
        let expected = json!({"glyphs": glyphs, "tofu": tofu, "missingFont": missing});
        assert_eq!(pick(&probed["probe"], &["glyphs", "tofu", "missingFont"]), expected, "{text} in {family}");
        assert_eq!(probed["fontsetDigest"], digest, "probing registers nothing");
    }
    assert_eq!(engine.fonts.families(), families, "nothing registered or removed");
    let oversized = lts_view_core::fonts(&mut engine, &json!({"probe": {"text": "x".repeat(4097), "family": "Inter", "sizePt": 12}}));
    assert_eq!(code(oversized, "a probe over 4096 bytes"), "budget_exceeded");
}

/// T-PH-09: describe names every document by a DocId that survives activation, reordering and
/// edits (indexes and revisions move, ids never do), and the watermark carries the same ids. A
/// reference whose id and index disagree, or that names nothing, is a typed read failure.
#[test]
fn describe_doc_id_stable_across_activation() {
    let mut app = fixture();
    let families = bundled_families();
    let (first, second) = (doc_id(&app, 0), doc_id(&app, 1));
    let named = [(first.clone(), "first"), (second.clone(), "second")];
    let all = ok(assert_pure(&app, "describe", |app| lts_view_core::describe(&app.session, &json!({}))), "describe");
    let rows = all["documents"].as_array().expect("documents");
    assert_eq!(rows.len(), 2);
    for (index, (row, d)) in rows.iter().zip(app.session.documents()).enumerate() {
        let (id, revision) = (d.doc.id.0.to_string(), format!("p:{}", d.revision));
        let (width, height) = (d.doc.size.width, d.doc.size.height);
        let expected = json!({"index": index, "id": id, "name": d.doc.name, "width": width, "height": height, "rev": revision, "dirty": d.is_dirty(), "active": index == 0});
        assert_eq!(pick(row, &["index", "id", "name", "width", "height", "rev", "dirty", "active"]), expected, "document {index}");
    }
    let active = json!({"index": 0, "id": first, "name": "first", "active": true});
    assert_eq!(pick(&all["document"], &["index", "id", "name", "active"]), active, "no document key: the active one");

    for active in [1_usize, 0, 1] {
        run(&mut app, "document.activate", json!({"document": active}));
        for (index, (id, name)) in named.iter().enumerate() {
            let params = json!({"document": {"id": id}});
            let one = ok(assert_pure(&app, "describe by DocId", |app| lts_view_core::describe(&app.session, &params)), "describe by DocId");
            let expected = json!({"index": index, "id": id, "name": name, "active": index == active});
            assert_eq!(pick(&one["document"], &["index", "id", "name", "active"]), expected, "{name} while document {active} is active");
        }
        let current = ok(lts_view_core::describe(&app.session, &json!({})), "the active document");
        assert_eq!(current["document"]["id"], named[active].0);
        let watermark = assert_pure(&app, "watermark", |app| lts_view_core::watermark(&app.session, false, &families));
        let digest = lts_view_core::fontset_digest(&families);
        let expected = json!({"active": active, "activeId": named[active].0, "busy": false, "fontsetDigest": digest});
        assert_eq!(pick(&watermark, &["active", "activeId", "busy", "fontsetDigest"]), expected);
        let keys = ["index", "id", "rev", "dirty"];
        let described: Vec<Value> = current["documents"].as_array().expect("documents").iter().map(|row| pick(row, &keys)).collect();
        let marked: Vec<Value> = watermark["documents"].as_array().expect("watermark documents").iter().map(|row| pick(row, &keys)).collect();
        assert_eq!(marked, described, "the watermark names documents as describe does");
    }

    // The second document is active; moving the first behind it changes indexes only.
    run(&mut app, "document.move", json!({"document": 0, "to": 1}));
    let moved = ok(lts_view_core::describe(&app.session, &json!({"document": {"id": first}})), "describe after a move");
    assert_eq!(pick(&moved["document"], &["index", "id", "name"]), json!({"index": 1, "id": first, "name": "first"}));
    let order: Vec<Value> = moved["documents"].as_array().expect("documents").iter().map(|row| row["id"].clone()).collect();
    assert_eq!(order, [json!(second), json!(first)]);

    // An edit moves the revision token, never the id.
    let before = moved["document"]["rev"].clone();
    run(&mut app, "document.activate", json!({"document": 1}));
    run(&mut app, "layer.new.layer", json!({"name": "Edit"}));
    let edited = ok(lts_view_core::describe(&app.session, &json!({"document": {"id": first}, "view": "layers"})), "describe after an edit");
    assert_eq!(edited["document"]["id"], first);
    assert_ne!(edited["document"]["rev"], before, "an edit moves the revision token");
    let d = &app.session.documents()[1];
    let entries = d.history.entries();
    let history = json!({"entries": entries, "currentIndex": entries.len() - 1, "canUndo": d.history.can_undo(), "canRedo": d.history.can_redo()});
    assert_eq!(pick(&edited["document"]["history"], &["entries", "currentIndex", "canUndo", "canRedo"]), history);
    let layers: Vec<Value> = edited["document"]["layers"].as_array().expect("layers").iter().map(|layer| pick(layer, &["id", "name", "kind"])).collect();
    let expected: Vec<Value> =
        d.doc.layers.iter().rev().map(|layer| json!({"id": layer.id.0, "name": layer.name, "kind": layer.content.kind_name()})).collect();
    assert_eq!(layers, expected, "layers top to bottom");
    let caption = layer_id(&d.doc, "Caption");
    let detail = ok(lts_view_core::describe(&app.session, &json!({"document": {"id": first}, "layerId": caption.0})), "a layer's detail");
    assert_eq!(pick(&detail["layer"], &["id", "name", "kind"]), json!({"id": caption.0, "name": "Caption", "kind": "Type"}));
    assert_eq!(detail["layer"]["text"]["text"], "LTS");

    for document in [json!({"id": first, "index": 0}), json!({"id": "999999999999"}), json!({"id": 5}), json!({"index": 9}), json!({"name": "first"})] {
        let params = json!({"document": document});
        assert_eq!(code(lts_view_core::describe(&app.session, &params), "a bad document reference"), "read_failed", "{params}");
    }
}

/// T-PH-10: every typed `not_applied` code is decided before the run function is called, with
/// nothing changed (INV-4, INV-34, INV-35, INV-58); once the run function was called the outcome is
/// never `not_applied` (INV-41).
#[test]
fn typed_not_applied_is_decided_before_run() {
    let mut app = fixture();
    let families = bundled_families();
    let ok_env = env(&families);
    let add = json!({"name": "Agent layer"});
    let options = options_for(&app, 0);
    let (first, second, current) = (doc_id(&app, 0), doc_id(&app, 1), rev(&app, 0));
    let filled = layer_id(&app.session.documents()[0].doc, "Filled").0;
    let length = app.session.documents()[0].history.entries().len();
    let stale = with(options.clone(), "rev", json!("p:999999"));

    // The page, the queue and the human come first, in this order.
    let gates = [
        (Env { hidden: true, ..env(&families) }, "page_hidden"),
        (Env { expired: true, ..env(&families) }, "queue_expired"),
        (Env { busy: true, ..env(&families) }, "editor_busy"),
        (Env { hidden: true, expired: true, busy: true, ..env(&families) }, "page_hidden"),
        (Env { expired: true, busy: true, ..env(&families) }, "queue_expired"),
    ];
    for (gate, code) in &gates {
        assert_refused(&mut app, gate, "layer.new.layer", add.clone(), options.clone(), code);
        assert_refused(&mut app, gate, "layer.new.layer", add.clone(), stale.clone(), code);
    }

    // The target: the document the preconditions name must exist and be the active one.
    for (document, code) in [
        (json!({"id": "999999999999"}), "target_not_found"),
        (json!({"index": 7}), "target_not_found"),
        (json!({"id": second}), "target_not_active"),
        (json!({"index": 1}), "target_not_active"),
    ] {
        assert_refused(&mut app, &ok_env, "layer.new.layer", add.clone(), with(options.clone(), "document", document), code);
    }
    let mut empty = PhotocraftApp::new(Session::new(), Default::default());
    assert_refused(&mut empty, &ok_env, "layer.new.layer", add.clone(), json!({}), "target_not_found");

    // Preconditions, checked atomically with the call.
    let refused = assert_refused(&mut app, &ok_env, "layer.new.layer", add.clone(), stale.clone(), "revision_conflict");
    let reported = json!({"documentId": first, "revisionBefore": current, "revisionAfter": current});
    assert_eq!(pick(&refused, &["documentId", "revisionBefore", "revisionAfter"]), reported, "a refusal reports where the document stands");
    assert_refused(&mut app, &ok_env, "layer.new.layer", add.clone(), with(options.clone(), "activeLayer", json!(filled)), "layer_not_active");
    for head in [json!({"length": length, "label": "Fill"}), json!({"length": length - 1, "label": "New Type Layer"})] {
        assert_refused(&mut app, &ok_env, "layer.new.layer", add.clone(), with(options.clone(), "historyHead", head), "history_head_conflict");
    }

    // Undo and redo are global: an agent names the head it expects (INV-34).
    for command in ["edit.undo", "edit.redo"] {
        assert_refused(&mut app, &ok_env, command, json!({}), without(options.clone(), "historyHead"), "invalid_params");
        let wrong = with(options.clone(), "historyHead", json!({"length": length, "label": "Fill"}));
        assert_refused(&mut app, &ok_env, command, json!({}), wrong, "history_head_conflict");
    }
    // The right head passes the guard; with nothing to redo, the engine's own precondition refuses.
    assert_refused(&mut app, &ok_env, "edit.redo", json!({}), options.clone(), "engine_precheck_failed");

    // Fonts: an explicit family must be registered, and the font set must be the one probed (INV-35).
    let missing = json!({"text": "x", "x": 2, "y": 20, "size": 12, "color": "#000000", "font": "LTS Missing Family 20261014"});
    assert_refused(&mut app, &ok_env, "type.create", missing, options.clone(), "font_not_loaded");
    let text = json!({"text": "x", "x": 2, "y": 20, "size": 12, "color": "#000000", "font": "Inter"});
    let changed = with(options.clone(), "fontsetDigest", json!(format!("sha256:{}", "0".repeat(64))));
    assert_refused(&mut app, &ok_env, "type.create", text, changed, "font_not_loaded");

    // Targets inside the parameters: the layer exists, the crop lies inside the document, and the
    // document to activate is the one the preconditions name.
    assert_refused(&mut app, &ok_env, "layer.select", json!({"layer": 999_999_999}), options.clone(), "target_not_found");
    assert_refused(&mut app, &ok_env, "image.crop", json!({"x": 0, "y": 0, "width": 65, "height": 48}), options.clone(), "invalid_params");
    assert_refused(&mut app, &ok_env, "document.activate", json!({"document": 1}), json!({"document": {"id": first}}), "target_not_found");

    // The engine's own precondition (fill needs a pixel layer; the caption is active), then the
    // other owner's validator (`agent_context::validate`).
    let fill = json!({"color": "#ff0000", "target": "pixels"});
    assert_refused(&mut app, &ok_env, "edit.fill", fill, options.clone(), "engine_precheck_failed");
    let validator = Env { validate: |_, _, _| Err("refused by the bridge validator".into()), ..env(&families) };
    assert_refused(&mut app, &validator, "layer.new.layer", add.clone(), options.clone(), "engine_precheck_failed");

    // A human's Channels panel target or Quick Mask never retargets an agent's filter.
    run(&mut app, "layer.select", json!({"layer": filled}));
    let blur = json!({"radius": 2});
    app.session.active_mut().expect("the first document").channel_view.target = ChannelTarget::Color(0);
    let fresh = options_for(&app, 0);
    assert_refused(&mut app, &ok_env, "filter.blur.gaussianBlur", blur.clone(), fresh, "engine_precheck_failed");
    app.session.active_mut().expect("the first document").channel_view.target = ChannelTarget::Composite;
    run(&mut app, "select.editInQuickMaskMode", json!({"on": true}));
    let fresh = options_for(&app, 0);
    assert_refused(&mut app, &ok_env, "filter.blur.gaussianBlur", blur, fresh, "engine_precheck_failed");
    run(&mut app, "select.editInQuickMaskMode", json!({"on": false}));

    // Caps: eight documents, sixty-four layers.
    let mut full = fixture();
    for _ in 0..6 {
        run(&mut full, "file.new", json!({"width": 8, "height": 8}));
    }
    assert_eq!(full.session.documents().len(), 8);
    assert_refused(&mut full, &ok_env, "file.new", json!({"width": 8, "height": 8}), json!({}), "document_limit");
    let mut deep = fixture();
    while deep.session.documents()[0].doc.layer_count() < 64 {
        run(&mut deep, "layer.new.layer", json!({}));
    }
    let crowded = options_for(&deep, 0);
    assert_refused(&mut deep, &ok_env, "layer.new.layer", add.clone(), crowded.clone(), "layer_limit");
    let caption = json!({"text": "x", "x": 2, "y": 20, "size": 12, "color": "#000000"});
    assert_refused(&mut deep, &ok_env, "type.create", caption, crowded, "layer_limit");

    // After the run function was reached a failure is `unknown`, never `not_applied` (INV-41).
    let fresh = options_for(&app, 0);
    let (envelope, ran) = decide(&mut app, &ok_env, "layer.new.layer", &add, &fresh);
    assert_eq!(ran, 1, "{envelope}");
    let expected = json!({"resultVersion": TYPED, "outcome": "unknown", "code": "engine_error_after_dispatch"});
    assert_eq!(pick(&envelope, &["resultVersion", "outcome", "code"]), expected);
    assert!(app.session.authorize.is_none(), "the gate is removed after a failed call");
}

/// T-PH-11: an applied typed call reports the target's revision before and after, the engine's own
/// result and the damage; it runs inline under the typed gate with script events off and restores
/// all three; a busy editor refuses atomically (INV-58). Activation, a new document, text with the
/// probed font set and a guarded undo are applied the same way.
#[test]
fn typed_applied_reports_revision_before_after_and_busy_refuses() {
    let mut app = fixture();
    let families = bundled_families();
    let ok_env = env(&families);
    // A desktop-like session: script events and background jobs on.
    app.session.edit_prefs(|prefs| prefs.script_events.enabled = true);
    app.background_jobs = true;
    let (first, before) = (doc_id(&app, 0), rev(&app, 0));
    let options = with(options_for(&app, 0), "deadlineMs", json!(15_000));
    let params = json!({"name": "Agent layer"});
    let mut calls = Vec::new();
    let mut returned = None;
    let envelope = lts_view_core::execute(&mut app, &ok_env, "layer.new.layer", &params, &options, &mut |app: &mut PhotocraftApp, id: &str, p: Value| {
        let gate = app.session.authorize.expect("the typed step gate is installed for the call");
        assert!(gate(id, &p).is_ok(), "the typed command passes its own gate");
        assert!(gate("file.close", &json!({})).is_err(), "nested ambient steps are refused");
        assert!(!app.session.prefs().script_events.enabled, "script events are suppressed during the call");
        assert!(!app.background_jobs, "the typed call runs inline, never as a background job");
        calls.push((id.to_string(), p.clone()));
        let result = app.run(id, p);
        returned = Some(result.clone());
        result
    });
    assert_eq!(calls, [("layer.new.layer".to_string(), params.clone())], "dispatched once, unchanged");
    let state = &app.session.documents()[0];
    let after = format!("p:{}", state.revision);
    let expected = json!({"resultVersion": TYPED, "outcome": "applied", "documentId": first, "revisionBefore": before, "revisionAfter": after});
    assert_eq!(pick(&envelope, &["resultVersion", "outcome", "documentId", "revisionBefore", "revisionAfter"]), expected);
    assert_ne!(before, after, "the edit moved the revision");
    assert!(envelope["code"].is_null(), "{envelope}");
    assert_eq!(envelope["result"], returned.expect("dispatched").expect("the layer was created"), "the engine's own result");
    let damage = state.last_damage.map(|r| json!([r.x0, r.y0, r.width(), r.height()])).unwrap_or(Value::Null);
    assert_eq!(envelope["lastDamage"], damage);
    assert_eq!(state.doc.layers.last().map(|layer| layer.name.as_str()), Some("Agent layer"));
    assert!(app.session.authorize.is_none(), "the gate is removed after the call");
    assert!(app.session.prefs().script_events.enabled, "script events are restored");
    assert!(app.background_jobs, "background jobs are restored");

    // Busy: refused in the same call, nothing dispatched, the document where it stands.
    let options = options_for(&app, 0);
    let refused = assert_refused(&mut app, &Env { busy: true, ..env(&families) }, "layer.new.layer", json!({"name": "While busy"}), options, "editor_busy");
    let now = rev(&app, 0);
    assert_eq!(pick(&refused, &["documentId", "revisionBefore", "revisionAfter"]), json!({"documentId": first, "revisionBefore": now, "revisionAfter": now}));

    // Activation targets the document it activates; its revision does not move.
    let (second, other) = (doc_id(&app, 1), rev(&app, 1));
    let (envelope, calls) = typed(&mut app, &ok_env, "document.activate", json!({"document": 1}), json!({"document": {"id": second}, "rev": other}));
    assert_eq!(calls.len(), 1, "{envelope}");
    let expected = json!({"outcome": "applied", "documentId": second, "revisionBefore": other, "revisionAfter": other});
    assert_eq!(pick(&envelope, &["outcome", "documentId", "revisionBefore", "revisionAfter"]), expected);
    assert_eq!(app.session.active_index(), Some(1));

    // A new document has no revision before and is the target after.
    let (envelope, _) = typed(&mut app, &ok_env, "file.new", json!({"width": 16, "height": 8, "name": "third"}), json!({}));
    assert_eq!((app.session.documents().len(), app.session.active_index()), (3, Some(2)));
    let (third, created) = (doc_id(&app, 2), rev(&app, 2));
    let expected = json!({"outcome": "applied", "documentId": third, "revisionBefore": null, "revisionAfter": created});
    assert_eq!(pick(&envelope, &["outcome", "documentId", "revisionBefore", "revisionAfter"]), expected);

    // Text with the font set the agent probed.
    run(&mut app, "document.activate", json!({"document": 0}));
    let probed = with(options_for(&app, 0), "fontsetDigest", json!(lts_view_core::fontset_digest(&families)));
    let text = json!({"text": "Agent", "x": 4, "y": 44, "size": 12, "font": "Inter", "color": "#000000", "align": "left"});
    let (envelope, calls) = typed(&mut app, &ok_env, "type.create", text.clone(), probed);
    assert_eq!(envelope["outcome"], "applied", "{envelope}");
    assert_eq!(calls, [("type.create".to_string(), text)]);
    let top = app.session.documents()[0].doc.layers.last().expect("a top layer").content.kind_name();
    assert_eq!(top, "Type");

    // A guarded undo with the head the agent's own call produced.
    let guarded = options_for(&app, 0);
    let (envelope, _) = typed(&mut app, &ok_env, "edit.undo", json!({}), guarded);
    assert_eq!(envelope["outcome"], "applied", "{envelope}");
    assert_ne!(envelope["revisionAfter"], envelope["revisionBefore"], "undo moves the revision");
    assert_eq!(app.session.documents()[0].doc.layers.last().map(|layer| layer.name.as_str()), Some("Agent layer"));
}

/// T-PH-12: the typed policy table keeps the FP1 and R0 entries with their tiers and keys (INV-12,
/// INV-13; the whole table, R1 included, is compared exactly by
/// `r1_typed_policy_table_is_exact_with_widen_1_and_writes_stay_on_two_hooks`): unknown ids, engine
/// commands outside the table, unknown keys, out-of-bound values and malformed preconditions are
/// refused before dispatch, and while a typed call runs the session gate refuses nested ambient steps
/// (a recorded action, a file command) before any side effect.
#[test]
fn typed_policy_refuses_unknown_ids_keys_tier3_and_nested_ambient_steps() {
    for (id, tier, keys) in EXPECTED_POLICY.iter().chain(R0_POLICY) {
        let entry = lts_view_core::POLICY.iter().find(|entry| entry.id == *id).unwrap_or_else(|| panic!("`{id}` is in the typed table"));
        assert_eq!(entry.tier, *tier, "`{id}`: its tier");
        assert!(keys.iter().all(|key| entry.keys.contains(key)), "`{id}`: the keys {keys:?} are among {:?}", entry.keys);
    }
    for (id, _, _) in EXPECTED_POLICY {
        assert!(photocraft_engine::commands::find(id).is_some(), "`{id}` is an engine command");
    }
    // R0: every tier B key is named in its command's own params doc (`CommandSpec::params`).
    for (id, _, keys) in R0_POLICY {
        let spec = photocraft_engine::commands::find(id).unwrap_or_else(|| panic!("`{id}` is an engine command"));
        for key in keys.iter() {
            assert!(names(spec.params, key), "`{id}`: `{key}` is not named in its params doc {:?}", spec.params);
        }
    }
    // One table for the browser hook and a native worker (media agent contracts §15): every entry
    // runs in both today, and a native-only one would be refused in the browser only.
    for entry in lts_view_core::POLICY.iter().chain([&lts_view_core::PLACE_BYTES]) {
        assert_eq!(entry.runtimes, Runtimes::Both, "`{}` runs in both runtimes", entry.id);
    }
    assert_eq!((Runtime::HERE, Runtimes::Both.allows(Runtime::Browser), Runtimes::Both.allows(Runtime::Native)), (Runtime::Native, true, true));
    assert_eq!((Runtimes::NativeOnly.allows(Runtime::Browser), Runtimes::NativeOnly.allows(Runtime::Native)), (false, true));
    // The hooks: `placeBytes` takes File › Place Embedded's own `scale` and `center` (no `path`: the
    // bytes cross the hook; no `fit`, which an explicit scale overrides), `place_bytes`'s file name
    // and the restore `region` (R1 adds the import mode, `newDocument`); the reads have exact keys too.
    assert_eq!((lts_view_core::PLACE_BYTES.id, lts_view_core::PLACE_BYTES.tier), ("placeBytes", 1));
    for key in ["name", "scale", "center", "region"] {
        assert!(lts_view_core::PLACE_BYTES.keys.contains(&key), "`{key}` is a placeBytes key");
    }
    let placed = photocraft_engine::commands::find("file.placeEmbedded").map(|spec| spec.params).unwrap_or_default();
    for key in ["scale", "center"] {
        assert!(names(placed, key), "`{key}` is a key of File › Place Embedded: {placed}");
    }
    assert_eq!(lts_view_core::EXPORT_KEYS, ["document", "format", "quality", "maxBytes", "metadata", "removeLocation", "fields"]);
    assert_eq!(lts_view_core::EXPORT_FIELDS, ["creator", "copyright", "copyrightStatus", "copyrightUrl", "credit", "caption"]);
    assert_eq!(lts_view_core::TEXT_CHECK_KEYS, ["document", "layerId"]);

    let mut app = fixture();
    let families = bundled_families();
    let ok_env = env(&families);
    let options = options_for(&app, 0);
    for id in ["", "lts.unknown", "layer.new", "Layer.New.Layer", "LAYER.NEW.LAYER", "layer.new.layer "] {
        assert_refused(&mut app, &ok_env, id, json!({}), options.clone(), "command_not_allowed");
    }
    for id in OUTSIDE {
        assert!(photocraft_engine::commands::find(id).is_some(), "`{id}` is an engine command");
        assert_refused(&mut app, &ok_env, id, json!({}), options.clone(), "command_not_allowed");
    }
    for id in NEVER {
        assert!(photocraft_engine::commands::find(id).is_some(), "`{id}` is an engine command");
        assert!(lts_view_core::POLICY.iter().all(|entry| entry.id != id), "`{id}` is never in the table");
        assert_refused(&mut app, &ok_env, id, json!({}), options.clone(), "command_not_allowed");
    }

    let filled = layer_id(&app.session.documents()[0].doc, "Filled").0;
    let (long_text, long_name) = ("x".repeat(4097), "n".repeat(129));
    for (id, params) in [
        ("file.new", json!({"width": 0, "height": 10})),
        ("file.new", json!({"width": 4097, "height": 10})),
        ("file.new", json!({"width": 64.5, "height": 10})),
        ("file.new", json!({"height": 10})),
        ("file.new", json!({"width": 10, "height": 10, "mode": "cmyk"})),
        ("file.new", json!({"width": 10, "height": 10, "background": "backgroundColor"})),
        ("layer.new.layer", json!({"name": "x", "coalesce": "merge"})),
        ("layer.new.layer", json!({"name": long_name})),
        ("layer.new.layer", json!([])),
        ("layer.new.layer", json!("x")),
        ("type.create", json!({"text": "x", "color": "#000000", "transform": [1, 0, 0, 1, 0, 0]})),
        ("type.create", json!({"text": long_text, "color": "#000000"})),
        ("type.create", json!({"text": "x", "color": "red"})),
        ("type.create", json!({"text": "x"})),
        ("type.create", json!({"color": "#000000"})),
        ("type.create", json!({"text": "x", "color": "#000000", "align": "diagonal"})),
        ("type.create", json!({"text": "x", "color": "#000000", "size": 100_000})),
        ("type.create", json!({"text": "x", "color": "#000000", "box": [0, 0, 0, 10]})),
        ("select.rect", json!({"x": 0, "y": 0, "width": 4, "height": 4, "mode": "xor"})),
        ("select.rect", json!({"x": 0, "y": 0, "width": 4, "height": 4, "feather": -1})),
        ("select.rect", json!({"x": 0, "y": 0, "width": 0, "height": 4})),
        ("select.rect", json!({"x": 0, "y": 0, "width": 4, "height": 4, "antiAlias": false})),
        ("edit.fill", json!({"color": "#ff0000"})),
        ("edit.fill", json!({"target": "pixels"})),
        ("edit.fill", json!({"color": "#ff0000", "target": "mask"})),
        ("edit.fill", json!({"color": "#ff0000", "target": "pixels", "contents": "pattern"})),
        ("layer.newFillLayer.solidColor", json!({})),
        ("layer.setProps", json!({"layer": filled, "locks": {"all": true}})),
        ("layer.setProps", json!({"layer": filled, "opacity": 1.5})),
        ("layer.setProps", json!({"layer": filled, "blend": "Glitter"})),
        ("layer.translate", json!({"layer": filled, "dx": 10_000, "dy": 0})),
        ("layer.translate", json!({"dx": 1, "dy": 1})),
        ("filter.blur.gaussianBlur", json!({"radius": 65})),
        ("filter.blur.gaussianBlur", json!({"radius": 0})),
        ("filter.pixelate.mosaic", json!({"cellSize": 65})),
        ("document.activate", json!({"document": 8})),
        ("document.activate", json!({})),
        ("edit.undo", json!({"steps": 2})),
    ] {
        let preconditions = if id == "file.new" { json!({}) } else { options.clone() };
        assert_refused(&mut app, &ok_env, id, params, preconditions, "invalid_params");
    }
    for bad in [
        json!([]),
        with(options.clone(), "extra", json!(true)),
        with(options.clone(), "rev", json!("7")),
        with(options.clone(), "rev", json!(7)),
        with(options.clone(), "document", json!({"index": -1})),
        with(options.clone(), "document", json!({"id": 5})),
        with(options.clone(), "document", json!({"name": "first"})),
        with(options.clone(), "activeLayer", json!("Caption")),
        with(options.clone(), "historyHead", json!({"length": 5})),
        with(options.clone(), "fontsetDigest", json!("sha256:xyz")),
        with(options.clone(), "previewId", json!(format!("pv_{}", "0".repeat(32)))),
    ] {
        assert_refused(&mut app, &ok_env, "layer.new.layer", json!({"name": "x"}), bad, "invalid_params");
    }

    // R0 tier B: keys outside each subset (even documented ones), forced values and bounds.
    let caption = layer_id(&app.session.documents()[0].doc, "Caption").0;
    for (id, params) in [
        ("select.subject", json!({"mode": "xor"})),
        ("select.subject", json!({"sampleAllLayers": "yes"})),
        ("select.refineEdge", json!({"output": "newDocument"})),
        ("select.refineEdge", json!({"radius": 251})),
        ("select.colorRange", json!({"select": "skinTones"})),
        ("select.colorRange", json!({"fuzziness": 201})),
        ("select.colorRange", json!({"points": [[1, 2, 3]]})),
        ("select.colorRange", json!({"localized": true})),
        // Never clamped silently, never without effect: tones take a percentage, the hue presets and
        // the gamut warning no fuzziness, and only sampled colours take `color` or `points`.
        ("select.colorRange", json!({"select": "shadows", "fuzziness": 101})),
        ("select.colorRange", json!({"select": "reds", "fuzziness": 10})),
        ("select.colorRange", json!({"select": "outOfGamut", "fuzziness": 10})),
        ("select.colorRange", json!({"select": "reds", "points": [[1, 1]]})),
        ("select.colorRange", json!({"select": "highlights", "color": "#ffffff"})),
        ("select.colorRange", json!({"select": "outOfGamut", "sampleAllLayers": true})),
        ("select.object", json!({})),
        ("select.object", json!({"rect": [0, 0, 0, 10]})),
        ("select.quick", json!({"points": []})),
        ("select.quick", json!({"points": [[4, 4]], "size": 0})),
        ("edit.contentAwareFill", json!({})),
        ("edit.contentAwareFill", json!({"output": "current"})),
        ("edit.contentAwareFill", json!({"output": "duplicate"})),
        ("edit.contentAwareFill", json!({"output": "new", "sampling": "custom"})),
        ("edit.contentAwareFill", json!({"output": "new", "area": [0, 0, 8, 8]})),
        ("layer.layerStyle.dropShadow", json!({"layer": caption, "blend": "multiply"})),
        ("layer.layerStyle.dropShadow", json!({"layer": caption, "opacity": 101})),
        // An angle has its effect only off the global light.
        ("layer.layerStyle.dropShadow", json!({"layer": caption, "angle": 45})),
        ("layer.layerStyle.dropShadow", json!({"layer": caption, "angle": 45, "useGlobalLight": true})),
        ("layer.layerStyle.dropShadow", json!({"layer": caption, "useGlobalLight": "no"})),
        ("layer.layerStyle.stroke", json!({"position": "middle"})),
        ("layer.layerStyle.stroke", json!({"layer": caption, "size": 2})),
        ("type.edit", json!({"layer": caption})),
        ("type.edit", json!({"layer": caption, "transform": [1, 0, 0, 1, 0, 0]})),
        ("type.edit", json!({"layer": caption, "replace": {"start": 3, "end": 1, "text": "x"}})),
        ("type.edit", json!({"layer": caption, "text": long_text})),
        ("type.setStyle", json!({"layer": caption})),
        ("type.setStyle", json!({"layer": caption, "direction": "ttb"})),
        ("type.setStyle", json!({"layer": caption, "size": 0})),
        ("type.setStyle", json!({"layer": caption, "leading": "tight"})),
        ("type.setStyle", json!({"layer": caption, "range": [4, 2]})),
        // A restyle stays within `type.create`'s bounds.
        ("type.setStyle", json!({"layer": caption, "size": 257})),
        ("type.setStyle", json!({"layer": caption, "tracking": 1001})),
        ("type.setStyle", json!({"layer": caption, "leading": 1001})),
        ("layer.newAdjustmentLayer.levels", json!({"gamma": 10})),
        // Levels never raises `inWhite` silently: the pair keeps two levels between them.
        ("layer.newAdjustmentLayer.levels", json!({"inBlack": 250, "inWhite": 10})),
        ("layer.newAdjustmentLayer.levels", json!({"inBlack": 100, "inWhite": 101})),
        ("layer.newAdjustmentLayer.curves", json!({"points": [[0, 0]]})),
        ("layer.newAdjustmentLayer.curves", json!({"points": [[0, 0], [0, 255]]})),
        ("layer.newAdjustmentLayer.hueSaturation", json!({"colorize": true})),
        ("layer.newAdjustmentLayer.brightnessContrast", json!({"legacy": true})),
        ("image.autoTone", json!({"target": "mask"})),
        ("image.adjustments.shadowsHighlights", json!({"shadowRadius": 2500})),
        ("image.variables.define", json!({"defs": [{"name": "photo", "layer": filled, "type": "pixelReplacement"}]})),
        (
            "image.variables.define",
            json!({"defs": [{"name": "a", "layer": filled, "type": "visibility"}, {"name": "a", "layer": caption, "type": "visibility"}]}),
        ),
        ("image.variables.dataSets", json!({"dataSets": [{"name": "files", "values": [{"variable": "photo", "kind": "pixels", "value": "C:/x.png"}]}]})),
        ("image.variables.dataSets", json!({"dataSets": [{"name": "one", "values": [{"variable": "show", "kind": "visibility", "value": "yes"}]}]})),
        ("image.applyDataSet", json!({})),
        ("image.applyDataSet", json!({"name": "one", "index": 0})),
    ] {
        assert_refused(&mut app, &ok_env, id, params, options.clone(), "invalid_params");
    }

    // R0 refusals that depend on the target, decided before dispatch with nothing changed.
    assert_refused(&mut app, &ok_env, "type.setStyle", json!({"layer": filled, "align": "right"}), options.clone(), "engine_precheck_failed");
    assert_refused(&mut app, &ok_env, "type.edit", json!({"layer": filled, "text": "x"}), options.clone(), "engine_precheck_failed");
    let missing = json!({"layer": caption, "font": "LTS Missing Family 20261014"});
    assert_refused(&mut app, &ok_env, "type.setStyle", missing, options.clone(), "font_not_loaded");
    assert_refused(&mut app, &ok_env, "select.colorRange", json!({"points": [[64, 0]]}), options.clone(), "invalid_params");
    assert_refused(&mut app, &ok_env, "layer.layerStyle.dropShadow", json!({"layer": 999_999_999}), options.clone(), "target_not_found");
    let ghost = json!({"defs": [{"name": "headline", "layer": 999_999_999, "type": "textReplacement"}]});
    assert_refused(&mut app, &ok_env, "image.variables.define", ghost, options.clone(), "target_not_found");
    assert_refused(&mut app, &ok_env, "image.applyDataSet", json!({"name": "missing"}), options.clone(), "target_not_found");
    assert_refused(&mut app, &ok_env, "image.applyDataSet", json!({"index": 0}), options.clone(), "target_not_found");
    // Content-Aware Fill needs a pixel layer (the caption is active) and a selection.
    assert_refused(&mut app, &ok_env, "edit.contentAwareFill", json!({"output": "new"}), options.clone(), "engine_precheck_failed");
    // `file.new` creates its target: document preconditions do not apply to it.
    assert_refused(&mut app, &ok_env, "file.new", json!({"width": 8, "height": 8}), options.clone(), "invalid_params");
    // A data set a human made that replaces pixels from a file is never applied by an agent.
    run(&mut app, "image.variables.define", json!({"defs": [{"name": "photo", "layer": filled, "type": "pixelReplacement"}]}));
    let files = json!({"dataSets": [{"name": "files", "values": [{"variable": "photo", "kind": "pixels", "value": "C:/ambient/photo.png"}]}]});
    run(&mut app, "image.variables.dataSets", files);
    let fresh = options_for(&app, 0);
    assert_refused(&mut app, &ok_env, "image.applyDataSet", json!({"name": "files"}), fresh, "invalid_params");

    // Engine rejections and retargets decided before dispatch, each on a fresh fixture with the pixel
    // layer active: a floating selection, Quick Mask for a selection command, a targeted layer mask
    // or channel for a pixel command, a Select and Mask output that needs pixels, and a selection
    // that lies outside the canvas (the engine would refuse it once it runs).
    let prepared = |steps: &[(&str, Value)]| {
        let mut app = fixture();
        // Layer ids are unique in the process: each fixture's pixel layer has an id of its own.
        let pixel_layer = layer_id(&app.session.documents()[0].doc, "Filled").0;
        run(&mut app, "layer.select", json!({"layer": pixel_layer}));
        for (id, params) in steps {
            run(&mut app, id, params.clone());
        }
        app
    };
    let mut floating = prepared(&[("select.rect", json!({"x": 8, "y": 6, "width": 20, "height": 12})), ("select.float", json!({"dx": 1, "dy": 0}))]);
    let fresh = options_for(&floating, 0);
    assert_refused(&mut floating, &ok_env, "layer.new.layer", json!({"name": "x"}), fresh, "editor_busy");
    let mut quick_mask = prepared(&[("select.editInQuickMaskMode", json!({"on": true}))]);
    let fresh = options_for(&quick_mask, 0);
    assert_refused(&mut quick_mask, &ok_env, "select.subject", json!({}), fresh, "engine_precheck_failed");
    let [x, y, width, height] = MASKED;
    let mut mask_target =
        prepared(&[("select.rect", json!({"x": x, "y": y, "width": width, "height": height})), ("layer.layerMask.revealSelection", json!({}))]);
    mask_target.ui.mask_target = true;
    let fresh = options_for(&mask_target, 0);
    assert_refused(&mut mask_target, &ok_env, "image.adjustments.shadowsHighlights", json!({"shadowAmount": 50}), fresh, "engine_precheck_failed");
    let mut channel = prepared(&[]);
    channel.session.active_mut().expect("the first document").channel_view.target = ChannelTarget::Color(0);
    let fresh = options_for(&channel, 0);
    assert_refused(&mut channel, &ok_env, "image.autoTone", json!({}), fresh, "engine_precheck_failed");
    let mut no_pixels =
        prepared(&[("layer.newAdjustmentLayer.levels", json!({"gamma": 1.2})), ("select.rect", json!({"x": 8, "y": 6, "width": 20, "height": 12}))]);
    let fresh = options_for(&no_pixels, 0);
    assert_refused(&mut no_pixels, &ok_env, "select.refineEdge", json!({"output": "newLayer"}), fresh, "engine_precheck_failed");
    let mut outside = prepared(&[]);
    let mut beyond = Surface::new(PixelFormat::GRAY8);
    beyond.write_region(Rect::new(100, 100, 110, 110), &[1.0_f32; 100]);
    Arc::make_mut(&mut outside.session.active_mut().expect("the first document").doc).selection = Some(beyond);
    let fresh = options_for(&outside, 0);
    assert_refused(&mut outside, &ok_env, "edit.contentAwareFill", json!({"output": "new"}), fresh, "invalid_params");

    // The hole caps by runtime: the browser hook fills at most 512 pixels a side and 65,536 pixels
    // inline, so a 600×200 hole is `native_only` there and a native worker fills it (its call
    // reaches the run function); a hole over 1024 pixels a side is refused in both.
    let caf = json!({"output": "new"});
    let native = Env { runtime: Runtime::Native, ..env(&families) };
    for (width, height, browser, worker) in [(600, 200, "native_only", None), (1100, 40, "budget_exceeded", Some("budget_exceeded"))] {
        let mut wide = PhotocraftApp::new(Session::new(), Default::default());
        for (id, params) in [
            ("file.new", json!({"width": width, "height": height, "background": "white"})),
            ("layer.new.layer", json!({"name": "Pixels"})),
            ("select.all", json!({})),
            ("edit.fill", json!({"color": "#808080"})),
        ] {
            run(&mut wide, id, params);
        }
        let whole = options_for(&wide, 0);
        assert_refused(&mut wide, &ok_env, "edit.contentAwareFill", caf.clone(), whole.clone(), browser);
        match worker {
            Some(code) => {
                assert_refused(&mut wide, &native, "edit.contentAwareFill", caf.clone(), whole, code);
            }
            None => {
                let (envelope, ran) = decide(&mut wide, &native, "edit.contentAwareFill", &caf, &whole);
                assert_eq!((ran, envelope["outcome"].clone()), (1, json!("unknown")), "a native worker takes a {width}x{height} hole: {envelope}");
            }
        }
    }
    // Layers the R0 commands add count against the cap of sixty-four.
    let mut deep = fixture();
    while deep.session.documents()[0].doc.layer_count() < 64 {
        run(&mut deep, "layer.new.layer", json!({}));
    }
    let crowded = options_for(&deep, 0);
    for (id, params) in [
        ("layer.newAdjustmentLayer.levels", json!({"gamma": 1.2})),
        ("edit.contentAwareFill", json!({"output": "new"})),
        ("select.refineEdge", json!({"output": "newLayer"})),
    ] {
        assert_refused(&mut deep, &ok_env, id, params, crowded.clone(), "layer_limit");
    }

    // The gate a typed call installs in `session.authorize` applies the same table to every step.
    let typed_text =
        json!({"text": "שלום", "x": 2, "y": 20, "size": 12, "font": "Inter", "color": "#000000", "name": "Caption 2", "align": "right", "box": [0, 0, 64, 24]});
    assert!(lts_view_core::authorize_step("layer.new.layer", &json!({"name": "x"})).is_ok(), "a typed step passes the gate");
    assert!(lts_view_core::authorize_step("type.create", &typed_text).is_err(), "right-to-left text without a `direction` (R1, RD-12)");
    let typed_text = with(typed_text, "direction", json!("rtl"));
    assert!(lts_view_core::authorize_step("type.create", &typed_text).is_ok(), "every type.create key within bounds");
    let off_light = json!({"layer": filled, "angle": 45, "useGlobalLight": false});
    assert!(lts_view_core::authorize_step("layer.layerStyle.dropShadow", &off_light).is_ok(), "an angle off the global light");
    assert!(lts_view_core::authorize_step("select.colorRange", &json!({"select": "shadows", "fuzziness": 100})).is_ok(), "a tone range at 100 %");
    let levels = json!({"inBlack": 100, "inWhite": 102});
    assert!(lts_view_core::authorize_step("layer.newAdjustmentLayer.levels", &levels).is_ok(), "input levels two apart, as the engine keeps them");
    for (id, params) in [
        ("file.close", json!({})),
        ("actions.play", json!({"action": "Ambient"})),
        ("prefs.set", json!({"path": "", "value": {}})),
        ("plugin.install", json!({})),
        ("edit.assignProfile", json!({"profile": "srgb"})),
        ("layer.new.layer", json!({"name": "x", "coalesce": "merge"})),
        ("edit.fill", json!({"color": "#ff0000"})),
    ] {
        assert!(lts_view_core::authorize_step(id, &params).is_err(), "the gate refuses {id} {params}");
    }

    // During a typed call, a recorded action replaying an ambient step and a direct file command
    // are refused before any side effect; only the typed step runs.
    let steps = vec![("file.close".to_string(), json!({})), ("layer.new.layer".to_string(), json!({"name": "From the action"}))];
    app.session.actions.list.push(Action { name: "Ambient".into(), steps });
    let layers = app.session.documents()[0].doc.layer_count();
    let fresh = options_for(&app, 0);
    let mut nested = Vec::new();
    let envelope =
        lts_view_core::execute(&mut app, &ok_env, "layer.new.layer", &json!({"name": "Typed"}), &fresh, &mut |app: &mut PhotocraftApp, id: &str, p: Value| {
            nested.push(app.session.execute("actions.play", json!({"action": "Ambient"})).is_err());
            nested.push(app.session.execute("file.close", json!({})).is_err());
            app.run(id, p)
        });
    assert_eq!(nested, [true, true], "nested ambient steps are refused while the typed gate is installed");
    assert_eq!(envelope["outcome"], "applied", "{envelope}");
    assert_eq!(app.session.documents().len(), 2, "no document was closed");
    let top = &app.session.documents()[0].doc;
    assert_eq!((top.layer_count(), top.layers.last().map(|layer| layer.name.as_str())), (layers + 1, Some("Typed")), "only the typed layer");
    assert!(app.session.authorize.is_none(), "the gate is removed after the call");
}

/// T-PH-12 (extended, R0 tier B smoke): every newsroom family declared before the one WASM build
/// passes the typed path and applies through the engine with the keys the table allows, and its
/// envelope names its tier: smart selections (a tone range too) and Select and Mask, layer styles
/// (an angle off the global light, then cleared), type depth (a right-to-left paragraph), the six
/// tone layers, text and visibility templates, Content-Aware Fill on a new layer, and the four
/// enhance steps on that pixel layer.
#[test]
fn typed_r0_families_apply_through_the_engine() {
    let mut app = fixture();
    let families = bundled_families();
    let ok_env = env(&families);
    let doc = Arc::clone(&app.session.documents()[0].doc);
    let (filled, caption) = (layer_id(&doc, "Filled").0, layer_id(&doc, "Caption").0);
    let headline = json!({"name": "headline", "layer": caption, "type": "textReplacement"});
    let shown = json!({"name": "fill", "layer": filled, "type": "visibility"});
    let late = json!({"variable": "headline", "kind": "text", "value": "Late news"});
    let hidden = json!({"variable": "fill", "kind": "visibility", "value": false});
    let tones = json!({"shadowAmount": 35, "shadowTone": 50, "shadowRadius": 30, "highlightAmount": 10, "highlightTone": 50, "highlightRadius": 30, "color": 20, "midtone": 0});
    let steps = [
        ("select.subject", json!({"mode": "replace"})),
        ("select.object", json!({"rect": [6, 4, 26, 18]})),
        ("select.quick", json!({"points": [[12, 10]], "size": 6, "mode": "replace"})),
        ("select.colorRange", json!({"select": "sampledColors", "points": [[12, 10]], "fuzziness": 20})),
        ("select.colorRange", json!({"select": "highlights", "fuzziness": 50, "mode": "add"})),
        ("select.refineEdge", json!({"radius": 2, "smooth": 10, "output": "selection"})),
        ("layer.layerStyle.dropShadow", json!({"layer": caption, "color": "#000000", "opacity": 60, "distance": 2, "size": 2})),
        ("layer.layerStyle.dropShadow", json!({"layer": filled, "angle": 45, "useGlobalLight": false, "distance": 3})),
        ("type.setStyle", json!({"layer": caption, "align": "right", "direction": "rtl", "size": 14})),
        ("type.edit", json!({"layer": caption, "text": "Breaking"})),
        ("layer.newAdjustmentLayer.levels", json!({"inBlack": 10, "inWhite": 245})),
        ("layer.newAdjustmentLayer.curves", json!({"points": [[0, 0], [128, 150], [255, 255]]})),
        ("layer.newAdjustmentLayer.brightnessContrast", json!({"brightness": 10, "contrast": 5})),
        ("layer.newAdjustmentLayer.exposure", json!({"exposure": 0.5, "offset": 0, "gamma": 1})),
        ("layer.newAdjustmentLayer.vibrance", json!({"vibrance": 20, "saturation": 5})),
        ("layer.newAdjustmentLayer.hueSaturation", json!({"hue": 10, "saturation": 5, "lightness": 0})),
        ("image.variables.define", json!({"defs": [headline, shown]})),
        ("image.variables.dataSets", json!({"dataSets": [{"name": "late", "values": [late, hidden]}]})),
        ("image.applyDataSet", json!({"name": "late"})),
        ("layer.select", json!({"layer": filled})),
        ("select.rect", json!({"x": 12, "y": 8, "width": 6, "height": 6})),
        ("edit.contentAwareFill", json!({"output": "new", "seed": 7})),
        ("layer.layerStyle.stroke", json!({"size": 2, "position": "outside", "color": "#ffffff"})),
        ("image.autoContrast", json!({})),
        ("image.autoTone", json!({})),
        ("image.autoColor", json!({})),
        ("image.adjustments.shadowsHighlights", tones),
    ];
    let mut shadow = None;
    for (command, params) in steps {
        let options = options_for(&app, 0);
        let (envelope, calls) = typed(&mut app, &ok_env, command, params.clone(), options);
        assert_eq!(envelope["outcome"], "applied", "{command} {params}: {envelope}");
        let tier = lts_view_core::POLICY.iter().find(|entry| entry.id == command).map(|entry| entry.tier);
        assert_eq!(envelope["tier"], json!(tier), "{command}: the envelope names the table's tier");
        assert_eq!(calls, [(command.to_string(), params.clone())], "dispatched once, unchanged");
        if params.get("angle").is_some() {
            shadow = app.session.documents()[0].doc.layer(LayerId(filled)).and_then(|layer| layer.effects.items.first().cloned());
        }
    }
    let applied = match shadow {
        Some(Effect::DropShadow(applied)) => applied,
        other => panic!("a drop shadow on the fill: {other:?}"),
    };
    assert_eq!((applied.angle, applied.use_global_light), (45.0, false), "the applied angle has its effect");
    // Layer › Layer Style › Clear Layer Style takes it off again.
    let options = options_for(&app, 0);
    let (envelope, _) = typed(&mut app, &ok_env, "layer.layerStyle.clear", json!({"layer": filled}), options);
    assert_eq!(envelope["outcome"], "applied", "{envelope}");
    assert!(app.session.authorize.is_none(), "the gate is removed after the calls");
    let doc = &app.session.documents()[0].doc;
    let title = doc.layer(LayerId(caption)).expect("the caption");
    assert!(matches!(&title.content, LayerContent::Text(text) if text.text == "Late news"), "the data set replaced the headline");
    assert_eq!(title.effects.items.len(), 1, "a drop shadow on the caption");
    let fill = doc.layer(LayerId(filled)).expect("the fill");
    assert!(!fill.visible, "the data set hid the fill");
    assert!(fill.effects.items.is_empty(), "the fill's style is cleared");
    let kinds: Vec<&str> = doc.layers.iter().map(|layer| layer.content.kind_name()).collect();
    assert_eq!(kinds.iter().filter(|kind| **kind == "Adjustment").count(), 6, "six tone layers: {kinds:?}");
    let patched = doc.layers.iter().find(|layer| layer.name.starts_with("Content-Aware Fill")).expect("a Content-Aware Fill layer");
    assert_eq!((patched.content.kind_name(), patched.effects.items.len()), ("Pixel", 1), "a new pixel layer, stroked");
    let history = app.session.documents()[0].history.entries();
    for label in ["Auto Contrast", "Auto Tone", "Auto Color", "Shadows/Highlights"] {
        assert!(history.iter().any(|entry| entry == label), "`{label}` is a step of the history: {history:?}");
    }
}

/// T-PH-EXIF (R0 tier A): every agent export goes through the FP1 encoder. A GPS-tagged camera
/// JPEG exports to PNG, JPEG and WebP without EXIF, GPS, camera or location data or its
/// resolution; the OC-13 fields (creator, copyright; the default when the caller names no policy)
/// and, only when the caller asks for `newsroom`, the IPTC credit line and caption are the only
/// metadata written, from the document's File Info or the caller; `none` writes nothing and gives
/// FP1's render bytes. `removeLocation` is true or absent; JPEG quality and `maxBytes` hold; a
/// pure read.
#[test]
fn agent_export_strips_exif_and_gps_and_writes_only_the_positive_fields() {
    let mut app = fixture();
    let jpeg = camera_jpeg();
    let source = photocraft_codecs::decode(&jpeg).expect("the camera JPEG decodes");
    assert!(source.meta.exif.is_some() && source.meta.xmp.as_deref().is_some_and(|xmp| xmp.contains("GPSLatitude")), "a GPS-tagged JPEG");
    let opened = photocraft_engine::file_cmds::open_bytes_as(&mut app.session, "camera.jpg", &jpeg, None, None).expect("the JPEG opens");
    let index = usize::try_from(opened["document"].as_u64().expect("its index")).expect("an index");
    let id = doc_id(&app, index);
    let read = ok(lts_view_core::metadata(&app.session, &json!({"document": {"id": id}})), "the metadata read");
    assert_eq!((read["exif"]["gps"].clone(), read["xmp"]["gps"].clone()), (json!(true), json!(true)), "the document carries GPS: {read}");
    // A print resolution an export with positive fields would otherwise carry (PNG pHYs, JFIF).
    assert_eq!(app.session.active_index(), Some(index), "the opened JPEG is active");
    Arc::make_mut(&mut app.session.active_mut().expect("the camera document").doc).resolution_dpi = 300.0;

    let owner = json!({"creator": "LTS Photographer", "copyright": "© 2026 LTS Agency", "copyrightStatus": "copyrighted"});
    let newsroom = with(with(owner.clone(), "credit", json!("LTS Agency")), "caption", json!("A fixture caption"));
    for format in ["png", "jpeg", "webp"] {
        for (metadata, fields) in [(Some("none"), json!({})), (Some("copyright"), owner.clone()), (Some("newsroom"), newsroom.clone()), (None, owner.clone())] {
            let base = json!({"document": {"id": id}, "format": format});
            let params = match metadata {
                Some(policy) => with(base, "metadata", json!(policy)),
                None => base,
            };
            let case = format!("{format} {params}");
            let exported = ok(assert_pure(&app, &case, |app| lts_view_core::export(&app.session, &params)), &case);
            let expected = json!({"target": "export", "metadata": metadata.unwrap_or("copyright"), "removeLocation": true, "fields": fields});
            assert_eq!(pick(&exported.info, &["target", "metadata", "removeLocation", "fields"]), expected, "{case}");
            let decoded = photocraft_codecs::decode(&exported.bytes).expect("a decodable export");
            assert!(decoded.meta.exif.is_none(), "{case}: no EXIF");
            assert!(decoded.meta.dpi.is_none(), "{case}: no resolution: {:?}", decoded.meta.dpi);
            match decoded.meta.xmp.as_deref() {
                None => assert_eq!(metadata, Some("none"), "{case}: the positive fields are written"),
                Some(xmp) => {
                    assert_ne!(metadata, Some("none"), "{case}: `none` writes no XMP");
                    for private in PRIVATE {
                        assert!(!xmp.contains(private), "{case}: `{private}` leaked into {xmp}");
                    }
                    let info = photocraft_engine::file_cmds::read_file_info(Some(xmp));
                    let caption = fields.get("caption").cloned().unwrap_or_else(|| json!(""));
                    let expected =
                        json!({"author": "LTS Photographer", "copyright": "© 2026 LTS Agency", "copyrightStatus": "copyrighted", "description": caption});
                    assert_eq!(pick(&info, &["author", "copyright", "copyrightStatus", "description"]), expected, "{case}: {xmp}");
                    assert_eq!(xmp.contains("<photoshop:Credit>LTS Agency</photoshop:Credit>"), fields.get("credit").is_some(), "{case}: {xmp}");
                }
            }
        }
        // `none` is FP1's encoder exactly: the render's bytes.
        let exported = ok(lts_view_core::export(&app.session, &json!({"document": {"id": id}, "format": format, "metadata": "none"})), format);
        let rendered = ok(lts_view_core::render(&app.session, &json!({"document": {"id": id}, "target": "composite", "format": format})), format);
        assert_eq!(exported.bytes, rendered.bytes, "{format}: the FP1 encoder");
    }

    // Metadata never changes pixels; on request the caller's credit line and caption (Hebrew too)
    // replace the document's, and an empty value drops one.
    let plain = ok(lts_view_core::export(&app.session, &json!({"document": {"id": id}, "metadata": "none"})), "a plain PNG");
    let credited = ok(lts_view_core::export(&app.session, &json!({"document": {"id": id}})), "a credited PNG");
    assert_eq!(rgba(&credited.bytes), rgba(&plain.bytes), "the same pixels");
    let params = json!({"document": {"id": id}, "metadata": "newsroom", "fields": {"credit": "LTS Newsroom", "caption": "שלום from the studio"}});
    let exported = ok(lts_view_core::export(&app.session, &params), "the caller's fields");
    let xmp = photocraft_codecs::decode(&exported.bytes).expect("decodable").meta.xmp.expect("XMP");
    assert!(xmp.contains("<photoshop:Credit>LTS Newsroom</photoshop:Credit>"), "{xmp}");
    assert_eq!(photocraft_engine::file_cmds::read_file_info(Some(xmp.as_str()))["description"], "שלום from the studio");
    let params = json!({"document": {"id": id}, "metadata": "newsroom", "fields": {"caption": "", "credit": ""}});
    let exported = ok(lts_view_core::export(&app.session, &params), "dropped fields");
    assert_eq!(exported.info["fields"], owner, "only the OC-13 fields remain");
    let located = ok(lts_view_core::export(&app.session, &json!({"document": {"id": id}, "removeLocation": true})), "removeLocation true");
    assert_eq!(pick(&located.info, &["metadata", "removeLocation", "fields"]), json!({"metadata": "copyright", "removeLocation": true, "fields": owner}));

    // Refusals: fields outside the policy (the credit line and caption need `newsroom`), unknown
    // policies (Light's `allExceptCamera` too: Photo writes positive fields only) and keys, a
    // location kept, PNG quality, `maxBytes` 0.
    for (params, failure) in [
        (json!({"document": {"id": id}, "metadata": "copyright", "fields": {"credit": "x"}}), "read_failed"),
        (json!({"document": {"id": id}, "fields": {"caption": "x"}}), "read_failed"),
        (json!({"document": {"id": id}, "metadata": "none", "fields": {"copyright": "x"}}), "read_failed"),
        (json!({"document": {"id": id}, "fields": {"gps": "31,46.5N"}}), "read_failed"),
        (json!({"document": {"id": id}, "fields": {"copyrightStatus": "maybe"}}), "read_failed"),
        (json!({"document": {"id": id}, "fields": "credit"}), "read_failed"),
        (json!({"document": {"id": id}, "metadata": "all"}), "unsupported_read"),
        (json!({"document": {"id": id}, "metadata": "allExceptCamera"}), "unsupported_read"),
        (json!({"document": {"id": id}, "metadata": 1}), "read_failed"),
        (json!({"document": {"id": id}, "removeLocation": false}), "unsupported_read"),
        (json!({"document": {"id": id}, "removeLocation": "yes"}), "read_failed"),
        (json!({"document": {"id": id}, "format": "png", "quality": 90}), "read_failed"),
        (json!({"document": {"id": id}, "format": "gif"}), "unsupported_read"),
        (json!({"document": {"id": id}, "maxBytes": 0}), "read_failed"),
    ] {
        let case = params.to_string();
        assert_eq!(code(assert_pure(&app, &case, |app| lts_view_core::export(&app.session, &params)), &case), failure, "{case}");
    }

    // An Indexed Color document exports as a palette PNG, which holds no metadata: positive fields
    // are refused there rather than claimed; without them it exports, and as JPEG it carries them.
    let mut indexed = PhotocraftApp::new(Session::new(), Default::default());
    run(&mut indexed, "file.new", json!({"width": 16, "height": 8, "background": "white"}));
    run(&mut indexed, "image.mode.indexedColor", json!({"palette": "exact", "forced": "none"}));
    let rights = json!({"metadata": "copyright", "fields": {"copyright": "© 2026 LTS Agency"}});
    let palette = lts_view_core::export(&indexed.session, &with(rights.clone(), "format", json!("png")));
    assert_eq!(code(palette, "positive fields in a palette PNG"), "unsupported_read");
    ok(lts_view_core::export(&indexed.session, &json!({"format": "png", "metadata": "none"})), "a palette PNG without metadata");
    let indexed_jpeg = ok(lts_view_core::export(&indexed.session, &with(rights, "format", json!("jpeg"))), "an Indexed Color JPEG");
    let xmp = photocraft_codecs::decode(&indexed_jpeg.bytes).expect("decodable").meta.xmp.unwrap_or_default();
    assert!(xmp.contains("© 2026 LTS Agency"), "the JPEG carries the copyright: {xmp}");

    // JPEG quality, WebP lossless or lossy, and `maxBytes` (never a silent quality change).
    let quality =
        |q: u64| ok(lts_view_core::export(&app.session, &json!({"document": {"id": id}, "format": "jpeg", "quality": q, "metadata": "none"})), "a JPEG");
    let (low, high) = (quality(20), quality(95));
    assert!(low.bytes.len() < high.bytes.len(), "JPEG quality is honoured: {} vs {}", low.bytes.len(), high.bytes.len());
    let lossless = ok(lts_view_core::export(&app.session, &json!({"document": {"id": id}, "format": "webp", "metadata": "none"})), "a lossless WebP");
    let lossy = ok(lts_view_core::export(&app.session, &json!({"document": {"id": id}, "format": "webp", "quality": 80, "metadata": "none"})), "a lossy WebP");
    assert!(has_fourcc(&lossless.bytes, b"VP8L") && has_fourcc(&lossy.bytes, b"VP8 ") && !has_fourcc(&lossy.bytes, b"VP8L"), "WebP: lossless or lossy");
    let size = plain.bytes.len();
    let at = ok(lts_view_core::export(&app.session, &json!({"document": {"id": id}, "metadata": "none", "maxBytes": size})), "at maxBytes");
    assert_eq!(at.bytes, plain.bytes);
    let over = lts_view_core::export(&app.session, &json!({"document": {"id": id}, "metadata": "none", "maxBytes": size - 1}));
    assert_eq!(code(over, "over maxBytes"), "budget_exceeded");
}

/// Whether a RIFF (WebP) file holds a chunk with this FourCC.
fn has_fourcc(bytes: &[u8], tag: &[u8; 4]) -> bool {
    bytes.windows(4).any(|window| window == tag)
}

/// T-PH-13 (R0 tier A `placeBytes`, critic correction CC-27): explicit registration. Bytes placed
/// at 100 % on an explicit centre land exactly where the caller says: an odd-sized patch of a
/// checkpoint restores an off-centre region pixel for pixel as one undoable smart-object layer, and
/// a whole checkpoint masked to a protected region restores that region while the agent's edit
/// stays everywhere else (Codex scenario 6, `image_restore_region`). A checkpoint of another size,
/// a scale or centre that breaks the registration, an image too large once scaled, and every other
/// refusal are decided before the place function, with nothing changed.
#[test]
fn place_bytes_under_mask_restores_a_region() {
    let mut app = fixture();
    let families = bundled_families();
    let ok_env = env(&families);
    let lossless = json!({"document": {"index": 0}, "format": "png", "metadata": "none"});
    let checkpoint = ok(lts_view_core::export(&app.session, &lossless), "the checkpoint").bytes;
    let original = rgba(&checkpoint);
    // The edit the agent keeps: a red block over part of the fill, the background and the caption.
    let [x, y, width, height] = EDITED;
    run(&mut app, "layer.new.layer", json!({"name": "Edit"}));
    run(&mut app, "select.rect", json!({"x": x, "y": y, "width": width, "height": height}));
    run(&mut app, "edit.fill", json!({"color": "#cc2200"}));
    run(&mut app, "select.deselect", json!({}));
    let edited = delivered(&app, 0);
    for (i, pixel) in edited.2.iter().enumerate() {
        let (px, py) = (i as i32 % 64, i as i32 / 64);
        if inside(EDITED, px, py) {
            assert_eq!(*pixel, RED, "the edit at ({px}, {py})");
        }
    }

    // A 15×11 patch of the checkpoint, off-centre, restored on its region: 100 % on the region's
    // centre (28.5, 14.5), so every pixel lands on the pixel it was taken from.
    let (first, before) = (doc_id(&app, 0), rev(&app, 0));
    let steps = app.session.documents()[0].history.entries().len();
    let taken = crop(&original, PATCH);
    let params = restore("patch.png", PATCH);
    assert_eq!(params["center"], json!([28.5, 14.5]), "the region's centre");
    let options = options_for(&app, 0);
    let (envelope, calls) = place(&mut app, &ok_env, png(&taken), &params, &options);
    assert_eq!(calls, 1, "{envelope}");
    let after = rev(&app, 0);
    let expected = json!({"resultVersion": TYPED, "outcome": "applied", "tier": 1, "documentId": first, "revisionBefore": before, "revisionAfter": after});
    assert_eq!(pick(&envelope, &["resultVersion", "outcome", "tier", "documentId", "revisionBefore", "revisionAfter"]), expected);
    assert_ne!(before, after, "the place moved the revision");
    let [left, top, right, bottom] = [PATCH[0], PATCH[1], PATCH[0] + PATCH[2], PATCH[1] + PATCH[3]].map(f64::from);
    assert_eq!(envelope["result"]["bounds"], json!([left, top, right, bottom]), "the engine placed it on the region");
    assert!(app.session.authorize.is_none(), "the gate is removed after the call");
    let patch = LayerId(envelope["result"]["layer"].as_u64().expect("the placed layer"));
    let (bounds, pixels) = layer_render(&app, patch);
    assert_eq!(bounds, json!(PATCH), "registered pixel for pixel on the region");
    assert_eq!((pixels.0, pixels.1), (taken.0, taken.1), "at 100 %");
    assert!(pixels.2.iter().zip(&taken.2).all(|(a, b)| close(*a, *b)), "the patch's own pixels");
    let state = &app.session.documents()[0];
    let history = state.history.entries();
    assert_eq!((history.len(), history.last().map(String::as_str)), (steps + 1, Some("Place Embedded")), "one undoable step");
    let layer = state.doc.layer(patch).expect("the placed layer exists");
    assert_eq!((layer.name.as_str(), layer.content.kind_name()), ("patch", "Smart Object"));
    let restored = delivered(&app, 0);
    for (i, ((now, was), kept)) in restored.2.iter().zip(&original.2).zip(&edited.2).enumerate() {
        let (px, py) = (i as i32 % 64, i as i32 / 64);
        if inside(PATCH, px, py) {
            assert!(close(*now, *was), "restored from the checkpoint at ({px}, {py}): {now:?}, was {was:?}");
            assert_ne!(kept, was, "the edit had changed ({px}, {py})");
        } else {
            assert_eq!(now, kept, "the agent's edit stays at ({px}, {py})");
        }
    }
    // One step: a guarded undo takes it back.
    let options = options_for(&app, 0);
    let (envelope, _) = typed(&mut app, &ok_env, "edit.undo", json!({}), options);
    assert_eq!(envelope["outcome"], "applied", "{envelope}");
    assert_eq!(delivered(&app, 0), edited, "undone: the edit as it was");

    // The whole checkpoint, restored on the whole document and masked to the protected region.
    let options = options_for(&app, 0);
    let (envelope, calls) = place(&mut app, &ok_env, checkpoint.clone(), &restore("checkpoint.png", [0, 0, 64, 48]), &options);
    assert_eq!((calls, envelope["outcome"].clone()), (1, json!("applied")), "{envelope}");
    assert_eq!(envelope["result"]["bounds"], json!([0.0, 0.0, 64.0, 48.0]), "aligned with the document");
    let placed = LayerId(envelope["result"]["layer"].as_u64().expect("the placed layer"));
    let state = &app.session.documents()[0];
    let layer = state.doc.layer(placed).expect("the placed layer exists");
    assert_eq!((layer.name.as_str(), layer.content.kind_name()), ("checkpoint", "Smart Object"));
    assert_eq!((state.active_layer, state.doc.layers.last().map(|layer| layer.id)), (Some(placed), Some(placed)), "active, on top");
    let shown = delivered(&app, 0);
    assert!(shown.2.iter().zip(&original.2).all(|(a, b)| close(*a, *b)), "unmasked, the checkpoint shows the document as it was");
    let [x, y, width, height] = PROTECTED;
    for (command, params) in [
        ("select.rect", json!({"x": x, "y": y, "width": width, "height": height})),
        ("layer.layerMask.revealSelection", json!({"layer": placed.0})),
        ("select.deselect", json!({})),
    ] {
        let options = options_for(&app, 0);
        let (envelope, _) = typed(&mut app, &ok_env, command, params, options);
        assert_eq!(envelope["outcome"], "applied", "{command}: {envelope}");
    }
    let restored = delivered(&app, 0);
    for (i, ((now, was), kept)) in restored.2.iter().zip(&original.2).zip(&edited.2).enumerate() {
        let (px, py) = (i as i32 % 64, i as i32 / 64);
        if inside(PROTECTED, px, py) {
            assert!(close(*now, *was), "restored from the checkpoint at ({px}, {py}): {now:?}, was {was:?}");
            assert_ne!(kept, was, "the edit had changed ({px}, {py})");
        } else {
            assert_eq!(now, kept, "the agent's edit stays at ({px}, {py})");
        }
    }

    // Every refusal is decided before the place function, with nothing changed. The input first.
    let options = options_for(&app, 0);
    let free = json!({"name": "checkpoint.png", "scale": 100, "center": [32, 24]});
    for (bytes, failure) in [
        (Vec::new(), "input_rejected"),
        (b"not an image".to_vec(), "input_rejected"),
        (b"GIF89a\x01\x00\x01\x00\x00\x00\x00;".to_vec(), "input_rejected"),
        (checkpoint[..33].to_vec(), "decode_failed"),
        (solid_png(4097, 1), "budget_exceeded"),
    ] {
        assert_place_refused(&mut app, &ok_env, bytes, free.clone(), options.clone(), failure);
    }
    // CMYK (only a JPEG carries it) is refused, never converted to RGB on the way.
    let cmyk = cmyk_jpeg();
    assert_eq!(photocraft_codecs::decode(&cmyk).expect("the CMYK JPEG decodes").layout(), ChannelLayout::Cmyk, "a CMYK JPEG");
    let refused = assert_place_refused(&mut app, &ok_env, cmyk, with(free.clone(), "name", json!("cmyk.jpg")), options.clone(), "input_rejected");
    assert!(refused["message"].as_str().is_some_and(|message| message.contains("CMYK")), "refused as CMYK: {refused}");
    // A checkpoint of another size never restores this document.
    let other = json!({"document": {"index": 1}, "format": "png", "metadata": "none"});
    let other_size = ok(lts_view_core::export(&app.session, &other), "the 32×24 document").bytes;
    assert_place_refused(&mut app, &ok_env, other_size, restore("checkpoint.png", [0, 0, 64, 48]), options.clone(), "input_rejected");
    // The registration is explicit and whole: a scale and a centre, and for a region 100 % on its own
    // centre, inside the document.
    let patch_bytes = png(&taken);
    for params in [
        without(restore("patch.png", PATCH), "scale"),
        without(restore("patch.png", PATCH), "center"),
        with(restore("patch.png", PATCH), "scale", json!(50)),
        with(restore("patch.png", PATCH), "center", json!([32, 24])),
        with(restore("patch.png", PATCH), "region", json!([21, 9, 0, 11])),
        restore("patch.png", [53, 9, 15, 11]),
    ] {
        assert_place_refused(&mut app, &ok_env, patch_bytes.clone(), params, options.clone(), "invalid_params");
    }
    // Scaled, a placed image stays within 4096 pixels a side (R1: a whole 4096 × 4096 image at most).
    for (bytes, scale) in [(solid_png(2048, 1), 250), (solid_png(1024, 1024), 401)] {
        let params = json!({"name": "large.png", "scale": scale, "center": [32, 24]});
        assert_place_refused(&mut app, &ok_env, bytes, params, options.clone(), "budget_exceeded");
    }
    for params in [
        with(free.clone(), "path", json!("C:/ambient/photo.png")),
        with(free.clone(), "name", json!("../photo.png")),
        with(free.clone(), "name", json!("photo.pcraft")),
        with(free.clone(), "name", json!("photo")),
        with(free.clone(), "scale", json!(0)),
        with(free.clone(), "scale", json!(100_000)),
        with(free.clone(), "fit", json!(false)),
        with(free.clone(), "center", json!([1])),
        with(free.clone(), "center", json!([1, 2, 3])),
        with(free.clone(), "center", json!([5000, 0])),
        json!([]),
    ] {
        assert_place_refused(&mut app, &ok_env, checkpoint.clone(), params, options.clone(), "invalid_params");
    }
    assert_place_refused(&mut app, &Env { busy: true, ..env(&families) }, checkpoint.clone(), free.clone(), options.clone(), "editor_busy");
    assert_place_refused(&mut app, &Env { hidden: true, ..env(&families) }, checkpoint.clone(), free.clone(), options.clone(), "page_hidden");
    let stale = with(options.clone(), "rev", json!("p:999999"));
    assert_place_refused(&mut app, &ok_env, checkpoint.clone(), free.clone(), stale, "revision_conflict");
    let other = with(options.clone(), "document", json!({"id": doc_id(&app, 1)}));
    assert_place_refused(&mut app, &ok_env, checkpoint.clone(), free.clone(), other, "target_not_active");
    let mut empty = PhotocraftApp::new(Session::new(), Default::default());
    assert_place_refused(&mut empty, &ok_env, checkpoint.clone(), free.clone(), json!({}), "target_not_found");
    let mut deep = fixture();
    while deep.session.documents()[0].doc.layer_count() < 64 {
        run(&mut deep, "layer.new.layer", json!({}));
    }
    let crowded = options_for(&deep, 0);
    assert_place_refused(&mut deep, &ok_env, checkpoint, free, crowded, "layer_limit");
}

/// T-PH-13b (R0 tier A `placeBytes`): the caller's camera metadata never reaches the document. A
/// GPS-tagged camera JPEG is re-encoded as a lossless PNG of its upright pixels before the
/// engine's `place_bytes` embeds it, so the smart object (and every `.pcraft` or PSD that keeps it)
/// holds no EXIF, XMP, text or resolution, only the JPEG's pixels, registered where the caller says.
/// The bytes arrive by handle, never in the broker's 1 MiB command (final design §2.4, §8.1;
/// INV-47), so the browser hook and a native worker both take Photo's input cap (`MAX_INPUT_BYTES`,
/// 16 MiB in R0, 64 MiB from R1): an image larger than that command, and than a whole BB frame
/// (critic correction CC-27), is placed in the browser. The browser hook's queue holds at most one
/// full-size input at a time: a further input is refused `queue_full` before it is copied.
#[test]
fn place_bytes_reencodes_without_camera_metadata_and_caps_input_bytes() {
    let mut app = fixture();
    let families = bundled_families();
    let browser = env(&families);
    let jpeg = camera_jpeg();
    let source = photocraft_codecs::decode(&jpeg).expect("the camera JPEG decodes");
    assert!(source.meta.exif.is_some() && source.meta.xmp.as_deref().is_some_and(|xmp| xmp.contains("GPSLatitude")), "a GPS-tagged JPEG");
    let region = [8, 8, 48, 32];
    let options = options_for(&app, 0);
    let (envelope, calls) = place(&mut app, &browser, jpeg.clone(), &restore("camera.jpg", region), &options);
    assert_eq!((calls, envelope["outcome"].clone()), (1, json!("applied")), "{envelope}");
    let placed = LayerId(envelope["result"]["layer"].as_u64().expect("the placed layer"));
    let layer = app.session.documents()[0].doc.layer(placed).expect("the placed layer exists");
    assert_eq!(layer.name, "camera", "named after the caller's file");
    let LayerContent::Smart(smart) = &layer.content else { panic!("a smart object, not a {} layer", layer.content.kind_name()) };
    let SmartSource::Embedded { file_name, bytes } = &smart.source else { panic!("an embedded smart object") };
    assert_eq!(file_name, "camera.png", "embedded as the PNG it was re-encoded to");
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "a PNG");
    let chunks = png_chunks(bytes);
    assert!(chunks.iter().all(|chunk| matches!(chunk, b"IHDR" | b"IDAT" | b"IEND")), "pixels only, no metadata chunk: {chunks:?}");
    let embedded = photocraft_codecs::decode(bytes).expect("the embedded PNG decodes");
    assert!(embedded.meta.is_empty(), "no EXIF, XMP, text or resolution: {:?}", embedded.meta);
    let (bounds, pixels) = layer_render(&app, placed);
    assert_eq!(bounds, json!(region), "registered where the caller says");
    let camera = rgba(&jpeg);
    assert_eq!((pixels.0, pixels.1), (camera.0, camera.1), "at 100 %");
    assert!(pixels.2.iter().zip(&camera.2).all(|(a, b)| close(*a, *b)), "the JPEG's own pixels");

    // Both runtimes take Photo's input cap and refuse more. The bytes arrive by handle (the
    // lab parent substitutes them after a sha256 match), never inside the broker's 1 MiB command,
    // so an image larger than that command and than any whole BB frame (541-934 KB) is placed in
    // the browser.
    let noisy = noise_png(640, 640);
    assert!(noisy.len() > 1024 * 1024, "more than the broker's 1 MiB command: {} bytes", noisy.len());
    let params = json!({"name": "noise.png", "scale": 100, "center": [32, 24]});
    let options = options_for(&app, 0);
    let native = Env { runtime: Runtime::Native, ..env(&families) };
    assert!(noisy.len() <= lts_view_core::MAX_INPUT_BYTES, "within Photo's input cap: {} bytes", noisy.len());
    for runtime in [&browser, &native] {
        assert_place_refused(&mut app, runtime, vec![0; lts_view_core::MAX_INPUT_BYTES + 1], params.clone(), options.clone(), "input_rejected");
    }
    let (envelope, calls) = place(&mut app, &browser, noisy, &params, &options);
    assert_eq!((calls, envelope["outcome"].clone()), (1, json!("applied")), "the browser hook places it: {envelope}");

    // The queue bound: every input waiting in the browser hook's queue is a copy in the page's
    // memory until a frame places it, so all waiting `placeBytes` inputs together stay within one
    // full-size input. The glue asks `place_queue_refusal` with the bytes already waiting before it
    // copies another input; a refusal is the typed `queue_full` (nothing copied or queued, and the
    // same call can be sent again once the queue has drained).
    let full = lts_view_core::MAX_INPUT_BYTES;
    assert_eq!(lts_view_core::MAX_PENDING_PLACE_BYTES, full, "one full-size input waits at a time");
    for (waiting, length) in [(0, 1), (0, full), (full / 2, full / 2), (full - 1, 1)] {
        assert_eq!(lts_view_core::place_queue_refusal(waiting, length), None, "{length} bytes join {waiting} waiting bytes");
    }
    let queue_full = json!({"resultVersion": TYPED, "outcome": "not_applied", "code": "queue_full"});
    for (waiting, length) in [(full, 1), (full, full), (1, full), (full / 2 + 1, full / 2), (usize::MAX, full)] {
        let refusal = lts_view_core::place_queue_refusal(waiting, length).unwrap_or_else(|| panic!("{length} bytes must wait for {waiting} waiting bytes"));
        assert_eq!(pick(&refusal, &["resultVersion", "outcome", "code"]), queue_full, "{refusal}");
    }
}

/// T-PH-TXT (R0 tier A `textCheck`): a type layer's text against the lines `type.info` lays out
/// (type_cmds.rs:456-505): every line's character range, the text a paragraph box drops
/// (overflow) and tofu; a pure read that refuses other layers.
#[test]
fn text_check_reports_line_range_overflow_and_tofu() {
    let mut app = fixture();
    let first = doc_id(&app, 0);
    let doc = Arc::clone(&app.session.documents()[0].doc);
    let (caption, filled) = (layer_id(&doc, "Caption").0, layer_id(&doc, "Filled").0);
    let ranges =
        |info: &Value| -> Value { json!(info["lines"].as_array().expect("lines").iter().map(|line| pick(line, &["start", "end"])).collect::<Vec<_>>()) };

    // Point text: nothing hidden, and the line ranges `type.info` reports.
    let params = json!({"document": {"id": first}, "layerId": caption});
    let checked = ok(assert_pure(&app, "textCheck", |app| shared_check(app, &params)), "point text");
    let keys = ["documentId", "layerId", "text", "chars", "paragraphText", "overflow", "hiddenText", "shownChars"];
    let expected = json!({"documentId": first, "layerId": caption, "text": "LTS", "chars": 3, "paragraphText": false, "overflow": false, "hiddenText": "", "shownChars": 3});
    assert_eq!(pick(&checked, &keys), expected);
    let info = app.session.execute("type.info", json!({"layer": caption})).expect("type.info");
    assert_eq!(checked["lines"], ranges(&info), "the line ranges of type.info");

    // A paragraph box too small for its text drops the lines that do not fit.
    let long = "Breaking news from the studio tonight with many words";
    run(&mut app, "type.create", json!({"text": long, "box": [2, 2, 40, 14], "size": 10, "color": "#000000", "name": "Box"}));
    let boxed = layer_id(&app.session.documents()[0].doc, "Box").0;
    let checked = ok(shared_check(&app, &json!({"layerId": boxed})), "an overflowing box");
    assert_eq!(pick(&checked, &["paragraphText", "overflow"]), json!({"paragraphText": true, "overflow": true}), "{checked}");
    let hidden = checked["hiddenText"].as_str().expect("the hidden text").to_string();
    assert!(!hidden.trim().is_empty() && long.ends_with(&hidden), "the dropped text ends the text: {checked}");
    let shown = checked["shownChars"].as_u64().and_then(|n| usize::try_from(n).ok()).expect("shown characters");
    assert_eq!(shown + hidden.chars().count(), long.chars().count(), "{checked}");
    let info = app.session.execute("type.info", json!({"layer": boxed})).expect("type.info");
    assert_eq!(checked["lines"], ranges(&info), "the line ranges of type.info");

    // The same text in a box large enough shows every character.
    run(&mut app, "type.edit", json!({"layer": boxed, "box": [0, 0, 600, 200]}));
    let checked = ok(shared_check(&app, &json!({"layerId": boxed})), "a fitting box");
    let expected = json!({"overflow": false, "hiddenText": "", "shownChars": long.chars().count()});
    assert_eq!(pick(&checked, &["overflow", "hiddenText", "shownChars"]), expected, "{checked}");

    // Tofu: Hebrew in the bundled Latin family, as on a page that imported no Hebrew font.
    run(&mut app, "type.edit", json!({"layer": boxed, "text": "שלום"}));
    let checked = ok(lts_view_core::text_check(&mut TextEngine::new(), &app.session, &json!({"layerId": boxed})), "Hebrew without a Hebrew font");
    let expected = json!({"text": "שלום", "chars": 4, "glyphs": 4, "tofu": 4, "missingFont": false, "overflow": false});
    assert_eq!(pick(&checked, &["text", "chars", "glyphs", "tofu", "missingFont", "overflow"]), expected);

    // Other layers and unknown keys are typed read failures; no layer named reads the active one.
    for params in [json!({"layerId": filled}), json!({"layerId": 999_999_999}), json!({"layerId": boxed, "expect": "x"}), json!({"document": {"index": 9}})] {
        let case = params.to_string();
        assert_eq!(code(assert_pure(&app, &case, |app| shared_check(app, &params)), &case), "read_failed", "{case}");
    }
    assert_eq!(ok(shared_check(&app, &json!({})), "the active layer")["layerId"], boxed);
}

// ------------------------------------------------------------------ R1 (master plan v3 W3, widen-1)

/// R1 widen-1 (master plan v3 W3, fork-photo; topology §7.3): the entries the typed table gains, each
/// with the keys and tier of the engine command it names. Layer › Create and Release Clipping Mask
/// (commands.rs:575-590, `{"layer":id?}`); Layer › Adjustment Properties for the six tone kinds the
/// table creates (`layer.setAdjustment`, commands.rs:687-705: the values given merge over the
/// layer's own, and none resets it to its defaults); Select › Modify › Expand and Contract
/// (selection_cmds.rs:355-356, radius 1..500). Each is one undoable step: tier 1.
const R1_POLICY: &[(&str, u8, &[&str])] = &[
    ("layer.createClippingMask", 1, &["layer"]),
    ("layer.releaseClippingMask", 1, &["layer"]),
    ("layer.setAdjustment", 1, SET_ADJUSTMENT_KEYS),
    ("select.modify.contract", 1, &["radius"]),
    ("select.modify.expand", 1, &["radius"]),
];

/// `layer.setAdjustment`'s keys: the layer and the keys of the six tone kinds the table creates
/// (commands.rs:902-920). A key of another kind than the layer's is refused before dispatch.
const SET_ADJUSTMENT_KEYS: &[&str] = &[
    "layer",
    "brightness",
    "contrast",
    "inBlack",
    "gamma",
    "inWhite",
    "outBlack",
    "outWhite",
    "points",
    "exposure",
    "offset",
    "vibrance",
    "saturation",
    "hue",
    "lightness",
];

/// R1 keys of entries the earlier tables declare: Hebrew `direction` on new type (M6, RD-12; the
/// engine's `type.create` applies the paragraph keys `type.setStyle` documents, type_cmds.rs:534) and
/// the fill opacity of a layer (Layer Properties `fill`, 0..1, commands.rs:505, :527-529).
const R1_KEYS: &[(&str, &[&str])] = &[("layer.setProps", &["fill"]), ("type.create", &["direction"])];

/// A `placeBytes` request as the R1 glue runs it: `newDocument` opens the re-encoded bytes as a new
/// document through the engine's own import (`file_cmds::open_bytes_as`, decoded as the PNG they are
/// and named after the caller's file), any other request places them (`file_cmds::place_bytes`);
/// then the views follow the documents. Returns the envelope and the number of calls.
fn import(app: &mut PhotocraftApp, env: &Env<'_>, bytes: Vec<u8>, params: &Value, options: &Value) -> (Value, usize) {
    let mut calls = 0;
    let envelope = lts_view_core::place_bytes(app, env, bytes, params, options, &mut |app: &mut PhotocraftApp, name: &str, bytes: Vec<u8>, p: Value| {
        calls += 1;
        let done = if p["newDocument"] == true {
            photocraft_engine::file_cmds::open_bytes_as(&mut app.session, name, &bytes, Some("png"), None)
        } else {
            photocraft_engine::file_cmds::place_bytes(&mut app.session, name, bytes, None, &p)
        };
        app.sync_views();
        done.map_err(|error| error.to_string())
    });
    (envelope, calls)
}

/// A PNG of `rgb` samples without compression (stored deflate blocks), so its size is its pixels':
/// an input over R0's 16 MiB cap that decodes within the R1 bounds.
fn stored_png(width: u32, height: u32, rgb: Vec<u8>) -> Vec<u8> {
    let image = photocraft_codecs::Image::from_u8(width, height, ChannelLayout::Rgb, rgb).expect("an RGB image");
    let options = photocraft_codecs::EncodeOptions { png_compression: photocraft_codecs::PngCompression::None, ..Default::default() };
    photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &options).expect("a stored PNG")
}

/// `width`×`height` RGB samples of xorshift noise.
fn noise_rgb(width: u32, height: u32) -> Vec<u8> {
    let mut state = 0x2545_f491_u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        (state >> 24) as u8
    };
    (0..width * height * 3).map(|_| next()).collect()
}

/// `width`×`height` RGB samples of a smooth gradient: red across, green down, blue 128.
fn gradient_rgb(width: u32, height: u32) -> Vec<u8> {
    (0..width * height).flat_map(|i| [(i % width * 255 / (width - 1)) as u8, (i / width * 255 / (height - 1)) as u8, 128]).collect()
}

/// A `width`×`height` camera JPEG of a smooth gradient, with what `camera_jpeg` carries (EXIF with a
/// GPS IFD and `CAMERA_XMP`) and a print resolution of 300 ppi.
fn camera_jpeg_of(width: u32, height: u32) -> Vec<u8> {
    let image = photocraft_codecs::Image::from_u8(width, height, ChannelLayout::Rgb, gradient_rgb(width, height)).expect("an RGB image");
    let xmp = Some(CAMERA_XMP.to_string());
    let meta = photocraft_codecs::Metadata { exif: Some(exif(true, false)), xmp, dpi: Some((300.0, 300.0)), ..Default::default() };
    photocraft_codecs::encode(&image.with_meta(meta), photocraft_codecs::Format::Jpeg, &photocraft_codecs::EncodeOptions::default()).expect("a camera JPEG")
}

/// A `width`×`height` JPEG of the same gradient that embeds the ICC profile `icc` and nothing else.
fn tagged_jpeg_of(width: u32, height: u32, icc: Vec<u8>) -> Vec<u8> {
    let image = photocraft_codecs::Image::from_u8(width, height, ChannelLayout::Rgb, gradient_rgb(width, height)).expect("an RGB image");
    let options = photocraft_codecs::EncodeOptions::default();
    photocraft_codecs::encode(&image.with_icc(Some(icc)), photocraft_codecs::Format::Jpeg, &options).expect("a tagged JPEG")
}

/// The Display P3 profile as the engine embeds it: Edit › Assign Profile on a scratch document.
fn display_p3() -> Vec<u8> {
    let mut scratch = PhotocraftApp::new(Session::new(), Default::default());
    run(&mut scratch, "file.new", json!({"width": 8, "height": 8}));
    run(&mut scratch, "edit.assignProfile", json!({"profile": "display-p3"}));
    scratch.session.documents()[0].doc.icc_profile.as_ref().map(|icc| icc.to_vec()).expect("Display P3 assigned")
}

/// The rows of `describe` by layer id, groups' children included.
fn rows_by_id(rows: &Value, out: &mut BTreeMap<u64, Value>) {
    for row in rows.as_array().into_iter().flatten() {
        if let Some(id) = row["id"].as_u64() {
            out.insert(id, row.clone());
        }
        rows_by_id(&row["children"], out);
    }
}

/// The engine's own inspection of every layer by id (`photocraft_engine::inspect::layer`, what the
/// certified inspect reports and the adapter reads), groups' children included.
fn inspected_by_id(layers: &[Layer], out: &mut BTreeMap<u64, Value>) {
    for layer in layers {
        out.insert(layer.id.0, photocraft_engine::inspect::layer(layer));
        if let Some(children) = layer.children() {
            inspected_by_id(children, out);
        }
    }
}

/// Layer `id`'s adjustment in document 0, as serde writes it.
fn adjustment(app: &PhotocraftApp, id: u64) -> Value {
    match app.session.documents()[0].doc.layer(LayerId(id)).map(|layer| &layer.content) {
        Some(LayerContent::Adjustment(found)) => serde_json::to_value(found).expect("an adjustment serializes"),
        other => panic!("layer {id} is not an adjustment layer: {:?}", other.map(LayerContent::kind_name)),
    }
}

/// The adjustment a new `kind` layer of `params` holds, made by the engine in a document of its own:
/// what an edit that merges to `params` must leave.
fn new_adjustment(kind: &str, params: &Value) -> Value {
    let mut oracle = PhotocraftApp::new(Session::new(), Default::default());
    run(&mut oracle, "file.new", json!({"width": 8, "height": 8}));
    let made = run(&mut oracle, &format!("layer.newAdjustmentLayer.{kind}"), params.clone());
    adjustment(&oracle, made["layer"].as_u64().expect("the oracle's tone layer"))
}

/// `base` with every field of `over` set over it.
fn merged(base: &Value, over: &Value) -> Value {
    let mut out = base.clone();
    if let (Some(fields), Some(over)) = (out.as_object_mut(), over.as_object()) {
        fields.extend(over.clone());
    }
    out
}

/// Two JSON values equal up to 1e-4 in every number: a level kept as a fraction of 255, read back and
/// kept again by a merge, may move in its last float digit.
fn near(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64().zip(y.as_f64()).is_some_and(|(x, y)| (x - y).abs() <= 1e-4),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| near(x, y)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(key, x)| y.get(key).is_some_and(|y| near(x, y))),
        _ => a == b,
    }
}

/// The selection bounds `describe` reports for document 0.
fn selection_bounds(app: &PhotocraftApp) -> Value {
    ok(lts_view_core::describe(&app.session, &json!({"document": {"index": 0}})), "describe")["document"]["selection"]["bounds"].clone()
}

/// Document 0 of the F0 fixture as delivered with its fill layer at fill opacity `fill` and a 2-pixel
/// red outside stroke, every step driven through the engine (the fixture's own driver, not the path
/// under test, which refuses that pair below fill 1: OC-18).
fn stroked(fill: f64) -> (u32, u32, Vec<[u8; 4]>) {
    let mut oracle = fixture();
    let filled = layer_id(&oracle.session.documents()[0].doc, "Filled").0;
    run(&mut oracle, "layer.setProps", json!({"layer": filled, "fill": fill}));
    run(&mut oracle, "layer.select", json!({"layer": filled}));
    run(&mut oracle, "layer.layerStyle.stroke", json!({"size": 2, "position": "outside", "color": "#ff0000"}));
    delivered(&oracle, 0)
}

/// The names a source file quotes right after `marker`, such as `set(&view, "name"`.
fn quoted_after<'s>(source: &'s str, marker: &str) -> BTreeSet<&'s str> {
    source.match_indices(marker).filter_map(|(at, found)| source[at + found.len()..].split_once('"').map(|(name, _)| name)).collect()
}

/// The names a source file quotes right before `marker`, such as `("name", Read::`.
fn quoted_before<'s>(source: &'s str, marker: &str) -> BTreeSet<&'s str> {
    source.match_indices(marker).filter_map(|(at, _)| source[..at].rsplit_once('"').map(|(_, name)| name)).collect()
}

/// T-PH-12 extended (R1, master plan v3 W3 widen-1): the typed table is exact with R1's entries
/// (`R1_POLICY`) and keys (`R1_KEYS`), every entry runs in both runtimes, and no entry takes `weight`
/// (RD-12: the engine draws weight 400 only). Every R1 key is one the engine documents. R1's bounds
/// refuse before dispatch, and the session gate takes R1's steps within them. Writes stay on the two
/// hooks: the glue installs the reads, `watermark`, `execute` and `placeBytes` and nothing else, an
/// import is a `placeBytes` mode (`newDocument`), and the ambient open commands stay out of the table.
#[test]
fn r1_typed_policy_table_is_exact_with_widen_1_and_writes_stay_on_two_hooks() {
    let mut expected: BTreeMap<&str, (u8, BTreeSet<&str>)> =
        EXPECTED_POLICY.iter().chain(R0_POLICY).chain(R1_POLICY).map(|(id, tier, keys)| (*id, (*tier, keys.iter().copied().collect()))).collect();
    for (id, keys) in R1_KEYS {
        let entry = expected.get_mut(id).unwrap_or_else(|| panic!("`{id}` is an earlier entry"));
        entry.1.extend(keys.iter().copied());
    }
    let actual: BTreeMap<&str, (u8, BTreeSet<&str>)> =
        lts_view_core::POLICY.iter().map(|entry| (entry.id, (entry.tier, entry.keys.iter().copied().collect()))).collect();
    assert_eq!(actual, expected, "exact ids, keys and tiers, R1 included");
    assert_eq!(lts_view_core::POLICY.len(), EXPECTED_POLICY.len() + R0_POLICY.len() + R1_POLICY.len(), "one entry per command");
    assert_eq!(lts_view_core::PLACE_BYTES.keys, ["name", "scale", "center", "region", "newDocument"], "an import is a placeBytes mode");
    for entry in lts_view_core::POLICY.iter().chain([&lts_view_core::PLACE_BYTES]) {
        assert_eq!(entry.runtimes, Runtimes::Both, "`{}` runs in both runtimes", entry.id);
        assert!(!entry.keys.contains(&"weight"), "`{}` takes no weight: the engine draws weight 400 only (RD-12)", entry.id);
    }

    // Every R1 key is one the engine documents: in its command's params doc, a tone key in the doc of
    // its kind, and `direction` among the paragraph keys `type.setStyle` documents, which `type.create`
    // applies the same way (`apply_para_props`).
    let params_doc = |id: &str| photocraft_engine::commands::find(id).map(|spec| spec.params).unwrap_or_else(|| panic!("`{id}` is an engine command"));
    let tone_kinds = ["brightnessContrast", "levels", "curves", "exposure", "vibrance", "hueSaturation"];
    let tones = tone_kinds.map(|kind| params_doc(&format!("layer.newAdjustmentLayer.{kind}")));
    for &(id, _, keys) in R1_POLICY {
        for key in keys {
            let named = names(params_doc(id), key) || (id == "layer.setAdjustment" && tones.iter().any(|params| names(params, key)));
            assert!(named, "`{id}`: `{key}` is not named in the engine's params docs");
        }
    }
    assert!(names(params_doc("layer.setProps"), "fill"), "Layer Properties documents `fill`, the fill opacity");
    assert!(names(params_doc("type.setStyle"), "direction"), "`direction` is a paragraph key");

    // Writes stay on the two hooks: one global, `window.photocraftView`, with the reads, `watermark`,
    // `execute` and `placeBytes`, and no other method.
    let glue = include_str!("../src/web/lts_view.rs");
    let reads = BTreeSet::from(["describe", "export", "fonts", "metadata", "render", "sample", "textCheck"]);
    assert_eq!(quoted_before(glue, "\", Read::"), reads, "the reads the glue installs");
    let methods = BTreeSet::from(["execute", "placeBytes", "version", "watermark"]);
    assert_eq!(quoted_after(glue, "set(&view, \""), methods, "the glue's only writes are execute and placeBytes");
    assert_eq!(glue.matches("Reflect::set(&window").count(), 1, "one global, window.photocraftView");
    let mut app = fixture();
    let families = bundled_families();
    let ok_env = env(&families);
    let options = options_for(&app, 0);
    for id in ["file.openAs", "file.import.notes", "select.modify.border", "select.modify.smooth", "select.modify.feather"] {
        assert!(photocraft_engine::commands::find(id).is_some(), "`{id}` is an engine command");
        assert_refused(&mut app, &ok_env, id, json!({}), options.clone(), "command_not_allowed");
    }

    // R1's keys and bounds, refused before dispatch with nothing changed.
    let doc = Arc::clone(&app.session.documents()[0].doc);
    let (filled, caption) = (layer_id(&doc, "Filled").0, layer_id(&doc, "Caption").0);
    for (id, params) in [
        ("type.create", json!({"text": "x", "color": "#000000", "direction": "ttb"})),
        ("type.create", json!({"text": "x", "color": "#000000", "direction": 1})),
        ("type.create", json!({"text": "x", "color": "#000000", "weight": 700})),
        ("type.setStyle", json!({"layer": caption, "weight": 700})),
        ("layer.setProps", json!({"layer": filled, "fill": 1.5})),
        ("layer.setProps", json!({"layer": filled, "fill": -0.1})),
        ("layer.setProps", json!({"layer": filled, "fill": "50%"})),
        ("layer.setProps", json!({"layer": filled, "clipped": true})),
        ("layer.createClippingMask", json!({})),
        ("layer.createClippingMask", json!({"layer": caption, "mode": "group"})),
        ("layer.releaseClippingMask", json!({"layer": "Caption"})),
        ("layer.setAdjustment", json!({})),
        ("layer.setAdjustment", json!({"gamma": 1.2})),
        ("layer.setAdjustment", json!({"layer": filled})),
        ("layer.setAdjustment", json!({"layer": filled, "gamma": 10})),
        ("layer.setAdjustment", json!({"layer": filled, "inBlack": 250, "inWhite": 10})),
        ("layer.setAdjustment", json!({"layer": filled, "points": [[0, 0]]})),
        ("layer.setAdjustment", json!({"layer": filled, "legacy": true})),
        ("layer.setAdjustment", json!({"layer": filled, "colorize": true})),
        ("layer.setAdjustment", json!({"layer": filled, "red": {"gamma": 1.2}})),
        ("select.modify.expand", json!({})),
        ("select.modify.expand", json!({"radius": 0})),
        ("select.modify.expand", json!({"radius": 501})),
        ("select.modify.contract", json!({"radius": 2.5})),
        ("select.modify.contract", json!({"radius": 4, "applyAtCanvasBounds": true})),
        ("file.new", json!({"width": 4097, "height": 10})),
        ("file.new", json!({"width": 10, "height": 4097})),
        ("image.imageSize", json!({"width": 4097, "height": 10})),
        ("image.crop", json!({"x": 4097, "y": 0, "width": 1, "height": 1})),
        ("select.rect", json!({"x": 0, "y": 0, "width": 4097, "height": 4})),
    ] {
        let preconditions = if id == "file.new" { json!({}) } else { options.clone() };
        assert_refused(&mut app, &ok_env, id, params, preconditions, "invalid_params");
    }

    // The gate a typed call installs takes R1's steps within the bounds, and still no weight.
    let headline = json!({"text": "שלום", "x": 60, "y": 20, "size": 12, "font": "Inter", "color": "#000000", "align": "right", "direction": "rtl"});
    for (id, params) in [
        ("type.create", headline),
        ("layer.setProps", json!({"layer": filled, "fill": 0.5})),
        ("layer.createClippingMask", json!({"layer": caption})),
        ("layer.releaseClippingMask", json!({"layer": caption})),
        ("layer.setAdjustment", json!({"layer": filled, "gamma": 1.2, "inBlack": 10, "inWhite": 245})),
        ("select.modify.expand", json!({"radius": 8})),
        ("select.modify.contract", json!({"radius": 500})),
        ("file.new", json!({"width": 4096, "height": 4096})),
        ("image.imageSize", json!({"width": 4096, "height": 2304})),
        ("image.crop", json!({"x": 256, "y": 0, "width": 3840, "height": 2160})),
    ] {
        assert!(lts_view_core::authorize_step(id, &params).is_ok(), "the gate takes {id} {params}");
    }
    for (id, params) in [("type.create", json!({"text": "x", "color": "#000000", "weight": 700})), ("select.modify.feather", json!({"radius": 4}))] {
        assert!(lts_view_core::authorize_step(id, &params).is_err(), "the gate refuses {id} {params}");
    }
}

/// R1 widen-1 (M6, RD-12): `type.create` takes `direction`. A Hebrew headline that starts with Latin
/// letters is laid out right to left when asked (`rtl`), where `auto` follows the first strong
/// character and lays it out left to right (TY-8). The stored paragraph, the type engine's own layout
/// (the order of the clusters, no pixels) and `describe`'s text styling show it. Text with a
/// right-to-left letter must state its direction (RD-12: once direction is wired it is required for
/// Hebrew). No key takes a weight.
#[test]
fn r1_type_create_takes_direction_for_hebrew_and_never_weight() {
    let mut app = fixture();
    let families = bundled_families();
    let ok_env = env(&families);
    let headline = "BBC שלום";
    let hebrew = headline.find('ש').expect("the Hebrew word");
    for (direction, stored, rtl) in [("rtl", "Rtl", true), ("ltr", "Ltr", false), ("auto", "Auto", false)] {
        let params = json!({"text": headline, "x": 60, "y": 40, "size": 10, "color": "#000000", "align": "right", "direction": direction});
        let options = options_for(&app, 0);
        let (envelope, calls) = typed(&mut app, &ok_env, "type.create", params.clone(), options);
        assert_eq!(envelope["outcome"], "applied", "direction {direction}: {envelope}");
        assert_eq!(calls, [("type.create".to_string(), params)], "dispatched once, unchanged");
        let state = &app.session.documents()[0];
        let layer = state.doc.layers.last().expect("the new type layer on top");
        let LayerContent::Text(text) = &layer.content else { panic!("a type layer, not a {} layer", layer.content.kind_name()) };
        let paragraph = text.paragraph_runs().first().map(|run| format!("{:?} {:?}", run.style.direction, run.style.align));
        assert_eq!(paragraph, Some(format!("{stored} Right")), "direction {direction}: the stored paragraph");
        let layout = photocraft_text::shared().lock().unwrap_or_else(|error| error.into_inner()).layout(text, state.doc.resolution_dpi);
        let x = |at: usize| layout.clusters.iter().find(|cluster| cluster.range.start == at).map(|cluster| cluster.x).expect("a cluster");
        assert_eq!(x(0) > x(hebrew), rtl, "direction {direction}: the Latin word lies right of the Hebrew one only right to left");
        let params = json!({"document": {"index": 0}, "layerId": layer.id.0});
        let detail = ok(lts_view_core::describe(&app.session, &params), "the layer's detail");
        assert_eq!(pick(&detail["layer"]["text"], &["direction", "align"]), json!({"direction": direction, "align": "right"}), "direction {direction}");
    }

    // No key takes a weight, and only the three directions exist.
    let caption = layer_id(&app.session.documents()[0].doc, "Caption").0;
    let options = options_for(&app, 0);
    for (id, params) in [
        ("type.create", json!({"text": "x", "color": "#000000", "weight": 700})),
        ("type.setStyle", json!({"layer": caption, "weight": 700})),
        ("type.create", json!({"text": "x", "color": "#000000", "direction": "ttb"})),
    ] {
        assert_refused(&mut app, &ok_env, id, params, options.clone(), "invalid_params");
    }

    // RD-12: text with a right-to-left letter (Hebrew, Arabic, a right-to-left mark) states its
    // direction, `auto` included as above: without one it is refused before dispatch, at the gate too.
    for text in ["BBC שלום", "שלום", "مرحبا", "\u{200f}LTS"] {
        let params = json!({"text": text, "x": 60, "y": 40, "size": 10, "color": "#000000"});
        let refused = assert_refused(&mut app, &ok_env, "type.create", params.clone(), options.clone(), "invalid_params");
        assert!(refused["message"].as_str().is_some_and(|message| message.contains("`direction`")), "{text}: {refused}");
        assert!(lts_view_core::authorize_step("type.create", &params).is_err(), "the gate refuses {text} without a direction");
    }
}

/// R1 widen-1 (`act.compose.layer_props`, `act.mask.clipping`): Layer Properties takes `fill`, the
/// fill opacity, which reaches a layer's own pixels and never its effects, and Layer › Create and
/// Release Clipping Mask clip a layer to the one below it. Each is one typed, undoable step, and the
/// stored field, `describe` and the composite show it: a plain layer at fill 40 % composites as at
/// opacity 40 %, and at fill 0 as hidden; an inside stroke it then gets stays, an outline inside its
/// edge over the white background below (Photoshop's Fill 0 % with a stroke), and so does a shape
/// layer's outside stroke around its outline. Narrowed until the engine is fixed (OC-18): the engine
/// lays the outside or centre stroke of any other layer beneath its whole shape, which a fill below
/// 100 % would show, so a fill below 1 on a pixel or type layer with such a stroke, and such a stroke
/// on a layer below fill 1, are refused before dispatch with nothing changed (what Photoshop shows is
/// `oc18_fill_0_outside_stroke_is_an_outline_over_the_backdrop`, ignored until the fix). A caption
/// clipped to a fill it does not overlap leaves the composite, which then shows the state before the
/// caption, and comes back when released. The bottom layer has nothing below it to clip to: refused
/// before dispatch.
#[test]
fn r1_fill_opacity_and_clipping_masks_apply_and_read_back() {
    let mut app = fixture();
    let families = bundled_families();
    let ok_env = env(&families);
    let filled = layer_id(&app.session.documents()[0].doc, "Filled").0;
    let options = options_for(&app, 0);
    let (envelope, calls) = typed(&mut app, &ok_env, "layer.setProps", json!({"layer": filled, "fill": 0.4}), options);
    assert_eq!((envelope["outcome"].clone(), envelope["tier"].clone(), calls.len()), (json!("applied"), json!(1), 1), "{envelope}");
    let state = &app.session.documents()[0];
    assert_eq!(state.doc.layer(LayerId(filled)).map(|layer| layer.fill_opacity), Some(0.4), "the stored fill opacity");
    assert_eq!(state.history.entries().last().map(String::as_str), Some("Layer Properties"), "one undoable step");
    let detail = ok(lts_view_core::describe(&app.session, &json!({"document": {"index": 0}, "layerId": filled})), "the fill's detail");
    assert_eq!(detail["layer"]["fillOpacity"], json!(0.4_f32), "describe reads it back");
    let mut oracle = fixture();
    let plain = layer_id(&oracle.session.documents()[0].doc, "Filled").0;
    run(&mut oracle, "layer.setProps", json!({"layer": plain, "opacity": 0.4}));
    let (shown, expected) = (delivered(&app, 0), delivered(&oracle, 0));
    assert!(shown.2.iter().zip(&expected.2).all(|(a, b)| close(*a, *b)), "fill 40 % composites a plain layer as opacity 40 % does");
    assert_ne!(shown, delivered(&fixture(), 0), "the fill opacity changed the composite");

    // Fill 0: the layer's own pixels go, so it composites as hidden.
    let options = options_for(&app, 0);
    let (envelope, _) = typed(&mut app, &ok_env, "layer.setProps", json!({"layer": filled, "fill": 0.0}), options);
    assert_eq!(envelope["outcome"], "applied", "fill 0: {envelope}");
    let mut hidden = fixture();
    let unseen = layer_id(&hidden.session.documents()[0].doc, "Filled").0;
    run(&mut hidden, "layer.setProps", json!({"layer": unseen, "visible": false}));
    let (shown, expected) = (delivered(&app, 0), delivered(&hidden, 0));
    assert!(shown.2.iter().zip(&expected.2).all(|(a, b)| close(*a, *b)), "fill 0 composites a plain layer as hidden: its own pixels are gone");

    // Narrowed (OC-18): on it an outside or centre stroke, the outside one by default, is refused
    // before dispatch, with nothing changed.
    let options = options_for(&app, 0);
    let (envelope, _) = typed(&mut app, &ok_env, "layer.select", json!({"layer": filled}), options);
    assert_eq!(envelope["outcome"], "applied", "{envelope}");
    let outside = json!({"size": 2, "position": "outside", "color": "#ff0000"});
    let options = options_for(&app, 0);
    for params in [outside.clone(), with(outside.clone(), "position", json!("center")), without(outside.clone(), "position")] {
        let refused = assert_refused(&mut app, &ok_env, "layer.layerStyle.stroke", params, options.clone(), "invalid_params");
        assert!(refused["message"].as_str().is_some_and(|message| message.contains("OC-18")), "refused as narrowed: {refused}");
    }

    // Its effects stay: an inside stroke is an outline inside its edge over the white background, as
    // Photoshop draws Fill 0 % with a stroke, and its own pixels stay gone.
    let options = options_for(&app, 0);
    let (envelope, _) = typed(&mut app, &ok_env, "layer.layerStyle.stroke", with(outside.clone(), "position", json!("inside")), options);
    assert_eq!(envelope["outcome"], "applied", "an inside stroke at fill 0: {envelope}");
    let red = [0xff, 0, 0, 0xff];
    let shown = delivered(&app, 0);
    let at = |x: u32, y: u32| shown.2[(y * shown.0 + x) as usize];
    for (x, y) in [(8, 12), (9, 12), (26, 12), (27, 12), (18, 6), (18, 7), (18, 16), (18, 17)] {
        assert!(close(at(x, y), red), "the inside stroke at ({x}, {y}): {:?}", at(x, y));
    }
    for (x, y) in [(12, 12), (18, 12), (23, 10), (6, 12), (30, 12), (18, 4)] {
        assert!(close(at(x, y), WHITE), "the white background at ({x}, {y}): {:?}", at(x, y));
    }

    // The other order: with an outside or centre stroke at fill 100 %, a fill below 1 on a pixel or a
    // type layer is refused before dispatch, with nothing changed; fill 1 and an opacity apply.
    let mut app = fixture();
    let doc = Arc::clone(&app.session.documents()[0].doc);
    let (filled, caption) = (layer_id(&doc, "Filled").0, layer_id(&doc, "Caption").0);
    let steps = [
        ("layer.select", json!({"layer": filled})),
        ("layer.layerStyle.stroke", outside.clone()),
        ("layer.select", json!({"layer": caption})),
        ("layer.layerStyle.stroke", with(outside.clone(), "position", json!("center"))),
    ];
    for (command, params) in steps {
        let options = options_for(&app, 0);
        let (envelope, _) = typed(&mut app, &ok_env, command, params, options);
        assert_eq!(envelope["outcome"], "applied", "{command}: {envelope}");
    }
    let options = options_for(&app, 0);
    let lower = [json!({"layer": filled, "fill": 0.0}), json!({"layer": filled, "fill": 0.99, "opacity": 0.5}), json!({"layer": caption, "fill": 0.5})];
    for params in lower {
        let refused = assert_refused(&mut app, &ok_env, "layer.setProps", params, options.clone(), "invalid_params");
        assert!(refused["message"].as_str().is_some_and(|message| message.contains("OC-18")), "refused as narrowed: {refused}");
    }
    for params in [json!({"layer": filled, "fill": 1.0}), json!({"layer": caption, "opacity": 0.5})] {
        let options = options_for(&app, 0);
        let (envelope, _) = typed(&mut app, &ok_env, "layer.setProps", params.clone(), options);
        assert_eq!(envelope["outcome"], "applied", "{params}: {envelope}");
    }

    // A shape layer's outside stroke follows its outline, so its fill goes below 1 as in Photoshop:
    // at fill 0 the stroke is an outline around the shape over the white background.
    let shape = json!({"kind": "rect", "rect": [36, 6, 16, 12], "fill": "#336699"});
    let shape = run(&mut app, "shape.create", shape)["layer"].as_u64().expect("a shape layer");
    for (command, params) in [("layer.layerStyle.stroke", outside.clone()), ("layer.setProps", json!({"layer": shape, "fill": 0.0}))] {
        let options = options_for(&app, 0);
        let (envelope, _) = typed(&mut app, &ok_env, command, params, options);
        assert_eq!(envelope["outcome"], "applied", "{command} on a shape layer: {envelope}");
    }
    let shown = delivered(&app, 0);
    let at = |x: u32, y: u32| shown.2[(y * shown.0 + x) as usize];
    for (x, y) in [(35, 12), (52, 12), (44, 5), (44, 18)] {
        assert!(close(at(x, y), red), "the shape's outside stroke at ({x}, {y}): {:?}", at(x, y));
    }
    for (x, y) in [(44, 12), (38, 9), (49, 15)] {
        assert!(close(at(x, y), WHITE), "inside the shape, the white background at ({x}, {y}): {:?}", at(x, y));
    }

    // A caption clipped to the fill, which it does not overlap, leaves the composite, which then shows
    // the state after Fill (history entry 3, before the caption); released, the caption comes back.
    let mut app = fixture();
    let (caption, bottom) = (layer_id(&app.session.documents()[0].doc, "Caption").0, app.session.documents()[0].doc.layers[0].id.0);
    let before = delivered(&app, 0);
    let params = json!({"document": {"index": 0}, "target": "history", "historyIndex": 3});
    let after_fill = rgba(&ok(lts_view_core::render(&app.session, &params), "the state after Fill").bytes);
    let toggles = [("layer.createClippingMask", true, "Create Clipping Mask"), ("layer.releaseClippingMask", false, "Release Clipping Mask")];
    for (command, clipped, label) in toggles {
        let options = options_for(&app, 0);
        let (envelope, calls) = typed(&mut app, &ok_env, command, json!({"layer": caption}), options);
        assert_eq!((envelope["outcome"].clone(), envelope["tier"].clone(), calls.len()), (json!("applied"), json!(1), 1), "{command}: {envelope}");
        let state = &app.session.documents()[0];
        assert_eq!(state.doc.layer(LayerId(caption)).map(|layer| layer.clipped), Some(clipped), "{command}: the stored flag");
        assert_eq!(state.history.entries().last().map(String::as_str), Some(label), "{command}: one undoable step");
        let described = ok(lts_view_core::describe(&app.session, &json!({"document": {"index": 0}, "view": "layers"})), "the layer stack");
        let mut rows = BTreeMap::new();
        rows_by_id(&described["document"]["layers"], &mut rows);
        assert_eq!(rows[&caption]["clipped"], clipped, "{command}: the layers read");
        let shown = delivered(&app, 0);
        let oracle = if clipped { &after_fill } else { &before };
        assert!(shown.2.iter().zip(&oracle.2).all(|(a, b)| close(*a, *b)), "{command}: the composite");
    }

    // The bottom layer has nothing below it to clip to; an unknown layer; no layer named.
    let options = options_for(&app, 0);
    assert_refused(&mut app, &ok_env, "layer.createClippingMask", json!({"layer": bottom}), options.clone(), "engine_precheck_failed");
    assert_refused(&mut app, &ok_env, "layer.createClippingMask", json!({"layer": 999_999_999}), options.clone(), "target_not_found");
    assert_refused(&mut app, &ok_env, "layer.releaseClippingMask", json!({}), options, "invalid_params");
}

/// A known engine divergence, not a claim (OC-18; the assertion R1-P-RED-01 made of the fill test): in
/// Photoshop a pixel layer at Fill 0 % with an outside stroke is an outline only, its inside showing
/// what lies below. The engine lays a pixel or type layer's outside stroke beneath the layer's whole
/// shape (crates/compose/src/effects.rs:1377-1383), so there the inside shows the stroke, and the typed
/// hook refuses that pair (`r1_fill_opacity_and_clipping_masks_apply_and_read_back`). Driven through
/// the engine itself (`stroked`). Ignored until the engine fix is GREEN: `--ignored` runs it, and it
/// fails today; once it passes, drop the `ignore` and the hook's narrowing (`stroke_under_shape`)
/// together.
#[test]
#[ignore = "OC-18 engine bug: a pixel layer's outside stroke is laid under its whole shape; the hook refuses the pair until the fix is GREEN"]
fn oc18_fill_0_outside_stroke_is_an_outline_over_the_backdrop() {
    let shown = stroked(0.0);
    let at = |x: u32, y: u32| shown.2[(y * shown.0 + x) as usize];
    assert!(close(at(7, 10), [0xff, 0, 0, 0xff]), "the outside stroke: {:?}", at(7, 10));
    assert!(close(at(12, 10), WHITE), "inside the shape, the white background below it: {:?}", at(12, 10));
}

/// R1 widen-1 (`act.tone.adjustment_layer`): Layer › Adjustment Properties (`layer.setAdjustment`)
/// edits an existing tone layer of the six kinds the table creates, with that kind's own keys and
/// bounds. The values given merge over the layer's own, as the engine merges them, in one undoable
/// step, and the layer then holds what a new layer of the merged values holds. Refused before
/// dispatch: a key of another kind, a kind outside the six, a layer that is not an adjustment layer,
/// a Levels pair the merge would leave less than two levels apart (the engine would raise `inWhite`
/// silently), and a call with no layer or no value (the engine would reset the layer).
#[test]
fn r1_set_adjustment_edits_an_existing_tone_layer_with_its_own_keys() {
    let mut app = fixture();
    let families = bundled_families();
    let ok_env = env(&families);
    let filled = layer_id(&app.session.documents()[0].doc, "Filled").0;
    let tones = [
        ("levels", json!({"inBlack": 10, "inWhite": 245}), json!({"gamma": 1.4, "outWhite": 240})),
        ("curves", json!({"points": [[0, 0], [128, 150], [255, 255]]}), json!({"points": [[0, 10], [255, 245]]})),
        ("brightnessContrast", json!({"brightness": 10, "contrast": 5}), json!({"contrast": 20})),
        ("exposure", json!({"exposure": 0.5, "offset": 0, "gamma": 1}), json!({"exposure": -0.5})),
        ("vibrance", json!({"vibrance": 20, "saturation": 5}), json!({"saturation": -10})),
        ("hueSaturation", json!({"hue": 10, "saturation": 5, "lightness": 0}), json!({"lightness": 15})),
    ];
    let mut layers = BTreeMap::new();
    for (kind, made, _) in &tones {
        let created = run(&mut app, &format!("layer.newAdjustmentLayer.{kind}"), made.clone());
        layers.insert(*kind, created["layer"].as_u64().expect("a new tone layer"));
    }
    let posterize = run(&mut app, "layer.newAdjustmentLayer.posterize", json!({"levels": 4}))["layer"].as_u64().expect("a posterize layer");
    let (levels, curves) = (layers["levels"], layers["curves"]);

    // Decided before dispatch, with nothing changed.
    let options = options_for(&app, 0);
    for (params, code) in [
        (json!({"layer": levels, "hue": 10}), "invalid_params"),
        (json!({"layer": curves, "gamma": 1.2}), "invalid_params"),
        (json!({"layer": levels, "inWhite": 11}), "invalid_params"),
        (json!({"layer": levels, "inBlack": 244}), "invalid_params"),
        (json!({"layer": posterize, "brightness": 10}), "invalid_params"),
        (json!({"layer": filled, "brightness": 10}), "engine_precheck_failed"),
        (json!({"layer": 999_999_999, "brightness": 10}), "target_not_found"),
        (json!({"layer": levels}), "invalid_params"),
        (json!({"brightness": 10}), "invalid_params"),
    ] {
        assert_refused(&mut app, &ok_env, "layer.setAdjustment", params, options.clone(), code);
    }
    // At the boundary the merged pair keeps two levels apart, and the call reaches the engine.
    let (envelope, ran) = decide(&mut app, &ok_env, "layer.setAdjustment", &json!({"layer": levels, "inBlack": 243}), &options);
    assert_eq!(ran, 1, "inBlack 243 under inWhite 245 is dispatched: {envelope}");

    // Each kind, edited with its own keys: the layer holds what a new layer of the merged values holds.
    for (kind, made, edit) in &tones {
        let mut params = edit.clone();
        params["layer"] = json!(layers[kind]);
        let options = options_for(&app, 0);
        let (envelope, calls) = typed(&mut app, &ok_env, "layer.setAdjustment", params.clone(), options);
        assert_eq!((envelope["outcome"].clone(), envelope["tier"].clone()), (json!("applied"), json!(1)), "{kind} {params}: {envelope}");
        assert_eq!(calls, [("layer.setAdjustment".to_string(), params.clone())], "{kind}: dispatched once, unchanged");
        let state = &app.session.documents()[0];
        assert_eq!(state.history.entries().last().map(String::as_str), Some("Modify Adjustment"), "{kind}: one undoable step");
        let values = merged(made, edit);
        assert!(near(&adjustment(&app, layers[kind]), &new_adjustment(kind, &values)), "{kind}: the layer holds what a new {kind} layer of {values} holds");
    }

    // One step each: a guarded undo leaves the last layer as it was made.
    let options = options_for(&app, 0);
    let (envelope, _) = typed(&mut app, &ok_env, "edit.undo", json!({}), options);
    assert_eq!(envelope["outcome"], "applied", "{envelope}");
    let (_, made, _) = &tones[5];
    assert!(near(&adjustment(&app, layers["hueSaturation"]), &new_adjustment("hueSaturation", made)), "undone: the layer as it was made");
}

/// R1 widen-1 (`act.target.select_modify`): Select › Modify › Expand and Contract by a whole number of
/// pixels, given explicitly (1 to 500, the engine's dialog range), one undoable step each: the
/// selection's bounds move out or in by exactly the radius on every side, and a contracted rectangle
/// is a rectangle again. Without a selection, or in Quick Mask, they are refused before dispatch.
#[test]
fn r1_selection_expand_and_contract_move_the_bounds() {
    let mut app = fixture();
    let families = bundled_families();
    let ok_env = env(&families);
    let rect = json!({"x": 20, "y": 16, "width": 16, "height": 12});
    let modified = [("select.modify.expand", "Expand Selection", [16, 12, 24, 20]), ("select.modify.contract", "Contract Selection", [24, 20, 8, 4])];
    for (command, label, bounds) in modified {
        let options = options_for(&app, 0);
        let (envelope, _) = typed(&mut app, &ok_env, "select.rect", rect.clone(), options);
        assert_eq!(envelope["outcome"], "applied", "{envelope}");
        let options = options_for(&app, 0);
        let (envelope, calls) = typed(&mut app, &ok_env, command, json!({"radius": 4}), options);
        assert_eq!((envelope["outcome"].clone(), envelope["tier"].clone(), calls.len()), (json!("applied"), json!(1), 1), "{command}: {envelope}");
        assert_eq!(selection_bounds(&app), json!(bounds), "{command} by 4: every side moves by 4");
        let state = &app.session.documents()[0];
        assert_eq!(state.history.entries().last().map(String::as_str), Some(label), "{command}: one undoable step");
    }
    // The contracted rectangle, exactly (the selection render's gray PNG).
    let rendered = ok(lts_view_core::render(&app.session, &json!({"document": {"index": 0}, "target": "selection"})), "the selection");
    assert_gray(&rendered.bytes, [24, 20, 8, 4], "the contracted selection");
    // A guarded undo restores the rectangle.
    let options = options_for(&app, 0);
    let (envelope, _) = typed(&mut app, &ok_env, "edit.undo", json!({}), options);
    assert_eq!(envelope["outcome"], "applied", "{envelope}");
    assert_eq!(selection_bounds(&app), json!([20, 16, 16, 12]), "undone: the rectangle");

    // No selection, or Quick Mask: refused before dispatch.
    run(&mut app, "select.deselect", json!({}));
    let options = options_for(&app, 0);
    assert_refused(&mut app, &ok_env, "select.modify.expand", json!({"radius": 4}), options, "engine_precheck_failed");
    run(&mut app, "select.rect", rect);
    run(&mut app, "select.editInQuickMaskMode", json!({"on": true}));
    let options = options_for(&app, 0);
    assert_refused(&mut app, &ok_env, "select.modify.contract", json!({"radius": 4}), options, "engine_precheck_failed");
}

/// R1 widen-1, IN-8 (topology §7.3 `observe.describe.layers`): `describe` passes through the layer and
/// document fields the engine's own inspection computes, so an adapter forwards them instead of
/// dropping them. Per layer (the `full` view and a layer's detail): the fill opacity, the layer style
/// kinds (`effects.items`, `[]` without styles), Blend If and the blending channels when set, the link
/// group, the locks and the vector mask, and for type its paragraph direction and alignment. Per
/// document (every view): the channel list with its alpha and spot channels, the guides and the
/// colour profile. A pure read.
#[test]
fn r1_describe_passes_layer_and_document_fields_through() {
    let mut app = fixture();
    let doc = Arc::clone(&app.session.documents()[0].doc);
    let (filled, caption) = (layer_id(&doc, "Filled").0, layer_id(&doc, "Caption").0);
    for (id, params) in [
        ("layer.setProps", json!({"layer": filled, "fill": 0.5, "locks": {"position": true}, "channels": [true, false, true]})),
        ("layer.layerStyle.blendingOptions", json!({"layer": filled, "blendIf": {"channel": "gray", "thisLayer": [20, 235]}})),
        ("layer.layerStyle.dropShadow", json!({"layer": filled, "opacity": 60, "distance": 2, "size": 2})),
        ("layer.select", json!({"layer": filled})),
        ("layer.layerStyle.stroke", json!({"size": 2, "position": "outside", "color": "#ffffff"})),
        ("layer.select", json!({"layer": caption, "mode": "add"})),
        ("layer.linkLayers", json!({})),
        ("layer.vectorMask.revealAll", json!({"layer": caption})),
        ("type.setStyle", json!({"layer": caption, "direction": "rtl", "align": "right"})),
        ("channel.new", json!({"name": "Saved"})),
        ("channel.newSpot", json!({"name": "Spot", "color": "#00a0ff", "solidity": 40, "fromSelection": false})),
        ("channel.target", json!({"channel": "composite"})),
        ("view.newGuide", json!({"orientation": "horizontal", "position": 12})),
        ("view.newGuide", json!({"orientation": "vertical", "position": 30})),
        ("edit.assignProfile", json!({"profile": "display-p3"})),
    ] {
        run(&mut app, id, params);
    }
    let state = &app.session.documents()[0];
    let mut inspected = BTreeMap::new();
    inspected_by_id(&state.doc.layers, &mut inspected);
    let params = json!({"document": {"index": 0}, "view": "full"});
    let described = ok(assert_pure(&app, "describe, full view", |app| lts_view_core::describe(&app.session, &params)), "describe");

    // The document: the engine's own channel list, the guides and the colour profile.
    let document = &described["document"];
    assert_eq!(document["channels"], photocraft_engine::channel_cmds::channels_json(state), "the engine's channel list");
    let extra = document["channels"]["alpha"].as_array().expect("the alpha and spot channels");
    let listed: Vec<&str> = extra.iter().filter_map(|channel| channel["name"].as_str()).collect();
    assert_eq!(listed, ["Saved", "Spot"], "the alpha and the spot channel");
    assert_eq!(document["guides"], json!({"horizontal": [12.0], "vertical": [30.0]}), "the guides");
    assert_eq!(document["profile"], json!({"present": true, "description": "Display P3"}), "the colour profile");
    let summary = ok(lts_view_core::describe(&app.session, &json!({"document": {"index": 0}})), "the summary view");
    assert_eq!(pick(&summary["document"], &["channels", "guides", "profile"]), pick(document, &["channels", "guides", "profile"]), "in every view");

    // Every layer: the fields the engine's own inspection reports, passed through.
    let mut rows = BTreeMap::new();
    rows_by_id(&document["layers"], &mut rows);
    assert_eq!(rows.keys().collect::<Vec<_>>(), inspected.keys().collect::<Vec<_>>(), "every layer");
    for (id, inspect) in &inspected {
        let row = &rows[id];
        let items = inspect.get("effects").map_or(json!([]), |effects| effects["items"].clone());
        assert_eq!(row["effects"]["items"], items, "layer {id}: the layer style kinds");
        for key in ["blendIf", "channels"] {
            assert_eq!(row.get(key), inspect.get(key), "layer {id}: `{key}` as the engine reports it");
        }
        assert_eq!((&row["linkGroup"], &row["fillOpacity"]), (&inspect["linkGroup"], &inspect["fill"]), "layer {id}: the link group and fill opacity");
    }
    let fill = &rows[&filled];
    let kinds: Vec<&str> = fill["effects"]["items"].as_array().expect("style items").iter().filter_map(|item| item["kind"].as_str()).collect();
    assert_eq!(kinds, ["Drop Shadow", "Stroke"], "the fill's styles, by kind");
    assert_eq!(fill["channels"], json!([true, false, true, true]), "green is left out of the fill's blending");
    assert!(fill["blendIf"].is_array() && fill["locks"]["position"] == true, "Blend If and the position lock: {fill}");
    assert!(fill["linkGroup"].is_u64() && fill["linkGroup"] == rows[&caption]["linkGroup"], "linked with the caption: {fill}");
    let title = &rows[&caption];
    assert!(title["vectorMask"].is_object(), "the caption's vector mask: {title}");
    assert_eq!(pick(&title["text"], &["direction", "align"]), json!({"direction": "rtl", "align": "right"}), "the caption's paragraph");
    let detail = ok(lts_view_core::describe(&app.session, &json!({"document": {"index": 0}, "layerId": filled})), "the fill's detail");
    assert_eq!(&detail["layer"], fill, "a layer's detail carries the same fields");
}

/// R1 widen-1, RD-17 sizes (M18): new documents, crops and resizes go up to 4096 pixels a side, the
/// coordinates of typed parameters with them, and every read takes such a document: a JPEG render, a
/// sample, the agent export. A noisy 4096×2304 image of 28 MB, over R0's 16 MiB input cap and its
/// 2048-pixel bound, is imported and every sampled pixel reads back exactly, although its PNG composite
/// is larger than a read may return (`MAX_OUTPUT_BYTES` bounds what leaves the hook, not what it
/// computes). Placed bytes decode and place up to 4096 pixels a side too, a whole 4096 × 4096 image
/// at 100 % included. Over 4096 pixels or 64 MiB: refused.
#[test]
fn r1_documents_crops_and_resizes_reach_4096_and_stay_observable() {
    let mib = 1024 * 1024;
    assert_eq!((lts_view_core::MAX_INPUT_BYTES, lts_view_core::MAX_PENDING_PLACE_BYTES), (64 * mib, 64 * mib), "RD-17: 64 MiB, one input waiting");
    assert_eq!(lts_view_core::place_queue_refusal(0, 64 * mib), None, "a full-size input waits");
    assert!(lts_view_core::place_queue_refusal(64 * mib, 1).is_some(), "and nothing beside it");

    let mut app = fixture();
    let families = bundled_families();
    let ok_env = env(&families);
    let steps = [
        ("file.new", json!({"width": 4096, "height": 2304, "background": "white", "name": "wide"})),
        ("select.rect", json!({"x": 3000, "y": 2000, "width": 1000, "height": 200})),
        ("type.create", json!({"text": "LTS", "x": 4000, "y": 2200, "size": 48, "color": "#000000"})),
        ("document.pixel", json!({"x": 4095, "y": 2303})),
        ("image.crop", json!({"x": 0, "y": 0, "width": 3840, "height": 2160})),
        ("image.imageSize", json!({"width": 4096, "height": 2304})),
    ];
    for (command, params) in steps {
        let options = if command == "file.new" { json!({}) } else { options_for(&app, 2) };
        let (envelope, calls) = typed(&mut app, &ok_env, command, params.clone(), options);
        assert_eq!((envelope["outcome"].clone(), calls.len()), (json!("applied"), 1), "{command} {params}: {envelope}");
    }
    let state = &app.session.documents()[2];
    assert_eq!((state.doc.size.width, state.doc.size.height), (4096, 2304), "cropped to 3840×2160, then resized to 4096×2304");
    let params = json!({"document": {"index": 2}, "target": "composite", "format": "jpeg", "quality": 80});
    let rendered = ok(lts_view_core::render(&app.session, &params), "a JPEG render");
    assert_eq!(pick(&rendered.info, &["width", "height"]), json!({"width": 4096, "height": 2304}), "a render at 4096 pixels");
    let sampled = ok(lts_view_core::sample(&app.session, &json!({"document": {"index": 2}, "points": [{"x": 4095, "y": 2303}]})), "a sample");
    assert!(sampled["samples"][0]["rgba"].is_array(), "the far corner: {sampled}");
    let exported = ok(lts_view_core::export(&app.session, &json!({"document": {"index": 2}, "format": "jpeg"})), "the agent export");
    assert_eq!(pick(&exported.info, &["width", "height"]), json!({"width": 4096, "height": 2304}), "an export at 4096 pixels");

    // A noisy 4096×2304 image of 28 MB (a stored PNG), imported: every sampled pixel reads back.
    let noise = noise_rgb(4096, 2304);
    let input = stored_png(4096, 2304, noise.clone());
    assert!(input.len() > 16 * mib, "over R0's 16 MiB input cap: {} bytes", input.len());
    let (envelope, calls) = import(&mut app, &ok_env, input, &json!({"name": "noise.png", "newDocument": true}), &json!({}));
    assert_eq!((calls, envelope["outcome"].clone()), (1, json!("applied")), "{envelope}");
    let points: Vec<(u32, u32)> = (0..8).flat_map(|row| (0..8).map(move |col| (col * 585, row * 329))).collect();
    let requested: Vec<Value> = points.iter().map(|(x, y)| json!({"x": x, "y": y})).collect();
    let sampled = ok(lts_view_core::sample(&app.session, &json!({"document": {"index": 3}, "points": requested})), "64 samples of the noise");
    for (sample, (x, y)) in sampled["samples"].as_array().expect("samples").iter().zip(points) {
        let at = ((y * 4096 + x) * 3) as usize;
        assert_eq!(sample["rgba"], json!([noise[at], noise[at + 1], noise[at + 2], 255]), "the noise at ({x}, {y}), exactly");
    }

    // Placed bytes decode up to 4096 pixels a side: a 4096×2 strip, 100 % on the first document's centre.
    run(&mut app, "document.activate", json!({"document": 0}));
    let options = options_for(&app, 0);
    let (envelope, calls) = place(&mut app, &ok_env, solid_png(4096, 2), &json!({"name": "strip.png", "scale": 100, "center": [32, 24]}), &options);
    assert_eq!((calls, envelope["outcome"].clone()), (1, json!("applied")), "{envelope}");
    assert_eq!(envelope["result"]["bounds"], json!([-2016.0, 23.0, 2080.0, 25.0]), "100 % on the centre");
    // And a whole 4096 × 4096 image at 100 % (16,777,216 pixels, which R0's 16-megapixel cap refused).
    let options = options_for(&app, 0);
    let (envelope, calls) = place(&mut app, &ok_env, solid_png(4096, 4096), &json!({"name": "square.png", "scale": 100, "center": [32, 24]}), &options);
    assert_eq!((calls, envelope["outcome"].clone()), (1, json!("applied")), "{envelope}");
    assert_eq!(envelope["result"]["bounds"], json!([-2016.0, -2024.0, 2080.0, 2072.0]), "4096 × 4096 at 100 % on the centre");

    // Over 4096 pixels a side every read refuses, from the header.
    let (too_wide, _) = add_document(&mut app, "too wide", 4097, 1, 1);
    let document = json!({"id": too_wide});
    let render = lts_view_core::render(&app.session, &json!({"document": document, "target": "composite"}));
    assert_eq!(code(render, "a render 4097 pixels wide"), "budget_exceeded");
    let sample = lts_view_core::sample(&app.session, &json!({"document": document, "points": [{"x": 0, "y": 0}]}));
    assert_eq!(code(sample, "a sample 4097 pixels wide"), "budget_exceeded");
    let export = lts_view_core::export(&app.session, &json!({"document": document}));
    assert_eq!(code(export, "an export 4097 pixels wide"), "budget_exceeded");
}

/// RD-17 import (M9, M18; the plan's `jpeg_import_4096`): `placeBytes` with `newDocument: true` opens a
/// PNG or JPEG of up to 4096 pixels a side and 64 MiB as a new document through the engine's own
/// import, with the typed discipline: one call, every `not_applied` code decided before the open, the
/// new document the target (no revision before it), even in an editor with no document. The document
/// holds the image's upright pixels and the positive File Info fields of OC-13 and OD-14 (creator,
/// copyright, credit, caption), never its EXIF, GPS, camera or location data nor its print resolution
/// (an agent document is 72 ppi: pt = px), so no checkpoint of it carries them. The metadata read,
/// samples, a JPEG render and the agent export (OC-13: creator and copyright) observe it. A Display P3
/// JPEG of 4096 × 4096, RD-17's largest import, keeps its profile (the engine's Color Settings preserve
/// it, and `describe` and the metadata read name it) and its own pixels, not converted.
#[test]
fn jpeg_import_4096() {
    let families = bundled_families();
    let ok_env = env(&families);
    let jpeg = camera_jpeg_of(4096, 2304);
    let source = photocraft_codecs::decode(&jpeg).expect("the camera JPEG decodes");
    assert_eq!((source.meta.dpi, source.meta.exif.is_some()), (Some((300.0, 300.0)), true), "a 300 ppi camera JPEG with EXIF");
    let mut app = PhotocraftApp::new(Session::new(), Default::default());
    let new = json!({"name": "wide.jpg", "newDocument": true});
    let (envelope, calls) = import(&mut app, &ok_env, jpeg.clone(), &new, &json!({}));
    assert_eq!((calls, envelope["outcome"].clone()), (1, json!("applied")), "{envelope}");
    let (id, revision) = (doc_id(&app, 0), rev(&app, 0));
    let expected = json!({"resultVersion": TYPED, "tier": 1, "documentId": id, "revisionBefore": null, "revisionAfter": revision});
    assert_eq!(pick(&envelope, &["resultVersion", "tier", "documentId", "revisionBefore", "revisionAfter"]), expected);
    assert!(app.session.authorize.is_none(), "the gate is removed after the call");
    let state = &app.session.documents()[0];
    assert_eq!((app.session.active_index(), state.doc.name.as_str()), (Some(0), "wide.jpg"), "a new active document named after the file");
    assert_eq!((state.doc.size.width, state.doc.size.height, state.doc.layer_count()), (4096, 2304, 1), "the image at its own size");
    assert_eq!(state.doc.resolution_dpi, 72.0, "no print resolution: an agent document is 72 ppi");
    assert!(state.doc.metadata.exif.is_none(), "no EXIF");
    let xmp = state.doc.metadata.xmp.clone().unwrap_or_default();
    for private in PRIVATE {
        assert!(!xmp.contains(private), "`{private}` reached the document: {xmp}");
    }
    let info = photocraft_engine::file_cmds::read_file_info(Some(xmp.as_str()));
    let kept = json!({"author": "LTS Photographer", "copyright": "© 2026 LTS Agency", "copyrightStatus": "copyrighted", "description": "A fixture caption"});
    assert_eq!(pick(&info, &["author", "copyright", "copyrightStatus", "description"]), kept, "the positive fields: {xmp}");
    assert!(xmp.contains("<photoshop:Credit>LTS Agency</photoshop:Credit>"), "the credit line: {xmp}");

    // Observed: the metadata read, 64 samples (the JPEG's own pixels), a JPEG render, the agent export.
    let read = ok(lts_view_core::metadata(&app.session, &json!({"document": {"id": id}})), "the metadata read");
    assert_eq!((&read["exif"]["present"], &read["xmp"]["gps"]), (&json!(false), &json!(false)), "{read}");
    let pixels = rgba(&jpeg);
    let points: Vec<(u32, u32)> = (0..8).flat_map(|row| (0..8).map(move |col| (col * 585, row * 329))).collect();
    let requested: Vec<Value> = points.iter().map(|(x, y)| json!({"x": x, "y": y})).collect();
    let sampled = ok(lts_view_core::sample(&app.session, &json!({"document": {"id": id}, "points": requested})), "64 samples of the import");
    for (sample, (x, y)) in sampled["samples"].as_array().expect("samples").iter().zip(points) {
        assert_eq!(sample["rgba"], json!(pixels.2[(y * 4096 + x) as usize]), "the JPEG's own pixel at ({x}, {y})");
    }
    let params = json!({"document": {"id": id}, "target": "composite", "format": "jpeg", "quality": 80});
    let rendered = ok(lts_view_core::render(&app.session, &params), "a JPEG render at 4096 pixels");
    assert_eq!(pick(&rendered.info, &["width", "height"]), json!({"width": 4096, "height": 2304}));
    let exported = ok(lts_view_core::export(&app.session, &json!({"document": {"id": id}, "format": "jpeg", "quality": 85})), "the agent export");
    let decoded = photocraft_codecs::decode(&exported.bytes).expect("a decodable export");
    assert_eq!((decoded.width(), decoded.height()), (4096, 2304));
    assert!(decoded.meta.exif.is_none() && decoded.meta.dpi.is_none(), "no EXIF and no resolution: {:?}", decoded.meta.dpi);
    let written = decoded.meta.xmp.unwrap_or_default();
    let info = photocraft_engine::file_cmds::read_file_info(Some(written.as_str()));
    assert_eq!(pick(&info, &["author", "copyright"]), json!({"author": "LTS Photographer", "copyright": "© 2026 LTS Agency"}), "OC-13: {written}");
    assert!(!written.contains("Credit"), "the credit line only on request (`newsroom`): {written}");

    // A Display P3 JPEG of 4096 × 4096 (16,777,216 pixels): its profile is kept and named, and its
    // pixels are its own, not converted to the working space.
    let p3 = tagged_jpeg_of(4096, 4096, display_p3());
    let (envelope, calls) = import(&mut app, &ok_env, p3.clone(), &json!({"name": "square.jpg", "newDocument": true}), &json!({}));
    assert_eq!((calls, envelope["outcome"].clone()), (1, json!("applied")), "{envelope}");
    let color = pick(&envelope["result"]["color"], &["embedded", "action"]);
    assert_eq!(color, json!({"embedded": "Display P3", "action": "kept"}), "the engine's colour policy: {envelope}");
    let index = app.session.active_index().expect("the new document is active");
    let state = &app.session.documents()[index];
    assert_eq!((state.doc.size.width, state.doc.size.height, state.doc.name.as_str()), (4096, 4096, "square.jpg"), "the image at its own size");
    let document = json!({"id": state.doc.id.0.to_string()});
    let described = ok(lts_view_core::describe(&app.session, &json!({"document": document})), "describe");
    assert_eq!(described["document"]["profile"], json!({"present": true, "description": "Display P3"}), "the colour profile");
    let read = ok(lts_view_core::metadata(&app.session, &json!({"document": document})), "the metadata read");
    let profile = json!({"present": true, "description": "Display P3", "colorSpace": "Rgb"});
    assert_eq!(pick(&read["icc"], &["present", "description", "colorSpace"]), profile, "{read}");
    let pixels = rgba(&p3);
    let corners = [(0, 0), (4095, 0), (0, 4095), (4095, 4095), (2048, 2048)];
    let requested: Vec<Value> = corners.iter().map(|(x, y)| json!({"x": x, "y": y})).collect();
    let sampled = ok(lts_view_core::sample(&app.session, &json!({"document": document, "points": requested})), "samples of the P3 import");
    for (sample, (x, y)) in sampled["samples"].as_array().expect("samples").iter().zip(corners) {
        assert_eq!(sample["rgba"], json!(pixels.2[(y * 4096 + x) as usize]), "the JPEG's own pixel at ({x}, {y}), not converted");
    }

    // Refused before the open, with nothing changed: over 4096 pixels or 64 MiB, CMYK, a registration,
    // document preconditions (the import creates its target), a malformed mode, a path.
    let current = rev(&app, 0);
    for (bytes, params, options, failure) in [
        (solid_png(4097, 1), new.clone(), json!({}), "budget_exceeded"),
        (vec![0; 64 * 1024 * 1024 + 1], new.clone(), json!({}), "input_rejected"),
        (cmyk_jpeg(), new.clone(), json!({}), "input_rejected"),
        (jpeg.clone(), with(new.clone(), "scale", json!(100)), json!({}), "invalid_params"),
        (jpeg.clone(), with(new.clone(), "center", json!([2048, 1152])), json!({}), "invalid_params"),
        (jpeg.clone(), with(new.clone(), "region", json!([0, 0, 4096, 2304])), json!({}), "invalid_params"),
        (jpeg.clone(), with(new.clone(), "newDocument", json!("yes")), json!({}), "invalid_params"),
        (jpeg.clone(), with(new.clone(), "name", json!("../wide.jpg")), json!({}), "invalid_params"),
        (jpeg.clone(), new.clone(), json!({"document": {"index": 0}}), "invalid_params"),
        (jpeg.clone(), new.clone(), json!({"rev": current}), "invalid_params"),
    ] {
        assert_place_refused(&mut app, &ok_env, bytes, params, options, failure);
    }
    // Eight documents open: the import is refused before it opens a ninth.
    while app.session.documents().len() < 8 {
        run(&mut app, "file.new", json!({"width": 8, "height": 8}));
    }
    assert_place_refused(&mut app, &ok_env, jpeg, new, json!({}), "document_limit");
}
