//! An unknown resampling or interpolation name is a typed error that names it, never a silent
//! bicubic (`image_cmds.rs` `parse_resample`, `algo::transform::Interp::parse`). Every command
//! that takes one checks it before doing any work, so the error is the same whatever else is
//! wrong with the call. The documented names and Photoshop's own (the Preferences › General ›
//! Image Interpolation choices: Nearest Neighbor, Bicubic Automatic, Bicubic Smoother, Bicubic
//! Sharper) keep working.

use photocraft_engine::{EngineError, Session};
use serde_json::{Value, json};

/// A 40×30 document with a second pixel layer holding a red square, active.
fn session() -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 40, "height": 30})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.edit("paint", |doc, active| {
        let id = active.ok_or(EngineError::NoDocument)?;
        let fmt = doc.pixel_format();
        let surf = doc.layer_mut(id).and_then(|l| l.surface_mut()).ok_or(EngineError::NoLayer(id))?;
        surf.fill_rect(photocraft_geom::Rect::new(8, 6, 28, 22), &photocraft_raster::from_rgba(&fmt, [1.0, 0.0, 0.0, 1.0]));
        Ok(())
    })
    .unwrap();
    s
}

fn assert_unknown_name(r: photocraft_engine::Result<Value>, what: &str) {
    match r {
        Err(EngineError::BadParams { msg, .. }) => assert!(msg.contains("bogus"), "{what}: the error must name the value: {msg}"),
        other => panic!("{what}: an unknown name must be a typed error (BadParams naming it), got {other:?}"),
    }
}

#[test]
fn image_size_refuses_an_unknown_resample_name() {
    let mut s = session();
    assert_unknown_name(s.execute("image.imageSize", json!({"width": 20, "resample": "bogus"})), "image.imageSize");
    assert_eq!(s.active().unwrap().doc.size.width, 40, "nothing was resized");
}

#[test]
fn image_size_accepts_every_documented_name_and_photoshop_alias() {
    let names = ["bicubic", "bilinear", "nearest", "lanczos", "preserveDetails", "none"];
    let aliases = ["nearestNeighbor", "bicubicAutomatic", "bicubicSmoother", "bicubicSharper"];
    for name in names.into_iter().chain(aliases) {
        let mut s = session();
        s.execute("image.imageSize", json!({"width": 20, "resample": name})).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

#[test]
fn transforms_refuse_an_unknown_interpolation_name() {
    let mut s = session();
    assert_unknown_name(s.execute("image.rotation.arbitrary", json!({"angle": 10, "interpolation": "bogus"})), "image.rotation.arbitrary");
    assert_eq!(s.active().unwrap().doc.size.width, 40, "nothing was rotated");
    assert_unknown_name(s.execute("edit.transform", json!({"matrix": [1, 0, 0, 1, 3, 2], "interpolation": "bogus"})), "edit.transform");
    assert_unknown_name(s.execute("edit.transform.warp", json!({"style": "flag", "bend": 30, "interpolation": "bogus"})), "edit.transform.warp");
    assert_unknown_name(s.execute("edit.puppetWarp", json!({"interpolation": "bogus"})), "edit.puppetWarp");
    assert_unknown_name(s.execute("edit.perspectiveWarp", json!({"interpolation": "bogus"})), "edit.perspectiveWarp");
    assert_unknown_name(s.execute("filter.adaptiveWideAngle", json!({"interpolation": "bogus"})), "filter.adaptiveWideAngle");
    s.execute("select.all", json!({})).unwrap();
    assert_unknown_name(s.execute("select.transformSelection", json!({"matrix": [1, 0, 0, 1, 2, 2], "interpolation": "bogus"})), "select.transformSelection");
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("select.allLayers", json!({})).unwrap();
    assert_unknown_name(s.execute("edit.autoAlignLayers", json!({"interpolation": "bogus"})), "edit.autoAlignLayers");
}

#[test]
fn transforms_accept_the_interpolation_names_and_photoshop_aliases() {
    for name in ["bicubic", "bilinear", "nearest", "nearestNeighbor", "bicubicAutomatic", "bicubicSmoother", "bicubicSharper"] {
        let mut s = session();
        s.execute("image.rotation.arbitrary", json!({"angle": 10, "interpolation": name})).unwrap_or_else(|e| panic!("rotate {name}: {e}"));
        let mut s = session();
        s.execute("edit.transform", json!({"matrix": [1, 0, 0, 1, 3, 2], "interpolation": name})).unwrap_or_else(|e| panic!("transform {name}: {e}"));
    }
}
