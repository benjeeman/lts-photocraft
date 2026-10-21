//! `window.photocraftView`: the LTS agent view of the visible editor (final design Appendix A.2;
//! implementation plan §1 FP1).
//!
//! `describe`, `render`, `sample`, `fonts`, `metadata`, `watermark` and the typed `execute` wait
//! in this module's own ordered queue (reads and edits in one FIFO), drained right after the
//! legacy `window.photocraft` queue, so they see the session between frames like every other
//! agent request. Reads resolve with their value or reject with an `Error` whose `code` is a
//! registry read failure; `execute` always resolves with the `photocraft.typed-execute.v1`
//! envelope that `lts_view_core` builds.
//!
//! The glue adds what only the page knows: whether it is hidden, whether a request's deadline
//! passed while it waited (a timer settles an expired request even when no frame runs, and it can
//! no longer run afterwards), and the other owner's `agent_context::busy` and
//! `agent_context::validate`, called rather than copied.
//!
//! R0 (master plan v2 §3.1) adds two reads, `export` (the agent export with the positive metadata
//! fields only) and `textCheck` (a type layer's line ranges), and `placeBytes(bytes, params,
//! options)`, which queues a `Uint8Array` like a typed edit and resolves with the same envelope; it
//! places through the engine's own `file_cmds::place_bytes`. The bytes arrive by handle, never in
//! the broker's 1 MiB command: the lab parent substitutes a staged input's bytes after a sha256
//! match and the frame's adapter passes them here (final design §2.4, §8.1; INV-47). The hook takes
//! RD-17's 64 MiB input cap (`lts_view_core::MAX_INPUT_BYTES`), and at most that many input bytes
//! wait in the queue at once (`lts_view_core::MAX_PENDING_PLACE_BYTES`, decided before the copy).
//!
//! R1 (master plan v3 W3) keeps the writes on these two hooks: an import is a `placeBytes` mode
//! (`newDocument: true`, `lts_view_core::is_import`), which opens the bytes the core re-encodes as
//! a new document through the engine's own `file_cmds::open_bytes_as`.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use photocraft_ui_egui::PhotocraftApp;
use serde_json::{Map, Value};
use wasm_bindgen::{JsCast, prelude::*};

use super::lts_view_core::{self, Env, Rendered, Runtime, ViewError};

const VERSION: &str = "photocraft.agent-view.v1";
const MAX_PENDING: usize = 8;
const READ_DEADLINE_MS: i32 = 15_000;
const DEFAULT_DEADLINE_MS: i32 = 15_000;
const MAX_DEADLINE_MS: i32 = 30_000;
const MAX_ARGUMENT_BYTES: usize = 16 * 1024;
const MAX_RESULT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy)]
enum Read {
    Describe,
    Render,
    Sample,
    Fonts,
    Metadata,
    Watermark,
    Export,
    TextCheck,
}

enum Request {
    Read(Read, Value),
    Execute { command: String, params: Value, options: Value },
    Place { bytes: Vec<u8>, params: Value, options: Value },
}

struct Pending {
    id: u32,
    request: Request,
    resolve: js_sys::Function,
    reject: js_sys::Function,
    expires: f64,
}

thread_local! {
    static QUEUE: RefCell<VecDeque<Pending>> = const { RefCell::new(VecDeque::new()) };
    static NEXT_ID: Cell<u32> = const { Cell::new(0) };
}

/// Installs `window.photocraftView`. A failure is logged and leaves `window.photocraft` as it is.
pub fn install(ctx: egui::Context) {
    if let Err(error) = try_install(&ctx) {
        log::error!("PhotoCraft agent view failed to install: {error:?}");
    }
}

fn try_install(ctx: &egui::Context) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| js_error("no browser window"))?;
    let view = js_sys::Object::new();
    set(&view, "version", JsValue::from_str(VERSION))?;
    let reads = [
        ("describe", Read::Describe),
        ("render", Read::Render),
        ("sample", Read::Sample),
        ("fonts", Read::Fonts),
        ("metadata", Read::Metadata),
        ("export", Read::Export),
        ("textCheck", Read::TextCheck),
    ];
    for (name, read) in reads {
        let ctx = ctx.clone();
        let method = Closure::<dyn Fn(JsValue) -> js_sys::Promise>::new(move |params: JsValue| match argument(&params) {
            Ok(params) => enqueue(Request::Read(read, params), READ_DEADLINE_MS, &ctx),
            Err(message) => js_sys::Promise::reject(&read_error("read_failed", &message)),
        });
        set(&view, name, method.into_js_value())?;
    }
    let watermark_ctx = ctx.clone();
    let watermark = Closure::<dyn Fn() -> js_sys::Promise>::new(move || enqueue(Request::Read(Read::Watermark, Value::Null), READ_DEADLINE_MS, &watermark_ctx));
    set(&view, "watermark", watermark.into_js_value())?;
    let execute_ctx = ctx.clone();
    let execute = Closure::<dyn Fn(JsValue, JsValue, JsValue) -> js_sys::Promise>::new(move |command: JsValue, params: JsValue, options: JsValue| {
        submit(&command, &params, &options, &execute_ctx)
    });
    set(&view, "execute", execute.into_js_value())?;
    let place_ctx = ctx.clone();
    let place = Closure::<dyn Fn(JsValue, JsValue, JsValue) -> js_sys::Promise>::new(move |bytes: JsValue, params: JsValue, options: JsValue| {
        submit_place(&bytes, &params, &options, &place_ctx)
    });
    set(&view, "placeBytes", place.into_js_value())?;
    js_sys::Reflect::set(&window, &"photocraftView".into(), &view)?;
    Ok(())
}

