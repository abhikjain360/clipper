# Object Kind Plumbing

Reference material for [D3 in `docs/schedule-plan.md`](schedule-plan.md#d3-the-object-layer-gets-generalized-before-the-schedule-is-built-on-it):
what it currently costs to add an `ObjectKind` to Clipper, and a design for
collapsing the mechanical part of that cost before the schedule module (and the
habits/tasks modules after it) are built.

**Provenance.** Produced by a delegated agent surveying the repo at `f49ee53`
(2026-09-07), then spot-checked by hand. Verified directly: the hardcoded
`Set("file".into())` at `routes/objects.rs:1539` and the `kind != ObjectKind::File`
delete gate at `:1438`; the `LocalObjectData` enum at `local_store.rs:52`; the
CHECK constraints in `m20260615_000002` and `m20260826_000003`; the
`patches/uniffi-bindgen-react-native@0.31.0-5.patch` filename; the `AppState`
per-kind `Vec` fields in `app-types`. Every sampled line reference was accurate.
The site counts in Part 2 are approximate by the author's own framing. Parts 3
and 4 are a proposal, not a decision — the mutability fork in Part 4 is open.

---

Method: (a) exhaustive grep for `ObjectKind` and its variants plus per-kind
structural code (per-kind `Vec` fields, per-kind IPC variants, per-kind routes,
per-kind UI panels) across Rust and TypeScript; (b) git archaeology of the
commits that introduced `Collab`. Raw grep count for the three variant literals
alone: **86 occurrences across 7 Rust files**
(`client/engine.rs: 24`, `server/routes/objects.rs: 39`,
`server/routes/collab.rs: 10`, `client/local_store.rs: 10`, plus 1 each in
`client/api_client.rs`, `server/ws.rs`, `server/state.rs`) — but the variant
literals are the minority of the real cost, which lives in the per-kind
*shapes* around them.

Legend: **MECH** = mechanical (same shape repeated per kind; a checklist edit);
**SPEC** = kind-specific (real domain logic no registry can write for you).

---

## PART 1 — EXHAUSTIVE TOUCHPOINT INVENTORY

### Git archaeology (method b): what Collab actually cost

`Collab` was threaded end-to-end by six commits on `main`:

| SHA (short) | Date | Message | Files | +/- |
|---|---|---|---|---|
| `746c259` | 2026-06-14 | feat: collab docs schema, api types, and server CRUD routes | 13 | +791/-18 |
| `8cffd25` | 2026-06-14 | feat: collab docs schema, api types, and server CRUD routes (client/daemon/web half) | 13 | +861/-39 |
| `c3ca0c8` | 2026-06-20 | mobile: get the Android app building/running and wire collab docs through the bridge | 9 | +170/-17 |
| `e36ec32` | 2026-06-20 | mobile: add collab docs UI and support per-login server selection | 3 | +174/-22 |
| `d14be93` | 2026-06-27 | feat(collab): live editing, language selector, editor vim toggle | 15 | +1450/-104 |
| `0295964` | 2026-08-26 | fix(collab): renameable docs, correct share links, working desktop editor | 43 | +1695/-164 |

The minimal end-to-end threading is `746c259` + `8cffd25`; everything after is
domain behavior (Y-sync live editing, sharing, rename, mobile viewer).
Consolidated, deduplicated file list across all six commits (~45 unique files):

- **api-types**: `crates/api-types/src/lib.rs`
- **app-types**: `crates/app-types/src/lib.rs`
- **server**: `config.rs`, `main.rs`, `state.rs`, `collab_sync.rs`,
  `entity/{collab_docs,mod,objects,access_keys,devices,event_log,object_payloads,server_config,sessions,users}.rs`,
  `routes/{collab,mod,objects}.rs`, `Cargo.toml`
- **migrations**: `migration/mod.rs`, `m20260615_000002_collab_docs.rs`,
  `m20260826_000003_collab_doc_title.rs`
- **client**: `api_client.rs`, `engine.rs`, `local_store.rs`
- **daemon**: `daemon-types/src/protocol.rs`, `daemon/src/handler.rs`,
  `daemon/src/protocol.rs`
- **adapters**: `web-wasm/src/lib.rs`, `mobile-uniffi/src/lib.rs`,
  `web/src-tauri/src/lib.rs`, `web/src-tauri/tauri.conf.json`
- **TS contracts**: `packages/shared/src/{types,index}.ts`,
  `packages/mobile-bridge/src/adapter.ts`,
  `packages/mobile-bridge/react-native.config.js`
- **web UI**: `web/src/App.tsx`, `CodeEditor.tsx`, `LanguageSelect.tsx`,
  `languages.ts`, `backend/{index,tauri}.ts`, `web/public/_redirects`
- **mobile UI**: `mobile/index.ts`, `mobile/src/{App,backend,collabDoc,webcryptoPolyfill}.tsx/ts`
- **docs/build**: `docs/collab-docs-plan.md`, `AGENTS.md`, `Cargo.lock`,
  `pnpm-lock.yaml`, `pnpm-workspace.yaml`, `scripts/server-entities.ts`,
  `web/package.json`, `mobile/package.json`, `mobile/metro.config.js`,
  `patches/uniffi-bindgen-react-native@0.31.0-3.patch`

Note that collab is the *expensive* kind template: it is server-visible, has a
dedicated table, dedicated JSON routes, a Y-sync WebSocket, and public share
links. A new **encrypted** kind (the likely shape of schedule/habits/tasks)
reuses the generic `/api/objects/*` pipeline and is substantially cheaper —
its file list is roughly the `8cffd25` list plus a migration CHECK-constraint
touch, minus `collab.rs`, `collab_sync.rs`, `entity/collab_docs.rs`,
`CodeEditor.tsx`, `collabDoc.ts`, and the share machinery.

### api-types (`crates/api-types/src/lib.rs`)

| Line | Code (quoted) | Change for new kind | Class |
|---|---|---|---|
| 311 | `pub enum ObjectKind { Clipboard, File, Collab }` | Add variant; `serde`/`strum` snake_case wire name auto-derives. | MECH |
| 322 | `pub enum ObjectEventType { Created, Updated, Deleted }` with doc *"Only collab docs can be updated… encrypted objects are immutable"* | No structural change, but the doc and every server CHECK constraint encoding this policy must be revisited if the new kind mutates. | SPEC (policy) |
| 299 | `pub struct ClipboardMeta { mime_type, size }` | Add per-kind encrypted metadata struct (e.g. `ScheduleEventMeta`). | SPEC |
| 837 | `pub struct FileMeta { … }` | Same. | SPEC |
| 527–571 | `RenameCollabDocRequest`, `CreateCollabDocResponse`, `CollabDocMeta`, `CollabDocListResponse`, `ShareMeta` | Only if the new kind is server-visible. | SPEC |
| 357 | `pub object_type: ObjectKind` in `ObjectEnvelopeBodyV1` | None — flows through envelope/AAD automatically. | MECH (zero) |
| 408 | `pub kind: ObjectKind` in `ObjectInitRequest` | None. | MECH (zero) |
| 486 | `pub kind: ObjectKind` in `ObjectListItem` | None. | MECH (zero) |
| 623 | `object_kind: ObjectKind` in `WsServerMessage::Event` | None. | MECH (zero) |

### app-types (`crates/app-types/src/lib.rs`) — the UniFFI-derivation layer

| Line | Code | Change | Class |
|---|---|---|---|
| 17, 29, 49 | `DecryptedClipboardItem`, `DecryptedFileItem`, `CollabItem` — each `#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]` | Add one display record per kind. Fields are kind-shaped (`text` vs `filename` vs `title`/`share_url`). | SPEC (but tiny) |
| 96–98 | `pub clipboard_items: Vec<DecryptedClipboardItem>, pub files: Vec<DecryptedFileItem>, pub collab_docs: Vec<CollabItem>` inside `AppState` (itself a `uniffi::Record`) | Add one `Vec` field. | MECH |
| 137 | `pub struct ClipboardPayload { mime_type, bytes, text }` | Add on-demand payload struct if the kind has lazy payloads. | SPEC |

### core (`crates/core`) — crypto

`crypto.rs:218` carries `object_type: ObjectKind` inside `ObjectAadV1`;
`models.rs:7` is `pub use clipper_api_types::*;`. **Zero per-kind changes.**
This is the best-generalized layer already.

### client engine (`crates/client/src/engine.rs`) — the densest Rust layer

| Line | Code | Change | Class |
|---|---|---|---|
| 543–711 | `pub async fn send_clipboard_payload(...)` — builds `ObjectKind::Clipboard` + `ClipboardMeta` envelope, dedups, calls `persist_local_clipboard_present_encrypted` | New `send_<kind>` creator; MIME/dup rules are domain logic. | SPEC |
| 880–994 | `pub async fn upload_file_bytes(...)` — `ObjectKind::File` + `FileMeta` | New creator (or generalize). | SPEC |
| 996–1031 | `download_file_bytes` checks `file_item.kind != ObjectKind::File` | New downloader; kind check is mechanical, dialog/stream logic isn't. | MECH + SPEC |
| 1068–1085 | `delete_file` → `apply_local_delete(ObjectKind::File, …)` | New delete wrapper. | MECH |
| 1097–1165 | `create_collab_doc`, `rename_collab_doc`, `delete_collab_doc` (client-side seq allocation) | Only for server-visible kinds. | SPEC |
| 1181–1189 | `publish_visible_state` assigns `state.clipboard_items / files / collab_docs` | Add one assignment. | MECH |
| 1191–1221 | `start_reconciliation` spawns `snapshot_files`, `snapshot_clipboard`, `snapshot_collab_docs` | Spawn one more snapshot task. | MECH |
| 1248–1275 | `handle_ws_text` matches `ObjectEventType::Updated if object_kind == ObjectKind::Collab` and `Deleted if kind == File \|\| Collab` | Decide the new kind's event semantics. | SPEC (policy) |
| 1292–1443 | `snapshot_files` / `snapshot_clipboard` / `snapshot_collab_docs` — `list_objects(Some(kind), …)` + `sweep_kind(kind, …)` (collab uses dedicated `list_collab_docs()`) | Add `snapshot_<kind>`; the encrypted two are near-identical clones. | MECH |
| 1456–1514 | `persist_file_snapshot_item`, `persist_clipboard_snapshot_item`, `persist_collab_snapshot_item` | Add persist helper. | MECH |
| 1705–1726 | `materialize_object` match: `Clipboard => decrypt_clipboard_object_item_with_api…`, `File => decrypt_file_object_item…`, `Collab => {}` | Add a materialization arm. | SPEC (decryption differs) |

### client api_client (`crates/client/src/api_client.rs`)

| Line | Code | Change | Class |
|---|---|---|---|
| 956–1058 | `encrypt_clipboard_meta` / `decrypt_clipboard_meta` / `encrypt_clipboard_payload` / `decrypt_clipboard_payload` / `encrypt_file_meta_bytes` / … `_blob_bytes` — all thin AEAD wrappers around `K` + `ObjectAadV1` | Add meta/payload encrypt+decrypt helpers per encrypted kind. The bodies are clones; only the meta type differs. | MECH (near-clone) |
| 686–774 | `create_collab_doc`, `get_collab_doc_meta`, `list_collab_docs`, `rename_collab_doc`, `delete_collab_doc` — JSON (not postcard) endpoints | Only for server-visible kinds. | SPEC |
| 577–611 | `list_objects(kind: Option<ObjectKind>, …)` | None — already generic. | MECH (zero) |

### client local_store (`crates/client/src/local_store.rs`) — second densest

| Line | Code | Change | Class |
|---|---|---|---|
| 52–58 | `enum LocalObjectData { Clipboard(LocalClipboardRecord), File(LocalFileRecord), Collab(LocalCollabRecord) }` (`#[serde(tag = "kind", content = "record")]`) | Add variant + record struct. | MECH |
| 60–80 | `LocalClipboardRecord { text, mime_type, payload_size }`, `LocalFileRecord { filename, … }`, `LocalCollabRecord { title, share_token, … }` | Record fields are kind-shaped. | SPEC (tiny) |
| 121–126 | `enum StoredPresentContent { Encrypted(EncryptedObject), Collab(StoredCollabRecord) }` | Binary choice per new kind: encrypted blob or plaintext. | SPEC (one word) |
| 180–184 | `struct LocalVisibleState { clipboard_items, files, collab_docs }` | Add field. | MECH |
| 234–695 | Six persist fns: `persist_local_{clipboard,file,collab}_present*` + `_inner` variants | Two near-clone persist helpers per kind. | MECH |
| 814–838 | `recent_clipboard_items_inner`, `file_items_inner`, `collab_items_inner` | Add one filter fn. | MECH |
| 852–872 | `decrypt_stored_object_record_preview` match over `ObjectKind::{Clipboard, File, Collab}` | Add preview arm; the decrypt is kind-shaped. | SPEC |
| 1173–1185, 1391–1406 | `remove_payloads_for_object` special-cases `ObjectKind::Clipboard` (sidecar payload layout) | Extend if the new kind has sidecar payloads. | SPEC |
| 1544–1627 | `decrypt_file_record`, `clipboard_item_from_record`, `file_item_from_record`, `collab_item_from_record` | Add `<kind>_item_from_record`. | MECH |

### server routes — generic objects (`crates/server/src/routes/objects.rs`)

| Line | Code | Change | Class |
|---|---|---|---|
| 188–202 | `let expires_at = match req.kind { ObjectKind::Clipboard => …, ObjectKind::File \| ObjectKind::Collab => None }` | Add TTL policy arm. | SPEC (policy, one line) |
| 331–335, 790–796 | `broadcast_created(…)`; `if kind == ObjectKind::Clipboard { spawn_clipboard_trim(…) }` | Post-create side effects (retention trim) per kind. | SPEC |
| 903–955 | `list_objects` parses `kind` query; applies `retained_clipboard_object_ids` only for Clipboard | Retention filter per kind. | SPEC |
| 1091–1163 | `retained_clipboard_object_ids_raw`, `ensure_object_read_retained` (Clipboard-only) | Analogous retention if needed. | SPEC |
| 1438–1448 | `if kind != ObjectKind::File { return Err(ObjectDeleteUnsupported) }` | Delete policy per kind. | SPEC (policy) |
| 1539 | `object_kind: Set("file".into())` in the delete `event_log` row | **Hardcoded literal** — already a latent bug/assumption; must become `kind.to_string()` before a second deletable kind exists. | MECH (fix first) |
| 250, 306, 774, 1270, 1317 | `kind: Set(kind.to_string())`, `insert_created_event(…, kind, …)`, list/download paths | None — generic. | MECH (zero) |

### server routes — collab (`crates/server/src/routes/collab.rs`, ~7 endpoints)

`create_collab_doc` (62–183), `get_collab_doc_meta` (188–223),
`rename_collab_doc` (232–321, emits the only `ObjectEventType::Updated` in the
system), `list_collab_docs` (329–372), `delete_collab_doc` (378–461),
`collab_ws_handler`/`authorize_collab_ws` (558–628, Y-sync), `get_share_meta`
(639–674, public link). **All SPEC** — this whole file only exists because
collab is server-visible. An encrypted kind skips this file entirely.

### server support files

| File:line | Code | Change | Class |
|---|---|---|---|
| `migration/m20260312_000001_create_tables.rs:179` | `.check(Expr::col(Objects::Kind).is_in(["clipboard", "file"]))` | Extend (superseded by m20260615's `CHECK (kind IN ('clipboard','file','collab'))` at line 75; a new kind needs a new migration widening it). | MECH |
| `migration/m20260615_000002_collab_docs.rs:203,206` | `CHECK (object_kind IN ('clipboard','file','collab'))`, `(event_type='deleted' AND object_kind IN ('file','collab'))` | Widen; delete eligibility is a policy choice. | MECH + SPEC |
| `migration/m20260826_000003_collab_doc_title.rs:44–48` | `event_type IN ('created','updated','deleted')`, `updated` restricted to `object_kind='collab'` | Widen if the new kind mutates. | SPEC (policy) |
| `entity/collab_docs.rs` + `entity/mod.rs` | Dedicated SeaORM table | Only for server-visible kinds; then `nix run .#server-entities`. | SPEC |
| `cleanup.rs:65–152` | `cleanup_expired_clipboard_objects`, `cleanup_excess_clipboard_objects`, `trim_user_clipboard` | Clone per kind with TTL/cap retention. | MECH (near-clone) |
| `state.rs:54,383–409` | collab room map, `acquire_collab_room`/`release_collab_room` | Only for live-sync kinds. | SPEC |
| `ws.rs:88–97,418–441` | `WsBroadcast { object_kind: ObjectKind, … }`, `get_latest_seq` | None — generic (one test fixture uses `ObjectKind::File`). | MECH (zero) |

### daemon-types protocol (`crates/daemon-types/src/protocol.rs`)

| Line | Code | Change | Class |
|---|---|---|---|
| 66–86 | `enum DaemonCommand { … SendClipboard(SendClipboardParams), SendClipboardPayload(…), CopyToLocal(…), ClipboardPayload(…), UploadFile(…), DownloadFile(…), DeleteFile(…), CreateCollabDoc, DeleteCollabDoc(…), RenameCollabDoc(…), GetCollabDocMeta(…) }` | Add 1–4 variants per kind. | MECH |
| 145–201 | Per-variant params structs (`SendClipboardParams { text }`, `UploadFileParams { file_path }`, `RenameCollabDocParams { object_id, title }`, …) | Add params/result structs. | MECH |
| 358 | test fixture JSON containing `clipboard_items`, `files`, `collab_docs` | Update fixture. | MECH |

### daemon handler (`crates/daemon/src/handler.rs`)

| Line | Code | Change | Class |
|---|---|---|---|
| 392–434 | `match command { DaemonCommand::SendClipboard(..) => …, CreateCollabDoc => cmd_create_collab_doc(..), … }` | Add dispatch arm(s). | MECH |
| 578–729 | `cmd_send_clipboard`, `cmd_upload_file`, `cmd_download_file`, `cmd_delete_file`, `cmd_create_collab_doc`, `cmd_delete_collab_doc`, `cmd_rename_collab_doc`, `cmd_get_collab_doc_meta` | Add thin handler(s) calling the engine. | MECH |

### web-wasm (`crates/web-wasm/src/lib.rs`)

282–389: one `#[wasm_bindgen(js_name = …)] pub fn … -> Promise` per operation —
`sendClipboardText`, `sendClipboardPayload`, `clipboardPayload`,
`uploadFileBytes`, `downloadFileBytes`, `deleteFile`, `createCollabDoc`,
`deleteCollabDoc`, `renameCollabDoc`, `getCollabDocMeta`. Each body is a
3–8 line `ok_promise(async { engine().<op>().await … serde_wasm_bindgen::to_value })`.
**All MECH.** 4–6 exports per kind.

### mobile-uniffi (`crates/mobile-uniffi/src/lib.rs`)

157–228: the same ten operations as `#[uniffi::export]` methods on
`MobileClipperClient`. **All MECH** thin wrappers. Then regenerate the bridge
(`nix run .#mobile-uniffi-android`).

### web/src-tauri (`web/src-tauri/src/lib.rs`)

| Line | Code | Change | Class |
|---|---|---|---|
| 69–138 | `generate_handler!([…, send_clipboard_text, upload_file_bytes, create_collab_doc, …])` | Register new commands. | MECH |
| 237–499 | 12 `#[tauri::command]` fns, each forwarding one `DaemonCommand` variant | Add per-kind commands. | MECH |

### packages/shared (`packages/shared/src/types.ts`, `index.ts`)

| Line | Code | Change | Class |
|---|---|---|---|
| 3, 12, 21 | `ClipboardItem`, `FileItem`, `CollabItem` record types | Add `*Item` type. | MECH |
| 62–64 | `clipboard_items: ClipboardItem[]; files: FileItem[]; collab_docs: CollabItem[]` in `AppState` | Add list field. | MECH |
| 101–114 | `ClipperBackend` methods: `sendClipboardText`, `sendClipboardPayload`, `clipboardPayload`, `uploadFileBytes`, `downloadFileBytes`, `deleteFile`, `createCollabDoc`, `deleteCollabDoc`, `renameCollabDoc`, `getCollabDocMeta`, plus optional native-only hooks (`sendCurrentClipboardText?`, `uploadFileFromDialog?`, …) | Add 3–6 method signatures. | MECH |

### web/src (shared React UI)

| File:line | Code | Change | Class |
|---|---|---|---|
| `backend/tauri.ts:30–51` | One `invoke("<snake_case_cmd>", …)` per `ClipperBackend` method | Add mappings. | MECH |
| `backend/index.ts:38–42,178` | `readClipboardText`, `writeClipboardText`, `resolveServerUrl` | Platform helpers are kind-shaped. | SPEC |
| `App.tsx:399–453` | Nav buttons + `<Route>`s for `/`, `/files`, `/collab`, `/collab/:id` | Add nav + routes. | MECH |
| `App.tsx:465,581,823,933` | `ClipboardPanel`, `FilesPanel`, `CollabPanel`, `CollabDocView` | Implement list/detail panels. | SPEC (real UI) |
| `App.tsx:1115,1193,1423,1442` | `TitleField`, `SharePage`, `collabTitle`, `shareLink` | Collab/share-specific. | SPEC |
| `CodeEditor.tsx` (whole file), `languages.ts:84` | Y-sync provider lifecycle, `DEFAULT_COLLAB_LANGUAGE_ID` | Only for live-collab kinds. | SPEC |

### mobile/src

| File:line | Code | Change | Class |
|---|---|---|---|
| `App.tsx:54` | `type TabName = "clipboard" \| "files" \| "devices" \| "collab"` | Add tab literal. | MECH |
| `App.tsx:348–388` | `<Tabs.Tab>`/`<Tabs.Content>` per kind | Add tab + content. | MECH |
| `App.tsx:399–557,719–1080` | `ClipboardPanel`, `FilesPanel`, `CollabPanel`, `CollabDocReader`, `RenameDocDialog`, status label/color maps | Implement panels/dialogs. | SPEC (real UI) |
| `backend.ts:35–65` | `readClipboardText`, `writeClipboardText`, `pickUploadFile`, `shareDownloadedFile` | Platform helpers. | SPEC |
| `collabDoc.ts` (whole) | `subscribeToCollabDoc`, `collabWsUrl`, `CollabDocStatus` | Only for live-sync kinds. | SPEC |

### packages/mobile-bridge + generated bindings

`adapter.ts`: ~19 hand-written mappings (`mapCollabItem`, `mapClipboardItem`,
`mapFileItem`, `mapClipboardPayload`, state field mapping
`state.clipboardItems.map(mapClipboardItem)`, one forwarding method per
`ClipperBackend` op). **MECH.**

Generated (do not hand-edit; regenerated by `nix run .#mobile-uniffi-android`
and the wasm-pack wrappers): `generated/clipper_app_types.ts` (641 lines; one
record + factory + `FfiConverterType*` + sequence converter per kind),
`generated/clipper_mobile_uniffi.ts` (1139 lines; 10 kind-scoped methods +
checksums), `generated/clipper_mobile_uniffi-ffi.ts`, and
`web/src/generated/wasm/clipper_web_wasm.{d.ts,js}` (10 kind-scoped exports
each). Cost is zero *if* the codegen stays healthy; each new `uniffi::Record`
and `#[uniffi::export]` is ~60–100 generated lines.

### Reconciliation of methods (a) and (b)

The two methods agree. Every file in the consolidated collab commit list is
also a grep hit for per-kind structure, and vice versa, with three caveats:
- grep misses *new* files a kind introduces (`collab.rs`, `collab_sync.rs`,
  `entity/collab_docs.rs`, migrations, `CodeEditor.tsx`, `collabDoc.ts`) —
  method (b) catches these.
- method (b)'s diffs contain one-time collateral that is not per-kind cost
  (lockfiles, `scripts/server-entities.ts` UUID rewrite, the UniFFI patch,
  `metro.config.js`, docs) — excluded from the inventory above.
- grep overcounts adapter layers slightly (imports), which the table marks as
  zero-change or trivial.

---

## PART 2 — COST SUMMARY

Approximate per-kind touchpoints (hand-written sites; generated code excluded):

| Layer | Files | Sites | MECH | SPEC |
|---|---|---|---|---|
| api-types | 1 | ~10 | 8 (5 zero-change) | 2–5 |
| app-types | 1 | ~5 | 1 | 4 (all tiny) |
| core | 0 | 0 | — | — |
| client engine | 1 | ~25 | 14 | 11 |
| client api_client | 1 | ~8 | 2 (near-clones) | 6 |
| client local_store | 1 | ~15 | 9 | 6 |
| server routes/objects | 1 | ~12 | 5 | 7 |
| server routes/collab (+sync) | 2 | ~7 | 0 | 7 |
| migrations/entities | 3–5 | ~10 | 6 | 4 |
| server cleanup/state | 2 | ~5 | 3 (near-clones) | 2 |
| daemon-types + daemon | 2 | ~8 | 8 | 0 |
| web-wasm | 1 | 4–6 | all | 0 |
| mobile-uniffi | 1 | 4–6 | all | 0 |
| web/src-tauri | 1 | 5–7 | all | 0 |
| packages/shared | 2 | ~22 | 21 | 1 |
| web/src | 6 | ~40 | ~20 | ~20 |
| mobile/src | 3 | ~38 | ~15 | ~23 |
| mobile-bridge (hand-written) | 1 | ~19 | all | 0 |
| **Total** | ~30 files | **~225 sites** | **~135 (60%)** | **~90 (40%)** |

The four layers that dominate:

1. **The UI pair (`web/src` + `mobile/src`), ~78 sites.** But this is mostly
   *real product work* (panels, viewers, dialogs) that no refactoring removes —
   it is the point of adding a module. Only the nav/route/tab wiring (~35
   sites) is boilerplate.
2. **The adapter triple (`web-wasm`, `mobile-uniffi`, `web/src-tauri`) plus
   `daemon-types`/`daemon`/`packages/shared`/`mobile-bridge`, ~60 sites —
   100% boilerplate.** Six separate hand-written forwarding layers, each
   re-declaring the same 4–6 operations per kind in a slightly different FFI
   dialect. This is the purest waste in the current design.
3. **client engine + local_store, ~40 sites.** Roughly half mechanical
   (snapshot clones, persist clones, `LocalVisibleState` fields, match arms)
   and half genuinely per-kind (metadata crypto shapes, preview decryption,
   retention sidecars).
4. **server, ~25 sites.** Mixed: retention/TTL/delete policy is per-kind by
   nature, but the CHECK-constraint widening, the hardcoded `"file"` delete
   event, and the clipboard trim cloning are mechanical.

Pure boilerplate, plainly: the six forwarding layers (daemon IPC variant +
params struct + handler arm + tauri command + wasm export + uniffi export +
shared TS method + bridge mapping ≈ **8 declarations of the same operation**),
the `AppState`/`LocalVisibleState`/`AppState`-in-TS triple field addition, and
the snapshot/persist clone pairs in `engine.rs`/`local_store.rs`.

---

## PART 3 — GENERALIZATION DESIGN

Goal: adding an encrypted module = one `ObjectKind` variant + one metadata
struct + one display record + domain logic + UI. Everything else derives.

### 3.1 A kind registry behind a trait

Define the per-kind contract once, in `crates/client` (the only layer with real
per-kind logic below the UI):

```rust
// crates/client/src/kind.rs
use async_trait::async_trait;

/// Static description of one object kind. Implemented once per module.
pub trait KindModule: Send + Sync + 'static {
    const KIND: ObjectKind;
    /// The encrypted metadata payload type (JSON inside the meta ciphertext).
    type Meta: Serialize + DeserializeOwned + Send + Sync;
    /// The app-types display record for this kind.
    type Display;

    /// Encrypted (default) or server-visible (collab-style).
    const SERVER_VISIBLE: bool = false;
    /// Retention policy enforced by server trim + client sweep.
    const RETENTION: Option<RetentionPolicy> = None;
    /// Whether this kind admits `updated` events (Part 4 decides what
    /// "update" means for encrypted kinds).
    const MUTABLE: bool = false;

    /// Decrypt a stored encrypted object into its display record.
    fn display_from_encrypted(
        obj: &EncryptedObject,
        key: &DataKey,
    ) -> Result<Self::Display, ClientError>;

    /// Build the metadata plaintext for a new object.
    fn build_meta(input: &Self::NewInput) -> Self::Meta;
}

pub struct RetentionPolicy {
    pub max_items_per_user: Option<usize>, // clipboard: RECENT_CLIPBOARD_LIMIT
    pub ttl: Option<Duration>,             // clipboard: expires_at arm
}
```

The engine then holds one registry instead of N match arms:

```rust
// Engine-side dynamic dispatch over the registry. Boxed futures keep this
// object-safe without enum_dispatch.
pub trait KindOps: Send + Sync {
    fn kind(&self) -> ObjectKind;
    fn retention(&self) -> Option<RetentionPolicy>;
    fn decrypt_preview(
        &self,
        record: &StoredPresentObjectRecord,
        key: &DataKey,
    ) -> Result<DisplayObject, ClientError>;
    fn snapshot_source(&self) -> SnapshotSource; // ObjectsApi(kind) | DedicatedEndpoint
}

pub struct KindRegistry {
    ops: HashMap<ObjectKind, Arc<dyn KindOps>>,
}
```

Every `match kind { Clipboard => …, File => …, Collab => {} }` in
`engine.rs:1705`, `local_store.rs:852`, `objects.rs:188` becomes a registry
lookup. **Trade-off to state honestly:** those match sites are currently the
compile-time checklist that forces you to handle a new kind everywhere; a
`dyn` registry replaces compile errors with a runtime "kind not registered"
error. Mitigate with a registry-completeness test (`for kind in
ObjectKind::iter() { assert!(registry.contains(kind)) }` — `strum`'s
`EnumIter` is already a dependency).

### 3.2 Collapse the local store's per-kind triple

Today: `LocalObjectData` (3 variants) + `StoredPresentContent` (2 variants) +
`LocalVisibleState` (3 Vec fields). After:

```rust
// crates/client/src/local_store.rs
#[derive(Serialize, Deserialize)]
#[serde(tag = "content_kind", content = "data", rename_all = "snake_case")]
enum StoredPresentContent {
    /// All E2E kinds — meta ciphertext + payload descriptors. Kind lives on
    /// the parent record (`StoredPresentObjectRecord.kind`) already.
    Encrypted(EncryptedObject),
    /// Server-visible kinds — plaintext metadata as canonical JSON.
    Plaintext { meta_json: String },
}
```

The six `persist_*_present*` clones become one
`persist_present(kind, content, …)`; the `<kind>_items_inner` filters become
`items_inner(kind)`. `LocalVisibleState` becomes:

```rust
pub struct LocalVisibleState {
    /// Display-ready records across all kinds, kind-tagged.
    pub objects: Vec<DisplayObject>,
}

/// Kind-agnostic display record produced by the registry.
pub struct DisplayObject {
    pub id: String,
    pub kind: ObjectKind,
    pub created_at: String,
    pub source_device_id: String,
    /// Kind-specific display fields, already decrypted. See 3.3 for why this
    /// is a typed enum at the app-types boundary, not JSON.
    pub view: DisplayView,
}
```

This deletes ~15 of the local_store touchpoints and keeps the invariant the
current enum exists for (a collab record can never carry ciphertext fields —
that invariant moves from "two enum variants per family" to "two enum variants
total", which is *stronger*, not weaker).

### 3.3 Surviving UniFFI — the hardest constraint

`crates/app-types` derives `uniffi::Record` directly on `AppState` and the
item structs, and UniFFI has no generics: `AppState { objects: Vec<DisplayObject<T>> }`
cannot cross the FFI. Options, with a recommendation:

**Option 1 — tagged enum payload (recommended).** UniFFI *does* support
non-generic enums with named-field variants (`uniffi::Enum`), and serde
supports the matching externally/internally tagged shape. Keep one generic
container + one closed enum of kind views:

```rust
// crates/app-types/src/lib.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DisplayObject {
    pub id: String,
    pub created_at: String,
    pub source_device_id: String,
    pub view: DisplayView,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum DisplayView {
    Clipboard { text: String, mime_type: String, payload_size: i64 },
    File { filename: String, mime_type: String, blob_size: i64 },
    Collab { title: String, share_token: String, share_url: Option<String>, updated_at: String },
    // module #2 adds one variant here
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AppState {
    #[serde(default)] pub session: Option<AuthenticatedSession>,
    #[serde(default)] pub saved_profile: Option<SavedProfile>,
    pub connection_status: ConnectionStatus,
    pub objects: Vec<DisplayObject>,   // replaces the three Vec fields
    pub error: Option<String>,
}
```

Cost per new kind at this boundary: **one enum variant** in `DisplayView`
(~5 lines) — instead of a new record + `AppState` field + `LocalVisibleState`
field + TS `AppState` field + bridge mapping. The TS side gets a proper
discriminated union (`view.kind`), which is *better* for module UIs than
three parallel arrays. Caveat: verify `uniffi-bindgen-react-native` generates
tagged enums cleanly (the repo already patches that codegen —
`patches/uniffi-bindgen-react-native@0.31.0-5.patch` — so this is the first
thing to prototype).

**Option 2 — macro-generated parallel fields (fallback).** If UniFFI enum
payloads misbehave, keep today's shape but write it once:

```rust
define_app_state! {
    clipboard_items: DecryptedClipboardItem => ClipboardView,
    files:           DecryptedFileItem      => FileView,
    collab_docs:     CollabItem             => CollabView,
}
```

expanding to `AppState`, `LocalVisibleState`, and `publish_visible_state` in
one go. Adding a kind = one macro line + one record. This is the low-risk
fallback; Option 1 is the better end state.

### 3.4 Generic daemon IPC instead of one variant per operation

Today each operation is declared ~8 times (daemon variant, params struct,
handler arm, tauri command, wasm export, uniffi export, shared TS method,
bridge mapping). Replace the per-kind CRUD with kind-generic commands:

```rust
// crates/daemon-types/src/protocol.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", content = "params", rename_all = "snake_case")]
pub enum DaemonCommand {
    // … auth/session/device commands unchanged …

    /// Create an encrypted object. `meta` is the kind's metadata as JSON;
    /// the engine's registry validates/encrypts it for `kind`.
    CreateObject(CreateObjectParams),
    DeleteObject(DeleteObjectParams),
    /// On-demand payload bytes (clipboard text, file blob, …).
    ObjectPayload(ObjectPayloadParams),
    /// Only for MUTABLE kinds; see Part 4.
    UpdateObject(UpdateObjectParams),
    /// Server-visible kinds only.
    ObjectShareMeta(ObjectShareMetaParams),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateObjectParams {
    pub kind: ObjectKind,
    pub meta_json: String,          // serialized <Kind as KindModule>::Meta
    pub payloads: Vec<PayloadBytes>,
}
```

Kind-shaped *platform* operations stay per-kind because they genuinely are:
`SendCurrentClipboard` (reads the OS clipboard), `UploadFileFromDialog`,
`DownloadFileToDialog` — these are shell capabilities, not sync operations.
That leaves ~4 generic commands + a small number of platform helpers per
shell, versus today's 10 per-kind commands.

### 3.5 The wasm-bindgen boundary (`crates/web-wasm`)

wasm-bindgen has the same no-generics limit, but the *state* path is already
generic: `getState`/`waitForStateChange` return `serde_wasm_bindgen` JsValues,
so the `AppState` reshape in 3.3 flows to TS with zero new exports. Only
commands are per-kind exports today. Mirror 3.4:

```rust
#[wasm_bindgen(js_name = createObject)]
pub fn create_object(kind: String, meta_json: String, payloads: …) -> Promise { … }

#[wasm_bindgen(js_name = deleteObject)]
pub fn delete_object(object_id: String) -> Promise { … }

#[wasm_bindgen(js_name = objectPayload)]
pub fn object_payload(object_id: String) -> Promise { … }
```

`kind` crosses as a string and is parsed via `ObjectKind::from_str` (strum's
`EnumString` is already derived). New module = zero new wasm exports.

### 3.6 Server side

The generic object pipeline (`objects.rs`) already handles any encrypted kind;
the per-kind residue is policy, which belongs in one table:

```rust
// crates/server/src/kind_policy.rs (or a config table)
pub struct KindPolicy {
    pub kind: ObjectKind,
    pub ttl: Option<Duration>,
    pub per_user_cap: Option<usize>,   // drives the trim task
    pub allow_delete: bool,
    pub allow_update: bool,            // widens the event_log CHECKs (migration)
}
```

`objects.rs:188` (TTL), `:331` (trim spawn), `:1438` (delete gate), and
`cleanup.rs` all read this. **Fix `objects.rs:1539`'s hardcoded
`Set("file".into())` first** — it silently mislabels delete events for any
second deletable kind. Migration CHECK constraints still widen per kind
(mechanical, one migration each), unless the `kind` column CHECK is dropped in
favor of application-level validation — reasonable here, since the server
never interprets the kind beyond policy lookup.

### What remains per-kind after all this (by design)

The metadata struct, the display view variant, the decrypt/preview logic
(inside `KindModule`), retention policy values, and both UIs. That is exactly
the "domain logic plus UI" target.

---

## PART 4 — TWO VARIANTS: mutable vs. immutable encrypted objects

### (A) Encrypted objects gain mutation (`revision: u64`)

Changes to the design above:

- **Envelope/AAD**: `ObjectEnvelopeBodyV1` gains `revision: u64` (garde
  `range(min = 0)`), and `ObjectAadV1` gains the same field so a server cannot
  replay an old revision's ciphertext under the same identity — the AAD
  projection in `docs/object-envelopes.md` binds exactly the identity fields,
  and revision is now one. Practically this is `ObjectEnvelopeBodyV2` /
  `object_version = 2` (postcard is positional; `Canon(body)` changes shape),
  and `validate_object_init_envelope` / `verify_object_list_item_envelope`
  accept v2. Repo policy ("do not preserve legacy schema… unless asked")
  permits just cutting over.
- **Server**: `PUT /api/objects/{id}` validating a fresh envelope with
  `revision == current + 1`, updating `meta_ciphertext`/payloads and emitting
  an `updated` event — which requires widening the
  `event_log` CHECK (`m20260826`'s `updated … object_kind = 'collab'`) to the
  mutable encrypted kinds, per `KindPolicy.allow_update`.
- **Client**: `StoredPresentObjectRecord` gains `highest_revision: u64`; live
  `updated` events and snapshots reject `revision <= highest_revision`. The
  registry's `MUTABLE` const gates `UpdateObject`. `handle_ws_text`'s
  `Updated if kind == Collab` arm generalizes to "any kind with `MUTABLE`".
- **Local store**: a new `PendingUpdate` marker alongside
  `PendingCreate`/`Deleted`, so an offline client doesn't resurrect stale
  ciphertext.

Registry impact: **small**. `KindModule` gains `MUTABLE` and an optional
`merge_meta`/`build_update`; the generic `UpdateObject` command and
`updateObject` exports already exist from 3.4/3.5. Mutable kinds are the
*same* machinery with one more envelope field and one more event arm.

### (B) Encrypted objects stay immutable (create-new-then-tombstone)

- **Envelope/AAD**: unchanged. Mutation lives inside the encrypted metadata:
  `Meta` gains `logical_id: Uuid` and `supersedes: Option<Uuid>` (invisible to
  the server).
- **Server**: unchanged, except delete must become available for the kind
  (today gated to `File` at `objects.rs:1438`) so tombstones can be issued —
  a `KindPolicy.allow_delete` flag, plus the `deleted` event CHECK widening.
  **Retention interaction**: clipboard-style trim must not garbage-collect the
  newest member of a logical chain while keeping an older one; with
  server-blind chains the server can't see lineage, so retention for
  chain-using kinds must be disabled or made conservative. This is the hidden
  cost of (B).
- **Client**: the registry gains chain resolution —
  `KindModule::fold(records) -> Vec<DisplayObject>` collapses each
  `logical_id` chain to its head and treats tombstones as deletions. Local
  store keeps all revisions until their delete events land.
- **IPC/IPC exports**: no `UpdateObject` at all; the UI's "edit" is
  `CreateObject` + `DeleteObject` composed in the engine.

Registry impact: also small, but the complexity moves into *client-side chain
folding* and *retention policy edge cases*, and every new module re-answers
"how do I edit?" individually. **(A) generalizes better**: one revision
counter benefits every future mutable module; (B) leaves each module to invent
its own chain semantics inside encrypted JSON. If any of schedule/habits/tasks
needs edits (a calendar certainly does), (A) is the better foundation; if the
owner values the current hard immutability invariant (server can never be
tricked into serving stale ciphertext as current), (B) keeps it at the price
of per-module chain logic.

---

## PART 5 — RISKS: what fights this refactor

1. **The exhaustive `match` arms are load-bearing documentation.**
   `engine.rs:1705`, `local_store.rs:852`, `objects.rs:188`, `objects.rs:1438`
   compile-error on a new variant today; a `dyn KindOps` registry turns those
   into runtime gaps. Without the completeness test (3.1) and per-kind
   integration tests, a half-registered kind ships.

2. **Server kind assumptions are stringly-typed and scattered.** The SQLite
   CHECK constraints hardcode kind lists in three migrations
   (`m20260312:179,306`, `m20260615:75,203,206`, `m20260826:44–48`) and
   *capability* lists (`deleted` ∈ file/collab, `updated` ∈ collab) separately
   from kind lists — a `KindPolicy` table must drive future migrations or they
   will drift from the Rust registry. `objects.rs:1539`'s literal
   `Set("file".into())` shows this drift has already started.

3. **Clipboard retention is woven through three layers.** TTL on init
   (`objects.rs:188`), trim task (`objects.rs:331`, `cleanup.rs:109–152`),
   read-path filtering (`objects.rs:1091–1163`), and a payload-sidecar special
   case (`local_store.rs:1173–1185`). Generalizing retention means touching
   all four consistently; missing one produces objects that vanish from the UI
   but not the server (or vice versa).

4. **UniFFI codegen is patched and version-pinned.**
   `patches/uniffi-bindgen-react-native@0.31.0-5.patch` exists because the
   generator needed fixing; `DisplayView` (3.3, Option 1) depends on tagged
   enums surviving that generator. Prototype this first — if it fails, fall
   back to Option 2 rather than growing the patch surface.

5. **Postcard's positional canonicalization.** `Canon(body)` for both the
   signature and the AAD means adding `revision` (Part 4A) is a hard format
   break requiring `object_version = 2` handling on both peers, not a field
   addition. The externally-tagged-enum comment at `api-types:429–434` shows
   this wire format has already bitten once.

6. **`LocalVisibleState`/`AppState` reshape ripples through six consumers at
   once**: daemon `StateChanged` events, wasm `getState`, Tauri state
   commands, UniFFI `AppState` record, `packages/shared` `AppState` type, and
   both `App.tsx` files. There is no staged rollout; the cut must land in one
   commit or every shell desyncs (the daemon IPC has a
   `protocol_version` handshake — `protocol.rs:88–99` — bump it).

7. **Server-visible vs. encrypted is a fork, not a spectrum.** Collab bypasses
   the envelope pipeline entirely (plaintext table, JSON routes, Y-sync
   socket). The registry's `SERVER_VISIBLE` flag papers over two genuinely
   different stacks; if module #2 ever needs *both* (encrypted content,
   server-queryable schedule ranges — very plausible for a calendar), neither
   existing track fits, and the design needs a third: encrypted payload +
   server-visible index columns. Worth deciding before the schedule module,
   because it determines whether `CollabItem`'s track or the envelope track is
   the template.

8. **The frontend's parallel-track structure.** There is no `ObjectKind` in
   TypeScript; each kind is a separate `*Item` type, backend method set, and
   panel tree in two `App.tsx` files. The registry reduces Rust boilerplate
   but the UI will still grow linearly per module — accept this (it is product
   surface) but adopt the `DisplayView` discriminated union so list rendering,
   empty states, and error handling can be shared components keyed on
   `view.kind`.

9. **`objects.rs` test fixtures and `daemon-types` test JSON**
   (`protocol.rs:358`) snapshot the current per-kind shapes; a reshape breaks
   them noisily — fine, but budget for it.
