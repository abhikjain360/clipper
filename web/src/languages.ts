import type { Extension } from "@codemirror/state";

// A syntax-highlighting language the editor can load on demand. Each pack's
// grammar is dynamically imported so it splits into its own chunk and is only
// fetched when that language is actually selected.
export interface LanguagePack {
    // Canonical id — the value used in the language dropdown and (for collab
    // docs) persisted inside the Y.Doc so the choice syncs across clients.
    id: string;
    label: string;
    // Filename extensions that map to this language for auto-detection.
    extensions: string[];
    // Resolve the CodeMirror language extension, or null for no highlighting.
    load: () => Promise<Extension | null>;
}

// Order here is the order shown in the dropdown.
export const LANGUAGES: LanguagePack[] = [
    {
        id: "plaintext",
        label: "Plain Text",
        extensions: ["txt", "text", "log"],
        load: async () => null,
    },
    {
        id: "markdown",
        label: "Markdown",
        extensions: ["md", "markdown"],
        load: async () => (await import("@codemirror/lang-markdown")).markdown(),
    },
    {
        id: "javascript",
        label: "JavaScript",
        extensions: ["js", "jsx", "mjs", "cjs"],
        load: async () => (await import("@codemirror/lang-javascript")).javascript({ jsx: true }),
    },
    {
        id: "typescript",
        label: "TypeScript",
        extensions: ["ts", "tsx", "mts", "cts"],
        load: async () =>
            (await import("@codemirror/lang-javascript")).javascript({
                typescript: true,
                jsx: true,
            }),
    },
    {
        id: "json",
        label: "JSON",
        extensions: ["json"],
        load: async () => (await import("@codemirror/lang-json")).json(),
    },
    {
        id: "css",
        label: "CSS",
        extensions: ["css"],
        load: async () => (await import("@codemirror/lang-css")).css(),
    },
    {
        id: "html",
        label: "HTML",
        extensions: ["html", "htm"],
        load: async () => (await import("@codemirror/lang-html")).html(),
    },
    {
        id: "rust",
        label: "Rust",
        extensions: ["rs"],
        load: async () => (await import("@codemirror/lang-rust")).rust(),
    },
    {
        id: "python",
        label: "Python",
        extensions: ["py", "pyw"],
        load: async () => (await import("@codemirror/lang-python")).python(),
    },
];

// Fallback when a hint matches nothing. Collab docs start here; the file viewer
// uses this only for unrecognised extensions.
export const DEFAULT_LANGUAGE_ID = "plaintext";

// Language a new collab doc defaults to (markdown is the most common note shape).
export const DEFAULT_COLLAB_LANGUAGE_ID = "markdown";

const BY_ID = new Map(LANGUAGES.map((lang) => [lang.id, lang]));

// Map a filename, path, bare extension, or MIME-ish hint to a language id.
export function detectLanguageId(hint: string | undefined): string {
    if (!hint) return DEFAULT_LANGUAGE_ID;
    const lower = hint.toLowerCase();
    const lastDot = lower.lastIndexOf(".");
    // Handle a filename/path ("a/b.rs"), a bare extension ("rs"), or a MIME-ish
    // string by taking the trailing token after the last dot or slash.
    const tail = lastDot >= 0 ? lower.slice(lastDot + 1) : lower;
    const ext = tail.split(/[/\\]/).pop() ?? tail;
    const match = LANGUAGES.find((lang) => lang.extensions.includes(ext));
    return match?.id ?? DEFAULT_LANGUAGE_ID;
}

// Load the CodeMirror extension for a language id (null if unknown / plaintext).
export function loadLanguageExtension(id: string): Promise<Extension | null> {
    return BY_ID.get(id)?.load() ?? Promise.resolve(null);
}

// The dropdown's stable list of { id, label } options.
export const LANGUAGE_OPTIONS: ReadonlyArray<{ id: string; label: string }> = LANGUAGES.map(
    ({ id, label }) => ({ id, label }),
);
