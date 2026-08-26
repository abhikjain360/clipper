import { useEffect, useRef, useState } from "react";
import { Compartment, EditorState } from "@codemirror/state";
import { EditorView, keymap, lineNumbers } from "@codemirror/view";
import { defaultKeymap } from "@codemirror/commands";
import { oneDark } from "@codemirror/theme-one-dark";
import * as Y from "yjs";
import { WebsocketProvider } from "y-websocket";
import { yCollab, yUndoManagerKeymap } from "y-codemirror.next";
import { DEFAULT_COLLAB_LANGUAGE_ID, detectLanguageId, loadLanguageExtension } from "./languages";
import { LanguageSelect } from "./LanguageSelect";

// Live collaborative document the editor binds to instead of static `content`.
export interface CollabConfig {
    // The collab object's id — the Y-sync WebSocket route is keyed by it.
    objectId: string;
    // The share token, the sole credential for the WebSocket.
    shareToken: string;
    // Resolved http(s) server base URL; converted to ws(s) for the socket.
    serverUrl: string;
    // Name shown on this client's remote cursor to other editors.
    displayName: string;
}

interface CodeEditorProps {
    // Static, read-only text (file viewer). Ignored when `collab` is set.
    content?: string;
    // Filename, extension, or id used to pick the initial language. A user can
    // override it via the toolbar; collab docs persist the choice in the Y.Doc.
    lang?: string;
    collab?: CollabConfig;
}

// Runtime state for a live collab session, created once per document.
interface CollabRuntime {
    ydoc: Y.Doc;
    ytext: Y.Text;
    ymeta: Y.Map<unknown>;
    provider: WebsocketProvider;
}

const fillHeightTheme = EditorView.theme({
    "&": { height: "100%" },
    ".cm-scroller": { overflow: "auto" },
});

// Distinct-ish remote cursor colours, indexed by the Yjs client id so each
// connected editor gets a stable colour without coordinating.
const CURSOR_COLORS = [
    "#30bced",
    "#6eeb83",
    "#ffbc42",
    "#ee6352",
    "#9b5de5",
    "#f15bb5",
    "#00bbf9",
    "#fee440",
];

// The Y-sync WebSocket base the provider connects under. y-websocket appends
// `/<room>` and `?params`, yielding `…/api/collab-docs/<objectId>/ws?token=…`.
//
// NOTE (desktop): this socket is opened by the webview itself, unlike every
// other server call in the Tauri shell, which goes over IPC to the daemon. So
// the Tauri CSP's `connect-src` must admit `ws:`/`wss:` — with only `'self'`,
// WebKit rejects the constructor with "SecurityError: The operation is
// insecure." and the editor never mounts. See `src-tauri/tauri.conf.json`.
function collabWsBaseUrl(serverHttpUrl: string): string {
    const url = new URL(serverHttpUrl);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    return `${url.toString().replace(/\/$/, "")}/api/collab-docs`;
}

// Editor-wide Vim opt-in, persisted so the preference is shared across the file
// viewer, collab docs, and the public share page. Same key the file viewer used
// before the Vim toggle moved into the editor toolbar.
const VIM_MODE_STORAGE_KEY = "clipper_vim_mode";

// `localStorage` is not merely absent in some contexts — WebKit *throws*
// `SecurityError` on the property itself (a blocked custom-scheme origin, a
// browser set to reject site data). An optional chain does not catch that, and
// this runs during render, so an unguarded read would take the whole editor down
// with it. A remembered preference is not worth that.
function readVimMode(): boolean {
    try {
        return globalThis.localStorage?.getItem(VIM_MODE_STORAGE_KEY) === "true";
    } catch {
        return false;
    }
}

function writeVimMode(enabled: boolean): void {
    try {
        globalThis.localStorage?.setItem(VIM_MODE_STORAGE_KEY, String(enabled));
    } catch {
        // Preference is best-effort; the toggle still applies for this session.
    }
}

