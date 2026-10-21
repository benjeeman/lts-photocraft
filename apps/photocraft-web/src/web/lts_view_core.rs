//! The pure core of `window.photocraftView`, the LTS agent view of the visible editor (final
//! design Appendix A.2; implementation plan §1 FP1).
//!
//! Reads (`describe`, `render`, `sample`, `fonts`, `metadata`, `watermark`) borrow the session
//! immutably and change no document, selection, history, journal, UI state or session setting
//! (INV-18). Renders are bounded from the document header before any pixel is allocated (INV-28)
//! and carry none of the document's EXIF or XMP. Read failures are typed `ViewError`s.
//!
//! `execute` is the typed mutation path: a reviewed policy table with exact keys, bounds and
//! tiers (INV-12, INV-13), and every `not_applied` code decided before the run function is
//! called (INV-4, INV-41). While the run function runs, the session gate is `authorize_step`
//! over the same table, script events and background jobs are off, and all three are restored
//! afterwards. Undo and redo need the history head the caller expects (INV-34); an explicit font
//! family must be loaded and the font set must be the one probed (INV-35); a busy editor refuses
//! (INV-58).
//!
//! The module uses external crates only, so `tests/lts_view_core.rs` builds it natively.
//!
//! R0 (master plan v2 §3.1, before the one Photo WASM build) declares the newsroom families as
//! tier B entries of the same table, each with a subset of the keys its command's own params doc
//! names (cited by file and line), and adds three tier A hooks. `export` is the agent export
//! through the FP1 encoder: never EXIF, GPS, camera or location data, only the positive fields of
//! the metadata policy (OC-13 `copyright` by default; IPTC credit and caption on request), JPEG
//! quality and `maxBytes`. `place_bytes` places encoded bytes through the engine's own
//! `file_cmds::place_bytes` with the typed execute discipline (composites, region restore): an
//! explicit scale and centre, a restore `region` that pins the registration, size caps decided
//! before dispatch, and the input re-encoded to a metadata-free PNG first. `text_check` reports a
//! type layer's line ranges as `type.info` lays them out (overflow, tofu). Never in the table:
//! Content-Aware Scale, path commands, tier 3.
//!
//! R1 (master plan v3 W3, widen-1: M6, M9, M18, RD-12, RD-17, IN-8) adds to the same table Layer ›
//! Create and Release Clipping Mask, Adjustment Properties for the six tone kinds it creates
//! (`layer.setAdjustment`: a value of another kind, or one the engine would change silently, is
//! refused before dispatch), Select › Modify › Expand and Contract, `direction` on `type.create`
//! (required when the text has a right-to-left letter, RD-12) and the fill opacity (`fill`) on
//! `layer.setProps`. The fill is narrowed until an engine bug is fixed (OC-18): the engine lays an
//! outside or centre stroke of any layer but a shape layer beneath the layer's whole shape, which a
//! fill below 100 % shows where Photoshop shows what lies below, so that pair is refused in either
//! order (`stroke_under_shape`). No key takes a weight (RD-12). RD-17 sizes: documents, crops,
//! resizes, placements, typed coordinates and reads up to 4096 pixels a side, and 64 MiB of
//! `placeBytes` input, which also imports (`newDocument: true`): the bytes open as a new document
//! with their pixels, colour profile and positive File Info fields only. `describe` passes through
//! the layer and document fields the engine's own inspection computes (IN-8). Writes stay on the
//! two hooks, `execute` and `placeBytes`.
//!
//! One table serves the browser hook and a native worker that compiles this file (media agent
//! contracts §15): every entry says where it may run (`Runtimes`), and work that would hold the
//! page's only thread too long is refused in the browser as `native_only`.

use std::ops::RangeInclusive;
use std::sync::Arc;

use photocraft_codecs::{ChannelLayout, CodecError, DecodeOptions, EncodeOptions, Format, Image, Limits, Metadata, PngCompression};
use photocraft_doc::adjust::ToneSpace;
use photocraft_doc::text::{ParagraphRun, ParagraphStyle, TextAlign, TextDirection};
use photocraft_doc::variables::Value as VariableValue;
use photocraft_doc::{Adjustment, BlendMode, ColorMode, DocId, Document, Fill, Layer, LayerContent, LayerId, LayerMask, Rect, Surface, TextLayer};
use photocraft_engine::channel_cmds::ChannelTarget;
use photocraft_engine::{DocState, EngineError, Session};
use photocraft_text::TextEngine;
use photocraft_ui_egui::PhotocraftApp;
use serde_json::{Map, Value, json};

/// The version of the typed execute envelope (design Appendix A.2).
pub const RESULT_VERSION: &str = "photocraft.typed-execute.v1";

/// Renders and samples: at most this many pixels per side (RD-17) and layers per document.
const MAX_SIDE: u32 = 4096;
const MAX_LAYERS: usize = 64;
/// Documents an agent may have open (`file.new` refuses beyond it).
const MAX_DOCUMENTS: usize = 8;
const MAX_POINTS: usize = 64;
/// Document rows in `describe` and `watermark`, and layer rows in `describe`.
const MAX_DOCUMENT_ROWS: usize = 32;
const MAX_LAYER_ROWS: usize = 64;
const MAX_TEXT_BYTES: usize = 4096;
const MAX_NAME_BYTES: usize = 128;
const MAX_PARAMS_BYTES: usize = 16 * 1024;
const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_MESSAGE_CHARS: usize = 300;
/// Typed parameter bounds: coordinates and sides in document pixels (RD-17: new documents, crops
/// and resizes up to 4096 pixels a side), layer moves.
const COORD: i64 = 4096;
const SIDE: i64 = 4096;
const SHIFT: i64 = 4096;
/// R0 bounds: the bytes a `placeBytes` call carries and where it may centre them, a Content-Aware
/// Fill hole (its bounding box), metadata field values, the lines a `textCheck` lists, character
/// offsets, and Image › Variables templates.
///
/// The bytes: RD-17's 64 MiB (master plan v3; R0 took the 16 MiB of `agent_files::MAX_BYTES`,
/// final design §8.1), in the browser hook and a native worker alike: this hook's own bound. They
/// arrive by handle, never inside the LTS command: the command names a staged input by its sha256,
/// and the lab parent fetches the bytes, checks the sha256 and posts them to the frame beside the
/// command (final design §2.4, §2.5 `input.bytes`, §8.1; INV-47), so the broker's 1 MiB command cap
/// does not bound them (lts-creative/src/lib.rs:595). The stages before this hook do bound them,
/// below RD-17 until its follow-up raises them (lts-broker, lts-web-adapters): the broker stages at
/// most 32 MiB of any input and admits at most 16 MiB as a Photo `import_bytes` handle
/// (lts-creative/src/inputs.rs:35, :45, :456), and the Photo adapter passes at most 16 MiB
/// (web/creative-photocraft-adapter.mjs:67, :280). Through `creative_execute` a Photo input is
/// therefore 16 MiB at most today. A whole BB frame (541-934 KB, critic correction CC-27) fits
/// every stage; a 4096-pixel photo fits this hook. The 4096-pixel decode bound and the placement
/// cap below bound the memory one call uses.
pub const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;
/// The `placeBytes` input that may wait in the browser hook's queue at once, all requests together:
/// one image at Photo's input cap. Each waiting input is a copy in the page's memory until a frame
/// places it, so two full-size inputs never wait together (`place_queue_refusal`).
pub const MAX_PENDING_PLACE_BYTES: usize = MAX_INPUT_BYTES;
const MAX_CENTER: f64 = 4096.0;
/// A placed image once scaled: the engine resamples the whole image before it moves it
/// (crates/engine/src/file_cmds.rs:379-385) into a surface of the scaled size
/// (crates/algo/src/resample.rs:157-159), inline on the page. RD-17's 4096 pixels a side, so at
/// most a whole 4096 × 4096 image, as an import or a new document takes (R0 also capped placements
/// at 16,000,000 pixels, which refused a 4096 × 4096 image at 100 %).
const MAX_PLACED_SIDE: f64 = 4096.0;
/// The Content-Aware Fill hole the browser hook fills inline on the page's only thread, and the
/// larger one a native worker fills (`native_only` in the browser between the two).
const MAX_HOLE_SIDE: u32 = 512;
const MAX_HOLE_PIXELS: u64 = 65_536;
const MAX_NATIVE_HOLE_SIDE: u32 = 1024;
const MAX_NATIVE_HOLE_PIXELS: u64 = 262_144;
/// Type sizes in points (`type.create` and `type.setStyle` alike), tracking in thousandths of an
/// em and leading in points, sized in R0 for documents of at most 2048 pixels a side and kept for
/// RD-17's 4096. The engine takes 0.1 to 1296 pt and tracking up to 10,000 (type_cmds.rs:507),
/// whose rasters only the text crate's 256-megapixel guard bounds (crates/text/src/render.rs:20).
const MIN_TYPE_PT: f64 = 1.0;
const MAX_TYPE_PT: f64 = 256.0;
const MAX_TRACKING: i64 = 1000;
const MAX_LEADING_PT: f64 = 1000.0;
const MAX_FIELD_BYTES: usize = 2000;
const MAX_LINE_ROWS: usize = 256;
const MAX_CHARS: i64 = 100_000;
const MAX_VARIABLES: usize = 32;
const MAX_DATA_SETS: usize = 64;
const MAX_VARIABLE_NAME_BYTES: usize = 64;
/// R1 bounds: Select › Modify › Expand and Contract in whole pixels (the engine's dialog range,
/// selection_cmds.rs:226-237), the guides `describe` lists per orientation, and what a Levels level
/// may lose to its storage as a fraction of 255 (adjust_params.rs:435-439).
const MAX_RADIUS: i64 = 500;
const MAX_GUIDES: usize = 256;
const LEVEL_EPSILON: f32 = 1e-3;

/// `agent_context::validate`: the other owner's bridge validator, called rather than copied.
pub type Validate = fn(&str, &Value, &Session) -> Result<(), String>;

/// The function that dispatches a validated command (the glue passes `PhotocraftApp::run`).
pub type RunFn<'a> = dyn FnMut(&mut PhotocraftApp, &str, Value) -> Result<Value, String> + 'a;

/// What the glue knows when it drains a typed request.
pub struct Env<'a> {
    /// The page is hidden.
    pub hidden: bool,
    /// The request's deadline passed while it waited in the queue.
    pub expired: bool,
    /// `agent_context::busy`: a manual edit, dialog or pointer input is in progress.
    pub busy: bool,
    /// The font families registered in this page.
    pub families: &'a [String],
    /// `agent_context::validate`.
    pub validate: Validate,
    /// Where the call runs: the browser hook passes [`Runtime::HERE`], a native worker
    /// `Runtime::Native`.
    pub runtime: Runtime,
}

/// Where a policy entry, or one of its values, may run (media agent contracts §15).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runtimes {
    /// In the browser hook and in a native worker.
    Both,
    /// In a native worker only: work that would hold the page's only thread for too long.
    NativeOnly,
}

impl Runtimes {
    /// Whether work marked so runs in `runtime` (the browser refuses it as `native_only`).
    pub fn allows(self, runtime: Runtime) -> bool {
        self == Runtimes::Both || runtime == Runtime::Native
    }
}

/// Where this core runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runtime {
    /// `window.photocraftView` in the browser (wasm32).
    Browser,
    /// A native worker that hosts a headless `PhotocraftApp` (and this file's native tests).
    Native,
}

impl Runtime {
    /// The runtime this build of the file serves: the browser hook on wasm32, a native worker on
    /// every other target.
    pub const HERE: Runtime = if cfg!(target_arch = "wasm32") { Runtime::Browser } else { Runtime::Native };
}

/// An encoded image and what it shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    pub bytes: Vec<u8>,
    pub info: Value,
}

/// A typed read failure: `read_failed`, `unsupported_read` or `budget_exceeded`.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewError {
    pub code: &'static str,
    pub message: String,
}

/// One command of the typed policy table: its exact parameter keys, its tier (1: undoable or view
/// state; 2: destructive or outside the history, restored from a checkpoint the broker pins),
/// where it may run, and its bounds.
pub struct PolicyEntry {
    pub id: &'static str,
    pub keys: &'static [&'static str],
    pub tier: u8,
    pub runtimes: Runtimes,
    check: fn(&Params) -> Result<(), String>,
}

type Params = Map<String, Value>;

/// An entry that runs in the browser hook and in a native worker alike (every entry today; work too
/// long for the page is refused by value, as a large Content-Aware Fill hole is in `check_limits`).
const fn entry(id: &'static str, keys: &'static [&'static str], tier: u8, check: fn(&Params) -> Result<(), String>) -> PolicyEntry {
    PolicyEntry { id, keys, tier, runtimes: Runtimes::Both, check }
}

/// The legacy 13 commands plus the newsroom set (implementation plan §1 FP1), R0's tier B newsroom
/// families (master plan v2 §3.1) and R1's widen-1 (master plan v3 W3). Tier 3 (preferences,
/// plug-ins, scripts, automation, ambient files) is absent, and so are Content-Aware Scale and the
/// path commands.
pub static POLICY: &[PolicyEntry] = &[
    entry("document.activate", &["document"], 1, check_activate),
    entry("document.pixel", &["x", "y"], 1, check_pixel),
    entry("edit.fill", &["color", "target"], 1, check_fill),
    entry("edit.redo", &[], 1, check_nothing),
    entry("edit.undo", &[], 1, check_nothing),
    entry("file.new", &["width", "height", "background", "name"], 1, check_new_document),
    entry("filter.blur.gaussianBlur", &["radius"], 2, check_blur),
    entry("filter.pixelate.mosaic", &["cellSize"], 2, check_mosaic),
    entry("image.crop", &["x", "y", "width", "height"], 2, check_rect),
    entry("image.imageSize", &["width", "height"], 2, check_image_size),
    entry("layer.duplicate", &["layer"], 1, check_layer),
    entry("layer.layerMask.hideAll", &["layer"], 1, check_mask),
    entry("layer.layerMask.hideSelection", &["layer"], 1, check_mask),
    entry("layer.layerMask.revealAll", &["layer"], 1, check_mask),
    entry("layer.layerMask.revealSelection", &["layer"], 1, check_mask),
    entry("layer.new.layer", &["name"], 1, check_new_layer),
    entry("layer.newFillLayer.solidColor", &["color"], 1, check_fill_layer),
    entry("layer.select", &["layer"], 1, check_layer),
    // R1 adds the fill opacity, Layer Properties' own `fill` (commands.rs:505, :526-528).
    entry("layer.setProps", &["layer", "name", "visible", "opacity", "blend", "fill"], 1, check_set_props),
    entry("layer.translate", &["layer", "dx", "dy"], 1, check_translate),
    entry("select.all", &[], 1, check_nothing),
    entry("select.deselect", &[], 1, check_nothing),
    entry("select.inverse", &[], 1, check_nothing),
    entry("select.rect", &["x", "y", "width", "height", "ellipse", "mode", "feather"], 1, check_select_rect),
    // R1 adds Hebrew `direction` (M6, RD-12): `type.create` applies the paragraph keys
    // `type.setStyle` documents (`apply_para_props`, type_cmds.rs:252-292, :534).
    entry("type.create", &["text", "x", "y", "size", "font", "color", "name", "align", "box", "direction"], 1, check_type),
    // R0 tier B (best effort until L2 GREEN). Each entry's keys are a subset of the keys its own
    // params doc (`CommandSpec::params`) names, at crates/engine/src/<file>:<lines>.
    // Selections: smartselect_cmds.rs:245-252 (subject), 253-260 (Select and Mask), 237-244 (the
    // GrabCut object selection), 229-236 (Quick Selection); selection_cmds.rs:345-352 (Color Range).
    entry("select.subject", &["sampleAllLayers", "mode"], 1, check_subject),
    entry(
        "select.refineEdge",
        &["radius", "smartRadius", "smooth", "feather", "contrast", "shiftEdge", "decontaminate", "amount", "output", "sampleAllLayers"],
        1,
        check_refine_edge,
    ),
    entry("select.object", &["rect", "mode", "sampleAllLayers"], 1, check_object),
    entry("select.quick", &["points", "size", "mode", "sampleAllLayers", "enhanceEdge"], 1, check_quick),
    entry("select.colorRange", &["select", "color", "points", "fuzziness", "invert", "sampleAllLayers", "mode"], 1, check_color_range),
    // Content-Aware Fill onto a new layer only, its hole capped by runtime (`check_limits`):
    // edit_menu_cmds.rs:951-959.
    entry(
        "edit.contentAwareFill",
        &["sampling", "margin", "colorAdaptation", "rotationAdaptation", "scale", "mirror", "output", "seed"],
        1,
        check_content_aware_fill,
    ),
    // Layer styles (subset): layer_style.rs:620-624 (drop shadow), 640-644 (stroke, whose doc has
    // no `layer`: the active layer), 692-712 (clear).
    entry("layer.layerStyle.dropShadow", &["layer", "color", "opacity", "angle", "useGlobalLight", "distance", "spread", "size"], 1, check_drop_shadow),
    entry("layer.layerStyle.stroke", &["size", "position", "color", "opacity"], 1, check_stroke),
    entry("layer.layerStyle.clear", &["layer"], 1, check_mask),
    // Typography depth: type_cmds.rs:566-571 (type.edit), 686-694 with CHAR_PARAMS at 507
    // (type.setStyle, including right-to-left paragraphs).
    entry("type.edit", &["layer", "text", "replace", "box", "point", "move", "name"], 1, check_type_edit),
    entry("type.setStyle", &["layer", "range", "font", "size", "color", "tracking", "leading", "fauxBold", "align", "direction"], 1, check_type_style),
    // Tone layers: the ADJ table at commands.rs:902-955 (brightnessContrast 903, levels 904-908,
    // curves 909-913, exposure 914, vibrance 915, hueSaturation 916-920), ids built at 956-958.
    entry("layer.newAdjustmentLayer.brightnessContrast", &["brightness", "contrast"], 1, check_brightness_contrast),
    entry("layer.newAdjustmentLayer.levels", &["inBlack", "gamma", "inWhite", "outBlack", "outWhite"], 1, check_levels),
    entry("layer.newAdjustmentLayer.curves", &["points"], 1, check_curves),
    entry("layer.newAdjustmentLayer.exposure", &["exposure", "offset", "gamma"], 1, check_exposure),
    entry("layer.newAdjustmentLayer.vibrance", &["vibrance", "saturation"], 1, check_vibrance),
    entry("layer.newAdjustmentLayer.hueSaturation", &["hue", "saturation", "lightness"], 1, check_hue_saturation),
    // Enhance (destructive): edit_cmds.rs:527-529 (auto tone, contrast, colour), adjust_cmds.rs:352-359.
    entry("image.autoTone", &[], 2, check_nothing),
    entry("image.autoContrast", &[], 2, check_nothing),
    entry("image.autoColor", &[], 2, check_nothing),
    entry(
        "image.adjustments.shadowsHighlights",
        &["shadowAmount", "shadowTone", "shadowRadius", "highlightAmount", "highlightTone", "highlightRadius", "color", "midtone"],
        2,
        check_shadows_highlights,
    ),
    // Text and visibility templates (Image › Variables): variables_cmds.rs:394-400 (define),
    // 401-407 (data sets), 408-414 (apply); never pixel replacement, which reads image files.
    // Define and Data Sets change the document without a history step (variables_cmds.rs:100-108):
    // tier 2, so the broker pins a checkpoint first (a `.pcraft` keeps the variables), because an
    // undo cannot take them back and a guarded undo would take back the step before them.
    entry("image.variables.define", &["defs"], 2, check_define),
    entry("image.variables.dataSets", &["dataSets", "append"], 2, check_data_sets),
    entry("image.applyDataSet", &["name", "index"], 1, check_apply_data_set),
    // R1 widen-1 (master plan v3 W3), one undoable step each: Layer › Create and Release Clipping
    // Mask (commands.rs:575-590), Adjustment Properties for the six tone kinds above
    // (commands.rs:687-705: the values given merge over the layer's own, and none would reset it),
    // and Select › Modify › Expand and Contract (selection_cmds.rs:355-356).
    entry("layer.createClippingMask", &["layer"], 1, check_layer),
    entry("layer.releaseClippingMask", &["layer"], 1, check_layer),
    entry("layer.setAdjustment", SET_ADJUSTMENT_KEYS, 1, check_set_adjustment),
    entry("select.modify.expand", &["radius"], 1, check_modify),
    entry("select.modify.contract", &["radius"], 1, check_modify),
];

/// `layer.setAdjustment`'s keys: the layer and the keys of the six tone kinds the table creates,
/// each of which takes only its own kind's keys (`tone_keys`).
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

/// The `placeBytes` hook (R0 tier A): encoded PNG, JPEG or WebP bytes placed as an embedded smart
/// object above the active layer through the engine's own `file_cmds::place_bytes`
/// (crates/engine/src/file_cmds.rs:364-398), one undoable step, for composites and region
/// restore. `scale` and `center` are File › Place Embedded's own keys (file_cmds.rs:1131-1139),
/// both required so the engine never fits or centres an image on its own (file_cmds.rs:370-378);
/// `path` stays out (the bytes cross the hook instead, so that command stays out of the table) and
/// so does `fit`, which an explicit scale overrides. `region` pins a restore's registration
/// (`check_place`); `name` is `place_bytes`'s file name (the layer's name; never a path).
///
/// R1 (RD-17) adds the import, `newDocument: true` (`is_import`): the bytes open as a new document
/// named `name`, through the engine's own import (`file_cmds::open_bytes_as`), with no
/// registration (`scale`, `center` and `region` are refused) and no document preconditions.
pub static PLACE_BYTES: PolicyEntry = entry("placeBytes", &["name", "scale", "center", "region", "newDocument"], 1, check_place);

/// The `export` read's keys and the positive metadata fields it may write (OC-13; OD-14).
pub const EXPORT_KEYS: &[&str] = &["document", "format", "quality", "maxBytes", "metadata", "removeLocation", "fields"];
pub const EXPORT_FIELDS: &[&str] = &["creator", "copyright", "copyrightStatus", "copyrightUrl", "credit", "caption"];
/// OC-13 `copyright`: the creator and copyright fields only.
const COPYRIGHT_FIELDS: &[&str] = &["creator", "copyright", "copyrightStatus", "copyrightUrl"];
/// The `textCheck` read's keys.
pub const TEXT_CHECK_KEYS: &[&str] = &["document", "layerId"];

// ------------------------------------------------------------------ policy

fn policy(id: &str) -> Option<&'static PolicyEntry> {
    POLICY.iter().find(|entry| entry.id == id)
}

