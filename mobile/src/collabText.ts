import * as Y from "yjs";

export type TextSelection = { start: number; end: number };
export type EditorState = TextSelection & {
  text: string;
  revision: number;
  eventCount: number;
};
export type EditorEvent = EditorState & { applied: boolean };

export function changeText(text: Y.Text, value: string) {
  const old = text.toString();
  if (old === value) return;
  let start = 0;
  while (start < old.length && start < value.length && old[start] === value[start]) start += 1;
  if (start > 0 && /[\uDC00-\uDFFF]/.test(old[start] ?? value[start] ?? "")) start -= 1;
  let end = old.length;
  let nextEnd = value.length;
  while (end > start && nextEnd > start && old[end - 1] === value[nextEnd - 1]) {
    end -= 1;
    nextEnd -= 1;
  }
  if (/[\uDC00-\uDFFF]/.test(old[end] ?? value[nextEnd] ?? "")) {
    end += 1;
    nextEnd += 1;
  }
  text.doc!.transact(() => {
    if (end > start) text.delete(start, end - start);
    if (nextEnd > start) text.insert(start, value.slice(start, nextEnd));
  });
}

export function createCollabText(doc: Y.Doc, onState: (state: EditorState) => void) {
  const text = doc.getText("content");
  const views = new Map<number, Y.Doc>();
  const inputClient = new Y.Doc();
  const inputClientId = inputClient.clientID;
  inputClient.destroy();
  let revision = 0;
  let eventCount = 0;
  let changing = false;
  let start = Y.createRelativePositionFromTypeIndex(text, 0);
  let end = start;

  function selection(): TextSelection {
    return {
      start: Y.createAbsolutePositionFromRelativePosition(start, doc)?.index ?? 0,
      end: Y.createAbsolutePositionFromRelativePosition(end, doc)?.index ?? 0,
    };
  }

  function publish() {
    revision += 1;
    const view = new Y.Doc();
    Y.applyUpdate(view, Y.encodeStateAsUpdate(doc));
    view.clientID = inputClientId;
    views.set(revision, view);
    onState({ text: text.toString(), revision, eventCount, ...selection() });
  }

  function observe() {
    if (!changing) publish();
  }
  text.observe(observe);
  publish();

  return {
    receive(event: EditorEvent) {
      const view = views.get(event.revision);
      if (!view || event.eventCount < eventCount) return;
      for (const [id, previous] of views) {
        if (id < event.revision) {
          previous.destroy();
          views.delete(id);
        }
      }
      if (event.applied) return;
      eventCount = event.eventCount;
      const displayed = view.getText("content");
      changing = true;
      try {
        const before = Y.encodeStateVector(view);
        changeText(displayed, event.text);
        start = Y.createRelativePositionFromTypeIndex(displayed, event.start);
        end = Y.createRelativePositionFromTypeIndex(displayed, event.end);
        Y.applyUpdate(doc, Y.encodeStateAsUpdate(view, before), "input");
      } finally {
        changing = false;
      }
      publish();
    },
    close() {
      text.unobserve(observe);
      for (const view of views.values()) view.destroy();
      views.clear();
    },
  };
}
