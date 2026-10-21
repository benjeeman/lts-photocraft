//! Promise API for the visible editor. One queue runs after upstream frame logic.
use photocraft_ui_egui::{ControlRequest, PhotocraftApp, control};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use wasm_bindgen::{JsCast, prelude::*};

enum Operation {
    Inspect,
    Execute(String, Value),
    Import(String, Vec<u8>),
    Export(String, Option<usize>),
    Font(Vec<u8>),
}
struct Pending {
    id: u32,
    operation: Operation,
    resolve: js_sys::Function,
    reject: js_sys::Function,
    expires: f64,
}
thread_local! {
    static QUEUE: RefCell<VecDeque<Pending>> = const { RefCell::new(VecDeque::new()) };
    static NEXT_ID: Cell<u32> = const { Cell::new(0) };
    static FONT_IMPORTS: Cell<u8> = const { Cell::new(0) };
}
const TIMEOUT_MS: i32 = 30_000;
fn error(message: impl AsRef<str>) -> JsValue {
    js_sys::Error::new(message.as_ref()).into()
}
fn reject(message: impl AsRef<str>) -> js_sys::Promise {
    js_sys::Promise::reject(&error(message))
}
fn to_js(value: &Value) -> Result<JsValue, JsValue> {
    let encoded = value.to_string();
    if encoded.len() > 1024 * 1024 {
        return Err(error("browser response exceeds 1 MiB; inspect before retrying a mutation"));
    }
    js_sys::JSON::parse(&encoded)
}
fn from_js(value: &JsValue) -> Result<Value, JsValue> {
    let encoded = js_sys::JSON::stringify(value)?.as_string().ok_or_else(|| error("parameters must be JSON"))?;
    if encoded.len() > 16_384 {
        return Err(error("parameters exceed 16 KiB"));
    }
    serde_json::from_str(&encoded).map_err(|e| error(e.to_string()))
}
fn hidden() -> bool {
    web_sys::window().and_then(|w| w.document()).is_none_or(|d| d.hidden())
}
fn expire(id: u32) {
    let pending = QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        q.iter().position(|p| p.id == id).and_then(|index| q.remove(index))
    });
    if let Some(pending) = pending {
        let _ = pending.reject.call1(&JsValue::NULL, &error("request expired before execution; not applied"));
    }
}
fn enqueue(operation: Operation, ctx: &egui::Context) -> js_sys::Promise {
    if hidden() {
        return reject("editor page is hidden; request was not queued");
    }
    if QUEUE.with(|q| q.borrow().len() >= 8) {
        return reject("at most eight browser requests may be pending");
    }
    let Some(id) = NEXT_ID.get().checked_add(1) else {
        return reject("browser request ids exhausted");
    };
    NEXT_ID.set(id);
    let mut operation = Some(operation);
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let callback = Closure::once_into_js(move || expire(id));
        let timer = web_sys::window()
            .ok_or_else(|| error("browser closed"))
            .and_then(|w| w.set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), TIMEOUT_MS));
        match (timer, operation.take()) {
            (Ok(_), Some(operation)) => {
                QUEUE.with(|q| q.borrow_mut().push_back(Pending { id, operation, resolve, reject, expires: js_sys::Date::now() + f64::from(TIMEOUT_MS) }))
            }
            (Err(e), _) => {
                let _ = reject.call1(&JsValue::NULL, &e);
            }
            _ => {
                let _ = reject.call1(&JsValue::NULL, &error("request could not be queued"));
            }
        }
    });
    ctx.request_repaint();
    promise
}
fn perform(operation: Operation, app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<JsValue, JsValue> {
    if !matches!(&operation, Operation::Inspect) && crate::agent_context::busy(app, ctx) {
        return Err(error("editor busy with a manual edit or dialog; request not applied"));
    }
    match operation {
        Operation::Inspect => to_js(&control::inspect(app, ctx)),
        Operation::Execute(command, params) => {
            crate::agent_policy::authorize(&command, &params).map_err(error)?;
            crate::agent_context::validate(&command, &params, &app.session).map_err(error)?;
            if command == "file.new" && app.session.documents().len() >= 8 {
                return Err(error("lab document limit reached"));
            }
            if matches!(command.as_str(), "type.create" | "layer.new.layer") && app.session.active().is_some_and(|s| s.doc.layer_count() >= 64) {
                return Err(error("lab layer limit reached"));
            }
            let (request, _) = ControlRequest::new("engine.execute", json!({"command": command, "params": params}));
            match control::handle(app, ctx, &request) {
                control::Outcome::Done(reply) if reply.get("ok").and_then(Value::as_bool) == Some(true) => to_js(reply.get("result").unwrap_or(&Value::Null)),
                control::Outcome::Done(reply) => Err(error(reply.get("error").and_then(Value::as_str).unwrap_or("command failed; inspect before retrying"))),
                _ => Err(error("unexpected deferred command; outcome unknown, inspect before retrying")),
            }
        }
        Operation::Import(name, bytes) => {
            if app.session.documents().len() >= 8 {
                return Err(error("lab document limit reached"));
            }
            let warnings = app.open_automation_bytes(&name, &bytes).map_err(error)?;
            to_js(&json!({"warnings": warnings, "path": null}))
        }
        Operation::Export(format, document_index) => {
            let output = crate::agent_files::export_bytes(&app.session, &format, document_index).map_err(error)?;
            let result = to_js(&json!({"format": format, "documentIndex": document_index.or(app.session.active_index()), "warnings": output.warnings}))?;
            js_sys::Reflect::set(&result, &"bytes".into(), &js_sys::Uint8Array::from(output.bytes.as_slice()))?;
            Ok(result)
        }
        Operation::Font(bytes) => {
            if FONT_IMPORTS.get() >= 4 {
                return Err(error("at most four explicit font imports per page"));
            }
            let result = crate::agent_files::import_font(bytes).map_err(error)?;
            FONT_IMPORTS.set(FONT_IMPORTS.get() + 1);
            to_js(&result)
        }
    }
}
pub fn drain(app: &mut PhotocraftApp, ctx: &egui::Context) {
    loop {
        let Some(pending) = QUEUE.with(|q| q.borrow_mut().pop_front()) else {
            break;
        };
        // Let the one-shot timeout run even after success: its closure releases
        // itself when invoked and expire is a no-op for completed request IDs.
        let result = if hidden() || js_sys::Date::now() >= pending.expires {
            Err(error("request expired or page hidden before execution; not applied"))
        } else {
            perform(pending.operation, app, ctx)
        };
        match result {
            Ok(value) => {
                let _ = pending.resolve.call1(&JsValue::NULL, &value);
            }
            Err(e) => {
                let _ = pending.reject.call1(&JsValue::NULL, &e);
            }
        }
        ctx.request_repaint();
    }
}
fn set(object: &js_sys::Object, name: &str, value: JsValue) -> Result<(), JsValue> {
    js_sys::Reflect::set(object, &name.into(), &value).map(|_| ())
}
pub fn install(ctx: egui::Context) -> Result<(), JsValue> {
    let object = js_sys::Object::new();
    let execute_ctx = ctx.clone();
    set(
        &object,
        "execute",
        Closure::<dyn Fn(String, JsValue) -> js_sys::Promise>::new(move |command: String, params: JsValue| {
            let params = match from_js(&params) {
                Ok(p) => p,
                Err(e) => return js_sys::Promise::reject(&e),
            };
            if let Err(e) = crate::agent_policy::authorize(&command, &params) {
                return reject(e);
            }
            enqueue(Operation::Execute(command, params), &execute_ctx)
        })
        .into_js_value(),
    )?;
    let inspect_ctx = ctx.clone();
    set(&object, "inspect", Closure::<dyn Fn() -> js_sys::Promise>::new(move || enqueue(Operation::Inspect, &inspect_ctx)).into_js_value())?;
    let import_ctx = ctx.clone();
    set(
        &object,
        "importBytes",
        Closure::<dyn Fn(String, js_sys::Uint8Array) -> js_sys::Promise>::new(move |name: String, bytes: js_sys::Uint8Array| {
            if bytes.length() == 0 || bytes.length() as usize > crate::agent_files::MAX_BYTES {
                return reject("import must contain from 1 byte to 16 MiB");
            }
            let bytes = bytes.to_vec();
            if let Err(e) = crate::agent_files::validate_input(&name, &bytes) {
                return reject(e);
            }
            enqueue(Operation::Import(name, bytes), &import_ctx)
        })
        .into_js_value(),
    )?;
    let export_ctx = ctx.clone();
    set(
        &object,
        "exportBytes",
        Closure::<dyn Fn(String, JsValue) -> js_sys::Promise>::new(move |format: String, index: JsValue| {
            if !matches!(format.as_str(), "png" | "pcraft") {
                return reject("export format must be png or pcraft");
            }
            let document_index = if index.is_undefined() {
                None
            } else if let Some(value) = index.as_f64().filter(|n| n.is_finite() && n.fract() == 0.0 && (0.0..=7.0).contains(n)) {
                Some(value as usize)
            } else {
                return reject("document index must be an integer from 0 to 7");
            };
            enqueue(Operation::Export(format, document_index), &export_ctx)
        })
        .into_js_value(),
    )?;
    set(
        &object,
        "importFontBytes",
        Closure::<dyn Fn(js_sys::Uint8Array) -> js_sys::Promise>::new(move |bytes: js_sys::Uint8Array| {
            if bytes.length() == 0 || bytes.length() > 1024 * 1024 {
                return reject("font import requires from 1 byte to 1 MiB");
            }
            enqueue(Operation::Font(bytes.to_vec()), &ctx)
        })
        .into_js_value(),
    )?;
    set(&object, "info", Closure::<dyn Fn() -> JsValue>::new(|| to_js(&json!({"apiVersion":"lts-lab-1", "upstreamVersion":"0.5.0", "session":"visible-browser-editor", "nativeSessionShared":false, "commands":crate::agent_policy::COMMANDS, "queue":"ordered-frame-queue", "queuedExpiryMs":TIMEOUT_MS, "explicitFontImport":true, "fontImportCount":FONT_IMPORTS.get(), "fontsAffectFutureLayouts":true, "explicitFontRequiredLoaded":true, "documentIndexExport":true, "scope":"isolated-generated-fixture-lab"})).unwrap_or(JsValue::NULL)).into_js_value())?;
    let window = web_sys::window().ok_or_else(|| error("no browser window"))?;
    js_sys::Reflect::set(&window, &"photocraft".into(), &object)?;
    Ok(())
}
