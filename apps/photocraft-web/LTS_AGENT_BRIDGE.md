# LTS Creative Lab browser bridge

This fork adds a small Promise API at `window.photocraft` to the real browser
editor. It uses the upstream `PhotocraftApp` control dispatcher and its active
session, through one ordered queue drained after frame logic. The native CLI MCP
process has a separate session. A queued request expires after 30 seconds before
execution; synchronous codec work cannot be interrupted once it starts.
An unexpected WASM trap can leave a running Promise unresolved; queue expiry is
not execution cancellation, and recovery from a trapped runtime is unqualified.

The API is intended for a local, isolated experiment with generated fixtures.
It does not provide tenant authorization, an OS resource sandbox, durable
receipts, or an atomic expected-revision contract for simultaneous manual edits.
The embedding host is responsible for its origin checks and request sequencing.

## Methods

- `info()` returns bridge metadata and the command allowlist.
- `inspect()` resolves to the upstream UI inspection, including session state.
- `execute(command, params)` runs only commands in `agent_policy.rs` through the
  existing UI control dispatcher. Active manual text, pen, transform, crop/lasso,
  modal filters, unsaved-work prompts, context menus, and pointer press/release
  frames reject editing requests as busy. Explicit text font families must be
  loaded in this page first. Pixel fills require the composite channel view,
  without Quick Mask or a mask view. Paths,
  plugins, preferences, and scripts are not exposed.
- `importBytes(name, bytes)` opens a named PNG, JPEG, WebP, or PhotoCraft project
  from a `Uint8Array`, using a single-use in-memory transfer. No host path or URL
  is accepted. Signatures must match the permitted extension; PNG dimensions are
  limited to 2048 per side. The transfer cap is 16 MiB. Import does not create a
  saved path or an Open Recent entry.
- `exportBytes(format, documentIndex?)` resolves to
  `{format, documentIndex, warnings, bytes}` for `png` or `pcraft`. An optional
  zero-based index (0–7) reads an inactive document without switching tabs.
  Encoding borrows the document immutably, so receiving bytes does not
  mark the document saved or change its path. Export is limited to 2048 pixels
  per side, 64 layers, and 16 MiB of encoded output.
- `importFontBytes(bytes)` explicitly registers an in-memory font before text
  creation. The limit is 1 MiB per import and four successful imports per page.
  Its result contains registered family names, `existingTextReflowed:false`,
  `affectsFutureLayouts:true`, and whether a family was merged. Existing text
  models are unchanged, but future layout can use the new fallback or faces.
  No operating-system font is bundled.

The browser editor's manual file picker, drag/drop, editing tools, and downloads
remain available. Manual actions can affect the active session between agent
requests. An imported file can require much more memory after decompression than
its encoded byte size; these byte limits are not parser memory limits. Export
size is checked after encoding. Production import isolation remains separate work.

## Build

From the repository root, with the `wasm32-unknown-unknown` target installed:

```text
cargo check --locked -p photocraft-web --target wasm32-unknown-unknown -j 1
cargo build --locked -p photocraft-web --target wasm32-unknown-unknown --profile wasm-release -j 1
wasm-bindgen --target web --out-dir dist --out-name photocraft_web target/wasm32-unknown-unknown/wasm-release/photocraft-web.wasm
```

Use the wasm-bindgen CLI version matching `Cargo.lock` (currently 0.2.129).
Serve the generated JavaScript and WASM together with the canvas loading page;
the WASM response requires `application/wasm`. Localhost is suitable for the
browser's secure-context GPU requirement. The upstream `?cpu&webgl` options
are available for renderer diagnosis.

The bridge's Rust policy and byte-transfer tests live beside their modules. The
LTS JavaScript adapter has separate tests in the embedding repository. A
successful source check is not evidence that the browser renderer or the
embedding transport has been exercised.
