import { useEffect, useRef } from "react";
import { Compartment, EditorState, type Extension } from "@codemirror/state";
import { EditorView, lineNumbers } from "@codemirror/view";
import { oneDark } from "@codemirror/theme-one-dark";

interface CodeEditorProps {
    content: string;
    // Filename, extension, or MIME hint used to pick a language extension.
    lang?: string;
    vimMode?: boolean;
}

const fillHeightTheme = EditorView.theme({
    "&": { height: "100%" },
    ".cm-scroller": { overflow: "auto" },
});

// Dynamically import only the CodeMirror language pack the current file needs so
// each grammar splits into its own chunk instead of shipping all of them up
// front. Returns null when the type is unknown so we render with no language
// support.
async function loadLanguage(lang: string | undefined): Promise<Extension | null> {
    if (!lang) return null;

    const lower = lang.toLowerCase();
    // Pull the extension from a filename or path; fall back to the raw hint
    // (handles bare extensions like "rs" and MIME-ish strings).
    const lastDot = lower.lastIndexOf(".");
    const ext = lastDot >= 0 ? lower.slice(lastDot + 1) : lower;

    switch (ext) {
        case "md":
        case "markdown":
            return (await import("@codemirror/lang-markdown")).markdown();
        case "js":
        case "jsx":
        case "ts":
        case "tsx":
            return (await import("@codemirror/lang-javascript")).javascript();
        case "css":
            return (await import("@codemirror/lang-css")).css();
        case "html":
            return (await import("@codemirror/lang-html")).html();
        case "rs":
            return (await import("@codemirror/lang-rust")).rust();
        case "py":
            return (await import("@codemirror/lang-python")).python();
        case "json":
            return (await import("@codemirror/lang-json")).json();
        default:
            return null;
    }
}

export function CodeEditor({ content, lang, vimMode }: CodeEditorProps) {
    const hostRef = useRef<HTMLDivElement | null>(null);

    useEffect(() => {
        const host = hostRef.current;
        if (!host) return undefined;

        let cancelled = false;
        // Compartments let us slot vim and the language pack in after their
        // chunks load without blocking the editor's first paint. Vim sits first
        // so its keymap keeps precedence over the other bindings once loaded.
        const vimCompartment = new Compartment();
        const languageCompartment = new Compartment();

        const view = new EditorView({
            state: EditorState.create({
                doc: content,
                extensions: [
                    vimCompartment.of([]),
                    lineNumbers(),
                    oneDark,
                    fillHeightTheme,
                    EditorView.lineWrapping,
                    // File objects are immutable; the viewer is strictly read-only.
                    EditorState.readOnly.of(true),
                    EditorView.editable.of(false),
                    languageCompartment.of([]),
                ],
            }),
            parent: host,
        });

        if (vimMode) {
            void import("@replit/codemirror-vim").then(({ vim }) => {
                if (!cancelled) view.dispatch({ effects: vimCompartment.reconfigure(vim()) });
            });
        }
        void loadLanguage(lang).then((language) => {
            if (!cancelled && language) {
                view.dispatch({ effects: languageCompartment.reconfigure(language) });
            }
        });

        return () => {
            cancelled = true;
            view.destroy();
        };
    }, [content, lang, vimMode]);

    return <div ref={hostRef} style={{ height: "100%", overflow: "hidden" }} />;
}
