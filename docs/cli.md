# Local command-line client

1. Open Clipper to start the daemon.
2. Sign in through Clipper.
3. Run commands from the repository:

```sh
nix run .#clipper -- schedule items
cargo run -p clipper-cli -- schedule items
```

The binary is named `clipper`. The Cargo package is `clipper-cli`.
Each invocation authenticates over the local Unix socket, sends one command,
writes one JSON value to stdout and exits. Errors go to stderr with a nonzero
exit status. It does not start the daemon or sign in.

## Commands

```sh
clipper devices
clipper schedule items
clipper schedule occurrences --from 2026-10-12 --to 2026-10-19 --zone Europe/Berlin
clipper schedule add < item.json
clipper schedule update <object-id> --revision <n> < replacement.json
clipper schedule delete <object-id>
clipper actuals --from 2026-10-12 --to 2026-10-19
clipper schedule add --help
```

- `devices` returns an array of `{id, name, platform, is_current}` records from
  the signed-in account. `is_current` identifies the daemon's device.
- `items` returns an array of `{object_id, revision, item}` records.
- `item` is the complete `ScheduleItem` JSON definition.
- `add` accepts one item on stdin. An omitted `id` gets a new UUID.
- `add --help` shows a complete weekly item with an alarm, generated from the
  Rust schedule types.
- `update` accepts one complete replacement on stdin. Keep its `item.id` from
  `items`; the storage `object_id` is a different ID.
- `add` and `update` return `{object_id}`.
- Run `schedule items` for current revisions before an update.
- A revision conflict fails. Read `items` again and review the new definition
  before saving another replacement.
- `delete` returns `null` on success.
- `occurrences` and `actuals` return the daemon's result arrays.

An alarm can include `target_device` with an ID from `devices`. Omit it to ring
on all Android phones. A target can be a phone or a Mac; only that device rings.
A Mac uses a notification with sound while Clipper is running, including when
its window is closed.

## Dates and zones

- `--from` is inclusive. `--to` is exclusive and must be later than `--from`.
- `YYYY-MM-DD` means local midnight.
- `YYYY-MM-DDTHH:MM[:SS]` means a local datetime. A space can replace `T`.
- RFC 3339 datetimes with an offset or `Z` name exact instants.
- Occurrences use `--zone`, or the system IANA zone when it is omitted.
- Actuals interpret local dates and datetimes in the system IANA zone.
- Local datetime resolution follows the schedule domain's daylight-saving
  rules: choose the earlier instant in an overlap and shift forward by the
  offset change in a gap.

## Authentication

The CLI and desktop shell share the socket path lookup, handshake, line
transport and cached secret reader. `CLIPPER_DAEMON_SOCKET_PATH` overrides the
socket location. On macOS the secret comes from the daemon's existing keychain
item. On Linux it comes from `ipc-secret-v1` in the Clipper data directory.
The CLI only reads the secret and caches it for its process lifetime.

A protocol version mismatch means the daemon is stale. Restart the daemon
before retrying. A timed-out write can have completed; check Clipper before
repeating it.