/// Exact keys (an object of at most 16 KiB with no key outside the entry) and the entry's bounds.
fn check_params(entry: &PolicyEntry, params: &Value) -> Result<(), String> {
    let map = params.as_object().ok_or("parameters must be a JSON object")?;
    if params.to_string().len() > MAX_PARAMS_BYTES {
        return Err(format!("parameters are limited to {MAX_PARAMS_BYTES} bytes"));
    }
    if let Some(key) = map.keys().find(|key| !entry.keys.contains(&key.as_str())) {
        return Err(format!("unknown parameter `{}` for `{}`", bounded(key), entry.id));
    }
    (entry.check)(map)
}

/// The session gate while a typed call runs: every step the session dispatches, including steps a
/// command or a recorded action runs on its own behalf, must pass the same table with the same
/// bounds, in the runtime of this build, before any side effect.
pub fn authorize_step(id: &str, params: &Value) -> photocraft_engine::Result<()> {
    let entry = policy(id).ok_or_else(|| EngineError::Other(format!("`{}` is not allowed during a typed agent call", bounded(id))))?;
    if !entry.runtimes.allows(Runtime::HERE) {
        return Err(EngineError::Other(format!("`{}` runs in a native worker only", bounded(id))));
    }
    check_params(entry, params).map_err(EngineError::Other)
}

fn required(p: &Params, key: &str) -> Result<(), String> {
    if p.contains_key(key) { Ok(()) } else { Err(format!("`{key}` is required")) }
}

fn integer(p: &Params, key: &str, range: RangeInclusive<i64>) -> Result<(), String> {
    match p.get(key) {
        None => Ok(()),
        Some(value) if value.as_i64().is_some_and(|n| range.contains(&n)) => Ok(()),
        Some(_) => Err(format!("`{key}` must be an integer from {} to {}", range.start(), range.end())),
    }
}

fn number(p: &Params, key: &str, min: f64, max: f64) -> Result<(), String> {
    match p.get(key) {
        None => Ok(()),
        Some(value) if value.as_f64().is_some_and(|n| n.is_finite() && (min..=max).contains(&n)) => Ok(()),
        Some(_) => Err(format!("`{key}` must be a number from {min} to {max}")),
    }
}

fn text(p: &Params, key: &str, max: usize) -> Result<(), String> {
    match p.get(key) {
        None => Ok(()),
        Some(Value::String(s)) if s.len() <= max => Ok(()),
        Some(_) => Err(format!("`{key}` must be a string of at most {max} UTF-8 bytes")),
    }
}

fn color(p: &Params, key: &str) -> Result<(), String> {
    match p.get(key) {
        None => Ok(()),
        Some(Value::String(s)) if is_hex_color(s) => Ok(()),
        Some(_) => Err(format!("`{key}` must be a #rrggbb colour")),
    }
}

fn flag(p: &Params, key: &str) -> Result<(), String> {
    match p.get(key) {
        None | Some(Value::Bool(_)) => Ok(()),
        Some(_) => Err(format!("`{key}` must be a boolean")),
    }
}

fn choice(p: &Params, key: &str, allowed: &[&str]) -> Result<(), String> {
    match p.get(key) {
        None => Ok(()),
        Some(Value::String(s)) if allowed.contains(&s.as_str()) => Ok(()),
        Some(_) => Err(format!("`{key}` must be one of {}", allowed.join(", "))),
    }
}

fn layer_key(p: &Params) -> Result<(), String> {
    match p.get("layer") {
        None => Ok(()),
        Some(value) if value.as_u64().is_some() => Ok(()),
        Some(_) => Err("`layer` must be a layer id".into()),
    }
}

fn is_hex_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s.bytes().skip(1).all(|b| b.is_ascii_hexdigit())
}

fn check_nothing(_: &Params) -> Result<(), String> {
    Ok(())
}

fn check_activate(p: &Params) -> Result<(), String> {
    required(p, "document")?;
    integer(p, "document", 0..=7)
}

fn check_pixel(p: &Params) -> Result<(), String> {
    required(p, "x")?;
    required(p, "y")?;
    integer(p, "x", 0..=COORD)?;
    integer(p, "y", 0..=COORD)
}

fn check_fill(p: &Params) -> Result<(), String> {
    required(p, "color")?;
    required(p, "target")?;
    color(p, "color")?;
    choice(p, "target", &["pixels"])
}

fn check_new_document(p: &Params) -> Result<(), String> {
    required(p, "width")?;
    required(p, "height")?;
    integer(p, "width", 1..=SIDE)?;
    integer(p, "height", 1..=SIDE)?;
    text(p, "name", MAX_NAME_BYTES)?;
    match p.get("background") {
        None => Ok(()),
        Some(Value::String(s)) if matches!(s.as_str(), "white" | "black" | "transparent") || is_hex_color(s) => Ok(()),
        Some(_) => Err("`background` must be white, black, transparent or #rrggbb".into()),
    }
}

fn check_blur(p: &Params) -> Result<(), String> {
    required(p, "radius")?;
    number(p, "radius", 0.1, 64.0)
}

fn check_mosaic(p: &Params) -> Result<(), String> {
    required(p, "cellSize")?;
    integer(p, "cellSize", 2..=64)
}

fn check_rect(p: &Params) -> Result<(), String> {
    for key in ["x", "y", "width", "height"] {
        required(p, key)?;
    }
    integer(p, "x", 0..=COORD)?;
    integer(p, "y", 0..=COORD)?;
    integer(p, "width", 1..=SIDE)?;
    integer(p, "height", 1..=SIDE)
}

fn check_image_size(p: &Params) -> Result<(), String> {
    required(p, "width")?;
    required(p, "height")?;
    integer(p, "width", 1..=SIDE)?;
    integer(p, "height", 1..=SIDE)
}

fn check_layer(p: &Params) -> Result<(), String> {
    required(p, "layer")?;
    layer_key(p)
}

fn check_mask(p: &Params) -> Result<(), String> {
    layer_key(p)
}

fn check_new_layer(p: &Params) -> Result<(), String> {
    text(p, "name", MAX_NAME_BYTES)
}

fn check_fill_layer(p: &Params) -> Result<(), String> {
    required(p, "color")?;
    color(p, "color")
}

/// Layer Properties: a name, visibility, opacity, fill opacity (R1: the layer's own pixels, never
/// its effects; below 100 % never with an outside or centre stroke, `stroke_under_shape`) or blend
/// mode of a named layer.
fn check_set_props(p: &Params) -> Result<(), String> {
    check_layer(p)?;
    text(p, "name", MAX_NAME_BYTES)?;
    flag(p, "visible")?;
    number(p, "opacity", 0.0, 1.0)?;
    number(p, "fill", 0.0, 1.0)?;
    match p.get("blend") {
        None => {}
        Some(Value::String(name)) if photocraft_engine::commands::blend_from_str(name).is_some_and(|mode| BlendMode::LAYER_MODES.contains(&mode)) => {}
        Some(_) => return Err("`blend` must be a layer blend mode".into()),
    }
    if ["name", "visible", "opacity", "fill", "blend"].iter().any(|key| p.contains_key(*key)) {
        Ok(())
    } else {
        Err("`layer.setProps` needs at least one of name, visible, opacity, fill and blend".into())
    }
}

fn check_translate(p: &Params) -> Result<(), String> {
    check_layer(p)?;
    required(p, "dx")?;
    required(p, "dy")?;
    integer(p, "dx", -SHIFT..=SHIFT)?;
    integer(p, "dy", -SHIFT..=SHIFT)
}

fn check_select_rect(p: &Params) -> Result<(), String> {
    check_rect(p)?;
    flag(p, "ellipse")?;
    choice(p, "mode", &["replace", "add", "subtract", "intersect"])?;
    number(p, "feather", 0.0, 250.0)
}

fn check_type(p: &Params) -> Result<(), String> {
    required(p, "text")?;
    required(p, "color")?;
    text(p, "text", MAX_TEXT_BYTES)?;
    color(p, "color")?;
    integer(p, "x", 0..=COORD)?;
    integer(p, "y", 0..=COORD)?;
    number(p, "size", MIN_TYPE_PT, MAX_TYPE_PT)?;
    text(p, "font", MAX_NAME_BYTES)?;
    text(p, "name", MAX_NAME_BYTES)?;
    choice(p, "align", &["left", "center", "right", "justify"])?;
    choice(p, "direction", &["auto", "ltr", "rtl"])?;
    // RD-12: text with a right-to-left letter states its direction (`auto` too), so a Hebrew headline
    // that starts with Latin letters is never laid out left to right by default (TY-8).
    if p.get("text").and_then(Value::as_str).is_some_and(right_to_left) && !p.contains_key("direction") {
        return Err("text with right-to-left letters needs an explicit `direction`: rtl, ltr or auto (RD-12)".into());
    }
    match p.get("box") {
        None => Ok(()),
        Some(Value::Array(items)) if is_text_box(items) => Ok(()),
        Some(_) => Err("`box` must be [x, y, width, height] in document pixels".into()),
    }
}

/// The blocks of the right-to-left scripts as code points: Hebrew, Arabic, Syriac, Thaana, NKo,
/// Samaritan and Mandaic (U+0590-U+08FF), the right-to-left mark, the Hebrew and Arabic
/// presentation forms, and the right-to-left blocks beyond the Basic Multilingual Plane.
const RIGHT_TO_LEFT: [RangeInclusive<u32>; 6] = [0x0590..=0x08ff, 0x200f..=0x200f, 0xfb1d..=0xfdff, 0xfe70..=0xfefc, 0x10800..=0x10fff, 0x1e800..=0x1efff];

/// Whether `text` holds a character of a right-to-left block: its layout depends on the paragraph
/// direction, which `auto` takes from the first strong character.
fn right_to_left(text: &str) -> bool {
    text.chars().any(|c| RIGHT_TO_LEFT.iter().any(|block| block.contains(&u32::from(c))))
}

fn is_text_box(items: &[Value]) -> bool {
    let numbers: Vec<Option<i64>> = items.iter().map(Value::as_i64).collect();
    match numbers.as_slice() {
        [Some(x), Some(y), Some(width), Some(height)] => {
            (0..=COORD).contains(x) && (0..=COORD).contains(y) && (1..=SIDE).contains(width) && (1..=SIDE).contains(height)
        }
        _ => false,
    }
}

// ------------------------------------------------------------------ R0 tier B and hook bounds

const SELECTION_MODES: [&str; 4] = ["replace", "add", "subtract", "intersect"];

/// `[x, y]` in document pixels.
fn is_point(value: &Value) -> bool {
    match value.as_array().map(Vec::as_slice) {
        Some([x, y]) => [x, y].iter().all(|n| n.as_i64().is_some_and(|v| (0..=COORD).contains(&v))),
        _ => false,
    }
}

/// `key`, when present: `min` to 64 points `[x, y]` in document pixels.
fn points(p: &Params, key: &str, min: usize) -> Result<(), String> {
    match p.get(key) {
        None => Ok(()),
        Some(Value::Array(items)) if (min..=MAX_POINTS).contains(&items.len()) && items.iter().all(is_point) => Ok(()),
        Some(_) => Err(format!("`{key}` must be {min} to {MAX_POINTS} points [x, y] in document pixels")),
    }
}

/// `[start, end]` in characters, start before end.
fn is_char_range(items: &[Value]) -> bool {
    match items.iter().map(Value::as_i64).collect::<Vec<_>>().as_slice() {
        [Some(start), Some(end)] => (0..=MAX_CHARS).contains(start) && (0..=MAX_CHARS).contains(end) && start <= end,
        _ => false,
    }
}

fn check_subject(p: &Params) -> Result<(), String> {
    flag(p, "sampleAllLayers")?;
    choice(p, "mode", &SELECTION_MODES)
}

fn check_refine_edge(p: &Params) -> Result<(), String> {
    number(p, "radius", 0.0, 250.0)?;
    flag(p, "smartRadius")?;
    number(p, "smooth", 0.0, 100.0)?;
    number(p, "feather", 0.0, 1000.0)?;
    number(p, "contrast", 0.0, 100.0)?;
    number(p, "shiftEdge", -100.0, 100.0)?;
    flag(p, "decontaminate")?;
    number(p, "amount", 0.0, 100.0)?;
    choice(p, "output", &["selection", "layerMask", "newLayer", "newLayerWithMask"])?;
    flag(p, "sampleAllLayers")
}

fn check_object(p: &Params) -> Result<(), String> {
    if !matches!(p.get("rect"), Some(Value::Array(items)) if is_text_box(items)) {
        return Err("`rect` must be [x, y, width, height] in document pixels".into());
    }
    choice(p, "mode", &SELECTION_MODES)?;
    flag(p, "sampleAllLayers")
}

fn check_quick(p: &Params) -> Result<(), String> {
    required(p, "points")?;
    points(p, "points", 1)?;
    number(p, "size", 1.0, 5000.0)?;
    choice(p, "mode", &["add", "subtract", "replace"])?;
    flag(p, "sampleAllLayers")?;
    flag(p, "enhanceEdge")
}

/// Color Range (selection_cmds.rs:122-224). Every value given has its effect, and none is clamped
/// silently: `fuzziness` is 0 to 200 for sampled colours but a percentage the engine caps at 100
/// for the tones (`fuzz(20.0).min(100.0)`), and no other preset reads it; `color` and `points`
/// sample colours for `sampledColors` only; `outOfGamut` judges the composite, whatever
/// `sampleAllLayers` says.
fn check_color_range(p: &Params) -> Result<(), String> {
    let presets = ["sampledColors", "reds", "yellows", "greens", "cyans", "blues", "magentas", "highlights", "midtones", "shadows", "outOfGamut"];
    choice(p, "select", &presets)?;
    color(p, "color")?;
    points(p, "points", 1)?;
    flag(p, "invert")?;
    flag(p, "sampleAllLayers")?;
    choice(p, "mode", &SELECTION_MODES)?;
    let preset = p.get("select").and_then(Value::as_str).unwrap_or("sampledColors");
    let tones = matches!(preset, "highlights" | "midtones" | "shadows");
    if preset == "sampledColors" {
        number(p, "fuzziness", 0.0, 200.0)?;
    } else if tones {
        number(p, "fuzziness", 0.0, 100.0).map_err(|_| format!("`fuzziness` is a percentage from 0 to 100 for `{preset}`"))?;
    } else if p.contains_key("fuzziness") {
        return Err(format!("`fuzziness` has no effect on `{preset}`"));
    }
    if preset != "sampledColors" && (p.contains_key("color") || p.contains_key("points")) {
        return Err(format!("`color` and `points` sample colours for `sampledColors` only, not for `{preset}`"));
    }
    if preset == "outOfGamut" && p.contains_key("sampleAllLayers") {
        return Err("`outOfGamut` judges the composite: `sampleAllLayers` has no effect on it".into());
    }
    Ok(())
}

/// Content-Aware Fill writes a new layer only (`output` is required and must be `new`), samples
/// automatically or from a rectangle around the hole, never from a custom area or channel.
fn check_content_aware_fill(p: &Params) -> Result<(), String> {
    required(p, "output")?;
    choice(p, "output", &["new"])?;
    choice(p, "sampling", &["auto", "rectangular"])?;
    integer(p, "margin", 0..=1024)?;
    choice(p, "colorAdaptation", &["default", "none", "high", "veryHigh"])?;
    choice(p, "rotationAdaptation", &["none", "low", "medium", "high", "full"])?;
    flag(p, "scale")?;
    flag(p, "mirror")?;
    integer(p, "seed", 0..=i64::from(u32::MAX))
}

