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