/// A typed request: refused at once when it is malformed or the page is hidden, queued otherwise
/// with its own deadline (`options.deadlineMs`, at most 30 s).
fn submit(command: &JsValue, params: &JsValue, options: &JsValue, ctx: &egui::Context) -> js_sys::Promise {
    let Some(command) = command.as_string() else {
        return promise(to_js(&lts_view_core::not_applied("command_not_allowed", "the command must be a string id")));
    };
    let (params, options) = match (argument(params), argument(options)) {
        (Ok(params), Ok(options)) => (params, options),
        (Err(message), _) | (_, Err(message)) => return promise(to_js(&lts_view_core::not_applied("invalid_params", &message))),
    };
    if hidden() {
        return promise(to_js(&lts_view_core::not_applied("page_hidden", "the editor page is hidden; not applied")));
    }
    let deadline = deadline_ms(&options);
    enqueue(Request::Execute { command, params, options }, deadline, ctx)
}

/// A `placeBytes` request: the encoded image as a `Uint8Array` of 1 byte to
/// `lts_view_core::MAX_INPUT_BYTES` (RD-17's 64 MiB input cap, as the core checks it), copied into
/// the queue; refused at once like `execute` when it is malformed or the page is hidden, and
/// before the copy when the queue has no room for it.
fn submit_place(bytes: &JsValue, params: &JsValue, options: &JsValue, ctx: &egui::Context) -> js_sys::Promise {
    let Some(array) = bytes.dyn_ref::<js_sys::Uint8Array>() else {
        return promise(to_js(&lts_view_core::not_applied("input_rejected", "the image must be a Uint8Array")));
    };
    let length = usize::try_from(array.length()).unwrap_or(usize::MAX);
    if length == 0 || length > lts_view_core::MAX_INPUT_BYTES {
        let message = format!("the image must be 1 to {} bytes", lts_view_core::MAX_INPUT_BYTES);
        return promise(to_js(&lts_view_core::not_applied("input_rejected", &message)));
    }
    let (params, options) = match (argument(params), argument(options)) {
        (Ok(params), Ok(options)) => (params, options),
        (Err(message), _) | (_, Err(message)) => return promise(to_js(&lts_view_core::not_applied("invalid_params", &message))),
    };
    if hidden() {
        return promise(to_js(&lts_view_core::not_applied("page_hidden", "the editor page is hidden; not applied")));
    }
    if let Some(refusal) = place_room(length) {
        return promise(to_js(&refusal));
    }
    let deadline = deadline_ms(&options);
    enqueue(Request::Place { bytes: array.to_vec(), params, options }, deadline, ctx)
}

/// The typed `queue_full` refusal of one more `placeBytes` input of `length` bytes, decided before
/// the bytes are copied into the page's memory: `MAX_PENDING` requests already wait, or the waiting
/// inputs and this one would pass the core's byte bound (`lts_view_core::place_queue_refusal`).
fn place_room(length: usize) -> Option<Value> {
    QUEUE.with(|queue| {
        let queue = queue.borrow();
        if queue.len() >= MAX_PENDING {
            return Some(lts_view_core::not_applied("queue_full", "at most eight agent view requests may be pending"));
        }
        let waiting: usize = queue.iter().map(|pending| if let Request::Place { bytes, .. } = &pending.request { bytes.len() } else { 0 }).sum();
        lts_view_core::place_queue_refusal(waiting, length)
    })
}

/// A typed request's own deadline: `options.deadlineMs`, at most 30 s (15 s when absent).
fn deadline_ms(options: &Value) -> i32 {
    let deadline = options.get("deadlineMs").and_then(Value::as_i64).and_then(|ms| i32::try_from(ms).ok()).filter(|ms| *ms > 0);
    deadline.map_or(DEFAULT_DEADLINE_MS, |ms| ms.min(MAX_DEADLINE_MS))
}