/// Drop Shadow (subset). A new shadow follows the document's global light
/// (`use_global_light: true`, layer_style.rs:123-136), which the compositor draws at the global
/// angle (crates/compose/src/effects.rs:561), so an `angle` comes with `useGlobalLight: false`:
/// an applied angle always has its effect.
fn check_drop_shadow(p: &Params) -> Result<(), String> {
    layer_key(p)?;
    color(p, "color")?;
    number(p, "opacity", 0.0, 100.0)?;
    number(p, "angle", -360.0, 360.0)?;
    flag(p, "useGlobalLight")?;
    number(p, "distance", 0.0, 1000.0)?;
    number(p, "spread", 0.0, 100.0)?;
    number(p, "size", 0.0, 250.0)?;
    if p.contains_key("angle") && p.get("useGlobalLight") != Some(&Value::Bool(false)) {
        return Err("`angle` needs `useGlobalLight: false`: with the global light the shadow takes the document's angle".into());
    }
    Ok(())
}

fn check_stroke(p: &Params) -> Result<(), String> {
    number(p, "size", 1.0, 250.0)?;
    choice(p, "position", &["outside", "inside", "center"])?;
    color(p, "color")?;
    number(p, "opacity", 0.0, 100.0)
}

/// `type.edit`: new text (or one replaced range), a box or point, a move or a name; never a free
/// transform or per-run styles (`type.setStyle` styles ranges).
fn check_type_edit(p: &Params) -> Result<(), String> {
    layer_key(p)?;
    text(p, "text", MAX_TEXT_BYTES)?;
    text(p, "name", MAX_NAME_BYTES)?;
    match p.get("replace") {
        None => {}
        Some(Value::Object(replace)) if is_replacement(replace) => {}
        Some(_) => return Err("`replace` must be {start, end, text} in characters, start before end".into()),
    }
    match p.get("box") {
        None => {}
        Some(Value::Array(items)) if is_text_box(items) => {}
        Some(_) => return Err("`box` must be [x, y, width, height] in document pixels".into()),
    }
    for (key, range) in [("point", 0..=COORD), ("move", -SHIFT..=SHIFT)] {
        match p.get(key) {
            None => {}
            Some(Value::Array(items)) if items.len() == 2 && items.iter().all(|n| n.as_i64().is_some_and(|v| range.contains(&v))) => {}
            Some(_) => return Err(format!("`{key}` must be [x, y] in document pixels")),
        }
    }
    if ["text", "replace", "box", "point", "move", "name"].iter().any(|key| p.contains_key(*key)) {
        Ok(())
    } else {
        Err("`type.edit` needs at least one of text, replace, box, point, move and name".into())
    }
}

fn is_replacement(replace: &Params) -> bool {
    let at = |key: &str| replace.get(key).and_then(Value::as_i64).filter(|n| (0..=MAX_CHARS).contains(n));
    let text = replace.get("text").and_then(Value::as_str).is_some_and(|text| text.len() <= MAX_TEXT_BYTES);
    replace.len() == 3 && text && matches!((at("start"), at("end")), (Some(start), Some(end)) if start <= end)
}

/// `type.setStyle`: character and paragraph styles over a character range (all by default),
/// including right-to-left paragraphs, within `type.create`'s size bounds, so a restyle never
/// makes text larger than an agent may create it.
fn check_type_style(p: &Params) -> Result<(), String> {
    layer_key(p)?;
    match p.get("range") {
        None => {}
        Some(Value::Array(items)) if is_char_range(items) => {}
        Some(_) => return Err("`range` must be [start, end] in characters, start before end".into()),
    }
    text(p, "font", MAX_NAME_BYTES)?;
    number(p, "size", MIN_TYPE_PT, MAX_TYPE_PT)?;
    color(p, "color")?;
    integer(p, "tracking", -MAX_TRACKING..=MAX_TRACKING)?;
    match p.get("leading") {
        None => {}
        Some(Value::String(auto)) if auto == "auto" => {}
        Some(value) if value.as_f64().is_some_and(|pt| pt.is_finite() && (0.1..=MAX_LEADING_PT).contains(&pt)) => {}
        Some(_) => return Err(format!("`leading` must be a size from 0.1 to {MAX_LEADING_PT} points or \"auto\"")),
    }
    flag(p, "fauxBold")?;
    choice(p, "align", &["left", "center", "right", "justify", "justifyCenter", "justifyRight", "justifyAll"])?;
    choice(p, "direction", &["auto", "ltr", "rtl"])?;
    if ["font", "size", "color", "tracking", "leading", "fauxBold", "align", "direction"].iter().any(|key| p.contains_key(*key)) {
        Ok(())
    } else {
        Err("`type.setStyle` needs at least one character or paragraph style".into())
    }
}

fn check_brightness_contrast(p: &Params) -> Result<(), String> {
    number(p, "brightness", -150.0, 150.0)?;
    number(p, "contrast", -50.0, 100.0)
}

/// Levels (adjust_params.rs:423-446). The engine raises `inWhite` to `inBlack + 2` silently
/// (`.max(in_black + 2.0)`, :436), so a pair it would change is refused and every value given has
/// its effect. Alone, either key meets the other's default on a new layer (0 and 255), which never
/// moves it.
fn check_levels(p: &Params) -> Result<(), String> {
    number(p, "inBlack", 0.0, 253.0)?;
    number(p, "gamma", 0.01, 9.99)?;
    number(p, "inWhite", 2.0, 255.0)?;
    number(p, "outBlack", 0.0, 255.0)?;
    number(p, "outWhite", 0.0, 255.0)?;
    if let (Some(black), Some(white)) = (p.get("inBlack").and_then(Value::as_f64), p.get("inWhite").and_then(Value::as_f64))
        && white < black + 2.0
    {
        return Err(format!("`inWhite` must be at least `inBlack` + 2 ({}): the engine would raise it silently", black + 2.0));
    }
    Ok(())
}

fn check_curves(p: &Params) -> Result<(), String> {
    match p.get("points") {
        Some(Value::Array(points)) if is_curve(points) => Ok(()),
        _ => Err("`points` must be 2 to 19 [input, output] pairs in 0..255 with distinct inputs".into()),
    }
}

/// 2 to 19 `[input, output]` pairs in 0..255 (the engine's `MAX_CURVE_POINTS`), inputs distinct.
fn is_curve(points: &[Value]) -> bool {
    let mut inputs = Vec::with_capacity(points.len());
    for point in points {
        match point.as_array().map(Vec::as_slice) {
            Some([input, output]) if [input, output].iter().all(|n| n.as_i64().is_some_and(|v| (0..=255).contains(&v))) => {
                inputs.extend(input.as_i64());
            }
            _ => return false,
        }
    }
    inputs.sort_unstable();
    inputs.dedup();
    (2..=19).contains(&points.len()) && inputs.len() == points.len()
}

fn check_exposure(p: &Params) -> Result<(), String> {
    number(p, "exposure", -20.0, 20.0)?;
    number(p, "offset", -0.5, 0.5)?;
    number(p, "gamma", 0.01, 9.99)
}

fn check_vibrance(p: &Params) -> Result<(), String> {
    number(p, "vibrance", -100.0, 100.0)?;
    number(p, "saturation", -100.0, 100.0)
}

fn check_hue_saturation(p: &Params) -> Result<(), String> {
    number(p, "hue", -180.0, 180.0)?;
    number(p, "saturation", -100.0, 100.0)?;
    number(p, "lightness", -100.0, 100.0)
}

/// Shadows/Highlights with radii up to 250 pixels (the engine allows 2500).
fn check_shadows_highlights(p: &Params) -> Result<(), String> {
    for key in ["shadowAmount", "shadowTone", "highlightAmount", "highlightTone"] {
        number(p, key, 0.0, 100.0)?;
    }
    number(p, "shadowRadius", 0.0, 250.0)?;
    number(p, "highlightRadius", 0.0, 250.0)?;
    number(p, "color", -100.0, 100.0)?;
    number(p, "midtone", -100.0, 100.0)
}

fn is_variable_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_VARIABLE_NAME_BYTES
}

/// Image › Variables › Define: visibility and text replacement variables bound to layers, never
/// pixel replacement (it reads image files).
fn check_define(p: &Params) -> Result<(), String> {
    let defs = match p.get("defs") {
        Some(Value::Array(defs)) if (1..=MAX_VARIABLES).contains(&defs.len()) => defs,
        _ => return Err(format!("`defs` must be 1 to {MAX_VARIABLES} variable definitions")),
    };
    let mut names: Vec<&str> = Vec::with_capacity(defs.len());
    for def in defs {
        let def = def.as_object().filter(|def| def.len() == 3).ok_or("each definition is {name, layer, type}")?;
        let name = def.get("name").and_then(Value::as_str).filter(|name| is_variable_name(name)).ok_or("a variable `name` must be 1 to 64 bytes")?;
        def.get("layer").and_then(Value::as_u64).ok_or("a variable's `layer` must be a layer id")?;
        if !matches!(def.get("type").and_then(Value::as_str), Some("visibility" | "textReplacement")) {
            return Err("a variable's `type` must be visibility or textReplacement (never pixel replacement, which reads files)".into());
        }
        if names.contains(&name) {
            return Err(format!("variable `{}` is defined twice", bounded(name)));
        }
        names.push(name);
    }
    Ok(())
}

/// Image › Variables › Data Sets: visibility booleans and texts, never pixels (file paths).
fn check_data_sets(p: &Params) -> Result<(), String> {
    flag(p, "append")?;
    let sets = match p.get("dataSets") {
        Some(Value::Array(sets)) if (1..=MAX_DATA_SETS).contains(&sets.len()) => sets,
        _ => return Err(format!("`dataSets` must be 1 to {MAX_DATA_SETS} data sets")),
    };
    for set in sets {
        let set = set.as_object().filter(|set| set.len() == 2).ok_or("each data set is {name, values}")?;
        set.get("name").and_then(Value::as_str).filter(|name| is_variable_name(name)).ok_or("a data set `name` must be 1 to 64 bytes")?;
        let values = set.get("values").and_then(Value::as_array).filter(|values| values.len() <= MAX_VARIABLES);
        for value in values.ok_or("a data set's `values` must be a list of at most 32 values")? {
            let value = value.as_object().filter(|value| value.len() == 3).ok_or("each value is {variable, kind, value}")?;
            value.get("variable").and_then(Value::as_str).filter(|name| is_variable_name(name)).ok_or("a value's `variable` must be 1 to 64 bytes")?;
            match (value.get("kind").and_then(Value::as_str), value.get("value")) {
                (Some("visibility"), Some(Value::Bool(_))) => {}
                (Some("text"), Some(Value::String(text))) if text.len() <= MAX_TEXT_BYTES => {}
                _ => return Err("a value is a visibility boolean or a text of at most 4096 bytes (never pixels, which reads files)".into()),
            }
        }
    }
    Ok(())
}

/// Image › Apply Data Set: by `name` or by `index`, not both.
fn check_apply_data_set(p: &Params) -> Result<(), String> {
    let index = |value: &Value| value.as_u64().and_then(|n| usize::try_from(n).ok()).is_some_and(|n| n < MAX_DATA_SETS);
    match (p.get("name"), p.get("index")) {
        (Some(Value::String(name)), None) if is_variable_name(name) => Ok(()),
        (None, Some(value)) if index(value) => Ok(()),
        _ => Err("name a data set by `name` (1 to 64 bytes) or by `index` (0 to 63), not both".into()),
    }
}

/// `placeBytes` (critic correction CC-27): a file name (never a path) ending in an image
/// extension; the registration, explicit: `scale` in per cent and `center` in document pixels,
/// both required; and, for a restore, the `region` `[x, y, width, height]` the bytes were taken
/// from, which pins that registration to 100 % on the region's centre, so every pixel lands on the
/// pixel it was taken from (`check_placement` checks the region against the document and the
/// image). An import (`newDocument: true`) takes the name only (`check_import`).
fn check_place(p: &Params) -> Result<(), String> {
    match p.get("name") {
        None => {}
        Some(Value::String(name)) if is_image_name(name) => {}
        Some(_) => return Err("`name` must be a file name of at most 128 bytes ending in .png, .jpg, .jpeg or .webp, not a path".into()),
    }
    match p.get("newDocument") {
        None | Some(Value::Bool(false)) => {}
        Some(Value::Bool(true)) => return check_import(p),
        Some(_) => return Err("`newDocument` must be a boolean".into()),
    }
    required(p, "scale")?;
    number(p, "scale", 1.0, 1000.0)?;
    let (x, y) = place_center(p.get("center")).ok_or("`center` must be [x, y] in document pixels")?;
    let Some(region) = p.get("region") else { return Ok(()) };
    let [left, top, width, height] = place_region(region).ok_or("`region` must be [x, y, width, height] in document pixels")?;
    let scale = p.get("scale").and_then(Value::as_f64).unwrap_or_default();
    if (scale - 100.0).abs() > 1e-9 {
        return Err("a `region` is restored at its own size: `scale` must be 100".into());
    }
    let (center_x, center_y) = (left as f64 + width as f64 / 2.0, top as f64 + height as f64 / 2.0);
    if (x - center_x).abs() > 1e-9 || (y - center_y).abs() > 1e-9 {
        return Err(format!("a `region` is restored on its own centre: `center` must be [{center_x}, {center_y}]"));
    }
    Ok(())
}

/// `placeBytes`'s `center`: two finite numbers within 4096 document pixels of the origin.
fn place_center(value: Option<&Value>) -> Option<(f64, f64)> {
    match value?.as_array()?.as_slice() {
        [x, y] => {
            let (x, y) = (x.as_f64()?, y.as_f64()?);
            [x, y].iter().all(|v| v.is_finite() && v.abs() <= MAX_CENTER).then_some((x, y))
        }
        _ => None,
    }
}

/// `placeBytes`'s `region`: `[x, y, width, height]` in document pixels, as `type.create`'s `box`.
fn place_region(value: &Value) -> Option<[i64; 4]> {
    let items = value.as_array().filter(|items| is_text_box(items))?;
    items.iter().map(Value::as_i64).collect::<Option<Vec<i64>>>()?.try_into().ok()
}

fn is_image_name(name: &str) -> bool {
    let plain = name.len() <= MAX_NAME_BYTES && !name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'));
    let extension = name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()).map(|(_, extension)| extension.to_ascii_lowercase());
    plain && extension.is_some_and(|extension| matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp"))
}

// ------------------------------------------------------------------ R1 widen-1 bounds

/// Layer › Adjustment Properties (`layer.setAdjustment`): a named layer and at least one value
/// (with none the engine resets the layer to its defaults, commands.rs:697-702), each within the
/// bounds a new layer of its kind takes, so the engine clamps none. Whether the values are of the
/// layer's kind and what the merge leaves is decided against the layer (`check_adjustment`).
fn check_set_adjustment(p: &Params) -> Result<(), String> {
    check_layer(p)?;
    if p.keys().all(|key| key == "layer") {
        return Err("`layer.setAdjustment` needs at least one value: with none the engine resets the layer to its defaults".into());
    }
    check_brightness_contrast(p)?;
    check_levels(p)?;
    check_exposure(p)?;
    check_vibrance(p)?;
    check_hue_saturation(p)?;
    if p.contains_key("points") { check_curves(p) } else { Ok(()) }
}

/// Select › Modify › Expand and Contract: a radius in whole pixels, given explicitly (the engine
/// would take 1 pixel), within the engine's dialog range of 1 to 500.
fn check_modify(p: &Params) -> Result<(), String> {
    required(p, "radius")?;
    integer(p, "radius", 1..=MAX_RADIUS)
}

/// The import (`newDocument: true`, RD-17): the bytes open as a new document at their own size, so
/// no key places them on a document.
fn check_import(p: &Params) -> Result<(), String> {
    match ["scale", "center", "region"].into_iter().find(|key| p.contains_key(*key)) {
        Some(key) => Err(format!("`{key}` places bytes on a document: an import (`newDocument: true`) opens them as a new document")),
        None => Ok(()),
    }
}

// ------------------------------------------------------------------ typed execute

