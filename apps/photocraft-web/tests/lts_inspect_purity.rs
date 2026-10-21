//! F0 certification of the Photo read that the LTS adapter's pre-checks and watermark rely on
//! (implementation plan §1 F0; final design §6.3, INV-18): `control::inspect(&PhotocraftApp,
//! &egui::Context)` leaves every document, the selection, the status bar and the timings exactly
//! as they were (T-PH-01), and it exposes every field those pre-checks read (T-PH-01b).
//!
//! Native only. `photocraft-web` is a binary crate, so this test reaches `PhotocraftApp` and
//! `control::inspect` through its `photocraft-ui-egui` dependency; the binary's own modules
//! (wasm-only, or compiled only for its unit tests) are not built into this test. No browser,
//! network, model call or file output.
//!
//! A failure is a design finding, not a test to relax: Photo pre-checks then report
//! `guarantee: "unavailable"` and are never minted as `not_applied`.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use photocraft_engine::Session;
use photocraft_engine::doc::{DocId, LayerId, Rect};
use photocraft_ui_egui::layer_pick_ui::LayerMenu;
use photocraft_ui_egui::state::{DialogKind, TextEdit};
use photocraft_ui_egui::{PhotocraftApp, Tool, UiState, canvas_tool_menu, control};
use serde_json::{Value, json};

/// The first document's selection and the pixels filled inside it: x, y, width, height.
const FILLED: [i32; 4] = [8, 6, 20, 12];

/// Two documents as an editing session leaves them. The first (64×48) has a type layer (active)
/// over a pixel layer filled inside a live selection, over its background; it was saved under a
/// path and is active again although it is not the last. The second (32×24) has an unsaved empty
/// layer. The last request was refused, so the status bar shows its error.
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
        if let Err(error) = app.run(id, params.clone()) {
            panic!("fixture {id} {params}: {error}");
        }
    }
    assert!(app.session.set_active(0), "the first document exists");
    let first = app.session.active_mut().expect("the first document is active");
    first.path = Some("first.pcraft".into());
    first.saved_revision = first.revision;
    assert!(app.run("edit.redo", json!({})).is_err(), "nothing to redo: the request is refused");
    app
}

/// One document's identity and editing state, as T-PH-01 compares it.
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
}

/// What a read must leave alone (T-PH-01): every document, the active index, the status bar and
/// the last command the app ran.
#[derive(Debug, PartialEq)]
struct Observed {
    documents: Vec<DocumentState>,
    active: Option<usize>,
    status: String,
    status_error: bool,
    last_command: String,
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
        })
        .collect();
    Observed {
        documents,
        active: app.session.active_index(),
        status: app.ui.status.clone(),
        status_error: app.ui.status_error,
        last_command: app.perf.last_command.clone(),
    }
}

/// Three reads, each with a fresh context as between frames of the browser bridge. None changes
/// the observed state, the UI state, the timings, the frame count, the command journal or any
/// document snapshot, and all three return the same snapshot.
fn assert_inspect_is_pure(app: &PhotocraftApp, situation: &str) {
    let before = observe(app);
    let ui = app.ui.clone();
    let perf = json!(app.perf);
    let (frame, journal) = (app.frame, app.session.journal.len());
    let snapshots: Vec<_> = app.session.documents().iter().map(|d| Arc::clone(&d.doc)).collect();
    let mut reads = Vec::new();
    for call in 1..=3 {
        reads.push(control::inspect(app, &egui::Context::default()));
        assert_eq!(observe(app), before, "{situation}: read {call} changed a document, the selection, the status or perf.last_command");
        assert_eq!(app.ui, ui, "{situation}: read {call} changed the UI state");
        assert_eq!(json!(app.perf), perf, "{situation}: read {call} changed the timings");
        assert_eq!((app.frame, app.session.journal.len()), (frame, journal), "{situation}: read {call} ran a frame or journaled a command");
        let documents = app.session.documents();
        let same = documents.len() == snapshots.len() && documents.iter().zip(&snapshots).all(|(d, snapshot)| Arc::ptr_eq(&d.doc, snapshot));
        assert!(same, "{situation}: read {call} replaced a document snapshot");
    }
    assert_eq!(reads[0], reads[1], "{situation}: a repeated read differs");
    assert_eq!(reads[1], reads[2], "{situation}: a repeated read differs");
}

/// The id of the active document's top-level layer `name`.
fn layer_id(app: &PhotocraftApp, name: &str) -> LayerId {
    let doc = &app.session.active().expect("an active document").doc;
    doc.layers.iter().find(|layer| layer.name == name).map(|layer| layer.id).unwrap_or_else(|| panic!("no layer {name}"))
}

/// Inline type editing on `layer`, the caret after its third character.
fn text_edit(layer: LayerId) -> TextEdit {
    TextEdit { layer: layer.0, caret: 3, anchor: 0, session: "lts-f0-fixture".into(), created: false, dragging: false, resize: None, preedit: None }
}

