import type { ClipperBackend } from "./types";

let backendPromise: Promise<ClipperBackend> | undefined;

export function clipperBackend(): Promise<ClipperBackend> {
    backendPromise ??= (
        isTauriRuntime()
            ? import("./tauri").then((module) => module.tauriBackend())
            : import("./wasm").then((module) => module.clipperWasm())
    ).catch((error) => {
        // Don't cache a rejected promise: a transient dynamic-import / wasm-init
        // failure would otherwise brick the backend for the whole session. Reset
        // the cache so a later call can retry, while still memoizing success
        // (which prevents double wasm init under React StrictMode).
        backendPromise = undefined;
        throw error;
    });
    return backendPromise;
}

export function isTauriRuntime(): boolean {
    return typeof window !== "undefined" && Reflect.has(window, "__TAURI_INTERNALS__");
}

export async function defaultServerUrl(): Promise<string> {
    return await (await clipperBackend()).defaultServerUrl();
}

export async function readClipboardText(): Promise<string> {
    return await navigator.clipboard.readText();
}

export async function writeClipboardText(text: string): Promise<void> {
    await navigator.clipboard.writeText(text);
}

export function formatBackendError(error: unknown): string {
    if (error instanceof Error) return error.message;

    if (typeof error === "object" && error !== null) {
        const message = (error as { message?: unknown }).message;
        if (typeof message === "string" && message.length > 0) return message;

        try {
            return JSON.stringify(error);
        } catch {
            return String(error);
        }
    }

    return String(error);
}

// ── Browser session resume (sessionStorage) ──
//
// The passphrase is otherwise never persisted (the E2E encryption keys derive
// from it). To skip re-login on every page reload, the browser client keeps the
// login credentials in sessionStorage: they survive an F5/reload but are wiped
// when the tab closes, bounding exposure to the tab's lifetime. Skipped under
// Tauri, where the desktop daemon owns credentials and the passphrase must not
// land in the webview store.
//
// sessionStorage is plaintext and same-origin-script readable, so this is only
// as safe as the page is against XSS; the localStorage device identity stays
// encrypted regardless.

const SESSION_CREDENTIALS_KEY = "clipper.session.v1";

type SessionCredentials = {
    passphrase: string;
    username: string;
    deviceName: string;
    serverUrl: string;
};

function sessionResumeAvailable(): boolean {
    return !isTauriRuntime() && typeof sessionStorage !== "undefined";
}

export function saveSessionCredentials(creds: SessionCredentials): void {
    if (!sessionResumeAvailable()) return;
    try {
        sessionStorage.setItem(SESSION_CREDENTIALS_KEY, JSON.stringify(creds));
    } catch {
        // Private mode / quota — resume is best-effort; the login still succeeded.
    }
}

export function clearSessionCredentials(): void {
    if (typeof sessionStorage === "undefined") return;
    try {
        sessionStorage.removeItem(SESSION_CREDENTIALS_KEY);
    } catch {
        /* ignore */
    }
}

// Replay a stored login so a reload lands straight on the app. Returns true on
// success. Never throws: a missing entry, a parse failure, or a failed login
// (stale creds / server down) just falls back to the manual login screen.
export async function resumeSession(): Promise<boolean> {
    if (!sessionResumeAvailable()) return false;

    let raw: string | null = null;
    try {
        raw = sessionStorage.getItem(SESSION_CREDENTIALS_KEY);
    } catch {
        return false;
    }
    if (!raw) return false;

    let creds: SessionCredentials;
    try {
        creds = JSON.parse(raw) as SessionCredentials;
    } catch {
        clearSessionCredentials();
        return false;
    }

    try {
        const backend = await clipperBackend();
        await backend.login(creds.passphrase, creds.username, creds.deviceName, creds.serverUrl);
        return true;
    } catch {
        // Transient or stale — keep the entry (a later reload may succeed) and
        // fall through to manual login, which overwrites it on success.
        return false;
    }
}