/// A refusal decided before the run function: the registry code and a short reason.
type Refusal = (&'static str, String);
type Decision<T> = Result<T, Refusal>;

fn refusal(code: &'static str, message: impl Into<String>) -> Refusal {
    (code, message.into())
}

/// A document reference: `{"index": n}`, `{"id": "<DocId>"}` or both (they must agree).
#[derive(Clone, Copy, Default)]
struct DocRef {
    index: Option<usize>,
    id: Option<u64>,
}

impl DocRef {
    fn is_empty(self) -> bool {
        self.index.is_none() && self.id.is_none()
    }
}

/// The preconditions of a typed call (design Appendix B.3, engine-checked part).
#[derive(Default)]
struct Preconditions {
    document: Option<DocRef>,
    rev: Option<u64>,
    active_layer: Option<u64>,
    history_head: Option<(usize, String)>,
    fontset_digest: Option<String>,
}

fn preconditions(options: &Value) -> Result<Preconditions, String> {
    let map = options.as_object().ok_or("preconditions must be a JSON object")?;
    let mut out = Preconditions::default();
    for (key, value) in map {
        let present = !value.is_null();
        match key.as_str() {
            "document" if present => out.document = Some(doc_ref(value)?),
            "rev" if present => out.rev = Some(revision(value)?),
            "activeLayer" if present => out.active_layer = Some(value.as_u64().ok_or("`activeLayer` must be a layer id")?),
            "historyHead" if present => out.history_head = Some(history_head(value)?),
            "fontsetDigest" if present => out.fontset_digest = Some(font_digest(value)?),
            "deadlineMs" if present => {
                value.as_u64().filter(|ms| (1..=120_000).contains(ms)).ok_or("`deadlineMs` must be an integer from 1 to 120000")?;
            }
            "document" | "rev" | "activeLayer" | "historyHead" | "fontsetDigest" | "deadlineMs" => {}
            other => return Err(format!("unknown precondition `{}`", bounded(other))),
        }
    }
    Ok(out)
}

fn doc_ref(value: &Value) -> Result<DocRef, String> {
    let map = value.as_object().ok_or("`document` must be an object with `index` or `id`")?;
    let mut out = DocRef::default();
    for (key, item) in map {
        match key.as_str() {
            "index" => {
                let index = item.as_u64().filter(|index| *index <= 7).and_then(|index| usize::try_from(index).ok());
                out.index = Some(index.ok_or("`document.index` must be an integer from 0 to 7")?);
            }
            "id" => out.id = Some(doc_id(item).ok_or("`document.id` must be a DocId string of 1 to 20 digits")?),
            other => return Err(format!("unknown document key `{}`", bounded(other))),
        }
    }
    Ok(out)
}

fn doc_id(value: &Value) -> Option<u64> {
    let digits = value.as_str()?;
    if digits.is_empty() || digits.len() > 20 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn revision(value: &Value) -> Result<u64, String> {
    value
        .as_str()
        .and_then(|token| token.strip_prefix("p:"))
        .filter(|digits| !digits.is_empty() && digits.len() <= 20 && digits.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|digits| digits.parse().ok())
        .ok_or_else(|| "`rev` must be a revision token `p:<digits>`".to_string())
}

fn history_head(value: &Value) -> Result<(usize, String), String> {
    let map = value.as_object().ok_or("`historyHead` must be an object with `length` and `label`")?;
    if let Some(key) = map.keys().find(|key| !matches!(key.as_str(), "length" | "label")) {
        return Err(format!("unknown historyHead key `{}`", bounded(key)));
    }
    let length = map.get("length").and_then(Value::as_u64).filter(|n| *n <= 100_000).and_then(|n| usize::try_from(n).ok());
    let length = length.ok_or("`historyHead.length` must be an integer from 0 to 100000")?;
    let label = map.get("label").and_then(Value::as_str).filter(|label| label.len() <= 200);
    let label = label.ok_or("`historyHead.label` must be a string of at most 200 bytes")?;
    Ok((length, label.to_string()))
}

fn font_digest(value: &Value) -> Result<String, String> {
    let lower_hex = |hex: &str| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    value
        .as_str()
        .filter(|digest| digest.strip_prefix("sha256:").is_some_and(lower_hex))
        .map(str::to_string)
        .ok_or_else(|| "`fontsetDigest` must be `sha256:` and 64 lowercase hex digits".to_string())
}

/// The open document a reference names: the active one when it names nothing.
fn find_document(session: &Session, reference: DocRef) -> Option<usize> {
    let documents = session.documents();
    let by_id = reference.id.map(|id| documents.iter().position(|state| state.doc.id.0 == id));
    match (reference.index, by_id) {
        (None, None) => session.active_index(),
        (Some(index), None) => (index < documents.len()).then_some(index),
        (None, Some(found)) => found,
        (Some(index), Some(found)) => found.filter(|found| *found == index),
    }
}

fn document_by_id(session: &Session, id: DocId) -> Option<&DocState> {
    session.documents().iter().find(|state| state.doc.id == id)
}

/// Runs a typed command on the visible editor and returns the `photocraft.typed-execute.v1`
/// envelope. Every `not_applied` code is decided before `run` is called, with nothing changed;
/// once `run` was called the outcome is `applied` or `unknown`.
pub fn execute(app: &mut PhotocraftApp, env: &Env<'_>, command: &str, params: &Value, options: &Value, run: &mut RunFn<'_>) -> Value {
    let report = report_index(&app.session, command, params, options);
    match decide(app, env, command, params, options) {
        Ok((entry, target)) => dispatch(app, entry, target, |app: &mut PhotocraftApp| run(app, command, params.clone())),
        Err((code, message)) => refused(&app.session, report, code, &message),
    }
}

/// The function that places a validated `placeBytes` input: the app, the file name, the bytes and
/// the hook's parameters. The glue passes the engine's `file_cmds::place_bytes`, or for an import
/// (`is_import`) `file_cmds::open_bytes_as(.., Some("png"), None)`, and then syncs the views.
pub type PlaceFn<'a> = dyn FnMut(&mut PhotocraftApp, &str, Vec<u8>, Value) -> Result<Value, String> + 'a;

/// The `placeBytes` hook (R0 tier A; keys and bounds in `PLACE_BYTES`): places encoded PNG, JPEG
/// or WebP bytes on the active document as one undoable smart-object layer, with `execute`'s
/// discipline and envelope. Every `not_applied` code is decided before `place` is called (the
/// input codes too: `input_rejected`, `decode_failed`, `budget_exceeded`); once it was called the
/// outcome is `applied` or `unknown`. `place` receives the input re-encoded as a lossless PNG
/// without the caller's metadata (`clean_png`), named after the caller's file name.
///
/// An import (`newDocument: true`, R1, RD-17) creates its target as `file.new` does: no document
/// precondition applies, at most `MAX_DOCUMENTS` documents are open, and the envelope names the new
/// document with no revision before it. `place` receives the input as `import_png` re-encodes it,
/// named by the caller's file name, and the glue opens it as a new document.
pub fn place_bytes(app: &mut PhotocraftApp, env: &Env<'_>, bytes: Vec<u8>, params: &Value, options: &Value, place: &mut PlaceFn<'_>) -> Value {
    let report = report_index(&app.session, PLACE_BYTES.id, params, options);
    match decide_place(app, env, &bytes, params, options) {
        Ok((target, name, clean)) => {
            drop(bytes);
            dispatch(app, &PLACE_BYTES, target, |app: &mut PhotocraftApp| place(app, name.as_str(), clean, params.clone()))
        }
        Err((code, message)) => refused(&app.session, report, code, &message),
    }
}

/// Whether a `placeBytes` request is an import (`newDocument: true`): its bytes open as a new
/// document instead of a layer of the active one. The glue and a native worker branch on it.
pub fn is_import(params: &Value) -> bool {
    params.get("newDocument") == Some(&Value::Bool(true))
}

/// The file name the engine embeds and names the layer after (its stem): the caller's stem, or
/// `Placed`, with `.png`, the format of the bytes it gets.
fn placed_name(params: &Value) -> String {
    let stem = params.get("name").and_then(Value::as_str).and_then(|name| name.rsplit_once('.')).map_or("Placed", |(stem, _)| stem);
    format!("{stem}.png")
}

/// The name an import's new document takes: the caller's file name, or `Imported` with the
/// extension of the input's format.
fn imported_name(params: &Value, bytes: &[u8]) -> String {
    if let Some(name) = params.get("name").and_then(Value::as_str) {
        return name.to_string();
    }
    let extension = match photocraft_codecs::detect(bytes) {
        Some(Format::Jpeg) => "jpg",
        Some(Format::WebP) => "webp",
        _ => "png",
    };
    format!("Imported.{extension}")
}

/// The browser hook's bound on queued `placeBytes` input, which the glue asks before it copies an
/// input of `length` bytes into the page's memory: the inputs already waiting (`waiting` bytes, all
/// requests together) and this one stay within `MAX_PENDING_PLACE_BYTES`. Otherwise the typed
/// `queue_full` refusal: nothing was copied or queued, and the same call can be sent again once
/// the queue has drained. `None` when the input fits.
pub fn place_queue_refusal(waiting: usize, length: usize) -> Option<Value> {
    let pending = waiting.saturating_add(length);
    if pending <= MAX_PENDING_PLACE_BYTES {
        return None;
    }
    let message = format!("at most {MAX_PENDING_PLACE_BYTES} bytes of placeBytes input may wait in the queue at once, not {pending}");
    Some(not_applied("queue_full", &message))
}

/// Runs a decided call (`execute`, `place_bytes`) under the typed step gate, with script events
/// and background jobs off, and builds its envelope: `applied`, or `unknown` once dispatched.
fn dispatch(app: &mut PhotocraftApp, entry: &PolicyEntry, target: Option<DocId>, run: impl FnOnce(&mut PhotocraftApp) -> Result<Value, String>) -> Value {
    let before = target.and_then(|id| document_by_id(&app.session, id)).map(|state| state.revision);
    let open: Vec<DocId> = app.session.documents().iter().map(|state| state.doc.id).collect();
    // The typed step gate, script events off (as `control::run_waiting` does) and inline jobs.
    let gate = app.session.authorize.replace(authorize_step);
    let events = app.session.prefs().script_events.enabled;
    if events {
        app.session.edit_prefs(|prefs| prefs.script_events.enabled = false);
    }
    let jobs = std::mem::take(&mut app.background_jobs);
    let result = run(app);
    app.background_jobs = jobs;
    if events {
        app.session.edit_prefs(|prefs| prefs.script_events.enabled = true);
    }
    app.session.authorize = gate;
    // `file.new` targets the document it creates.
    let document = target.or_else(|| app.session.documents().iter().map(|state| state.doc.id).find(|id| !open.contains(id)));
    let state = document.and_then(|id| document_by_id(&app.session, id));
    let mut envelope = json!({
        "resultVersion": RESULT_VERSION,
        "tier": entry.tier,
        "documentId": document.map(|id| id.0.to_string()),
        "revisionBefore": before.map(rev_token),
        "revisionAfter": state.map(|state| rev_token(state.revision)),
        "historyHead": state.map(history_head_json),
        "activeLayer": state.and_then(|state| state.active_layer).map(|layer| layer.0),
        "lastDamage": state.and_then(|state| state.last_damage).map(rect_json),
    });
    if let Some(object) = envelope.as_object_mut() {
        match result {
            Ok(value) => {
                object.insert("outcome".into(), json!("applied"));
                object.insert("code".into(), Value::Null);
                object.insert("result".into(), value);
            }
            Err(error) => {
                object.insert("outcome".into(), json!("unknown"));
                object.insert("code".into(), json!("engine_error_after_dispatch"));
                object.insert("detail".into(), json!(bounded(&error)));
            }
        }
    }
    envelope
}

/// A `not_applied` envelope that names no document (the glue's own refusals use it as is).
pub fn not_applied(code: &str, message: &str) -> Value {
    json!({
        "resultVersion": RESULT_VERSION,
        "outcome": "not_applied",
        "code": code,
        "message": bounded(message),
        "documentId": null,
        "revisionBefore": null,
        "revisionAfter": null,
    })
}

/// A refusal reports where its target stands (before and after are the same: nothing changed).
fn refused(session: &Session, report: Option<usize>, code: &'static str, message: &str) -> Value {
    let mut envelope = not_applied(code, message);
    if let Some(state) = report.and_then(|index| session.documents().get(index))
        && let Some(object) = envelope.as_object_mut()
    {
        let revision = json!(rev_token(state.revision));
        object.insert("documentId".into(), json!(state.doc.id.0.to_string()));
        object.insert("revisionBefore".into(), revision.clone());
        object.insert("revisionAfter".into(), revision);
        object.insert("historyHead".into(), history_head_json(state));
        object.insert("activeLayer".into(), json!(state.active_layer.map(|layer| layer.0)));
    }
    envelope
}

/// The document a refusal reports, read leniently from the request (no document for `file.new` and
/// an import, which create their targets).
fn report_index(session: &Session, command: &str, params: &Value, options: &Value) -> Option<usize> {
    if command == "file.new" || (command == PLACE_BYTES.id && is_import(params)) {
        return None;
    }
    if let Some(reference) = options.get("document").and_then(|value| doc_ref(value).ok()).filter(|reference| !reference.is_empty()) {
        return find_document(session, reference);
    }
    if command == "document.activate" {
        let wanted = params.get("document").and_then(Value::as_u64).and_then(|index| usize::try_from(index).ok());
        return wanted.filter(|index| *index < session.documents().len());
    }
    session.active_index()
}

/// Every check before dispatch, in the order of the code list (design Appendix A.2): the page, the
/// queue and the human; the policy table and the runtime; the target and its preconditions; fonts;
/// parameter targets; caps; the engine's own preconditions; the bridge validator.
fn decide(app: &PhotocraftApp, env: &Env<'_>, command: &str, params: &Value, options: &Value) -> Decision<(&'static PolicyEntry, Option<DocId>)> {
    check_env(env)?;
    let entry = policy(command).ok_or_else(|| refusal("command_not_allowed", format!("`{}` is not in the typed policy table", bounded(command))))?;
    check_runtime(entry, env.runtime)?;
    check_params(entry, params).map_err(|message| refusal("invalid_params", message))?;
    let pre = preconditions(options).map_err(|message| refusal("invalid_params", message))?;
    if matches!(command, "edit.undo" | "edit.redo") && pre.history_head.is_none() {
        return Err(refusal("invalid_params", "undo and redo are global: `historyHead` must name the head they expect"));
    }
    if command == "file.new" && (pre.document.is_some() || pre.rev.is_some() || pre.active_layer.is_some() || pre.history_head.is_some()) {
        return Err(refusal("invalid_params", "`file.new` creates its target: document preconditions do not apply"));
    }
    let session = &app.session;
    let index = target_index(session, command, params, &pre)?;
    let state = index.and_then(|index| session.documents().get(index));
    if let Some(state) = state {
        check_state(state, &pre)?;
    }
    check_fonts(command, params, env.families, &pre)?;
    check_targets(command, params, state)?;
    check_limits(session, command, params, state, env.runtime)?;
    check_engine(app, command, params, state)?;
    (env.validate)(command, params, session).map_err(|message| refusal("engine_precheck_failed", message))?;
    Ok((entry, state.map(|state| state.doc.id)))
}

/// The browser hook refuses an entry only a native worker runs (`native_only`; media agent
/// contracts §15, §9.9).
fn check_runtime(entry: &PolicyEntry, runtime: Runtime) -> Decision<()> {
    if entry.runtimes.allows(runtime) {
        Ok(())
    } else {
        Err(refusal("native_only", format!("`{}` runs in a native worker only, not in the browser hook", entry.id)))
    }
}

/// The page, the queue and the human come first, in this order.
fn check_env(env: &Env<'_>) -> Decision<()> {
    if env.hidden {
        return Err(refusal("page_hidden", "the editor page is hidden; not applied"));
    }
    if env.expired {
        return Err(refusal("queue_expired", "the request expired before the editor could run it; not applied"));
    }
    if env.busy {
        return Err(refusal("editor_busy", "a manual edit, dialog or pointer input is in progress; not applied"));
    }
    Ok(())
}

/// A decided `placeBytes` call: its target (none for an import, which creates it), the file name
/// `place` gets and the re-encoded bytes.
type Placement = (Option<DocId>, String, Vec<u8>);

/// Every check before a `placeBytes` call, in `decide`'s order: the page, the queue and the human;
/// the hook's keys and the input bytes; the target (the active document) and its preconditions;
/// fonts; the layer cap; the engine's own precondition for a new layer; where the image lands and
/// how large it becomes; the bridge validator. Returns the target, the file name (`placed_name`)
/// and the input re-encoded without metadata (`clean_png`), the bytes `place` gets. An import is
/// decided by `decide_import`.
fn decide_place(app: &PhotocraftApp, env: &Env<'_>, bytes: &[u8], params: &Value, options: &Value) -> Decision<Placement> {
    check_env(env)?;
    check_runtime(&PLACE_BYTES, env.runtime)?;
    check_params(&PLACE_BYTES, params).map_err(|message| refusal("invalid_params", message))?;
    let pre = preconditions(options).map_err(|message| refusal("invalid_params", message))?;
    if is_import(params) {
        return decide_import(app, env, bytes, params, &pre);
    }
    let image = check_input(bytes)?;
    let session = &app.session;
    let index = target_index(session, PLACE_BYTES.id, params, &pre)?.ok_or_else(|| refusal("target_not_found", "no document is open"))?;
    let state = session.documents().get(index).ok_or_else(|| refusal("target_not_found", "no document is open"))?;
    check_state(state, &pre)?;
    check_fonts(PLACE_BYTES.id, params, env.families, &pre)?;
    if state.doc.layer_count() >= MAX_LAYERS {
        return Err(refusal("layer_limit", format!("at most {MAX_LAYERS} layers per document")));
    }
    // Placing adds a layer: the engine's own precondition for one (an open document, no job on it).
    if let Some(reason) = session.disabled_reason_with("layer.new.layer", &Value::Null) {
        return Err(refusal("engine_precheck_failed", reason));
    }
    let (width, height) = image.dimensions();
    check_placement(params, width, height, state)?;
    (env.validate)(PLACE_BYTES.id, params, session).map_err(|message| refusal("engine_precheck_failed", message))?;
    Ok((Some(state.doc.id), placed_name(params), clean_png(image)?))
}

/// Every check before an import (`newDocument: true`), in `decide`'s order for `file.new`: the
/// import creates its target, so no document precondition applies; the input bytes; fonts; the
/// document cap; the bridge validator. Returns no target, the new document's name
/// (`imported_name`) and the input as `import_png` re-encodes it.
///
/// In the browser all of it runs on the page's only thread: the input decodes here, re-encodes as
/// a PNG, and the engine decodes that PNG again when it opens it (`file_cmds::open_bytes_as`), for
/// up to 4096 × 4096 pixels from up to 64 MiB of input. Only native timings exist so far: FP2's lab
/// acceptance measures a 4096 × 4096 import, and if it passes the frame or deadline budget an import
/// over a measured size becomes `native_only`, as a large Content-Aware Fill hole is (`check_limits`).
fn decide_import(app: &PhotocraftApp, env: &Env<'_>, bytes: &[u8], params: &Value, pre: &Preconditions) -> Decision<Placement> {
    if pre.document.is_some() || pre.rev.is_some() || pre.active_layer.is_some() || pre.history_head.is_some() {
        return Err(refusal("invalid_params", "an import (`newDocument: true`) creates its target: document preconditions do not apply"));
    }
    let image = check_input(bytes)?;
    check_fonts(PLACE_BYTES.id, params, env.families, pre)?;
    let session = &app.session;
    if session.documents().len() >= MAX_DOCUMENTS {
        return Err(refusal("document_limit", format!("at most {MAX_DOCUMENTS} documents may be open")));
    }
    (env.validate)(PLACE_BYTES.id, params, session).map_err(|message| refusal("engine_precheck_failed", message))?;
    Ok((None, imported_name(params, bytes), import_png(image)?))
}

/// The bytes a `placeBytes` call carries: PNG, JPEG or WebP of 1 byte to 64 MiB
/// (`MAX_INPUT_BYTES`, RD-17, in both runtimes), not CMYK, that decode within the render bound of
/// 4096 pixels a side. The decode is pure (nothing is placed yet) and turns the pixels upright as
/// the engine's own import does (crates/codecs/src/lib.rs:71-79): the image is returned.
fn check_input(bytes: &[u8]) -> Decision<Image> {
    if bytes.is_empty() || bytes.len() > MAX_INPUT_BYTES {
        return Err(refusal("input_rejected", format!("the image must be 1 to {MAX_INPUT_BYTES} bytes")));
    }
    if !matches!(photocraft_codecs::detect(bytes), Some(Format::Png | Format::Jpeg | Format::WebP)) {
        return Err(refusal("input_rejected", "only PNG, JPEG and WebP images can be placed"));
    }
    let pixels = u64::from(MAX_SIDE) * u64::from(MAX_SIDE);
    let limits = Limits { max_width: MAX_SIDE, max_height: MAX_SIDE, max_pixels: pixels, max_alloc: pixels * 16 };
    let image = match photocraft_codecs::decode_with(bytes, &DecodeOptions { limits, ..Default::default() }) {
        Ok(image) => image,
        Err(CodecError::LimitExceeded(message)) => {
            return Err(refusal("budget_exceeded", format!("the image exceeds {MAX_SIDE} pixels a side: {}", bounded(&message))));
        }
        Err(error) => return Err(refusal("decode_failed", format!("the image does not decode: {}", bounded(&error.to_string())))),
    };
    if image.layout().is_cmyk() {
        return Err(refusal("input_rejected", "a CMYK image is not placed through the agent hook: convert it to RGB first"));
    }
    Ok(image)
}

/// Where a `placeBytes` image lands and how large it becomes, before dispatch. A restore `region`
/// lies inside the document and is exactly the image's size: a checkpoint of another size would
/// land on other pixels. Scaled, the image stays within 4096 pixels a side, because the engine
/// resamples all of it inline before it moves it (`MAX_PLACED_SIDE`).
fn check_placement(params: &Value, width: u32, height: u32, state: &DocState) -> Decision<()> {
    if let Some([left, top, region_width, region_height]) = params.get("region").and_then(place_region) {
        let size = state.doc.size;
        if left + region_width > i64::from(size.width) || top + region_height > i64::from(size.height) {
            return Err(refusal("invalid_params", format!("the region must lie inside the {}x{} document", size.width, size.height)));
        }
        if (i64::from(width), i64::from(height)) != (region_width, region_height) {
            let mismatch = format!("the image is {width}x{height}, not the {region_width}x{region_height} of its region");
            return Err(refusal("input_rejected", format!("{mismatch}: a checkpoint of another size does not restore this document")));
        }
    }
    let scale = params.get("scale").and_then(Value::as_f64).unwrap_or(100.0) / 100.0;
    let (placed_width, placed_height) = ((f64::from(width) * scale).ceil(), (f64::from(height) * scale).ceil());
    if placed_width > MAX_PLACED_SIDE || placed_height > MAX_PLACED_SIDE {
        let caps = format!("at most {MAX_PLACED_SIDE} pixels a side");
        return Err(refusal("budget_exceeded", format!("placed at that scale the image would be {placed_width}x{placed_height}: {caps}")));
    }
    Ok(())
}

/// The decoded input as a lossless PNG of its upright pixels and colour profile, and nothing else:
/// the caller's EXIF, GPS, XMP, text and resolution never reach the smart object, its `.pcraft`
/// checkpoint or a PSD, because the engine embeds the bytes it is given
/// (crates/engine/src/file_cmds.rs:386-388).
fn clean_png(image: Image) -> Decision<Vec<u8>> {
    let image = image.with_meta(Metadata::default());
    let options = EncodeOptions { embed_metadata: false, png_compression: PngCompression::Fast, ..Default::default() };
    photocraft_codecs::encode(&image, Format::Png, &options)
        .map_err(|error| refusal("input_rejected", format!("the image cannot be re-encoded without its metadata: {}", bounded(&error.to_string()))))
}

/// An import's decoded input as a lossless PNG of its upright pixels, its colour profile and a new
/// XMP packet of the positive File Info fields it carries (OC-13 and OD-14: creator, copyright,
/// credit and caption, `EXPORT_FIELDS`), and nothing else: the PNG encoder writes EXIF, a print
/// resolution and text only when the image holds them (png.rs:137-169), and it holds none. So the
/// caller's EXIF, GPS, camera and location data, text and resolution never reach the new document
/// (72 ppi: pt = px) nor any checkpoint of it.
fn import_png(image: Image) -> Decision<Vec<u8>> {
    let fields = merged_fields(image.meta.xmp.as_deref(), EXPORT_FIELDS, &Params::new());
    let image = image.with_meta(Metadata { xmp: xmp_packet(&fields), ..Metadata::default() });
    let options = EncodeOptions { png_compression: PngCompression::Fast, ..Default::default() };
    photocraft_codecs::encode(&image, Format::Png, &options)
        .map_err(|error| refusal("input_rejected", format!("the image cannot be re-encoded without its metadata: {}", bounded(&error.to_string()))))
}

/// The document a typed command acts on: the active one (which the preconditions may name), the
/// one `document.activate` activates, or none for `file.new`.
fn target_index(session: &Session, command: &str, params: &Value, pre: &Preconditions) -> Decision<Option<usize>> {
    if command == "file.new" {
        return Ok(None);
    }
    let named = pre
        .document
        .filter(|reference| !reference.is_empty())
        .map(|reference| find_document(session, reference).ok_or_else(|| refusal("target_not_found", "the document the preconditions name is not open")))
        .transpose()?;
    if command == "document.activate" {
        let wanted = params.get("document").and_then(Value::as_u64).and_then(|index| usize::try_from(index).ok());
        let wanted = wanted.filter(|index| *index < session.documents().len()).ok_or_else(|| refusal("target_not_found", "no document at that index"))?;
        return match named {
            Some(index) if index != wanted => Err(refusal("target_not_found", "the document to activate is not the one the preconditions name")),
            _ => Ok(Some(wanted)),
        };
    }
    let active = session.active_index().ok_or_else(|| refusal("target_not_found", "no document is open"))?;
    match named {
        Some(index) if index != active => Err(refusal("target_not_active", "the document the preconditions name is not the active document")),
        _ => Ok(Some(active)),
    }
}

fn check_state(state: &DocState, pre: &Preconditions) -> Decision<()> {
    if pre.rev.is_some_and(|rev| rev != state.revision) {
        return Err(refusal("revision_conflict", format!("the document is at {}, not at the expected revision", rev_token(state.revision))));
    }
    if let Some(layer) = pre.active_layer
        && state.active_layer.map(|active| active.0) != Some(layer)
    {
        return Err(refusal("layer_not_active", "the expected layer is not the active layer"));
    }
    if let Some((length, label)) = &pre.history_head {
        let entries = state.history.entries();
        if entries.len() != *length || entries.last() != Some(label) {
            return Err(refusal("history_head_conflict", "the history head is not the one the caller expects"));
        }
    }
    if state.floating.is_some() {
        return Err(refusal("editor_busy", "a floating selection is being placed; not applied"));
    }
    Ok(())
}

fn check_fonts(command: &str, params: &Value, families: &[String], pre: &Preconditions) -> Decision<()> {
    if matches!(command, "type.create" | "type.setStyle")
        && let Some(font) = params.get("font").and_then(Value::as_str)
        && !families.iter().any(|family| family == font)
    {
        return Err(refusal("font_not_loaded", format!("the font family `{}` is not loaded in this page", bounded(font))));
    }
    if let Some(expected) = &pre.fontset_digest
        && *expected != fontset_digest(families)
    {
        return Err(refusal("font_not_loaded", "the page's font set is not the one the caller probed"));
    }
    Ok(())
}

fn check_targets(command: &str, params: &Value, state: Option<&DocState>) -> Decision<()> {
    if let Some(layer) = params.get("layer").and_then(Value::as_u64)
        && state.is_none_or(|state| state.doc.layer(LayerId(layer)).is_none())
    {
        return Err(refusal("target_not_found", format!("no layer {layer} in the target document")));
    }
    if command == "image.crop"
        && let Some(state) = state
    {
        let size = state.doc.size;
        let inside = |start: &str, extent: &str, limit: u32| {
            let start = params.get(start).and_then(Value::as_i64).unwrap_or(0);
            let extent = params.get(extent).and_then(Value::as_i64).unwrap_or(0);
            start.checked_add(extent).is_some_and(|end| end <= i64::from(limit))
        };
        if !(inside("x", "width", size.width) && inside("y", "height", size.height)) {
            return Err(refusal("invalid_params", format!("the crop must lie inside the {}x{} document", size.width, size.height)));
        }
    }
    // Color Range's eyedropper samples lie inside the document (the engine refuses others).
    if command == "select.colorRange"
        && let Some(state) = state
        && let Some(Value::Array(points)) = params.get("points")
        && !points.iter().all(|point| sampled_inside(point, state.doc.size.width, state.doc.size.height))
    {
        let size = state.doc.size;
        return Err(refusal("invalid_params", format!("Color Range samples inside the {}x{} document", size.width, size.height)));
    }
    // A variable binds a layer of the target document.
    if command == "image.variables.define"
        && let Some(state) = state
        && let Some(Value::Array(defs)) = params.get("defs")
        && let Some(layer) = defs.iter().filter_map(|def| def.get("layer").and_then(Value::as_u64)).find(|layer| state.doc.layer(LayerId(*layer)).is_none())
    {
        return Err(refusal("target_not_found", format!("no layer {layer} in the target document")));
    }
    // A data set exists, and never replaces pixels from a file (an ambient file read).
    if command == "image.applyDataSet" {
        let set = state
            .and_then(|state| data_set(&state.doc.variables, params))
            .ok_or_else(|| refusal("target_not_found", "no such data set in the target document"))?;
        if set.values.iter().any(|value| matches!(value.value, VariableValue::Pixels(_))) {
            return Err(refusal("invalid_params", "the data set replaces pixels from image files; not applied"));
        }
    }
    // Adjustment Properties edits a tone layer with its own kind's values (`check_adjustment`); a
    // layer that is no adjustment layer is the engine's precondition (`check_engine`).
    if command == "layer.setAdjustment"
        && let Some(state) = state
        && let Some(LayerContent::Adjustment(adjustment)) = named_layer(params, state).map(|layer| &layer.content)
    {
        check_adjustment(adjustment, params)?;
    }
    // Narrowed until an engine bug is fixed (OC-18): a fill opacity below 100 % with an outside or
    // centre stroke, in either order. The fill opacity reaches a layer's own pixels and never its
    // effects, but the engine lays such a stroke of any layer but a shape layer beneath the layer's
    // whole shape (crates/compose/src/effects.rs:1377-1383), which a lower fill shows where Photoshop
    // shows what lies below (`stroke_under_shape`).
    if command == "layer.setProps"
        && let Some(state) = state
        && params.get("fill").and_then(Value::as_f64).is_some_and(|fill| fill < 1.0)
        && named_layer(params, state).is_some_and(stroke_under_shape)
    {
        return Err(refusal("invalid_params", FILL_WITH_STROKE));
    }
    if command == "layer.layerStyle.stroke"
        && let Some(state) = state
        && let Some(layer) = state.active_layer.and_then(|id| state.doc.layer(id))
        && layer.fill_opacity < 1.0
        && !matches!(layer.content, LayerContent::Shape(_))
        && stroke_reaches_outside(params)
    {
        return Err(refusal("invalid_params", FILL_WITH_STROKE));
    }
    Ok(())
}

/// Why a fill opacity below 100 % and an outside or centre stroke are not applied together (OC-18).
const FILL_WITH_STROKE: &str =
    "fill below 1 with an outside or centre stroke is narrowed (OC-18): the engine paints such a stroke under the whole shape; keep fill 1 or stroke inside";

/// Whether the engine lays one of `layer`'s strokes beneath the layer's whole shape (OC-18): an
/// enabled outside or centre stroke that paints, with the layer's styles on, of any layer but a
/// shape layer, whose stroke follows its outline instead (crates/compose/src/effects.rs:1377-1383).
fn stroke_under_shape(layer: &Layer) -> bool {
    let shape = matches!(layer.content, LayerContent::Shape(_));
    !shape && layer.effects.enabled && layer.effects.items.iter().any(|effect| effect.enabled() && outside_stroke(effect))
}

/// An outside or centre stroke that paints: a size and an opacity above 0.
fn outside_stroke(effect: &photocraft_doc::Effect) -> bool {
    let photocraft_doc::Effect::Stroke(stroke) = effect else { return false };
    stroke.position != photocraft_doc::StrokePosition::Inside && stroke.size > 0.0 && stroke.common.opacity > 0.0
}

/// Whether a `layer.layerStyle.stroke` call gives the active layer a stroke that reaches outside its
/// shape: an outside one (the engine's default position, crates/engine/src/layer_style.rs:156-161)
/// or a centre one, at an opacity above 0.
fn stroke_reaches_outside(params: &Value) -> bool {
    let position = params.get("position").and_then(Value::as_str).unwrap_or("outside");
    position != "inside" && params.get("opacity").and_then(Value::as_f64).is_none_or(|opacity| opacity > 0.0)
}

/// The layer a call names by its `layer` parameter in the target document.
fn named_layer<'s>(params: &Value, state: &'s DocState) -> Option<&'s Layer> {
    params.get("layer").and_then(Value::as_u64).and_then(|id| state.doc.layer(LayerId(id)))
}