/// `[x, y, width, height]`, as inspect reports bounds.
fn xywh(r: Rect) -> [i64; 4] {
    [r.x0.into(), r.y0.into(), r.width().into(), r.height().into()]
}

/// The listed fields of an inspect object (a missing field reads as `null`).
fn pick(value: &Value, keys: &[&str]) -> Value {
    Value::Object(keys.iter().map(|key| ((*key).to_string(), value[*key].clone())).collect())
}

/// T-PH-01: three `control::inspect` calls leave document ids, revisions, saved revisions, paths,
/// dirty flags, the active index, selection bounds, `ui.status` and `perf.last_command` unchanged,
/// both with the editor idle and with every surface the adapter's busy hint reads open.
#[test]
fn control_inspect_leaves_documents_selection_status_and_perf_unchanged() {
    let mut app = fixture();
    let before = observe(&app);
    // Every observed field holds a value worth keeping.
    assert_eq!(before.active, Some(0), "the first document is active although it is not the last");
    let [first, second] = &before.documents[..] else { panic!("two documents: {before:?}") };
    assert_ne!(first.id, second.id);
    assert_eq!((first.dirty, first.path.as_deref()), (false, Some("first.pcraft")));
    assert_eq!((second.dirty, second.path.as_deref()), (true, None));
    let [x, y, width, height] = FILLED;
    assert_eq!(first.selection, Some(Rect::new(x, y, x + width, y + height)));
    assert_eq!(second.selection, None);
    assert!(before.status_error && !before.status.is_empty(), "the refused request's error is in the status bar: {before:?}");
    assert_eq!(before.last_command, "edit.redo");

    assert_inspect_is_pure(&app, "idle editor");

    // A dialog, inline type editing, the layer menu and the canvas menu are read too; reading them
    // must not close or change them.
    assert!(canvas_tool_menu::open(&mut app, Tool::RectMarquee, [40.0, 30.0]), "a selection tool's canvas menu opens");
    let filled = layer_id(&app, "Filled");
    let caption = layer_id(&app, "Caption");
    app.ui.layer_menu = Some(LayerMenu { pos: [12.0, 10.0], layers: vec![(filled.0, "Filled".into())] });
    app.ui.open_dialog(DialogKind::NewDocument, UiState::new_document_fields());
    app.ui.text_edit = Some(text_edit(caption));
    assert_inspect_is_pure(&app, "busy editor");
}

