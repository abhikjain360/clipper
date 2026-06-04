import type { ClipperBackend } from "./types";

let backendPromise: Promise<ClipperBackend> | undefined;

export function clipperBackend(): Promise<ClipperBackend> {
    backendPromise ??= isTauriRuntime()
        ? import("./tauri").then((module) => module.tauriBackend())
        : import("./wasm").then((module) => module.clipperWasm());
    return backendPromise;
}

export function isTauriRuntime(): boolean {
    return typeof window !== "undefined" && Reflect.has(window, "__TAURI_INTERNALS__");
}

export async function defaultServerUrl(): Promise<string> {
    return await (await clipperBackend()).defaultServerUrl();
}

export async function readClipboardText(): Promise<string> {
    if (isTauriRuntime()) {
        const { readText } = await import("@tauri-apps/plugin-clipboard-manager");
        return await readText();
    }

    return await navigator.clipboard.readText();
}

export async function writeClipboardText(text: string): Promise<void> {
    if (isTauriRuntime()) {
        const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
        await writeText(text);
        return;
    }

    await navigator.clipboard.writeText(text);
}

export async function openNativeFilePath(): Promise<string | null> {
    if (!isTauriRuntime()) return null;

    const { open } = await import("@tauri-apps/plugin-dialog");
    const selected = await open({ directory: false, multiple: false });
    if (typeof selected === "string") return selected;
    if (Array.isArray(selected)) return selected[0] ?? null;
    return null;
}

export async function saveNativeFilePath(defaultPath: string): Promise<string | null> {
    if (!isTauriRuntime()) return null;

    const { save } = await import("@tauri-apps/plugin-dialog");
    return await save({ defaultPath });
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
