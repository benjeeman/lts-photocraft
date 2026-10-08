//! Guards for agent commands that share the interactive editor's document.
use photocraft_ui_egui::PhotocraftApp;
use serde_json::Value;

pub fn busy(app: &PhotocraftApp, ctx: &egui::Context) -> bool {
    app.has_pending_modal_edit()
        || app.ui.text_edit.is_some()
        || app.ui.transform.is_some()
        || app.ui.pen.is_some()
        || app.ui.crop_rect.is_some()
        || !app.ui.polygon.is_empty()
        || app.ui.magnetic.active()
        || !app.ui.dialogs.is_empty()
        || app.ui.layer_menu.is_some()
        || app.ui.canvas_tool_menu.is_some()
        || app.ui.brush_picker.is_some()
        || app.ui.palette_open
        || app.live_adjust.is_some()
        || ctx.text_edit_focused()
        || ctx.input(|i| i.pointer.any_down() || i.pointer.any_released())
}

pub fn validate(command: &str, params: &Value, session: &photocraft_engine::Session) -> Result<(), String> {
    if command == "type.create"
        && let Some(family) = params.get("font").and_then(Value::as_str)
    {
        let mut engine = photocraft_text::shared().lock().unwrap_or_else(|error| error.into_inner());
        if !engine.fonts.has_family(family) {
            return Err("font not loaded in this page; import it first; not applied".into());
        }
    }
    if command == "edit.fill"
        && session.active().is_some_and(|state| {
            state.channel_view.target != photocraft_engine::channel_cmds::ChannelTarget::Composite
                || state.doc.quick_mask.is_some()
                || state.channel_view.layer_mask.is_some()
        })
    {
        return Err("pixel fill requires composite channels without Quick Mask or a mask view; not applied".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn app() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width":16,"height":16,"background":"white"})).expect("fixture");
        app.run("layer.new.layer", json!({})).expect("editable layer");
        app.run("edit.fill", json!({"color":"#808080"})).expect("fixture pixels");
        app
    }

    #[test]
    fn between_click_gestures_and_pointer_release_are_busy() {
        let mut app = app();
        let ctx = egui::Context::default();
        assert!(!busy(&app, &ctx));
        app.ui.crop_rect = Some([0.0, 0.0, 8.0, 8.0]);
        assert!(busy(&app, &ctx));
        app.ui.crop_rect = None;
        app.ui.polygon.push([0.0, 0.0]);
        assert!(busy(&app, &ctx));
        app.ui.polygon.clear();
        app.ui.magnetic.path.push([0.0, 0.0]);
        assert!(busy(&app, &ctx));
        app.ui.magnetic.path.clear();
        for pressed in [true, false] {
            let input = egui::RawInput {
                events: vec![egui::Event::PointerButton {
                    pos: egui::pos2(2.0, 2.0),
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                }],
                ..Default::default()
            };
            ctx.run_ui(input, |ui| assert!(busy(&app, ui.ctx()), "press and release frames must be busy")).textures_delta.clear();
        }
        ctx.run_ui(Default::default(), |ui| assert!(!busy(&app, ui.ctx()))).textures_delta.clear();
    }

    #[test]
    fn modal_sessions_outside_generic_dialogs_are_busy() {
        let ctx = egui::Context::default();
        for command in ["filter.cameraRaw", "filter.adaptiveWideAngle", "filter.liquify", "filter.filterGallery", "edit.puppetWarp", "edit.perspectiveWarp"] {
            let mut app = app();
            photocraft_ui_egui::menus::invoke(&mut app, &ctx, command, json!({})).expect(command);
            assert!(busy(&app, &ctx), "manual session: {command}");
        }
        let mut app = app();
        photocraft_ui_egui::tiff_options_ui::park(&mut app, "fixture.tiff".into());
        assert!(busy(&app, &ctx));
        let mut app = self::app();
        photocraft_ui_egui::menus::invoke(&mut app, &ctx, "file.close", json!({})).expect("discard prompt");
        assert!(busy(&app, &ctx));
    }

    #[test]
    fn missing_explicit_fonts_and_manual_channel_targets_are_rejected() {
        let mut app = app();
        let params = json!({"text":"שלום","font":"LTS missing fixture family 20261008"});
        // The engine accepts an unknown family by falling back; the bridge must reject first.
        let before = app.session.active().expect("fixture").doc.layer_count();
        assert!(validate("type.create", &params, &app.session).is_err());
        assert_eq!(app.session.active().expect("fixture").doc.layer_count(), before);
        assert!(validate("type.create", &json!({"text":"x","font":"Inter"}), &app.session).is_ok());
        assert!(validate("edit.fill", &json!({}), &app.session).is_ok());
        app.session.active_mut().expect("fixture").channel_view.target = photocraft_engine::channel_cmds::ChannelTarget::Color(0);
        assert!(validate("edit.fill", &json!({}), &app.session).is_err());
        app.session.active_mut().expect("fixture").channel_view.target = photocraft_engine::channel_cmds::ChannelTarget::Composite;
        app.run("select.editInQuickMaskMode", json!({"on":true})).expect("Quick Mask");
        assert!(validate("edit.fill", &json!({}), &app.session).is_err());
    }
}