/// T-PH-01b: inspect exposes what the adapter's pre-checks read (document size, revision, active
/// layer, layer kinds and bounds, history labels, dialogs, text edit, layer and canvas menus), each
/// equal to the session's or the UI's own state.
#[test]
fn inspect_exposes_the_fields_used_by_adapter_prechecks() {
    let mut app = fixture();
    let ctx = egui::Context::default();
    let idle = control::inspect(&app, &ctx);

    // Documents: the count (the document cap), the active index, sizes, revisions, dirty flags, paths.
    assert_eq!(idle["session"]["active"], json!(0));
    let rows = idle["session"]["documents"].as_array().expect("session.documents");
    assert_eq!(rows.len(), 2);
    for (index, (row, d)) in rows.iter().zip(app.session.documents()).enumerate() {
        let expected = json!({"index": index, "name": d.doc.name, "path": d.path, "width": d.doc.size.width, "height": d.doc.size.height, "dirty": d.is_dirty(), "revision": d.revision});
        assert_eq!(pick(row, &["index", "name", "path", "width", "height", "dirty", "revision"]), expected, "document {index}");
    }
    assert_eq!(
        pick(&rows[0], &["name", "width", "height", "dirty", "path"]),
        json!({"name": "first", "width": 64, "height": 48, "dirty": false, "path": "first.pcraft"})
    );
    assert_eq!(
        pick(&rows[1], &["name", "width", "height", "dirty", "path"]),
        json!({"name": "second", "width": 32, "height": 24, "dirty": true, "path": null})
    );

    // The active document: size, revision, active and selected layers, selection.
    let active = app.session.active().expect("the first document is active");
    let caption = active.active_layer.expect("the type layer is active");
    let document = &idle["document"];
    assert_eq!(
        pick(document, &["name", "width", "height", "revision", "activeLayer", "selectedLayers", "hasSelection", "selectionBounds"]),
        json!({"name": "first", "width": 64, "height": 48, "revision": active.revision, "activeLayer": caption.0, "selectedLayers": [caption.0], "hasSelection": true, "selectionBounds": FILLED})
    );

    // Layers top to bottom with their kinds and bounds, as the session holds them.
    let layers: Vec<Value> = document["layers"].as_array().expect("document.layers").iter().map(|l| pick(l, &["id", "name", "kind", "bounds"])).collect();
    let expected: Vec<Value> = active
        .doc
        .layers
        .iter()
        .rev()
        .map(|l| json!({"id": l.id.0, "name": l.name, "kind": l.content.kind_name(), "bounds": l.surface().map(|s| xywh(s.content_bounds()))}))
        .collect();
    assert_eq!(layers, expected);
    let kinds: Vec<(&str, &str)> = layers.iter().map(|l| (l["name"].as_str().unwrap_or_default(), l["kind"].as_str().unwrap_or_default())).collect();
    assert_eq!(kinds, [("Caption", "Type"), ("Filled", "Pixel"), ("Background", "Pixel")]);
    assert_eq!(layers[0]["id"], json!(caption.0));
    assert_eq!(layers[1]["bounds"], json!(FILLED));
    assert_eq!(layers[2]["bounds"], json!([0, 0, 64, 48]));

    // History labels oldest to newest; the last one heads `expect.historyHead`.
    let history = active.history.entries();
    assert_eq!(document["history"], json!(history));
    assert_eq!(history.len(), 5, "{history:?}");
    assert_eq!(history[1..], ["New Layer", "Rectangular Marquee", "Fill", "New Type Layer"]);
    assert_eq!(pick(document, &["canUndo", "canRedo"]), json!({"canUndo": true, "canRedo": false}));

    // The status bar.
    assert_eq!(pick(&idle, &["status", "statusError"]), json!({"status": app.ui.status, "statusError": true}));

    // The busy hint's surfaces: none is open while idle.
    assert_eq!(idle["dialogs"], json!([]));
    for key in ["textEdit", "layerMenu", "canvasToolMenu"] {
        assert!(idle[key].is_null(), "{key} while idle: {}", idle[key]);
    }
    let filled = layer_id(&app, "Filled");
    let background = layer_id(&app, "Background");

    // A dialog.
    let dialog = app.ui.open_dialog(DialogKind::NewDocument, UiState::new_document_fields());
    let busy = control::inspect(&app, &ctx);
    assert_eq!(busy["dialogs"], json!([{"id": dialog, "kind": DialogKind::NewDocument, "title": "New Document", "fields": UiState::new_document_fields()}]));
    assert!(app.ui.close_dialog(dialog).is_some());

    // Inline type editing on the active type layer.
    app.ui.text_edit = Some(text_edit(caption));
    let busy = control::inspect(&app, &ctx);
    assert_eq!(busy["textEdit"], json!(app.ui.text_edit));
    assert_eq!(busy["textEdit"]["layer"], json!(caption.0));
    app.ui.text_edit = None;

    // The layer menu (the layers under the pointer).
    app.ui.layer_menu = Some(LayerMenu { pos: [12.0, 10.0], layers: vec![(filled.0, "Filled".into()), (background.0, "Background".into())] });
    let busy = control::inspect(&app, &ctx);
    assert_eq!(busy["layerMenu"], json!(app.ui.layer_menu));
    assert_eq!(busy["layerMenu"]["layers"], json!([[filled.0, "Filled"], [background.0, "Background"]]));
    app.ui.layer_menu = None;

    // The selection tools' canvas menu, opened over a live selection.
    assert!(canvas_tool_menu::open(&mut app, Tool::RectMarquee, [40.0, 30.0]), "a selection tool's canvas menu opens");
    let busy = control::inspect(&app, &ctx);
    let menu = &busy["canvasToolMenu"];
    assert_eq!(pick(menu, &["pos", "tool"]), json!({"pos": [40.0, 30.0], "tool": Tool::RectMarquee}));
    let entries = menu["entries"].as_array().expect("canvasToolMenu.entries");
    let ids: Vec<&str> = entries.iter().filter_map(|entry| entry["id"].as_str()).collect();
    let selection_rows: Vec<&str> = canvas_tool_menu::SELECTION_MENU.iter().flatten().map(|(_, id)| *id).collect();
    assert_eq!(ids, selection_rows, "the selection rows, since a selection is live");
    assert!(entries.iter().all(|entry| entry["separator"] == json!(true) || entry["enabled"].is_boolean()), "{entries:?}");
    app.ui.canvas_tool_menu = None;

    // Pre-checks compare `expect.document` with the active document: switching it moves both
    // `session.active` and `document`.
    assert!(app.session.set_active(1), "the second document exists");
    let other = control::inspect(&app, &ctx);
    let second = &app.session.documents()[1];
    assert_eq!(other["session"]["active"], json!(1));
    assert_eq!(
        pick(&other["document"], &["name", "width", "height", "revision"]),
        json!({"name": "second", "width": 32, "height": 24, "revision": second.revision})
    );
    let layers: Vec<Value> = other["document"]["layers"].as_array().expect("document.layers").iter().map(|l| pick(l, &["name", "kind", "bounds"])).collect();
    assert_eq!(
        layers,
        [json!({"name": "Empty", "kind": "Pixel", "bounds": [0, 0, 0, 0]}), json!({"name": "Background", "kind": "Pixel", "bounds": [0, 0, 32, 24]})]
    );
}