/// Adjustment Properties on a layer's `adjustment`: it is one of the six tone kinds the table
/// creates (`tone_kind`), every value is a key of that kind (`tone_keys`), the merged Levels pair
/// keeps `inWhite` two levels above `inBlack` (the engine would raise it silently,
/// adjust_params.rs:435-436), and a colorized Hue/Saturation takes no hue or saturation below 0
/// (the engine would clamp them to 0, adjust_params.rs:393-394).
fn check_adjustment(adjustment: &Adjustment, params: &Value) -> Decision<()> {
    let Some(kind) = tone_kind(adjustment) else {
        let label = adjustment.label();
        return Err(refusal("invalid_params", format!("the layer is a {label} adjustment: the typed table edits the six tone kinds it creates")));
    };
    let keys = tone_keys(kind);
    let given = params.as_object().ok_or_else(|| refusal("invalid_params", "parameters must be a JSON object"))?;
    if let Some(key) = given.keys().find(|key| key.as_str() != "layer" && !keys.contains(&key.as_str())) {
        let message = format!("`{}` is not a key of a {kind} layer, which takes {}", bounded(key), keys.join(", "));
        return Err(refusal("invalid_params", message));
    }
    if let Adjustment::Levels { master, per_channel, space, .. } = adjustment {
        // In Lab the top-level keys address the lightness channel: Lab has no composite.
        let channel = if *space == ToneSpace::Lab { per_channel.first().unwrap_or(master) } else { master };
        let (black, white) = (level(params, "inBlack", channel.in_black, 0.0, 253.0), level(params, "inWhite", channel.in_white, 2.0, 255.0));
        if white + LEVEL_EPSILON < black + 2.0 {
            let message = format!("`inWhite` would be {white}, less than `inBlack` {black} + 2: the engine would raise it silently");
            return Err(refusal("invalid_params", message));
        }
    }
    if let Adjustment::HueSaturation { colorize: true, .. } = adjustment
        && let Some(key) = ["hue", "saturation"].into_iter().find(|key| params.get(*key).and_then(Value::as_f64).is_some_and(|value| value < 0.0))
    {
        return Err(refusal("invalid_params", format!("a colorized Hue/Saturation takes `{key}` from 0: the engine would clamp it silently")));
    }
    Ok(())
}

/// A Levels level as the engine reads it for a merge (adjust_params.rs:435-436): the value given,
/// else the layer's own on the 0..255 scale, within the key's range.
fn level(params: &Value, key: &str, kept: f32, low: f32, high: f32) -> f32 {
    match params.get(key).and_then(Value::as_f64) {
        Some(given) => given as f32,
        None => (kept * 255.0).clamp(low, high),
    }
}

/// The six tone kinds the table creates (`layer.newAdjustmentLayer.<kind>`), by the engine's own
/// names (`adjustment_kind`).
fn tone_kind(adjustment: &Adjustment) -> Option<&'static str> {
    match adjustment {
        Adjustment::BrightnessContrast { .. } => Some("brightnessContrast"),
        Adjustment::Levels { .. } => Some("levels"),
        Adjustment::Curves { .. } => Some("curves"),
        Adjustment::Exposure { .. } => Some("exposure"),
        Adjustment::Vibrance { .. } => Some("vibrance"),
        Adjustment::HueSaturation { .. } => Some("hueSaturation"),
        _ => None,
    }
}

/// The keys a tone layer of `kind` takes: those of its `layer.newAdjustmentLayer.<kind>` entry.
fn tone_keys(kind: &str) -> &'static [&'static str] {
    POLICY.iter().find(|entry| entry.id.strip_prefix("layer.newAdjustmentLayer.") == Some(kind)).map(|entry| entry.keys).unwrap_or_default()
}

/// A Color Range sample point inside a `width` x `height` document.
fn sampled_inside(point: &Value, width: u32, height: u32) -> bool {
    match point.as_array().map(Vec::as_slice) {
        Some([x, y]) => x.as_i64().is_some_and(|x| x < i64::from(width)) && y.as_i64().is_some_and(|y| y < i64::from(height)),
        _ => false,
    }
}

/// The data set `image.applyDataSet` names, by `name` or by `index`.
fn data_set<'v>(variables: &'v photocraft_doc::Variables, params: &Value) -> Option<&'v photocraft_doc::DataSet> {
    match (params.get("name").and_then(Value::as_str), params.get("index").and_then(Value::as_u64)) {
        (Some(name), _) => variables.data_set(name),
        (None, Some(index)) => usize::try_from(index).ok().and_then(|index| variables.data_sets.get(index)),
        (None, None) => None,
    }
}

/// Select and Mask's effective output: decontamination turns a selection or mask output into a
/// new layer with a mask, as the engine does (smartselect_cmds.rs:155-162).
fn refine_output(params: &Value) -> &str {
    let output = params.get("output").and_then(Value::as_str).unwrap_or("selection");
    let decontaminate = params.get("decontaminate").and_then(Value::as_bool) == Some(true);
    if decontaminate && matches!(output, "selection" | "layerMask") { "newLayerWithMask" } else { output }
}

fn check_limits(session: &Session, command: &str, params: &Value, state: Option<&DocState>, runtime: Runtime) -> Decision<()> {
    if command == "file.new" && session.documents().len() >= MAX_DOCUMENTS {
        return Err(refusal("document_limit", format!("at most {MAX_DOCUMENTS} documents may be open")));
    }
    let adds_layer = matches!(command, "layer.new.layer" | "type.create" | "layer.duplicate" | "layer.newFillLayer.solidColor" | "edit.contentAwareFill")
        || command.starts_with("layer.newAdjustmentLayer.")
        || (command == "select.refineEdge" && matches!(refine_output(params), "newLayer" | "newLayerWithMask"));
    if adds_layer && state.is_some_and(|state| state.doc.layer_count() >= MAX_LAYERS) {
        return Err(refusal("layer_limit", format!("at most {MAX_LAYERS} layers per document")));
    }
    // The Content-Aware Fill hole: its bounding box within the document, which must exist (the
    // engine refuses a selection outside the canvas once it runs: edit_menu_cmds.rs:331-334). The
    // browser hook fills inline on the page's only thread, so it takes at most 512 pixels a side
    // and 65,536 pixels; a native worker takes up to 1024 a side and 262,144 pixels.
    if command == "edit.contentAwareFill"
        && let Some(state) = state
        && let Some(selection) = &state.doc.selection
    {
        let hole = selection.content_bounds().intersect(&state.doc.bounds());
        if hole.is_empty() {
            return Err(refusal("invalid_params", "the selection lies outside the canvas: there is no hole to fill"));
        }
        let (width, height) = (hole.width(), hole.height());
        let fits = |side: u32, pixels: u64| width <= side && height <= side && u64::from(width) * u64::from(height) <= pixels;
        if !fits(MAX_NATIVE_HOLE_SIDE, MAX_NATIVE_HOLE_PIXELS) {
            let caps = format!("at most {MAX_NATIVE_HOLE_SIDE} pixels a side and {MAX_NATIVE_HOLE_PIXELS} pixels");
            return Err(refusal("budget_exceeded", format!("the Content-Aware Fill hole is {width}x{height}: {caps}")));
        }
        let runs = if fits(MAX_HOLE_SIDE, MAX_HOLE_PIXELS) { Runtimes::Both } else { Runtimes::NativeOnly };
        if !runs.allows(runtime) {
            let caps = format!("the browser hook fills at most {MAX_HOLE_SIDE} pixels a side and {MAX_HOLE_PIXELS} pixels");
            return Err(refusal("native_only", format!("the Content-Aware Fill hole is {width}x{height}: {caps}; a native worker fills it")));
        }
    }
    Ok(())
}

