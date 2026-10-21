//! Narrow commands for the isolated Creative Lab browser experiment.
//! This is not tenant authorization or a substitute for process/resource limits.

use serde_json::Value;

pub const COMMANDS: &[&str] = &[
    "file.new",
    "type.create",
    "layer.new.layer",
    "select.all",
    "select.rect",
    "select.deselect",
    "edit.fill",
    "layer.layerMask.revealSelection",
    "edit.undo",
    "edit.redo",
    "document.pixel",
    "image.crop",
    "image.imageSize",
];

pub fn authorize(id: &str, params: &Value) -> Result<(), String> {
    if !COMMANDS.contains(&id) {
        return Err(format!("command `{id}` is not enabled for the Creative Lab browser bridge"));
    }
    if !params.is_object() || params.to_string().len() > 16_384 {
        return Err("command parameters must be an object of at most 16 KiB".into());
    }
    let allowed: &[&str] = match id {
        "file.new" => &["width", "height", "background", "name"],
        "type.create" => &["text", "x", "y", "size", "font", "color"],
        "layer.new.layer" => &["name"],
        "select.rect" => &["x", "y", "width", "height", "ellipse"],
        "image.crop" => &["x", "y", "width", "height"],
        "image.imageSize" => &["width", "height"],
        "edit.fill" => &["color", "target"],
        "document.pixel" => &["x", "y"],
        _ => &[],
    };
    if params.as_object().is_some_and(|p| p.keys().any(|key| !allowed.contains(&key.as_str()))) {
        return Err("unsupported command parameter".into());
    }
    if matches!(id, "file.new" | "image.crop" | "image.imageSize" | "select.rect") {
        for key in ["width", "height"] {
            if !params.get(key).and_then(Value::as_u64).is_some_and(|value| (1..=2048).contains(&value)) {
                return Err(format!("{key} must be an integer from 1 to 2048"));
            }
        }
    }
    for key in ["x", "y", "size"] {
        if let Some(value) = params.get(key) {
            let (min, max) = if key == "size" { (1, 256) } else { (0, 2048) };
            if !value.as_u64().is_some_and(|n| (min..=max).contains(&n)) {
                return Err(format!("{key} is outside the lab bounds"));
            }
        }
    }
    for key in ["name", "font"] {
        if let Some(value) = params.get(key)
            && !value.as_str().is_some_and(|s| s.len() <= 128)
        {
            return Err(format!("{key} must be at most 128 UTF-8 bytes"));
        }
    }
    let hex = |s: &str| s.len() == 7 && s.starts_with('#') && s.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit);
    if let Some(value) = params.get("color")
        && !value.as_str().is_some_and(hex)
    {
        return Err("color must use #rrggbb".into());
    }
    if let Some(value) = params.get("background")
        && !value.as_str().is_some_and(|s| matches!(s, "white" | "black" | "transparent") || hex(s))
    {
        return Err("invalid background".into());
    }
    if let Some(value) = params.get("ellipse")
        && !value.is_boolean()
    {
        return Err("ellipse must be a boolean".into());
    }
    if id == "edit.fill" && params.get("target").and_then(Value::as_str) != Some("pixels") {
        return Err("fill requires explicit pixels target".into());
    }
    if id == "type.create" && !params.get("text").is_some_and(Value::is_string) {
        return Err("text must be a string".into());
    }
    if let Some(text) = params.get("text").and_then(Value::as_str)
        && text.len() > 4096
    {
        return Err("text is limited to 4096 UTF-8 bytes in the Creative Lab bridge".into());
    }
    Ok(())
}

pub fn authorize_step(id: &str, params: &Value) -> photocraft_engine::Result<()> {
    authorize(id, params).map_err(photocraft_engine::EngineError::Other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_ambient_commands_and_nested_scripts() {
        for id in ["file.open", "app.quit", "actions.play", "plugin.install", "prefs.set"] {
            assert!(authorize(id, &json!({})).is_err(), "{id}");
            assert!(authorize_step(id, &json!({})).is_err(), "nested {id}");
        }
    }

    #[test]
    fn new_document_requires_small_explicit_dimensions() {
        for params in [json!({}), json!({"width":0,"height":100}), json!({"width":2049,"height":100}), json!({"width":64.5,"height":100})] {
            assert!(authorize("file.new", &params).is_err());
        }
        assert!(authorize("file.new", &json!({"width":640,"height":360})).is_ok());
    }

    #[test]
    fn accepts_literal_unicode_type_without_rewriting() {
        let params = json!({"text":"שלום \"PhotoCraft\"", "x":40, "y":90, "size":36});
        assert!(authorize("type.create", &params).is_ok());
        assert_eq!(params["text"], "שלום \"PhotoCraft\"");
    }

    #[test]
    fn direct_callers_cannot_bypass_typed_bounds() {
        for params in [json!({"text":"x","size":100000}), json!({"text":"x","transform":[1,0,0,1,0,0]}), json!({"text":"x","color":["#ffffff"]})] {
            assert!(authorize("type.create", &params).is_err());
        }
        assert!(authorize("edit.fill", &json!({"color":"#ff0000"})).is_err());
        assert!(authorize("edit.fill", &json!({"color":"#ff0000","target":"pixels"})).is_ok());
    }
}
