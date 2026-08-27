// Read-only live view of a collab doc, over the same Y-sync WebSocket the web
// editor uses.
//
// The web client gets this from `y-websocket`, which we deliberately do not use
// here: it drags in `lib0/broadcastchannel` and `lib0/environment` (cross-tab
// coordination and `process`/`location` sniffing) for a browser it is not
// running in. Read-only sync is a dozen lines of the protocol, so this speaks it
// directly against React Native's global WebSocket.
//
// Wire format, mirroring `crates/server/src/collab_sync.rs`:
//
//   [varuint msgType, ...]
//     msgType 0 = sync:      [varuint syncType, varUint8Array payload]
//       syncType 0 = step1 (a state vector; carries no content)
//       syncType 1 = step2, 2 = update (both carry content)
//     msgType 1 = awareness  (cursors; ignored — this view has none to share)
//
// The server sends a sync step1 as an application-level keepalive every 15s and
// closes a connection that goes 60s without an inbound frame, so replying to
// step1 (which `readSyncMessage` does for us) is what keeps the socket alive.

import * as decoding from "lib0/decoding";
import * as encoding from "lib0/encoding";
import * as syncProtocol from "y-protocols/sync";
import * as Y from "yjs";

const MESSAGE_SYNC = 0;
const SYNC_STEP2 = 1;
const SYNC_UPDATE = 2;

// Backoff between re-dial attempts: short enough that walking back into signal
// recovers without reopening the doc, capped so a server that is down is not
// hammered for as long as the screen stays open.
const RECONNECT_DELAYS_MS = [1000, 2000, 4000, 8000, 15_000];

// How many times to retry a socket that never opened before giving up. A drop
// after a successful sync is a network blip worth chasing indefinitely; failing
// to connect at all usually means the document is gone or the share token was
// revoked, and the upgrade is refused as an HTTP 403 the client cannot read. So
// that case gets a bounded number of tries and then reports failure.
const MAX_FAILED_HANDSHAKES = 4;

export type CollabDocStatus = "connecting" | "live" | "offline" | "unavailable";

export type CollabDocHandle = {
  close: () => void;
};

export type CollabDocOptions = {
  serverUrl: string;
  objectId: string;
  // The share token is the sole credential for this endpoint.
  shareToken: string;
  onText: (text: string) => void;
  onStatus: (status: CollabDocStatus) => void;
};

// Open a live read-only subscription to a collab doc. Emits the document text on
// every change (including the initial sync) and a coarse connection status;
// `unavailable` is terminal and means the document could not be opened at all.
// Returns a handle whose `close` tears down the socket, cancels any pending
// reconnect, and destroys the local replica.
export function subscribeToCollabDoc(options: CollabDocOptions): CollabDocHandle {
  const { serverUrl, objectId, shareToken, onText, onStatus } = options;

  const doc = new Y.Doc();
  const text = doc.getText("content");
  const emit = () => onText(text.toString());
  text.observe(emit);

  const url = collabWsUrl(serverUrl, objectId, shareToken);
  let socket: WebSocket | null = null;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let closed = false;
  let failedHandshakes = 0;

  function connect() {
    if (closed) return;
    onStatus("connecting");

    const ws = new WebSocket(url);
    ws.binaryType = "arraybuffer";
    socket = ws;
    // Whether this socket ever got far enough to sync. Distinguishes "the
    // network dropped" from "this document will not open for us".
    let synced = false;

    ws.addEventListener("open", () => {
      // A socket can open after the caller has torn the subscription down (the
      // listeners outlive `close()`), so every handler re-checks.
      if (closed) return;
      // Ask for everything we do not have. Our state vector is empty on a fresh
      // doc, so the server answers with the whole document.
      const encoder = encoding.createEncoder();
      encoding.writeVarUint(encoder, MESSAGE_SYNC);
      syncProtocol.writeSyncStep1(encoder, doc);
      ws.send(encoding.toUint8Array(encoder));
    });

    ws.addEventListener("message", (event) => {
      if (closed) return;
      if (!(event.data instanceof ArrayBuffer)) return;
      const decoder = decoding.createDecoder(new Uint8Array(event.data));
      if (decoding.readVarUint(decoder) !== MESSAGE_SYNC) return;

      // Peek the sync type without consuming it, so `readSyncMessage` below
      // still sees a decoder positioned where it expects.
      const afterMessageType = decoder.pos;
      const syncType = decoding.readVarUint(decoder);
      decoder.pos = afterMessageType;

      // `readSyncMessage` applies an incoming update and, for a step1, writes
      // the step2 reply into `encoder`. Anything longer than the message-type
      // byte alone is a reply worth sending — this is also what answers the
      // server's keepalive.
      const encoder = encoding.createEncoder();
      encoding.writeVarUint(encoder, MESSAGE_SYNC);
      syncProtocol.readSyncMessage(decoder, encoder, doc, "remote");
      if (encoding.length(encoder) > 1 && ws.readyState === WebSocket.OPEN) {
        ws.send(encoding.toUint8Array(encoder));
      }

      // Report "live" only for a frame that can carry content, and only after it
      // has been applied. The server opens with its own step1 — a bare state
      // vector — so treating any sync frame as the answer would put a green
      // "Live" over an empty document for a round trip, which is what the
      // spinner is for. Announcing after the apply means `onText` has already
      // delivered the content, rather than leaving that to the caller batching
      // two updates from one event.
      if (!synced && (syncType === SYNC_STEP2 || syncType === SYNC_UPDATE)) {
        synced = true;
        failedHandshakes = 0;
        onStatus("live");
      }
    });

    const scheduleReconnect = () => {
      if (closed || reconnectTimer !== null) return;
      if (!synced) failedHandshakes += 1;

      if (failedHandshakes >= MAX_FAILED_HANDSHAKES) {
        onStatus("unavailable");
        closed = true;
        return;
      }

      onStatus("offline");
      const delay =
        RECONNECT_DELAYS_MS[Math.min(failedHandshakes, RECONNECT_DELAYS_MS.length - 1)] ??
        RECONNECT_DELAYS_MS.at(-1)!;
      reconnectTimer = setTimeout(() => {
        reconnectTimer = null;
        connect();
      }, delay);
    };

    ws.addEventListener("error", scheduleReconnect);
    ws.addEventListener("close", scheduleReconnect);
  }

  connect();

  return {
    close: () => {
      closed = true;
      if (reconnectTimer !== null) clearTimeout(reconnectTimer);
      text.unobserve(emit);
      socket?.close();
      doc.destroy();
    },
  };
}

// `http(s)://host` -> `ws(s)://host/api/collab-docs/<id>/ws?token=<token>`.
function collabWsUrl(serverUrl: string, objectId: string, shareToken: string): string {
  const base = serverUrl.replace(/\/$/, "").replace(/^http/, "ws");
  return `${base}/api/collab-docs/${encodeURIComponent(objectId)}/ws?token=${encodeURIComponent(
    shareToken,
  )}`;
}
