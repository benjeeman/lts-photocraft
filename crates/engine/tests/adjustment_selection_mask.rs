//! A new adjustment or fill layer made while a selection is active gets a layer mask from that
//! selection, as in Photoshop: the adjustment applies inside the marching ants only. Without a
//! selection the layer has no mask. `commands.rs` `new_adjustment` and the fill-layer commands
//! ignored the selection.

use photocraft_engine::Session;
use photocraft_engine::doc::LayerId;
use serde_json::{Value, json};

/// A 40×30 document with a 12×8 rectangle selected at (10, 5).
fn selected() -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 40, "height": 30})).unwrap();
    s.execute("select.rect", json!({"x": 10, "y": 5, "width": 12, "height": 8})).unwrap();
    s
}

fn new_layer(s: &mut Session, cmd: &str, params: Value) -> LayerId {
    LayerId(s.execute(cmd, params).unwrap_or_else(|e| panic!("{cmd}: {e}"))["layer"].as_u64().unwrap())
}

/// The mask value of layer `id` at (x, y), or `None` when it has no mask.
fn mask_at(s: &Session, id: LayerId, x: i32, y: i32) -> Option<f32> {
    s.active().unwrap().doc.layer(id).unwrap().mask.as_ref().map(|m| m.value(x, y))
}

const LAYERS: [(&str, &str); 4] = [
    ("layer.newAdjustmentLayer.invert", "{}"),
    ("layer.newAdjustmentLayer.levels", "{}"),
    ("layer.newFillLayer.solidColor", r##"{"color": "#ff8800"}"##),
    ("layer.newFillLayer.gradient", r##"{"from": "#000000", "to": "#ffffff"}"##),
];

#[test]
fn a_new_adjustment_or_fill_layer_is_masked_to_the_selection() {
    for (cmd, params) in LAYERS {
        let mut s = selected();
        let id = new_layer(&mut s, cmd, serde_json::from_str(params).unwrap());
        let inside = mask_at(&s, id, 15, 8).unwrap_or_else(|| panic!("{cmd}: the new layer has no mask although a selection is active"));
        assert!(inside > 0.99, "{cmd}: revealed inside the selection: {inside}");
        for (x, y) in [(2, 2), (30, 20), (9, 8), (22, 8), (15, 4), (15, 13)] {
            let v = mask_at(&s, id, x, y).unwrap_or(1.0);
            assert!(v < 0.01, "{cmd}: hidden outside the selection at ({x}, {y}): {v}");
        }
        s.execute("edit.undo", json!({})).unwrap();
        assert!(s.active().unwrap().doc.layer(id).is_none(), "{cmd}: one undo step removes the layer and its mask");
    }
}

#[test]
fn without_a_selection_the_new_layer_has_no_mask() {
    for (cmd, params) in LAYERS {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 40, "height": 30})).unwrap();
        let id = new_layer(&mut s, cmd, serde_json::from_str(params).unwrap());
        assert_eq!(mask_at(&s, id, 15, 8), None, "{cmd}: no selection, no mask");
    }
}
