import assert from "node:assert/strict";
import { test } from "node:test";
import * as Y from "yjs";
import { changeText, createCollabText, type EditorState } from "../src/collabText.ts";

function replica(value = "") {
  const doc = new Y.Doc();
  doc.getText("content").insert(0, value);
  return doc;
}

function editor(doc: Y.Doc) {
  let state: EditorState;
  const binding = createCollabText(doc, (next) => {
    state = next;
  });
  return { ...binding, state: () => state! };
}

test("text replacement emits one update and preserves matching ends", () => {
  for (const [old, next] of [
    ["", "hello"],
    ["hello", ""],
    ["hello", "hello"],
    ["abc def xyz", "abc NEW xyz"],
    ["a\nb", "a\nnew\nb"],
    ["a😀b", "a😃b"],
    ["a😀b", "ab"],
    ["a😀b", "aX😀b"],
    ["😀", "X😀"],
    ["😀", "😀X"],
  ]) {
    const doc = replica(old);
    let updates = 0;
    doc.on("update", () => {
      updates += 1;
    });
    changeText(doc.getText("content"), next!);
    assert.equal(doc.getText("content").toString(), next);
    assert.equal(updates, old === next ? 0 : 1);
    const peer = new Y.Doc();
    Y.applyUpdate(peer, Y.encodeStateAsUpdate(doc));
    assert.equal(peer.getText("content").toString(), next);
    peer.destroy();
    doc.destroy();
  }
});

test("accepted revisions continue the same local Yjs author", () => {
  const doc = replica("start");
  const input = editor(doc);
  for (let count = 1; count <= 40; count += 1) {
    const shown = input.state();
    input.receive({
      ...shown,
      text: `${shown.text}x`,
      start: shown.text.length + 1,
      end: shown.text.length + 1,
      eventCount: count,
      applied: false,
    });
    input.receive({ ...input.state(), applied: true });
  }
  assert.equal(input.state().text, `start${"x".repeat(40)}`);
  assert.equal(Y.decodeStateVector(Y.encodeStateVector(doc)).size, 2);
  input.close();
  doc.destroy();
});

test("remote inserts and deletes shift both selection ends", () => {
  const doc = replica("abcdef");
  const input = editor(doc);
  input.receive({ ...input.state(), eventCount: 1, start: 3, end: 5, applied: false });
  doc.getText("content").insert(0, "XX");
  assert.deepEqual([input.state().start, input.state().end], [5, 7]);
  doc.getText("content").delete(0, 4);
  assert.deepEqual([input.state().start, input.state().end], [1, 3]);
  doc.getText("content").delete(0, 4);
  assert.deepEqual([input.state().start, input.state().end], [0, 0]);
  input.close();
  doc.destroy();
});

test("queued typing uses the displayed revision across remote updates", () => {
  const doc = replica("abc");
  const input = editor(doc);
  const displayed = input.state();
  doc.getText("content").insert(0, "REMOTE ");
  input.receive({ ...displayed, eventCount: 1, text: "abcX", start: 4, end: 4, applied: false });
  doc.getText("content").insert(0, "MORE ");
  input.receive({ ...displayed, eventCount: 2, text: "abcXY", start: 5, end: 5, applied: false });
  assert.equal(input.state().text, "MORE REMOTE abcXY");
  assert.equal(input.state().start, input.state().text.length);
  const accepted = input.state();
  input.receive({ ...accepted, applied: true });
  input.receive({
    ...accepted,
    eventCount: 3,
    text: `${accepted.text}Z`,
    start: accepted.end + 1,
    end: accepted.end + 1,
    applied: false,
  });
  assert.equal(input.state().text, "MORE REMOTE abcXYZ");
  input.close();
  doc.destroy();
});

test("a stale replacement preserves concurrent text inside the replaced range", () => {
  const doc = replica("abcdef");
  const input = editor(doc);
  const displayed = input.state();
  doc.getText("content").insert(3, "REMOTE");
  input.receive({ ...displayed, eventCount: 1, text: "aZf", start: 2, end: 2, applied: false });
  assert.ok(input.state().text.includes("REMOTE"));
  assert.ok(input.state().text.includes("Z"));
  assert.ok(!input.state().text.includes("bc"));
  input.close();
  doc.destroy();
});

test("two clients converge with interleaved and delayed edits", () => {
  const phone = replica("hello world");
  const desktop = new Y.Doc();
  Y.applyUpdate(desktop, Y.encodeStateAsUpdate(phone));
  const input = editor(phone);
  const shown = input.state();
  changeText(desktop.getText("content"), "hello desktop world");
  Y.applyUpdate(phone, Y.encodeStateAsUpdate(desktop), "remote");
  input.receive({
    ...shown,
    eventCount: 1,
    text: "hello world!",
    start: 12,
    end: 12,
    applied: false,
  });
  input.receive({
    ...shown,
    eventCount: 2,
    text: "hello world!!",
    start: 13,
    end: 13,
    applied: false,
  });
  changeText(desktop.getText("content"), "HEY hello desktop world");
  Y.applyUpdate(desktop, Y.encodeStateAsUpdate(phone));
  Y.applyUpdate(phone, Y.encodeStateAsUpdate(desktop), "remote");
  assert.equal(phone.getText("content").toString(), "HEY hello desktop world!!");
  assert.equal(desktop.getText("content").toString(), input.state().text);
  assert.equal(input.state().start, input.state().text.length);
  input.close();
  phone.destroy();
  desktop.destroy();
});
