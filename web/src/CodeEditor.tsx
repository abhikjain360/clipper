import { useEffect, useRef } from "react";
import { EditorState, type Extension } from "@codemirror/state";
import { EditorView, lineNumbers } from "@codemirror/view";
import { oneDark } from "@codemirror/theme-one-dark";
import { vim } from "@replit/codemirror-vim";
import { markdown } from "@codemirror/lang-markdown";
import { javascript } from "@codemirror/lang-javascript";
import { css } from "@codemirror/lang-css";
import { html } from "@codemirror/lang-html";
import { rust } from "@codemirror/lang-rust";
import { python } from "@codemirror/lang-python";
import { json } from "@codemirror/lang-json";

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

// Map a filename / extension / MIME hint to a CodeMirror language extension.
// Returns null when the type is unknown so we render with no language support.
function languageExtension(lang: string | undefined): Extension | null {
    if (!lang) return null;

    const lower = lang.toLowerCase();
    // Pull the extension from a filename or path; fall back to the raw hint
    // (handles bare extensions like "rs" and MIME-ish strings).
    const lastDot = lower.lastIndexOf(".");
    const ext = lastDot >= 0 ? lower.slice(lastDot + 1) : lower;

    switch (ext) {
        case "md":
        case "markdown":
            return markdown();
        case "js":
        case "jsx":
        case "ts":
        case "tsx":
            return javascript();
        case "css":
            return css();
        case "html":
            return html();
        case "rs":
            return rust();
        case "py":
            return python();
        case "json":
            return json();
        default:
            return null;
    }
}

export function CodeEditor({ content, lang, vimMode }: CodeEditorProps) {
    const hostRef = useRef<HTMLDivElement | null>(null);

    useEffect(() => {
        const host = hostRef.current;
        if (!host) return undefined;

        const extensions: Extension[] = [];
        // Vim has to be installed before other keymaps so its bindings win.
        if (vimMode) extensions.push(vim());
        extensions.push(
            lineNumbers(),
            oneDark,
            fillHeightTheme,
            EditorView.lineWrapping,
            // File objects are immutable; the viewer is strictly read-only.
            EditorState.readOnly.of(true),
            EditorView.editable.of(false),
        );

        const languageSupport = languageExtension(lang);
        if (languageSupport) extensions.push(languageSupport);

        const view = new EditorView({
            state: EditorState.create({ doc: content, extensions }),
            parent: host,
        });

        return () => {
            view.destroy();
        };
    }, [content, lang, vimMode]);

    return <div ref={hostRef} style={{ height: "100%", overflow: "hidden" }} />;
}