fn enqueue(request: Request, deadline_ms: i32, ctx: &egui::Context) -> js_sys::Promise {
    let execute = matches!(request, Request::Execute { .. } | Request::Place { .. });
    if QUEUE.with(|queue| queue.borrow().len() >= MAX_PENDING) {
        return promise(refused(execute, "queue_full", "at most eight agent view requests may be pending"));
    }
    let Some(id) = NEXT_ID.get().checked_add(1) else {
        return promise(refused(execute, "queue_unavailable", "agent view request ids are exhausted"));
    };
    NEXT_ID.set(id);
    let mut request = Some(request);
    let queued = js_sys::Promise::new(&mut |resolve, reject| {
        let callback = Closure::once_into_js(move || expire(id));
        let timer = web_sys::window()
            .ok_or_else(|| js_error("no browser window"))
            .and_then(|window| window.set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), deadline_ms));
        match (timer, request.take()) {
            (Ok(_), Some(request)) => {
                let expires = js_sys::Date::now() + f64::from(deadline_ms);
                QUEUE.with(|queue| queue.borrow_mut().push_back(Pending { id, request, resolve, reject, expires }));
            }
            _ => {
                // Nothing was queued: release the one-shot timer closure (`expire` finds no entry).
                let _ = callback.unchecked_ref::<js_sys::Function>().call0(&JsValue::NULL);
                settle(&resolve, &reject, refused(execute, "queue_unavailable", "the request could not be queued"));
            }
        }
    });
    ctx.request_repaint();
    queued
}

/// The deadline passed before any frame ran the request: it leaves the queue, so it never runs.
fn expire(id: u32) {
    let pending = QUEUE.with(|queue| {
        let mut queue = queue.borrow_mut();
        queue.iter().position(|pending| pending.id == id).and_then(|index| queue.remove(index))
    });
    if let Some(pending) = pending {
        let execute = matches!(pending.request, Request::Execute { .. } | Request::Place { .. });
        settle(&pending.resolve, &pending.reject, refused(execute, "queue_expired", "the request expired before the editor could run it; not applied"));
    }
}

/// Runs every queued request, oldest first. Called right after `agent_api::drain`.
pub fn drain(app: &mut PhotocraftApp, ctx: &egui::Context) {
    while let Some(pending) = QUEUE.with(|queue| queue.borrow_mut().pop_front()) {
        let expired = js_sys::Date::now() >= pending.expires;
        let outcome = perform(pending.request, app, ctx, hidden(), expired);
        settle(&pending.resolve, &pending.reject, outcome);
        ctx.request_repaint();
    }
}

fn perform(request: Request, app: &mut PhotocraftApp, ctx: &egui::Context, hidden: bool, expired: bool) -> Result<JsValue, JsValue> {
    match request {
        Request::Execute { command, params, options } => {
            let families = families();
            let busy = crate::agent_context::busy(app, ctx);
            let env = Env { hidden, expired, busy, families: &families, validate: crate::agent_context::validate, runtime: Runtime::HERE };
            let envelope =
                lts_view_core::execute(app, &env, &command, &params, &options, &mut |app: &mut PhotocraftApp, id: &str, params: Value| app.run(id, params));
            to_js(&envelope)
        }
        Request::Place { bytes, params, options } => {
            let families = families();
            let busy = crate::agent_context::busy(app, ctx);
            let env = Env { hidden, expired, busy, families: &families, validate: crate::agent_context::validate, runtime: Runtime::HERE };
            // An import opens the PNG the core re-encoded as a new document; any other request
            // places it on the active one.
            let mut place = |app: &mut PhotocraftApp, name: &str, bytes: Vec<u8>, params: Value| {
                let done = if lts_view_core::is_import(&params) {
                    photocraft_engine::file_cmds::open_bytes_as(&mut app.session, name, &bytes, Some("png"), None)
                } else {
                    photocraft_engine::file_cmds::place_bytes(&mut app.session, name, bytes, None, &params)
                };
                app.sync_views();
                done.map_err(|error| error.to_string())
            };
            to_js(&lts_view_core::place_bytes(app, &env, bytes, &params, &options, &mut place))
        }
        Request::Read(read, params) => {
            if hidden {
                return Err(read_error("page_hidden", "the editor page is hidden"));
            }
            if expired {
                return Err(read_error("read_timeout", "the read expired before the editor could run it"));
            }
            read_now(read, &params, app, ctx)
        }
    }
}

