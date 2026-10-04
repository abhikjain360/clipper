import assert from "node:assert/strict";
import { test } from "node:test";
import * as decoding from "lib0/decoding";
import * as encoding from "lib0/encoding";
import * as sync from "y-protocols/sync";
import * as Y from "yjs";
import { subscribeToCollabDoc, type CollabDocStatus } from "../src/collabDoc.ts";
import type { EditorState } from "../src/collabText.ts";

function frame(write: (encoder: encoding.Encoder) => void) {
  const encoder = encoding.createEncoder();
  encoding.writeVarUint(encoder, 0);
  write(encoder);
  return encoding.toUint8Array(encoder);
}

class Socket {
  static OPEN = 1;
  static instances: Socket[] = [];
  readyState = 0;
  binaryType = "";
  sent: Uint8Array[] = [];
  listeners = new Map<string, ((event: { data?: ArrayBuffer }) => void)[]>();
  url: string;
  constructor(url: string) {
    this.url = url;
    Socket.instances.push(this);
  }
  addEventListener(name: string, callback: (event: { data?: ArrayBuffer }) => void) {
    this.listeners.set(name, [...(this.listeners.get(name) ?? []), callback]);
  }
  emit(name: string, data?: Uint8Array) {
    const event = { data: data?.slice().buffer as ArrayBuffer | undefined };
    for (const callback of this.listeners.get(name) ?? []) callback(event);
  }
  open() {
    this.readyState = 1;
    this.emit("open");
  }
  send(message: Uint8Array) {
    this.sent.push(message);
  }
  close() {
    this.readyState = 3;
    this.emit("close");
  }
}

function apply(message: Uint8Array, doc: Y.Doc) {
  const decoder = decoding.createDecoder(message);
  assert.equal(decoding.readVarUint(decoder), 0);
  const encoder = encoding.createEncoder();
  sync.readSyncMessage(decoder, encoder, doc, "remote");
}

test("updates use Y-sync framing, offline edits reconnect after repeated failures", (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const original = globalThis.WebSocket;
  globalThis.WebSocket = Socket as unknown as typeof WebSocket;
  context.after(() => {
    globalThis.WebSocket = original;
  });
  Socket.instances = [];
  const server = new Y.Doc();
  server.getText("content").insert(0, "hello");
  let state: EditorState;
  let status: CollabDocStatus = "connecting";
  let localEdits = 0;
  const phone = subscribeToCollabDoc({
    objectId: "doc",
    serverUrl: "http://localhost:8787",
    shareToken: "token",
    onState: (next) => {
      state = next;
    },
    onStatus: (next) => {
      status = next;
    },
    onEdit: () => {
      localEdits += 1;
    },
  });
  const first = Socket.instances.at(-1)!;
  first.open();
  first.emit(
    "message",
    frame((encoder) => sync.writeSyncStep2(encoder, server)),
  );
  assert.equal(status, "live");
  assert.equal(localEdits, 0);
  phone.receive({
    ...state!,
    text: "hello phone",
    start: 11,
    end: 11,
    eventCount: 1,
    applied: false,
  });
  const update = first.sent.at(-1)!;
  const decoder = decoding.createDecoder(update);
  assert.equal(decoding.readVarUint(decoder), 0);
  assert.equal(decoding.readVarUint(decoder), sync.messageYjsUpdate);
  apply(update, server);
  assert.equal(server.getText("content").toString(), "hello phone");
  first.close();
  phone.receive({
    ...state!,
    text: "hello phone offline",
    start: 19,
    end: 19,
    eventCount: 2,
    applied: false,
  });
  assert.equal(status, "offline");
  for (let attempt = 0; attempt < 6; attempt += 1) {
    context.mock.timers.tick(15_000);
    Socket.instances.at(-1)!.close();
    assert.equal(status, "offline");
  }
  context.mock.timers.tick(15_000);
  const reconnected = Socket.instances.at(-1)!;
  reconnected.open();
  reconnected.emit(
    "message",
    frame((encoder) => sync.writeSyncStep1(encoder, server)),
  );
  apply(reconnected.sent.at(-1)!, server);
  reconnected.emit(
    "message",
    frame((encoder) => sync.writeSyncStep2(encoder, server)),
  );
  assert.equal(status, "live");
  assert.equal(server.getText("content").toString(), "hello phone offline");
  assert.equal(localEdits, 2);
  const sockets = Socket.instances.length;
  phone.close();
  context.mock.timers.tick(60_000);
  assert.equal(Socket.instances.length, sockets);
  server.destroy();
});
