//! Layer locks stop filters and destructive adjustments as they stop painting (Photoshop: "Could
//! not complete your request because the layer is locked"). Lock Image Pixels and Lock All refuse
//! them and leave the pixels as they were; Lock Position does not; Lock Transparent Pixels lets
//! them run but keeps every pixel's alpha, so a blur does not spread into the transparent area.
//! Filters only checked Relight's lock (`filters_ext.rs:497-505`); `filters.rs` `run_filter` and
//! `commands.rs` `destructive_adjust` wrote the pixels whatever the locks.

use photocraft_engine::doc::LayerId;
use photocraft_engine::{EngineError, Session};
use photocraft_geom::Rect;
use serde_json::{Value, json};

/// A 32×24 document with a second layer, active: a square, red on the left and blue on the
/// right, on transparency.
fn session() -> (Session, LayerId) {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 32, "height": 24})).unwrap();
    let id = LayerId(s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().unwrap());
    s.edit("paint", |doc, _| {
        let fmt = doc.pixel_format();
        let surf = doc.layer_mut(id).and_then(|l| l.surface_mut()).ok_or(EngineError::NoLayer(id))?;
        surf.fill_rect(Rect::new(8, 6, 16, 18), &photocraft_raster::from_rgba(&fmt, [1.0, 0.0, 0.0, 1.0]));
        surf.fill_rect(Rect::new(16, 6, 24, 18), &photocraft_raster::from_rgba(&fmt, [0.0, 0.0, 1.0, 1.0]));
        Ok(())
    })
    .unwrap();
    (s, id)
}

fn lock(s: &mut Session, id: LayerId, which: &str) {
    s.execute("layer.setProps", json!({"layer": id.0, "locks": {which: true}})).unwrap();
}

/// The layer's pixels (straight RGBA floats over the canvas).
fn pixels(s: &Session, id: LayerId) -> Vec<f32> {
    let d = s.active().unwrap();
    d.doc.layer(id).unwrap().surface().unwrap().read_region(d.doc.bounds())
}

fn refused(r: photocraft_engine::Result<Value>, what: &str) {
    let Err(e) = r else { panic!("{what}: a locked layer must refuse it") };
    assert!(e.to_string().contains("locked"), "{what}: the error says the layer is locked: {e}");
}

const FILTERS: [(&str, &str); 3] =
    [("filter.blur.gaussianBlur", r#"{"radius": 3}"#), ("filter.sharpen.unsharpMask", r#"{"amount": 200, "radius": 2}"#), ("filter.noise.despeckle", "{}")];

#[test]
fn pixel_and_all_locks_refuse_filters_and_adjustments() {
    for which in ["pixels", "all"] {
        let (mut s, id) = session();
        lock(&mut s, id, which);
        let before = pixels(&s, id);
        for (cmd, params) in FILTERS {
            let p: Value = serde_json::from_str(params).unwrap();
            refused(s.execute(cmd, p), &format!("{cmd} with {which} locked"));
        }
        refused(s.execute("image.adjustments.invert", json!({})), &format!("Invert with {which} locked"));
        refused(s.execute("image.adjustments.desaturate", json!({})), &format!("Desaturate with {which} locked"));
        assert!(pixels(&s, id) == before, "{which} lock: the pixels must not change");
    }
}

#[test]
fn a_position_lock_does_not_stop_filters() {
    let (mut s, id) = session();
    lock(&mut s, id, "position");
    let before = pixels(&s, id);
    s.execute("filter.blur.gaussianBlur", json!({"radius": 3})).unwrap();
    assert!(pixels(&s, id) != before, "the blur ran");
    s.execute("image.adjustments.invert", json!({})).unwrap();
}

#[test]
fn a_transparency_lock_keeps_every_alpha_under_a_filter() {
    let (mut s, id) = session();
    lock(&mut s, id, "transparency");
    let before = pixels(&s, id);
    s.execute("filter.blur.gaussianBlur", json!({"radius": 4})).unwrap();
    let after = pixels(&s, id);
    assert!(after != before, "the blur ran inside the opaque square");
    for (i, (a, b)) in before.chunks_exact(4).zip(after.chunks_exact(4)).enumerate() {
        assert!((a[3] - b[3]).abs() < 1e-3, "pixel {i}: alpha {} became {} under Lock Transparent Pixels", a[3], b[3]);
    }
    // Where red meets blue the colours mix: the filter itself ran.
    let mid = (12 * 32 + 16) * 4;
    assert!(after[mid] > 0.05 && after[mid + 2] > 0.05, "red and blue mix at the seam: {:?}", &after[mid..mid + 4]);
}