export function CodeEditor({ content = "", lang, collab }: CodeEditorProps) {
    const hostRef = useRef<HTMLDivElement | null>(null);
    const viewRef = useRef<EditorView | null>(null);
    // Compartments let language and vim slot in (and swap) after their chunks
    // load without rebuilding the editor or blocking first paint.
    const languageCompartment = useRef(new Compartment());
    const vimCompartment = useRef(new Compartment());

    const [collabRuntime, setCollabRuntime] = useState<CollabRuntime | null>(null);
    const [languageId, setLanguageId] = useState<string>(() =>
        collab ? DEFAULT_COLLAB_LANGUAGE_ID : detectLanguageId(lang),
    );
    // Vim key bindings: an editor-wide, persisted opt-in available in every mode.
    const [vimMode, setVimMode] = useState(readVimMode);

    // ── Provider lifecycle (collab only) ──
    // Kept separate from editor construction so toggling vim (which rebuilds the
    // view) does not tear down and reconnect the WebSocket.
    useEffect(() => {
        if (!collab) return undefined;

        const ydoc = new Y.Doc();
        const ytext = ydoc.getText("content");
        const ymeta = ydoc.getMap("meta");
        const provider = new WebsocketProvider(
            collabWsBaseUrl(collab.serverUrl),
            `${collab.objectId}/ws`,
            ydoc,
            { params: { token: collab.shareToken } },
        );
        const color = CURSOR_COLORS[ydoc.clientID % CURSOR_COLORS.length];
        provider.awareness.setLocalStateField("user", {
            name: collab.displayName,
            color,
            colorLight: `${color}33`,
        });

        // The doc's language lives in shared meta so the choice syncs to every
        // editor. Reflect the current value and follow remote changes.
        const applyMetaLanguage = () => {
            const stored = ymeta.get("language");
            if (typeof stored === "string") setLanguageId(stored);
        };
        applyMetaLanguage();
        ymeta.observe(applyMetaLanguage);

        setCollabRuntime({ ydoc, ytext, ymeta, provider });

        return () => {
            ymeta.unobserve(applyMetaLanguage);
            provider.destroy();
            ydoc.destroy();
            setCollabRuntime(null);
        };
    }, [collab?.objectId, collab?.serverUrl, collab?.shareToken, collab?.displayName]);

    // ── Editor construction ──
    // Rebuilds on static-content / vim / collab-runtime changes. Language is
    // applied through its compartment (below), never by rebuilding.
    //
    // Deliberately keyed on `isCollab`, not `collab`: callers pass an inline
    // object literal, so its identity changes on every parent render. Depending
    // on it would tear down and rebuild the whole EditorView — losing the
    // cursor, selection, scroll position, and undo history — every time the
    // surrounding view re-rendered for an unrelated reason (a "Copied" flash, a
    // title save). The provider effect above is already keyed on the config's
    // primitives, so `collabRuntime` only changes when the document really does.
    const isCollab = collab !== undefined;
    useEffect(() => {
        const host = hostRef.current;
        if (!host) return undefined;
        // In collab mode, wait until the provider/doc is ready.
        if (isCollab && !collabRuntime) return undefined;

        let cancelled = false;

        const extensions = [
            vimCompartment.current.of([]),
            lineNumbers(),
            oneDark,
            fillHeightTheme,
            EditorView.lineWrapping,
            languageCompartment.current.of([]),
        ];

        let doc = content;
        if (collabRuntime) {
            // Editable, bound to the shared text. ytext is empty until the first
            // sync; the yCollab plugin streams remote content in as it arrives.
            doc = collabRuntime.ytext.toString();
            extensions.push(
                keymap.of([...defaultKeymap, ...yUndoManagerKeymap]),
                yCollab(collabRuntime.ytext, collabRuntime.provider.awareness),
            );
        } else {
            // File objects are immutable; the viewer is strictly read-only.
            extensions.push(EditorState.readOnly.of(true), EditorView.editable.of(false));
        }

        const view = new EditorView({
            state: EditorState.create({ doc, extensions }),
            parent: host,
        });
        viewRef.current = view;

        if (vimMode) {
            void import("@replit/codemirror-vim").then(({ vim }) => {
                if (!cancelled)
                    view.dispatch({ effects: vimCompartment.current.reconfigure(vim()) });
            });
        }

        return () => {
            cancelled = true;
            view.destroy();
            viewRef.current = null;
        };
    }, [content, vimMode, isCollab, collabRuntime]);

    // ── Language compartment ──
    // Load the selected grammar's chunk on demand and slot it in.
    useEffect(() => {
        let cancelled = false;
        void loadLanguageExtension(languageId).then((extension) => {
            const view = viewRef.current;
            if (cancelled || !view) return;
            view.dispatch({
                effects: languageCompartment.current.reconfigure(extension ?? []),
            });
        });
        return () => {
            cancelled = true;
        };
    }, [languageId, collabRuntime]);

    function onLanguageChange(id: string) {
        setLanguageId(id);
        // Persist into the shared doc so collaborators follow the choice.
        collabRuntime?.ymeta.set("language", id);
    }

    function toggleVim() {
        setVimMode((previous) => {
            const next = !previous;
            writeVimMode(next);
            return next;
        });
    }

    return (
        <div style={{ height: "100%", display: "flex", flexDirection: "column", minHeight: 0 }}>
            <div
                style={{
                    display: "flex",
                    justifyContent: "flex-end",
                    alignItems: "center",
                    gap: 8,
                    padding: "6px 10px",
                    background: "#13161a",
                    borderBottom: "1px solid #252b31",
                }}
            >
                <button
                    type="button"
                    onClick={toggleVim}
                    aria-pressed={vimMode}
                    title="Toggle Vim key bindings"
                    style={{
                        background: vimMode ? "#1d3a5f" : "#171a1d",
                        color: vimMode ? "#9cd2ff" : "#e6e9ec",
                        border: `1px solid ${vimMode ? "#2f6db0" : "#252b31"}`,
                        borderRadius: 6,
                        padding: "4px 10px",
                        fontSize: 12,
                        fontFamily: "inherit",
                        cursor: "pointer",
                    }}
                >
                    Vim
                </button>
                <LanguageSelect value={languageId} onChange={onLanguageChange} />
            </div>
            <div ref={hostRef} style={{ flex: 1, minHeight: 0, overflow: "hidden" }} />
        </div>
    );
}