/// The engine's own precondition for the call, and the human's channel, Quick Mask and mask
/// targets, which would otherwise retarget an agent's pixel or selection edit.
fn check_engine(app: &PhotocraftApp, command: &str, params: &Value, state: Option<&DocState>) -> Decision<()> {
    if let Some(reason) = app.session.disabled_reason_with(command, params) {
        return Err(refusal("engine_precheck_failed", reason));
    }
    let Some(state) = state else { return Ok(()) };
    let pixels = command == "edit.fill"
        || command.starts_with("filter.")
        || command.starts_with("image.adjustments.")
        || matches!(command, "image.autoTone" | "image.autoContrast" | "image.autoColor" | "edit.contentAwareFill");
    let composite = state.channel_view.target == ChannelTarget::Composite && state.channel_view.layer_mask.is_none() && state.doc.quick_mask.is_none();
    if pixels && !composite {
        return Err(refusal("engine_precheck_failed", "the Channels panel targets a channel, Quick Mask or a mask view, not the layer's pixels"));
    }
    let masked = state.active_layer.and_then(|id| state.doc.layer(id)).is_some_and(|layer| layer.mask.is_some());
    if pixels && masked && app.ui.mask_target && params.get("target").is_none() && photocraft_engine::channel_cmds::follows_target(command) {
        return Err(refusal("engine_precheck_failed", "the layer mask is targeted in the Layers panel, not the layer's pixels"));
    }
    let selection = command.starts_with("select.") || matches!(command, "layer.layerMask.revealSelection" | "layer.layerMask.hideSelection");
    if selection && state.doc.quick_mask.is_some() {
        return Err(refusal("engine_precheck_failed", "Quick Mask mode is on: the selection is being edited as a channel"));
    }
    // Type depth edits a type layer: the named one, else the active one.
    if matches!(command, "type.edit" | "type.setStyle") {
        let layer = params.get("layer").and_then(Value::as_u64).map(LayerId).or(state.active_layer).and_then(|id| state.doc.layer(id));
        if !layer.is_some_and(|layer| matches!(layer.content, LayerContent::Text(_))) {
            return Err(refusal("engine_precheck_failed", "the target layer is not a type layer"));
        }
    }
    // Select and Mask writes a mask on the active layer, or copies its pixels to a new layer.
    if command == "select.refineEdge" {
        let active = state.active_layer.and_then(|id| state.doc.layer(id));
        let usable = match refine_output(params) {
            "selection" => true,
            "layerMask" => active.is_some(),
            _ => active.is_some_and(|layer| layer.surface().is_some()),
        };
        if !usable {
            return Err(refusal("engine_precheck_failed", "Select and Mask outputs to the active layer, which is missing or has no pixels"));
        }
    }
    // Adjustment Properties edits an adjustment layer (the engine refuses any other once it runs,
    // commands.rs:692).
    if command == "layer.setAdjustment" && !named_layer(params, state).is_some_and(|layer| matches!(layer.content, LayerContent::Adjustment(_))) {
        return Err(refusal("engine_precheck_failed", "the target layer is not an adjustment layer"));
    }
    // A clipping mask clips a layer to the one below it in its stack: the bottom one has none.
    if matches!(command, "layer.createClippingMask" | "layer.releaseClippingMask")
        && let Some(path) = named_layer(params, state).and_then(|layer| state.doc.path_of(layer.id))
        && path.last() == Some(&0)
    {
        return Err(refusal("engine_precheck_failed", "the layer is the bottom of its stack: there is no layer below it to clip to"));
    }
    Ok(())
}

// ------------------------------------------------------------------ reads

#[derive(Clone, Copy, PartialEq)]
enum View {
    Summary,
    Layers,
    Full,
}

enum Target {
    Composite,
    Layer(u64),
    Mask(u64),
    Selection,
    History(u64),
}

#[derive(Clone, Copy, PartialEq)]
enum Encoding {
    Png,
    Jpeg(u8),
    Webp(Option<u8>),
}

impl Encoding {
    /// File extension, MIME type and encoder options (never EXIF, XMP or text chunks).
    fn options(self) -> (&'static str, &'static str, EncodeOptions) {
        let base = EncodeOptions { embed_metadata: false, ..Default::default() };
        match self {
            Encoding::Png => ("png", "image/png", base),
            Encoding::Jpeg(quality) => ("jpg", "image/jpeg", EncodeOptions { jpeg_quality: quality, ..base }),
            Encoding::Webp(None) => ("webp", "image/webp", EncodeOptions { webp_lossless: true, ..base }),
            Encoding::Webp(Some(quality)) => ("webp", "image/webp", EncodeOptions { webp_lossless: false, webp_quality: quality, ..base }),
        }
    }
}

/// A composite decoded to straight 8-bit RGBA.
struct Decoded {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    icc: Option<Vec<u8>>,
}

impl Decoded {
    fn pixel(&self, x: i64, y: i64) -> Option<[u8; 4]> {
        let (x, y) = (u32::try_from(x).ok()?, u32::try_from(y).ok()?);
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = usize::try_from(y).ok()?.checked_mul(usize::try_from(self.width).ok()?)?.checked_add(usize::try_from(x).ok()?)?.checked_mul(4)?;
        self.rgba.get(at..at.checked_add(4)?)?.try_into().ok()
    }

    /// The box of the pixels whose alpha is not zero, in image (document) pixels.
    fn alpha_box(&self) -> Option<Rect> {
        let width = usize::try_from(self.width).ok()?.max(1);
        let mut found: Option<(usize, usize, usize, usize)> = None;
        for (i, pixel) in self.rgba.chunks_exact(4).enumerate() {
            if pixel.get(3).is_some_and(|alpha| *alpha > 0) {
                let (x, y) = (i % width, i / width);
                found = Some(match found {
                    None => (x, y, x, y),
                    Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                });
            }
        }
        let (x0, y0, x1, y1) = found?;
        Some(Rect::new(i32::try_from(x0).ok()?, i32::try_from(y0).ok()?, i32::try_from(x1 + 1).ok()?, i32::try_from(y1 + 1).ok()?))
    }

    fn crop(&self, area: Rect) -> Vec<u8> {
        let stride = self.width as usize * 4;
        let row = area.width() as usize * 4;
        let mut out = Vec::with_capacity(row * area.height() as usize);
        for y in area.y0..area.y1 {
            let start = y as usize * stride + area.x0 as usize * 4;
            if let Some(pixels) = self.rgba.get(start..start + row) {
                out.extend_from_slice(pixels);
            }
        }
        out
    }
}

fn read_failed(message: impl Into<String>) -> ViewError {
    ViewError { code: "read_failed", message: message.into() }
}

fn unsupported(message: impl Into<String>) -> ViewError {
    ViewError { code: "unsupported_read", message: message.into() }
}

fn over_budget(message: impl Into<String>) -> ViewError {
    ViewError { code: "budget_exceeded", message: message.into() }
}

/// A read's parameters: an object with no key outside `allowed`.
fn read_params<'p>(params: &'p Value, allowed: &[&str]) -> Result<&'p Params, ViewError> {
    let map = params.as_object().ok_or_else(|| read_failed("read parameters must be a JSON object"))?;
    if let Some(key) = map.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(read_failed(format!("unknown parameter `{}`", bounded(key))));
    }
    Ok(map)
}

fn resolve(session: &Session, reference: &Value) -> Result<usize, ViewError> {
    let reference = doc_ref(reference).map_err(read_failed)?;
    find_document(session, reference).ok_or_else(|| read_failed("no open document matches that reference"))
}

/// The document a read names (`{"index"}`, `{"id"}` or both), or the active one.
fn document<'s>(session: &'s Session, reference: Option<&Value>) -> Result<(usize, &'s DocState), ViewError> {
    let index = match reference {
        None | Some(Value::Null) => session.active_index().ok_or_else(|| read_failed("no document is open"))?,
        Some(reference) => resolve(session, reference)?,
    };
    let state = session.documents().get(index).ok_or_else(|| read_failed("no document is open"))?;
    Ok((index, state))
}

/// INV-28: decided from the document header, before any pixel is allocated.
fn within_budget(doc: &Document) -> Result<(), ViewError> {
    let (width, height) = (doc.size.width, doc.size.height);
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(over_budget(format!("{width}x{height} exceeds {MAX_SIDE} pixels per side")));
    }
    let layers = doc.layer_count();
    if layers > MAX_LAYERS {
        return Err(over_budget(format!("{layers} layers exceed {MAX_LAYERS}")));
    }
    Ok(())
}

fn layer_param(map: &Params) -> Result<Option<u64>, ViewError> {
    match map.get("layerId") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| read_failed("`layerId` must be a layer id")),
    }
}

fn render_target(map: &Params) -> Result<Target, ViewError> {
    let name = match map.get("target") {
        None => "composite",
        Some(Value::String(name)) => name.as_str(),
        Some(_) => return Err(read_failed("`target` must be a string")),
    };
    let layer = layer_param(map)?;
    let history = match map.get("historyIndex") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_u64().ok_or_else(|| read_failed("`historyIndex` must be a history entry index"))?),
    };
    match (name, layer, history) {
        ("composite", None, None) => Ok(Target::Composite),
        ("layer", Some(id), None) => Ok(Target::Layer(id)),
        ("mask", Some(id), None) => Ok(Target::Mask(id)),
        ("selection", None, None) => Ok(Target::Selection),
        ("history", None, Some(index)) => Ok(Target::History(index)),
        ("composite" | "layer" | "mask" | "selection" | "history", _, _) => {
            Err(read_failed("`layerId` goes with layer and mask renders, `historyIndex` with history renders"))
        }
        (other, _, _) => Err(unsupported(format!("unknown render target `{}`", bounded(other)))),
    }
}

fn encoding(map: &Params) -> Result<Encoding, ViewError> {
    let quality = match map.get("quality") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let quality = value.as_u64().filter(|q| (1..=100).contains(q)).and_then(|q| u8::try_from(q).ok());
            Some(quality.ok_or_else(|| read_failed("`quality` must be an integer from 1 to 100"))?)
        }
    };
    let format = match map.get("format") {
        None | Some(Value::Null) => "png",
        Some(Value::String(format)) => format.as_str(),
        Some(_) => return Err(read_failed("`format` must be png, jpeg or webp")),
    };
    match (format, quality) {
        ("png", None) => Ok(Encoding::Png),
        ("png", Some(_)) => Err(read_failed("`quality` applies to jpeg and webp")),
        ("jpeg", quality) => Ok(Encoding::Jpeg(quality.unwrap_or(90))),
        ("webp", quality) => Ok(Encoding::Webp(quality)),
        (other, _) => Err(unsupported(format!("unknown format `{}`", bounded(other)))),
    }
}

/// The document's composite as `photocraft_io::export` encodes it, without its EXIF or XMP.
fn encode_document(doc: &Document, encoding: Encoding) -> Result<(Vec<u8>, &'static str), ViewError> {
    let (_, mime, _) = encoding.options();
    Ok((bounded_output(encode_composite(doc, encoding)?)?, mime))
}

/// `encode_document`'s bytes before `MAX_OUTPUT_BYTES`, which bounds what a read returns, not what
/// it computes from (`decode_rgba`).
fn encode_composite(doc: &Document, encoding: Encoding) -> Result<Vec<u8>, ViewError> {
    let (extension, _, encode) = encoding.options();
    let options = photocraft_io::ExportOptions { encode, xmp: photocraft_io::XmpEmbed::None, ..Default::default() };
    let output = photocraft_io::export(doc, extension, &options).map_err(|error| read_failed(format!("encoding failed: {error}")))?;
    Ok(output.bytes)
}

fn encode_png(width: u32, height: u32, layout: ChannelLayout, data: Vec<u8>, icc: Option<Vec<u8>>) -> Result<Vec<u8>, ViewError> {
    let image = Image::from_u8(width, height, layout, data).map_err(|error| read_failed(format!("encoding failed: {error}")))?.with_icc(icc);
    let options = EncodeOptions { embed_metadata: false, ..Default::default() };
    let bytes =
        photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &options).map_err(|error| read_failed(format!("encoding failed: {error}")))?;
    bounded_output(bytes)
}

fn bounded_output(bytes: Vec<u8>) -> Result<Vec<u8>, ViewError> {
    if bytes.len() > MAX_OUTPUT_BYTES {
        return Err(over_budget(format!("the encoded image exceeds {MAX_OUTPUT_BYTES} bytes")));
    }
    Ok(bytes)
}

/// The composite's pixels as the PNG render delivers them. That PNG stays inside the read, so the
/// output bound does not apply to it: a 4096-pixel photo's PNG may exceed what a read returns.
fn decode_rgba(doc: &Document) -> Result<Decoded, ViewError> {
    let png = encode_composite(doc, Encoding::Png)?;
    let image = photocraft_codecs::decode(&png).map_err(|error| read_failed(format!("decoding failed: {error}")))?;
    Ok(Decoded { width: image.width(), height: image.height(), icc: image.icc.clone(), rgba: image.to_rgba8() })
}

/// The layer alone, as Layer › Export Layer composites it: visible, its blend mode ignored,
/// unclipped and without the selection, over a transparent canvas of the document's size.
fn solo(doc: &Document, layer: &Layer) -> Document {
    let mut alone = layer.clone();
    alone.visible = true;
    alone.blend = if alone.is_group() { BlendMode::PassThrough } else { BlendMode::Normal };
    alone.clipped = false;
    let mut out = doc.clone();
    out.layers = vec![alone];
    out.selection = None;
    out
}

/// A grayscale surface over the document as an 8-bit gray PNG (0 hidden or unselected, 255
/// revealed or selected).
fn gray_png(surface: &Surface, doc: &Document) -> Result<Vec<u8>, ViewError> {
    let area = doc.bounds();
    let channels = surface.channels().max(1);
    let gray: Vec<u8> = surface.read_region(area).iter().step_by(channels).map(|value| quantize(*value)).collect();
    encode_png(area.width(), area.height(), ChannelLayout::Gray, gray, None)
}

