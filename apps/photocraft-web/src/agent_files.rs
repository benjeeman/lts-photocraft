//! Ephemeral byte handoff for the isolated browser bridge. No host paths or URLs.

pub const MAX_BYTES: usize = 16 * 1024 * 1024;

pub fn validate_input(name: &str, bytes: &[u8]) -> Result<(), String> {
    if name.is_empty() || name.len() > 255 || name.contains(['/', '\\', ':', '\0']) {
        return Err("import requires a file name without a path".into());
    }
    let extension = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "webp" | "pcraft") {
        return Err("unsupported browser import format".into());
    }
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return Err("import must contain from 1 byte to 16 MiB".into());
    }
    let matches = match extension.as_str() {
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.get(12..16) == Some(b"IHDR"),
        "jpg" | "jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        "pcraft" => bytes.starts_with(b"PK\x03\x04"),
        _ => false,
    };
    if !matches {
        return Err("file signature does not match its permitted extension".into());
    }
    if extension == "png" {
        for offset in [16, 20] {
            let dimension = bytes.get(offset..offset + 4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(u32::from_be_bytes);
            if !dimension.is_some_and(|value| (1..=2048).contains(&value)) {
                return Err("PNG dimensions must be from 1 to 2048".into());
            }
        }
    }
    Ok(())
}

/// Encoding borrows the session immutably: obtaining bytes cannot mark work saved.
pub fn export_bytes(session: &photocraft_engine::Session, format: &str, document_index: Option<usize>) -> Result<photocraft_io::ExportResult, String> {
    if !matches!(format, "png" | "pcraft") {
        return Err("export format must be png or pcraft".into());
    }
    let state = match document_index {
        Some(index) => session.documents().get(index).ok_or("no document at that index")?,
        None => session.active().ok_or("no document")?,
    };
    if state.doc.size.width > 2048 || state.doc.size.height > 2048 || state.doc.layer_count() > 64 {
        return Err("Creative Lab byte export is limited to 2048 pixels per side and 64 layers".into());
    }
    let options = photocraft_io::ExportOptions { xmp: photocraft_io::XmpEmbed::None, ..Default::default() };
    let output = photocraft_io::export(&state.doc, format, &options).map_err(|error| error.to_string())?;
    if output.bytes.len() > MAX_BYTES {
        return Err("export exceeds 16 MiB".into());
    }
    Ok(output)
}

/// Explicit per-page font registration. Load before creating text that needs it;
/// this does not rewrite or reflow existing document text.
pub fn import_font(bytes: Vec<u8>) -> Result<serde_json::Value, String> {
    if bytes.is_empty() || bytes.len() > 1024 * 1024 {
        return Err("font import requires from 1 byte to 1 MiB".into());
    }
    let mut engine = photocraft_text::shared().lock().unwrap_or_else(|error| error.into_inner());
    let before = engine.fonts.families();
    let names = engine.fonts.register_font_data(bytes);
    if names.is_empty() {
        return Err("no supported font face was found".into());
    }
    let merged = names.iter().any(|name| before.contains(name));
    Ok(serde_json::json!({"families":names,"existingTextReflowed":false,"affectsFutureLayouts":true,"mergedIntoExistingFamily":merged}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_have_no_path_authority_or_mismatched_parser() {
        assert!(validate_input("../private.png", b"8BPS").is_err());
        assert!(validate_input("frame.png", b"8BPS").is_err());
        assert!(validate_input("empty.png", &[]).is_err());
        assert!(validate_input("frame.jpg", b"\xff\xd8\xff").is_ok());
        assert!(validate_input("frame.webp", b"RIFF1234WEBP").is_ok());
        assert!(validate_input("frame.pcraft", b"PK\x03\x04").is_ok());
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\x02\x80\0\0\x01h".to_vec();
        assert!(validate_input("frame.png", &png).is_ok());
        png[16..20].copy_from_slice(&2049_u32.to_be_bytes());
        assert!(validate_input("frame.png", &png).is_err());
    }

    #[test]
    fn exporting_bytes_keeps_document_dirty_and_preserves_its_path() {
        let mut session = photocraft_engine::Session::new();
        session.execute("file.new", serde_json::json!({"width":8,"height":8})).expect("new fixture");
        session.execute("layer.new.layer", serde_json::json!({"name":"Dirty fixture"})).expect("edit fixture");
        assert!(session.active().expect("fixture").is_dirty());
        let state = session.active_mut().expect("active fixture");
        state.path = Some("original.pcraft".into());
        state.saved_revision = 0;
        let before_revision = state.revision;
        for format in ["png", "pcraft"] {
            assert!(!export_bytes(&session, format, None).expect("encode fixture").bytes.is_empty());
            let state = session.active().expect("fixture retained");
            assert_eq!(state.path.as_deref(), Some("original.pcraft"));
            assert_eq!(state.saved_revision, 0);
            assert_eq!(state.revision, before_revision);
        }
        assert!(export_bytes(&session, "exe", None).is_err());
    }

    #[test]
    fn inactive_document_export_does_not_switch_or_save_either_document() {
        let mut session = photocraft_engine::Session::new();
        for (name, side) in [("first", 8), ("second", 16)] {
            session.execute("file.new", serde_json::json!({"width":side,"height":side,"name":name})).expect("new fixture");
            session.execute("layer.new.layer", serde_json::json!({"name":"Dirty"})).expect("edit fixture");
        }
        let state = |s: &photocraft_engine::Session| {
            s.documents().iter().map(|d| (d.doc.id, d.revision, d.saved_revision, d.path.clone(), d.is_dirty())).collect::<Vec<_>>()
        };
        let before = state(&session);
        assert_eq!(session.active_index(), Some(1));
        let first = export_bytes(&session, "png", Some(0)).expect("inactive PNG");
        assert_eq!(first.bytes.get(16..20), Some(8_u32.to_be_bytes().as_slice()));
        assert!(!export_bytes(&session, "pcraft", Some(0)).expect("inactive project").bytes.is_empty());
        assert_eq!(session.active_index(), Some(1));
        assert_eq!(state(&session), before);
        assert!(export_bytes(&session, "png", Some(7)).is_err());
    }

    #[test]
    fn invalid_font_data_is_an_error() {
        assert!(import_font(Vec::new()).is_err());
        assert!(import_font(vec![0, 1, 2]).is_err());
        assert!(import_font(vec![0; 1024 * 1024 + 1]).is_err());
    }
}
