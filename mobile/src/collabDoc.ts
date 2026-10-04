import * as decoding from "lib0/decoding";
import * as encoding from "lib0/encoding";
import * as syncProtocol from "y-protocols/sync";
import * as Y from "yjs";
import { createCollabText, type EditorEvent, type EditorState } from "./collabText.ts";

const MESSAGE_SYNC = 0;
const SYNC_STEP2 = 1;
const SYNC_UPDATE = 2;
const RECONNECT_DELAYS_MS = [1000, 2000, 4000, 8000, 15_000];
const MAX_FAILED_HANDSHAKES = 4;

export type CollabDocStatus = "connecting" | "live" | "offline" | "unavailable";

export type CollabDocHandle = {
  receive: (event: EditorEvent) => void;
  close: () => void;
};

export type CollabDocOptions = {
  serverUrl: string;
  objectId: string;
  shareToken: string;
  onState: (state: EditorState) => void;
  onStatus: (status: CollabDocStatus) => void;
  onEdit: () => void;
};

export function subscribeToCollabDoc(options: CollabDocOptions): CollabDocHandle {
  const { serverUrl, objectId, shareToken, onState, onStatus } = options;
  const doc = new Y.Doc();
  const editor = createCollabText(doc, onState);
  const url = collabWsUrl(serverUrl, objectId, shareToken);
  let socket: WebSocket | null = null;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let closed = false;
  let everSynced = false;
  let failedHandshakes = 0;

  function sendUpdate(update: Uint8Array, origin: unknown) {
    if (origin === "remote" || closed) return;
    options.onEdit();
    if (!socket) return;
    const encoder = encoding.createEncoder();
    encoding.writeVarUint(encoder, MESSAGE_SYNC);
    syncProtocol.writeUpdate(encoder, update);
    send(socket, encoding.toUint8Array(encoder));
  }
  doc.on("update", sendUpdate);

  function connect() {
    if (closed) return;
    onStatus("connecting");
    const ws = new WebSocket(url);
    ws.binaryType = "arraybuffer";
    socket = ws;
    let synced = false;

    ws.addEventListener("open", () => {
      if (closed || socket !== ws) return;
      const encoder = encoding.createEncoder();
      encoding.writeVarUint(encoder, MESSAGE_SYNC);
      syncProtocol.writeSyncStep1(encoder, doc);
      send(ws, encoding.toUint8Array(encoder));
    });

    ws.addEventListener("message", (event) => {
      if (closed || socket !== ws || !(event.data instanceof ArrayBuffer)) return;
      try {
        const decoder = decoding.createDecoder(new Uint8Array(event.data));
        if (decoding.readVarUint(decoder) !== MESSAGE_SYNC) return;
        const encoder = encoding.createEncoder();
        encoding.writeVarUint(encoder, MESSAGE_SYNC);
        const syncType = syncProtocol.readSyncMessage(decoder, encoder, doc, "remote");
        if (encoding.length(encoder) > 1) send(ws, encoding.toUint8Array(encoder));
        if (!synced && (syncType === SYNC_STEP2 || syncType === SYNC_UPDATE)) {
          synced = true;
          everSynced = true;
          failedHandshakes = 0;
          onStatus("live");
        }
      } catch {
        ws.close();
      }
    });

    const scheduleReconnect = () => {
      if (closed || socket !== ws || reconnectTimer !== null) return;
      socket = null;
      ws.close();
      if (!synced) failedHandshakes += 1;
      if (!everSynced && failedHandshakes >= MAX_FAILED_HANDSHAKES) {
        onStatus("unavailable");
        return;
      }
      onStatus("offline");
      const delay =
        RECONNECT_DELAYS_MS[Math.min(failedHandshakes, RECONNECT_DELAYS_MS.length - 1)]!;
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
    receive: editor.receive,
    close: () => {
      closed = true;
      if (reconnectTimer !== null) clearTimeout(reconnectTimer);
      doc.off("update", sendUpdate);
      editor.close();
      socket?.close();
      doc.destroy();
    },
  };
}

function collabWsUrl(serverUrl: string, objectId: string, shareToken: string): string {
  const base = serverUrl.replace(/\/$/, "").replace(/^http/, "ws");
  return `${base}/api/collab-docs/${encodeURIComponent(objectId)}/ws?token=${encodeURIComponent(shareToken)}`;
}

function send(ws: WebSocket, message: Uint8Array) {
  if (ws.readyState !== WebSocket.OPEN) return;
  try {
    ws.send(message);
  } catch {
    ws.close();
  }
}