fn quantize(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The fields every render reports.
fn render_info(target: &str, mime: &str, index: usize, state: &DocState, bytes: &[u8]) -> Value {
    json!({
        "target": target,
        "mime": mime,
        "documentId": state.doc.id.0.to_string(),
        "rev": rev_token(state.revision),
        "index": index,
        "byteLength": bytes.len(),
    })
}

fn extend(mut value: Value, fields: Value) -> Value {
    if let (Some(object), Value::Object(fields)) = (value.as_object_mut(), fields) {
        object.extend(fields);
    }
    value
}

/// Renders the composite, one layer (trimmed to its visible pixels), a layer mask, the selection
/// or a history state of any open document (active or not). PNG by default; composites and
/// history states also as JPEG or WebP (`format`, `quality`).
pub fn render(session: &Session, params: &Value) -> Result<Rendered, ViewError> {
    let map = read_params(params, &["document", "target", "layerId", "historyIndex", "format", "quality"])?;
    let target = render_target(map)?;
    let encoding = encoding(map)?;
    if !matches!(target, Target::Composite | Target::History(_)) && encoding != Encoding::Png {
        return Err(unsupported("layer, mask and selection renders are PNG"));
    }
    let (index, state) = document(session, map.get("document"))?;
    within_budget(&state.doc)?;
    let doc = &state.doc;
    let (width, height) = (doc.size.width, doc.size.height);
    match target {
        Target::Composite => {
            let (bytes, mime) = encode_document(doc, encoding)?;
            let info = extend(render_info("composite", mime, index, state, &bytes), json!({"width": width, "height": height}));
            Ok(Rendered { bytes, info })
        }
        Target::Layer(id) => {
            let layer = doc.layer(LayerId(id)).ok_or_else(|| read_failed(format!("no layer {id} in that document")))?;
            let pixels = decode_rgba(&solo(doc, layer))?;
            let bounds = pixels.alpha_box().ok_or_else(|| read_failed("the layer has no visible pixels"))?;
            let bytes = encode_png(bounds.width(), bounds.height(), ChannelLayout::Rgba, pixels.crop(bounds), pixels.icc)?;
            let fields = json!({
                "layerId": id,
                "name": layer.name,
                "kind": layer.content.kind_name(),
                "bounds": rect_json(bounds),
                "width": bounds.width(),
                "height": bounds.height(),
            });
            let info = extend(render_info("layer", "image/png", index, state, &bytes), fields);
            Ok(Rendered { bytes, info })
        }
        Target::Mask(id) => {
            let layer = doc.layer(LayerId(id)).ok_or_else(|| read_failed(format!("no layer {id} in that document")))?;
            let mask = layer.mask.as_ref().ok_or_else(|| read_failed("the layer has no layer mask"))?;
            let bytes = gray_png(&mask.surface, doc)?;
            let fields = json!({"layerId": id, "width": width, "height": height, "mask": mask_json(mask)});
            let info = extend(render_info("mask", "image/png", index, state, &bytes), fields);
            Ok(Rendered { bytes, info })
        }
        Target::Selection => {
            let selection = doc.selection.as_ref().ok_or_else(|| read_failed("the document has no selection"))?;
            let bytes = gray_png(selection, doc)?;
            let fields = json!({"width": width, "height": height, "bounds": rect_json(selection.content_bounds())});
            let info = extend(render_info("selection", "image/png", index, state, &bytes), fields);
            Ok(Rendered { bytes, info })
        }
        Target::History(position) => {
            let entries = state.history.entries();
            let position = usize::try_from(position).ok().filter(|position| *position < entries.len());
            let position = position.ok_or_else(|| read_failed("no history entry at that index"))?;
            let current = position + 1 == entries.len();
            let shown =
                if current { Arc::clone(&state.doc) } else { state.history.state(position).ok_or_else(|| read_failed("that history state is not held"))? };
            within_budget(&shown)?;
            let (bytes, mime) = encode_document(&shown, encoding)?;
            let fields = json!({
                "historyIndex": position,
                "label": entries.get(position),
                "current": current,
                "width": shown.size.width,
                "height": shown.size.height,
            });
            let info = extend(render_info("history", mime, index, state, &bytes), fields);
            Ok(Rendered { bytes, info })
        }
    }
}

fn point(value: &Value) -> Result<(i64, i64), ViewError> {
    let map = value.as_object().filter(|map| map.len() == 2).ok_or_else(|| read_failed("each point is an object with `x` and `y` only"))?;
    match (map.get("x").and_then(Value::as_i64), map.get("y").and_then(Value::as_i64)) {
        (Some(x), Some(y)) => Ok((x, y)),
        _ => Err(read_failed("each point needs integer document coordinates `x` and `y`")),
    }
}

/// Samples the composite's pixels (or one layer's, alone) at up to 64 document points; a point
/// outside the document samples nothing. Never `document.pixel`, which runs a command.
pub fn sample(session: &Session, params: &Value) -> Result<Value, ViewError> {
    let map = read_params(params, &["document", "points", "layerId"])?;
    let points = match map.get("points") {
        Some(Value::Array(points)) if !points.is_empty() => points,
        _ => return Err(read_failed("`points` must be a non-empty array of points with `x` and `y`")),
    };
    if points.len() > MAX_POINTS {
        return Err(over_budget(format!("at most {MAX_POINTS} points per sample")));
    }
    let wanted: Vec<(i64, i64)> = points.iter().map(point).collect::<Result<_, _>>()?;
    let layer_id = layer_param(map)?;
    let (index, state) = document(session, map.get("document"))?;
    within_budget(&state.doc)?;
    let pixels = match layer_id {
        Some(id) => {
            let layer = state.doc.layer(LayerId(id)).ok_or_else(|| read_failed(format!("no layer {id} in that document")))?;
            decode_rgba(&solo(&state.doc, layer))?
        }
        None => decode_rgba(&state.doc)?,
    };
    let samples: Vec<Value> = wanted
        .iter()
        .map(|&(x, y)| match pixels.pixel(x, y) {
            Some(rgba) => json!({"x": x, "y": y, "rgba": rgba, "hex": hex_rgb(rgba)}),
            None => json!({"x": x, "y": y, "rgba": null, "hex": null}),
        })
        .collect();
    Ok(json!({
        "documentId": state.doc.id.0.to_string(),
        "rev": rev_token(state.revision),
        "index": index,
        "layerId": layer_id,
        "width": pixels.width,
        "height": pixels.height,
        "samples": samples,
    }))
}

/// Every open document by DocId, and the named (or active) one with its history and, for the
/// `layers` and `full` views, its layers top to bottom; `layerId` adds one layer's detail.
pub fn describe(session: &Session, params: &Value) -> Result<Value, ViewError> {
    let map = read_params(params, &["document", "layerId", "view"])?;
    let view = match map.get("view") {
        None | Some(Value::Null) => View::Summary,
        Some(Value::String(view)) => match view.as_str() {
            "summary" => View::Summary,
            "layers" => View::Layers,
            "full" => View::Full,
            other => return Err(unsupported(format!("unknown view `{}`", bounded(other)))),
        },
        Some(_) => return Err(read_failed("`view` must be summary, layers or full")),
    };
    let target = match map.get("document") {
        None | Some(Value::Null) => session.active_index(),
        Some(reference) => Some(resolve(session, reference)?),
    };
    let active = session.active_index();
    let state = target.and_then(|index| session.documents().get(index));
    let layer = match layer_param(map)? {
        None => Value::Null,
        Some(id) => {
            let state = state.ok_or_else(|| read_failed("no document is open"))?;
            layer_detail(state.doc.layer(LayerId(id)).ok_or_else(|| read_failed(format!("no layer {id} in that document")))?)
        }
    };
    let documents: Vec<Value> =
        session.documents().iter().enumerate().take(MAX_DOCUMENT_ROWS).map(|(index, state)| document_row(index, state, active)).collect();
    let document = match (target, state) {
        (Some(index), Some(state)) => document_detail(index, state, active, view),
        _ => Value::Null,
    };
    Ok(json!({
        "documents": documents,
        "count": session.documents().len(),
        "active": active,
        "activeId": active_id(session),
        "document": document,
        "layer": layer,
    }))
}

fn document_row(index: usize, state: &DocState, active: Option<usize>) -> Value {
    json!({
        "index": index,
        "id": state.doc.id.0.to_string(),
        "name": state.doc.name,
        "width": state.doc.size.width,
        "height": state.doc.size.height,
        "rev": rev_token(state.revision),
        "dirty": state.is_dirty(),
        "active": active == Some(index),
    })
}

fn document_detail(index: usize, state: &DocState, active: Option<usize>, view: View) -> Value {
    let doc = &state.doc;
    let channel = match state.channel_view.target {
        ChannelTarget::Composite => "composite".to_string(),
        ChannelTarget::Color(k) => format!("color:{k}"),
        ChannelTarget::Alpha(i) => format!("alpha:{i}"),
    };
    let fields = json!({
        "mode": format!("{:?}", doc.mode),
        "depth": format!("{:?}", doc.depth),
        "resolution": doc.resolution_dpi,
        "layerCount": doc.layer_count(),
        "activeLayer": state.active_layer.map(|layer| layer.0),
        "selectedLayers": state.selected_layers().iter().map(|layer| layer.0).collect::<Vec<u64>>(),
        "selection": doc.selection.as_ref().map(|selection| json!({"bounds": rect_json(selection.content_bounds())})),
        "quickMask": doc.quick_mask.is_some(),
        "channelTarget": channel,
        "maskView": state.channel_view.layer_mask.is_some(),
        "floatingSelection": state.floating.is_some(),
        "history": history_json(state),
        "channels": photocraft_engine::channel_cmds::channels_json(state),
        "guides": {"horizontal": guides(&doc.guides.horizontal), "vertical": guides(&doc.guides.vertical)},
        "guidesTruncated": doc.guides.horizontal.len().max(doc.guides.vertical.len()) > MAX_GUIDES,
        "profile": profile_json(doc),
    });
    let mut detail = extend(document_row(index, state, active), fields);
    if view != View::Summary
        && let Some(object) = detail.as_object_mut()
    {
        let (mut budget, mut truncated) = (MAX_LAYER_ROWS, false);
        let layers = layer_rows(&doc.layers, view == View::Full, &mut budget, &mut truncated);
        object.insert("layers".into(), Value::Array(layers));
        object.insert("layersTruncated".into(), json!(truncated));
    }
    detail
}

/// At most `MAX_GUIDES` guide positions of one orientation, in document pixels.
fn guides(positions: &[f32]) -> Vec<f32> {
    positions.iter().take(MAX_GUIDES).copied().collect()
}

/// The document's colour profile: whether it embeds one, and its description when it parses (the
/// metadata read reports the rest).
fn profile_json(doc: &Document) -> Value {
    let profile = doc.icc_profile.as_ref().and_then(|bytes| photocraft_engine::color_cmds::profile_from_bytes(bytes).ok());
    json!({"present": doc.icc_profile.is_some(), "description": profile.map(|profile| bounded(&profile.description))})
}

fn history_json(state: &DocState) -> Value {
    let entries = state.history.entries();
    let redo: Vec<&str> = state.history.redo_labels().collect();
    json!({
        "entries": entries,
        "currentIndex": entries.len().saturating_sub(1),
        "canUndo": state.history.can_undo(),
        "canRedo": state.history.can_redo(),
        "head": history_head_json(state),
        "redo": redo,
    })
}

/// `{length, label}`: what `historyHead` preconditions compare.
fn history_head_json(state: &DocState) -> Value {
    let entries = state.history.entries();
    json!({"length": entries.len(), "label": entries.last()})
}

/// Layers top to bottom (groups with their children), at most 64 rows in all.
fn layer_rows(layers: &[Layer], full: bool, budget: &mut usize, truncated: &mut bool) -> Vec<Value> {
    let mut rows = Vec::new();
    for layer in layers.iter().rev() {
        if *budget == 0 {
            *truncated = true;
            break;
        }
        *budget -= 1;
        let mut row = if full { layer_detail(layer) } else { layer_summary(layer) };
        if let Some(children) = layer.children() {
            let nested = layer_rows(children, full, budget, truncated);
            if let Some(object) = row.as_object_mut() {
                object.insert("children".into(), Value::Array(nested));
            }
        }
        rows.push(row);
    }
    rows
}

fn layer_summary(layer: &Layer) -> Value {
    json!({
        "id": layer.id.0,
        "name": layer.name,
        "kind": layer.content.kind_name(),
        "visible": layer.visible,
        "opacity": layer.opacity,
        "blend": format!("{:?}", layer.blend),
        "clipped": layer.clipped,
        "hasMask": layer.mask.is_some(),
        "bounds": layer.surface().map(|surface| rect_json(surface.content_bounds())),
    })
}

/// A layer's detail (the `full` view and `layerId`): its summary, the fill opacity, locks, masks,
/// the layer style kinds, Blend If and the blending channels when set and the link group (IN-8),
/// and what its kind holds.
fn layer_detail(layer: &Layer) -> Value {
    let (channels, blend_if, items) = blending_fields(layer);
    let fields = json!({
        "fillOpacity": layer.fill_opacity,
        "locks": serde_json::to_value(layer.locks).unwrap_or(Value::Null),
        "mask": layer.mask.as_ref().map(mask_json),
        "vectorMask": layer.vector_mask.as_ref().map(|mask| {
            json!({"enabled": mask.enabled, "linked": mask.linked, "density": mask.density, "feather": mask.feather})
        }),
        "effects": {"enabled": layer.effects.enabled, "count": layer.effects.items.len(), "items": items},
        "linkGroup": layer.link_group,
    });
    let mut detail = extend(layer_summary(layer), fields);
    if let Some(object) = detail.as_object_mut() {
        if let Some(blend_if) = blend_if {
            object.insert("blendIf".into(), blend_if);
        }
        if let Some(channels) = channels {
            object.insert("channels".into(), channels);
        }
        match &layer.content {
            LayerContent::Text(text) => {
                object.insert("text".into(), text_json(text));
            }
            LayerContent::Fill(fill) => {
                object.insert("fill".into(), fill_json(fill));
            }
            LayerContent::Adjustment(adjustment) => {
                object.insert("adjustment".into(), json!(adjustment.label()));
            }
            LayerContent::Smart(smart) => {
                object.insert("smartObject".into(), json!({"smartFilters": smart.smart_filters.len(), "rendered": smart.cache.is_some()}));
            }
            LayerContent::Group(group) => {
                object.insert("childCount".into(), json!(group.children.len()));
            }
            LayerContent::Raster(_) | LayerContent::Shape(_) => {}
        }
    }
    detail
}

/// Layer › Blending Options and the layer style kinds as the engine's own inspection reports them
/// (`photocraft_engine::inspect::layer`, inspect.rs:111-133), without walking a group's children:
/// the blending channels and Blend If only when set, and the style items (`[]` without styles).
fn blending_fields(layer: &Layer) -> (Option<Value>, Option<Value>, Value) {
    let excluded = layer.excluded_channels;
    let channels = (excluded != 0).then(|| Value::Array((0..4).map(|i| Value::Bool((excluded & (1 << i)) == 0)).collect()));
    let ranges = layer.blend_if.ranges.iter().enumerate().filter(|(_, range)| !range.iter().all(photocraft_doc::BlendRange::is_full));
    let set: Vec<Value> = ranges.map(|(i, [this, under])| json!({"channel": i, "thisLayer": this.to_bytes(), "underlying": under.to_bytes()})).collect();
    let blend_if = (!layer.blend_if.is_default()).then_some(Value::Array(set));
    let items = layer.effects.items.iter().map(|effect| json!({"kind": effect.label(), "enabled": effect.enabled()})).collect();
    (channels, blend_if, Value::Array(items))
}

/// A layer mask's properties: what `describe` reports and what a mask render carries.
fn mask_json(mask: &LayerMask) -> Value {
    json!({
        "enabled": mask.enabled,
        "linked": mask.linked,
        "density": mask.density,
        "feather": mask.feather,
        "bounds": rect_json(mask.surface.content_bounds()),
    })
}

/// A type layer's text and styling, with its paragraphs' direction and alignment (R1, M6) by the
/// names `type.create` and `type.setStyle` take (`mixed` when its paragraphs differ).
fn text_json(text: &TextLayer) -> Value {
    let paragraphs = text.paragraph_runs();
    json!({
        "text": prefix(&text.text, MAX_TEXT_BYTES),
        "truncated": text.text.len() > MAX_TEXT_BYTES,
        "fontFamily": text.font_family,
        "sizePt": text.size_pt,
        "color": hex_rgb(text.color.to_rgba8()),
        "paragraph": !matches!(text.shape, photocraft_doc::text::TextShape::Point),
        "orientation": format!("{:?}", text.orientation),
        "runs": text.runs.len(),
        "direction": shared(&paragraphs, direction_name),
        "align": shared(&paragraphs, align_name),
    })
}

/// The name every paragraph's style shares, or `mixed` (the Paragraph panel then shows none).
fn shared(paragraphs: &[ParagraphRun], name: fn(&ParagraphStyle) -> &'static str) -> &'static str {
    let names: Vec<&'static str> = paragraphs.iter().map(|run| name(&run.style)).collect();
    match names.split_first() {
        Some((&first, rest)) if rest.iter().all(|&other| other == first) => first,
        _ => "mixed",
    }
}

fn direction_name(style: &ParagraphStyle) -> &'static str {
    match style.direction {
        TextDirection::Auto => "auto",
        TextDirection::Ltr => "ltr",
        TextDirection::Rtl => "rtl",
    }
}

fn align_name(style: &ParagraphStyle) -> &'static str {
    match style.align {
        TextAlign::Left => "left",
        TextAlign::Center => "center",
        TextAlign::Right => "right",
        TextAlign::JustifyLeft => "justify",
        TextAlign::JustifyCenter => "justifyCenter",
        TextAlign::JustifyRight => "justifyRight",
        TextAlign::JustifyAll => "justifyAll",
    }
}

fn fill_json(fill: &Fill) -> Value {
    if let Fill::Solid(color) = fill {
        return json!({"kind": "solid", "color": hex_rgb(color.to_rgba8()), "alpha": color.alpha});
    }
    let kind = if matches!(fill, Fill::Gradient { .. }) {
        "gradient"
    } else if matches!(fill, Fill::Pattern { .. }) {
        "pattern"
    } else {
        "other"
    };
    json!({"kind": kind})
}

/// The ICC profile, the XMP File Info fields and EXIF presence with GPS flagged, read from fields
/// only: never `file.fileInfo` or `edit.assignProfile`, which write when given parameters (INV-14).
pub fn metadata(session: &Session, params: &Value) -> Result<Value, ViewError> {
    let map = read_params(params, &["document"])?;
    let (index, state) = document(session, map.get("document"))?;
    let doc = &state.doc;
    let icc = match &doc.icc_profile {
        None => json!({"present": false}),
        Some(bytes) => {
            let profile = photocraft_engine::color_cmds::profile_from_bytes(bytes).ok();
            json!({
                "present": true,
                "bytes": bytes.len(),
                "valid": profile.is_some(),
                "description": profile.as_ref().map(|profile| profile.description.clone()),
                "colorSpace": profile.as_ref().map(|profile| format!("{:?}", profile.color_space)),
            })
        }
    };
    let xmp = match &doc.metadata.xmp {
        None => json!({"present": false}),
        Some(packet) => json!({
            "present": true,
            "bytes": packet.len(),
            "gps": packet.contains(":GPS"),
            "fields": photocraft_engine::file_cmds::read_file_info(Some(packet.as_str())),
        }),
    };
    let exif = match &doc.metadata.exif {
        None => json!({"present": false}),
        Some(block) => {
            let gps = exif_gps(block);
            json!({"present": true, "bytes": block.len(), "gps": gps, "parsed": gps.is_some()})
        }
    };
    Ok(json!({
        "documentId": doc.id.0.to_string(),
        "rev": rev_token(state.revision),
        "index": index,
        "mode": format!("{:?}", doc.mode),
        "depth": format!("{:?}", doc.depth),
        "resolution": doc.resolution_dpi,
        "icc": icc,
        "xmp": xmp,
        "exif": exif,
    }))
}

/// Whether an EXIF block (TIFF structure, optionally after `Exif\0\0`) points to a GPS IFD from
/// IFD0; `None` when the block cannot be read that far.
fn exif_gps(block: &[u8]) -> Option<bool> {
    let tiff = block.strip_prefix(b"Exif\0\0").unwrap_or(block);
    let big_endian = match tiff.get(..4)? {
        b"II*\0" => false,
        b"MM\0*" => true,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let bytes: [u8; 2] = tiff.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if big_endian { u16::from_be_bytes(bytes) } else { u16::from_le_bytes(bytes) })
    };
    let offset: [u8; 4] = tiff.get(4..8)?.try_into().ok()?;
    let ifd = usize::try_from(if big_endian { u32::from_be_bytes(offset) } else { u32::from_le_bytes(offset) }).ok()?;
    for entry in 0..usize::from(u16_at(ifd)?) {
        let at = ifd.checked_add(2)?.checked_add(entry.checked_mul(12)?)?;
        if u16_at(at)? == 0x8825 {
            return Some(true);
        }
    }
    Some(false)
}

/// The page's font families and their digest, and an optional glyph probe through the type
/// engine's own layout (falling back as type layers do) that counts tofu (.notdef). Probing
/// registers and removes nothing.
pub fn fonts(engine: &mut TextEngine, params: &Value) -> Result<Value, ViewError> {
    let map = read_params(params, &["probe"])?;
    let probe = match map.get("probe") {
        None | Some(Value::Null) => Value::Null,
        Some(probe) => glyph_probe(engine, probe)?,
    };
    let families = engine.fonts.families();
    Ok(json!({
        "families": families,
        "count": families.len(),
        "fontsetDigest": fontset_digest(&families),
        "probe": probe,
    }))
}

fn glyph_probe(engine: &mut TextEngine, probe: &Value) -> Result<Value, ViewError> {
    let map = read_params(probe, &["text", "family", "sizePt"])?;
    let text = map.get("text").and_then(Value::as_str).ok_or_else(|| read_failed("`probe.text` must be a string"))?;
    if text.len() > MAX_TEXT_BYTES {
        return Err(over_budget(format!("`probe.text` is limited to {MAX_TEXT_BYTES} UTF-8 bytes")));
    }
    let family = match map.get("family") {
        None | Some(Value::Null) => photocraft_text::fonts::DEFAULT_FAMILY,
        Some(Value::String(family)) if family.len() <= MAX_NAME_BYTES => family.as_str(),
        Some(_) => return Err(read_failed("`probe.family` must be a family name")),
    };
    let size = match map.get("sizePt") {
        None | Some(Value::Null) => 24.0,
        Some(value) => {
            value.as_f64().filter(|size| size.is_finite() && (1.0..=1296.0).contains(size)).ok_or_else(|| read_failed("`probe.sizePt` must be 1 to 1296"))?
        }
    };
    let missing = !engine.fonts.has_family(family);
    let layer = TextLayer { text: text.to_string(), font_family: family.to_string(), size_pt: size as f32, ..Default::default() };
    let layout = engine.layout(&layer, 72.0);
    let tofu = layout.glyphs.iter().filter(|glyph| glyph.id == 0).count();
    Ok(json!({
        "family": family,
        "sizePt": size,
        "glyphs": layout.glyphs.len(),
        "tofu": tofu,
        "missingFont": missing,
    }))
}

