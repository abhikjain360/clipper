import type { ClipperBackend } from "./types";
import type { SessionResumeMaterial } from "@clipper/shared";

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
// The passphrase is NEVER persisted. To skip re-login on a page reload, the
// browser client stashes a resume blob — the server's bearer token plus the two
// OPAQUE-derived child keys (data key + device-identity wrapping key) — in
// sessionStorage. It survives an F5/reload but is wiped when the tab closes,
// bounding exposure to the tab's lifetime. `resume()` re-mounts the existing
// device from it without an OPAQUE login. Skipped under Tauri, where the desktop
// daemon owns the session and survives webview reloads.
//
// What this is and is not: the stored keys are derived leaves, not the
// passphrase/OPAQUE root. They decrypt cached content and let the device act
// while the token is live, but they cannot re-derive the passphrase, re-run
// login, or enroll a new device, and the token is server-revocable (logout /
// device removal). The data key is still raw bytes a same-origin script (XSS)
// could read — the wasm AEAD needs raw key bytes, so a non-extractable WebCrypto
// handle cannot help — so this is only as safe as the page is against XSS. See
// docs/local-at-rest-encryption.md.

const SESSION_RESUME_KEY = "clipper.session.v2";

type SessionResume = {
    token: string;
    dataKey: string;
    wrappingKey: string;
    username: string;
    deviceName: string;
    serverUrl: string;
};

function sessionResumeAvailable(): boolean {
    return !isTauriRuntime() && typeof sessionStorage !== "undefined";
}

// Persist a resume blob after login. `material` comes from the backend (null
// under Tauri or before an engine exists), so this no-ops there. The passphrase
// is never part of it.
export function saveSessionResume(
    material: SessionResumeMaterial | null,
    username: string,
    deviceName: string,
    serverUrl: string,
): void {
    if (!sessionResumeAvailable() || !material) return;
    const resume: SessionResume = { ...material, username, deviceName, serverUrl };
    try {
        sessionStorage.setItem(SESSION_RESUME_KEY, JSON.stringify(resume));
    } catch {
        // Private mode / quota — resume is best-effort; the login still succeeded.
    }
}

export function clearSessionResume(): void {
    if (typeof sessionStorage === "undefined") return;
    try {
        sessionStorage.removeItem(SESSION_RESUME_KEY);
    } catch {
        /* ignore */
    }
}

// Replay a stored session so a reload lands straight on the app. Returns true on
// success. Never throws: a missing entry, a parse failure, or a failed resume
// (revoked/expired token, server down) just falls back to the manual login
// screen.
export async function resumeSession(): Promise<boolean> {
    if (!sessionResumeAvailable()) return false;

    let raw: string | null = null;
    try {
        raw = sessionStorage.getItem(SESSION_RESUME_KEY);
    } catch {
        return false;
    }
    if (!raw) return false;

    let resume: SessionResume;
    try {
        resume = JSON.parse(raw) as SessionResume;
    } catch {
        clearSessionResume();
        return false;
    }

    try {
        const backend = await clipperBackend();
        await backend.resume(
            resume.token,
            resume.dataKey,
            resume.wrappingKey,
            resume.username,
            resume.deviceName,
            resume.serverUrl,
        );
        return true;
    } catch {
        // Transient or stale — keep the entry (a later reload may succeed) and
        // fall through to manual login, which overwrites it on success.
        return false;
    }
}

// Resolve the server base URL for connections that bypass the wasm engine — the
// collab Y-sync WebSocket and the public share page open these directly. The
// priority mirrors the login screen:
//   1. VITE_SERVER_URL — baked into hosted builds (the production API).
//   2. The exact URL the user logged in with (browser session-resume store).
//   3. The engine's compiled-in default (local dev / native shell).
export async function resolveServerUrl(): Promise<string> {
    const envUrl = import.meta.env.VITE_SERVER_URL as string | undefined;
    if (envUrl) return envUrl;
    const stored = readStoredServerUrl();
    if (stored) return stored;
    return await defaultServerUrl();
}

function readStoredServerUrl(): string | null {
    if (typeof sessionStorage === "undefined") return null;
    try {
        const raw = sessionStorage.getItem(SESSION_RESUME_KEY);
        if (!raw) return null;
        const resume = JSON.parse(raw) as Partial<SessionResume>;
        return typeof resume.serverUrl === "string" && resume.serverUrl.length > 0
            ? resume.serverUrl
            : null;
    } catch {
        return null;
    }
}