fn read_now(read: Read, params: &Value, app: &PhotocraftApp, ctx: &egui::Context) -> Result<JsValue, JsValue> {
    match read {
        Read::Describe => {
            let mut value = lts_view_core::describe(&app.session, params).map_err(view_error)?;
            if let Some(object) = value.as_object_mut() {
                object.insert("busy".into(), Value::Bool(crate::agent_context::busy(app, ctx)));
            }
            to_js(&value)
        }
        Read::Render => rendered(lts_view_core::render(&app.session, params).map_err(view_error)?),
        Read::Sample => to_js(&lts_view_core::sample(&app.session, params).map_err(view_error)?),
        Read::Fonts => {
            let listed = lts_view_core::fonts(&mut photocraft_text::shared().lock().unwrap_or_else(|error| error.into_inner()), params);
            to_js(&listed.map_err(view_error)?)
        }
        Read::Metadata => to_js(&lts_view_core::metadata(&app.session, params).map_err(view_error)?),
        Read::Export => rendered(lts_view_core::export(&app.session, params).map_err(view_error)?),
        Read::TextCheck => {
            let checked = lts_view_core::text_check(&mut photocraft_text::shared().lock().unwrap_or_else(|error| error.into_inner()), &app.session, params);
            to_js(&checked.map_err(view_error)?)
        }
        Read::Watermark => {
            let families = families();
            to_js(&lts_view_core::watermark(&app.session, crate::agent_context::busy(app, ctx), &families))
        }
    }
}

/// The page's registered font families (the lock is released before any command runs).
fn families() -> Vec<String> {
    photocraft_text::shared().lock().unwrap_or_else(|error| error.into_inner()).fonts.families()
}

/// A render as its fields plus the encoded image (`bytes`, and `png` when it is a PNG).
fn rendered(rendered: Rendered) -> Result<JsValue, JsValue> {
    let value = to_js(&rendered.info)?;
    let bytes = js_sys::Uint8Array::from(rendered.bytes.as_slice());
    js_sys::Reflect::set(&value, &"bytes".into(), &bytes)?;
    if rendered.info.get("mime").and_then(Value::as_str) == Some("image/png") {
        js_sys::Reflect::set(&value, &"png".into(), &bytes)?;
    }
    Ok(value)
}

/// A refusal before the engine was reached: a typed `not_applied` envelope for `execute`, a
/// rejected read with the matching read failure code otherwise.
fn refused(execute: bool, code: &'static str, message: &str) -> Result<JsValue, JsValue> {
    if execute {
        return to_js(&lts_view_core::not_applied(code, message));
    }
    let read_code = match code {
        "queue_full" => "budget_exceeded",
        "queue_expired" => "read_timeout",
        "queue_unavailable" => "read_failed",
        other => other,
    };
    Err(read_error(read_code, message))
}

fn promise(outcome: Result<JsValue, JsValue>) -> js_sys::Promise {
    match outcome {
        Ok(value) => js_sys::Promise::resolve(&value),
        Err(error) => js_sys::Promise::reject(&error),
    }
}

fn settle(resolve: &js_sys::Function, reject: &js_sys::Function, outcome: Result<JsValue, JsValue>) {
    let _ = match outcome {
        Ok(value) => resolve.call1(&JsValue::NULL, &value),
        Err(error) => reject.call1(&JsValue::NULL, &error),
    };
}

/// One JSON argument of at most 16 KiB; `undefined` and `null` are `{}`.
fn argument(value: &JsValue) -> Result<Value, String> {
    if value.is_undefined() || value.is_null() {
        return Ok(Value::Object(Map::new()));
    }
    let encoded = js_sys::JSON::stringify(value).ok().and_then(|text| text.as_string()).ok_or("arguments must be JSON")?;
    if encoded.len() > MAX_ARGUMENT_BYTES {
        return Err(format!("an argument exceeds {} KiB", MAX_ARGUMENT_BYTES / 1024));
    }
    serde_json::from_str(&encoded).map_err(|error| error.to_string())
}

fn to_js(value: &Value) -> Result<JsValue, JsValue> {
    let encoded = value.to_string();
    if encoded.len() > MAX_RESULT_BYTES {
        return Err(read_error("budget_exceeded", "the result exceeds 1 MiB"));
    }
    js_sys::JSON::parse(&encoded)
}

fn view_error(error: ViewError) -> JsValue {
    read_error(error.code, &error.message)
}

fn read_error(code: &str, message: &str) -> JsValue {
    let error = js_sys::Error::new(message);
    let _ = js_sys::Reflect::set(&error, &"code".into(), &JsValue::from_str(code));
    error.into()
}

fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}

fn set(object: &js_sys::Object, name: &str, value: JsValue) -> Result<(), JsValue> {
    js_sys::Reflect::set(object, &name.into(), &value).map(|_| ())
}

fn hidden() -> bool {
    web_sys::window().and_then(|window| window.document()).is_none_or(|document| document.hidden())
}