/// The cheapest watermark: every document's DocId, revision and dirty flag, the active one, the
/// busy flag and the font set digest.
pub fn watermark(session: &Session, busy: bool, families: &[String]) -> Value {
    let documents: Vec<Value> = session
        .documents()
        .iter()
        .enumerate()
        .take(MAX_DOCUMENT_ROWS)
        .map(|(index, state)| json!({"index": index, "id": state.doc.id.0.to_string(), "rev": rev_token(state.revision), "dirty": state.is_dirty()}))
        .collect();
    json!({
        "documents": documents,
        "count": session.documents().len(),
        "active": session.active_index(),
        "activeId": active_id(session),
        "busy": busy,
        "fontsetDigest": fontset_digest(families),
    })
}

// ------------------------------------------------------------------ R0 tier A reads

/// What an agent export may embed (OC-13; OD-14). No policy writes EXIF, GPS, camera or location
/// data: each is a positive list of fields.
#[derive(Clone, Copy, PartialEq)]
enum MetadataPolicy {
    /// `none`: nothing (the FP1 encoder exactly as renders use it).
    Plain,
    /// `copyright` (OC-13, the default): the creator and copyright fields.
    Copyright,
    /// `newsroom` (OD-14's recommendation, only on request): OC-13 plus the IPTC credit line and
    /// caption.
    Newsroom,
}

/// The policy of an export that names none: the owner's default OC-13, `copyright` with the
/// location removed and no camera data (`H/design/owner-decisions.md`), until OD-14 is decided.
/// The IPTC credit line and caption (`newsroom`) are written only when the caller asks for them.
const DEFAULT_METADATA: MetadataPolicy = MetadataPolicy::Copyright;

impl MetadataPolicy {
    fn name(self) -> &'static str {
        match self {
            MetadataPolicy::Plain => "none",
            MetadataPolicy::Copyright => "copyright",
            MetadataPolicy::Newsroom => "newsroom",
        }
    }

    fn fields(self) -> &'static [&'static str] {
        match self {
            MetadataPolicy::Plain => &[],
            MetadataPolicy::Copyright => COPYRIGHT_FIELDS,
            MetadataPolicy::Newsroom => EXPORT_FIELDS,
        }
    }
}

fn metadata_policy(map: &Params) -> Result<MetadataPolicy, ViewError> {
    match map.get("metadata") {
        None | Some(Value::Null) => Ok(DEFAULT_METADATA),
        Some(Value::String(name)) => match name.as_str() {
            "none" => Ok(MetadataPolicy::Plain),
            "copyright" => Ok(MetadataPolicy::Copyright),
            "newsroom" => Ok(MetadataPolicy::Newsroom),
            other => {
                let known = "none, copyright or newsroom: Photo writes positive fields only, never camera or location data";
                Err(unsupported(format!("unknown metadata policy `{}` ({known})", bounded(other))))
            }
        },
        Some(_) => Err(read_failed("`metadata` must be none, copyright or newsroom")),
    }
}

/// `removeLocation`: true, or absent. No policy writes location data, so `false` is refused
/// rather than ignored.
fn remove_location(map: &Params) -> Result<(), ViewError> {
    match map.get("removeLocation") {
        None | Some(Value::Null) | Some(Value::Bool(true)) => Ok(()),
        Some(Value::Bool(false)) => Err(unsupported("the agent export never writes location data: `removeLocation` must be true")),
        Some(_) => Err(read_failed("`removeLocation` must be a boolean")),
    }
}

fn max_bytes(map: &Params) -> Result<Option<usize>, ViewError> {
    match map.get("maxBytes") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let limit = value.as_u64().and_then(|n| usize::try_from(n).ok()).filter(|n| (1..=MAX_OUTPUT_BYTES).contains(n));
            limit.map(Some).ok_or_else(|| read_failed(format!("`maxBytes` must be an integer from 1 to {MAX_OUTPUT_BYTES}")))
        }
    }
}

/// The positive fields an export writes under `policy`: the document's own File Info (the
/// engine's `read_file_info`; the credit line from `photoshop:Credit`), replaced by the caller's
/// `fields`, which may name only fields the policy writes; an empty value drops one.
fn positive_fields(doc: &Document, policy: MetadataPolicy, supplied: Option<&Value>) -> Result<Params, ViewError> {
    let allowed = policy.fields();
    let supplied = match supplied {
        None | Some(Value::Null) => Params::new(),
        Some(Value::Object(fields)) => fields.clone(),
        Some(_) => return Err(read_failed("`fields` must be an object")),
    };
    for (key, value) in &supplied {
        if !allowed.contains(&key.as_str()) {
            return Err(read_failed(format!("`fields.{}` is not written under metadata `{}`", bounded(key), policy.name())));
        }
        let valid = match (key.as_str(), value) {
            ("copyrightStatus", Value::String(status)) => matches!(status.as_str(), "" | "unknown" | "copyrighted" | "publicDomain"),
            (_, Value::String(field)) => field.len() <= MAX_FIELD_BYTES,
            _ => false,
        };
        if !valid {
            return Err(read_failed(format!("`fields.{}` must be a string of at most {MAX_FIELD_BYTES} bytes", bounded(key))));
        }
    }
    Ok(merged_fields(doc.metadata.xmp.as_deref(), allowed, &supplied))
}

/// The `allowed` positive fields an export (`positive_fields`) or an import (`import_png`) writes:
/// the caller's `supplied` value, else the packet's own File Info (the engine's `read_file_info`;
/// the credit line from `photoshop:Credit`); an empty value drops one, and an `unknown` copyright
/// status is not written.
fn merged_fields(xmp: Option<&str>, allowed: &[&str], supplied: &Params) -> Params {
    let info = photocraft_engine::file_cmds::read_file_info(xmp);
    let mut out = Params::new();
    for key in allowed {
        let inherited = match *key {
            "creator" => info.get("author").and_then(Value::as_str).map(str::to_string),
            "caption" => info.get("description").and_then(Value::as_str).map(str::to_string),
            "credit" => xmp.and_then(|xmp| xmp_property(xmp, "photoshop:Credit")),
            other => info.get(other).and_then(Value::as_str).map(str::to_string),
        };
        let value = match supplied.get(*key) {
            Some(Value::String(field)) => field.clone(),
            _ => inherited.map(|field| prefix(&field, MAX_FIELD_BYTES).to_string()).unwrap_or_default(),
        };
        let unknown = *key == "copyrightStatus" && value == "unknown";
        if !value.is_empty() && !unknown {
            out.insert((*key).to_string(), Value::String(value));
        }
    }
    out
}

/// A simple XMP property's text (`<prop>text</prop>` or `prop="text"`), found the way the engine's
/// File Info reader finds its own fields; `None` when absent, empty or structured.
fn xmp_property(xmp: &str, property: &str) -> Option<String> {
    let found = xmp_element(xmp, property).or_else(|| xmp_attribute(xmp, property));
    found.map(|text| xml_unescape(text.trim())).filter(|text| !text.is_empty() && !text.contains('<'))
}

fn xmp_element<'x>(xmp: &'x str, property: &str) -> Option<&'x str> {
    let (open, close) = (format!("<{property}>"), format!("</{property}>"));
    let rest = xmp.get(xmp.find(open.as_str())? + open.len()..)?;
    rest.get(..rest.find(close.as_str())?)
}

fn xmp_attribute<'x>(xmp: &'x str, property: &str) -> Option<&'x str> {
    let key = format!(" {property}=\"");
    let rest = xmp.get(xmp.find(key.as_str())? + key.len()..)?;
    rest.get(..rest.find('"')?)
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// A new XMP packet that holds the positive fields and nothing else (Dublin Core creator, rights
/// and description, XMP Rights, Photoshop's IPTC credit line), or `None` when there are none.
fn xmp_packet(fields: &Params) -> Option<String> {
    let field = |key: &str| fields.get(key).and_then(Value::as_str).map(xml_escape);
    let mut body = String::new();
    if let Some(creator) = field("creator") {
        body.push_str(&format!("   <dc:creator><rdf:Seq><rdf:li>{creator}</rdf:li></rdf:Seq></dc:creator>\n"));
    }
    if let Some(rights) = field("copyright") {
        body.push_str(&format!("   <dc:rights><rdf:Alt><rdf:li xml:lang=\"x-default\">{rights}</rdf:li></rdf:Alt></dc:rights>\n"));
    }
    match fields.get("copyrightStatus").and_then(Value::as_str) {
        Some("copyrighted") => body.push_str("   <xmpRights:Marked>True</xmpRights:Marked>\n"),
        Some("publicDomain") => body.push_str("   <xmpRights:Marked>False</xmpRights:Marked>\n"),
        _ => {}
    }
    if let Some(url) = field("copyrightUrl") {
        body.push_str(&format!("   <xmpRights:WebStatement>{url}</xmpRights:WebStatement>\n"));
    }
    if let Some(credit) = field("credit") {
        body.push_str(&format!("   <photoshop:Credit>{credit}</photoshop:Credit>\n"));
    }
    if let Some(caption) = field("caption") {
        body.push_str(&format!("   <dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{caption}</rdf:li></rdf:Alt></dc:description>\n"));
    }
    if body.is_empty() {
        return None;
    }
    let namespaces = concat!(
        "xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:photoshop=\"http://ns.adobe.com/photoshop/1.0/\" ",
        "xmlns:xmpRights=\"http://ns.adobe.com/xap/1.0/rights/\""
    );
    Some(format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  <rdf:Description rdf:about=\"\" {namespaces}>\n{body}  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>"
    ))
}

/// An agent export through the FP1 encoder (`Encoding::options`: `embed_metadata:false`, no XMP),
/// so the document's EXIF, XMP, resolution and text never reach the file. A packet of positive
/// fields needs `embed_metadata`, because the codecs write XMP only with it (png.rs:137-169,
/// jpeg.rs:244-257, webp.rs:60-67), and with it they would also write the EXIF and the resolution
/// `document_to_image` takes from the document (crates/io/src/flat.rs:233-237). So the packet is
/// encoded from a copy whose EXIF is removed, whose XMP is the packet and whose resolution is 0,
/// which the PNG and JPEG encoders leave out (png.rs:142-148, jpeg.rs:245-251; WebP writes none):
/// the packet is the only metadata in the file.
fn encode_export(doc: &Document, encoding: Encoding, packet: Option<String>) -> Result<(Vec<u8>, &'static str), ViewError> {
    let Some(packet) = packet else { return encode_document(doc, encoding) };
    let (extension, mime, encode) = encoding.options();
    let mut copy = doc.clone();
    copy.metadata.exif = None;
    copy.metadata.xmp = Some(packet);
    copy.resolution_dpi = 0.0;
    let options =
        photocraft_io::ExportOptions { encode: EncodeOptions { embed_metadata: true, ..encode }, xmp: photocraft_io::XmpEmbed::All, ..Default::default() };
    let output = photocraft_io::export(&copy, extension, &options).map_err(|error| read_failed(format!("encoding failed: {error}")))?;
    Ok((bounded_output(output.bytes)?, mime))
}

/// Whether `photocraft_io` writes the document as a palette PNG, which carries no metadata at all
/// (crates/io/src/flat.rs:409-421): an Indexed Color document with a colour table of 1 to 256
/// entries, exported as PNG.
fn palette_png(doc: &Document, encoding: Encoding) -> bool {
    let table = doc.color_table.as_ref().is_some_and(|table| (1..=256).contains(&table.colors.len()));
    encoding == Encoding::Png && doc.mode == ColorMode::Indexed && table
}

/// The agent export (R0 tier A): any open document's composite as PNG (the default), JPEG
/// (`quality`, 90 by default) or WebP (lossless, or lossy with `quality`) through the FP1 encoder,
/// carrying only the positive fields of the `metadata` policy (`DEFAULT_METADATA` when it names
/// none; see `positive_fields`) and never location data (`removeLocation` is true or absent),
/// refused `budget_exceeded` over `maxBytes` (never a silent quality change). A pure read: the
/// document, its metadata and its revision stay as they are.
pub fn export(session: &Session, params: &Value) -> Result<Rendered, ViewError> {
    let map = read_params(params, EXPORT_KEYS)?;
    let encoding = encoding(map)?;
    let policy = metadata_policy(map)?;
    remove_location(map)?;
    let limit = max_bytes(map)?;
    let (index, state) = document(session, map.get("document"))?;
    within_budget(&state.doc)?;
    let fields = positive_fields(&state.doc, policy, map.get("fields"))?;
    if !fields.is_empty() && palette_png(&state.doc, encoding) {
        let instead = "export it as JPEG or WebP, or with metadata none";
        return Err(unsupported(format!("an Indexed Color document exports as a palette PNG, which holds no metadata: {instead}")));
    }
    let (bytes, mime) = encode_export(&state.doc, encoding, xmp_packet(&fields))?;
    if let Some(limit) = limit
        && bytes.len() > limit
    {
        return Err(over_budget(format!("the export is {} bytes, over maxBytes {limit}: lower `quality` or export a smaller document", bytes.len())));
    }
    let (width, height) = (state.doc.size.width, state.doc.size.height);
    let extra =
        json!({"width": width, "height": height, "metadata": policy.name(), "removeLocation": true, "fields": Value::Object(fields), "maxBytes": limit});
    let info = extend(render_info("export", mime, index, state, &bytes), extra);
    Ok(Rendered { bytes, info })
}

/// `textCheck` (R0 tier A): a type layer's text against the lines the type engine lays it out in,
/// as `type.info` reports them (crates/engine/src/type_cmds.rs:456-505): each line's character
/// range, the characters shown, the text a paragraph box drops (overflow), glyphs and tofu. A pure
/// read; `engine` is the page's text engine (the glue passes the shared one).
pub fn text_check(engine: &mut TextEngine, session: &Session, params: &Value) -> Result<Value, ViewError> {
    let map = read_params(params, TEXT_CHECK_KEYS)?;
    let (index, state) = document(session, map.get("document"))?;
    let id = match layer_param(map)? {
        Some(id) => id,
        None => state.active_layer.map(|layer| layer.0).ok_or_else(|| read_failed("no layer is active"))?,
    };
    let layer = state.doc.layer(LayerId(id)).ok_or_else(|| read_failed(format!("no layer {id} in that document")))?;
    let LayerContent::Text(text) = &layer.content else {
        return Err(read_failed(format!("layer {id} is a {} layer, not a type layer", layer.content.kind_name())));
    };
    let missing = !engine.fonts.has_family(&text.font_family);
    let layout = engine.layout(text, state.doc.resolution_dpi);
    let content = text.text.as_str();
    let chars = |byte: usize| content.get(..byte.min(content.len())).map_or(0, |head| head.chars().count());
    let shown = layout.lines.iter().map(|line| line.range.end).max().unwrap_or(0).min(content.len());
    let hidden = content.get(shown..).unwrap_or_default();
    let lines: Vec<Value> =
        layout.lines.iter().take(MAX_LINE_ROWS).map(|line| json!({"start": chars(line.range.start), "end": chars(line.range.end)})).collect();
    Ok(json!({
        "documentId": state.doc.id.0.to_string(),
        "rev": rev_token(state.revision),
        "index": index,
        "layerId": id,
        "text": prefix(content, MAX_TEXT_BYTES),
        "truncated": content.len() > MAX_TEXT_BYTES,
        "chars": content.chars().count(),
        "paragraphText": !matches!(text.shape, photocraft_doc::text::TextShape::Point),
        "lines": lines,
        "lineCount": layout.lines.len(),
        "linesTruncated": layout.lines.len() > MAX_LINE_ROWS,
        "shownChars": chars(shown),
        "overflow": hidden.chars().any(|c| !c.is_whitespace()),
        "hiddenText": prefix(hidden, MAX_TEXT_BYTES),
        "glyphs": layout.glyphs.len(),
        "tofu": layout.glyphs.iter().filter(|glyph| glyph.id == 0).count(),
        "missingFont": missing,
    }))
}

// ------------------------------------------------------------------ small helpers

fn active_id(session: &Session) -> Option<String> {
    session.active().map(|state| state.doc.id.0.to_string())
}

fn rev_token(revision: u64) -> String {
    format!("p:{revision}")
}

fn rect_json(rect: Rect) -> Value {
    json!([rect.x0, rect.y0, rect.width(), rect.height()])
}

fn hex_rgb([r, g, b, _]: [u8; 4]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// At most 300 characters of a caller-supplied or engine-supplied text.
fn bounded(text: &str) -> String {
    text.chars().take(MAX_MESSAGE_CHARS).collect()
}

/// The first `max` bytes of `text`, cut at a character boundary.
fn prefix(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or_default()
}

/// `sha256:` and the SHA-256 of the JSON family list: changes whenever a family is added.
pub fn fontset_digest(families: &[String]) -> String {
    let listing = serde_json::to_string(families).unwrap_or_default();
    format!("sha256:{}", sha256_hex(listing.as_bytes()))
}

const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

const SHA256_INITIAL: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];

const SHA256_ROUNDS: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74,
    0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d,
    0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e,
    0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
    0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 (FIPS 180-4) as 64 lowercase hex digits, written here so the view needs no new crate.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut state = SHA256_INITIAL;
    let mut blocks = bytes.chunks_exact(64);
    for block in &mut blocks {
        sha256_block(&mut state, block);
    }
    let rest = blocks.remainder();
    let mut tail = [0u8; 128];
    let used = if rest.len() < 56 { 64 } else { 128 };
    tail[..rest.len()].copy_from_slice(rest);
    tail[rest.len()] = 0x80;
    let bits = (bytes.len() as u64).wrapping_mul(8);
    tail[used - 8..used].copy_from_slice(&bits.to_be_bytes());
    for block in tail[..used].chunks_exact(64) {
        sha256_block(&mut state, block);
    }
    let mut out = String::with_capacity(64);
    for byte in state.iter().flat_map(|word| word.to_be_bytes()) {
        out.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(HEX_DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

fn sha256_block(state: &mut [u32; 8], block: &[u8]) {
    let mut w = [0u32; 64];
    for (slot, word) in w.iter_mut().zip(block.chunks_exact(4)) {
        *slot = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }
    let mut v = *state;
    for (k, word) in SHA256_ROUNDS.iter().zip(w.iter()) {
        let [a, b, c, d, e, f, g, h] = v;
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choose = (e & f) ^ (!e & g);
        let t1 = h.wrapping_add(s1).wrapping_add(choose).wrapping_add(*k).wrapping_add(*word);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        v = [t1.wrapping_add(s0.wrapping_add(majority)), a, b, c, d.wrapping_add(t1), e, f, g];
    }
    for (slot, value) in state.iter_mut().zip(v) {
        *slot = slot.wrapping_add(value);
    }
}
