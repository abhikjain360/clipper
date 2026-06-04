import { invoke } from "@tauri-apps/api/core";
import type { AppState, ClipboardPayload, ClipperBackend } from "@clipper/shared";

type RawClipboardPayload = {
    mimeType: string;
    bytes: number[] | ArrayBuffer | Uint8Array;
    text: string | null;
};

export function tauriBackend(): ClipperBackend {
    return {
        connect: () => invoke<void>("connect"),
        defaultServerUrl: () => invoke<string>("default_server_url"),
        login: (passphrase, username, deviceName, serverUrl) =>
            invoke<void>("login", { passphrase, username, deviceName, serverUrl }),
        register: (accessKey, username, passphrase, deviceName, serverUrl) =>
            invoke<string>("register", { accessKey, username, passphrase, deviceName, serverUrl }),
        logout: () => invoke<void>("logout"),
        getState: () => invoke<AppState>("get_state"),
        stateVersion: () => invoke<number>("state_version"),
        waitForStateChange: (seenVersion) =>
            invoke<number>("wait_for_state_change", { seenVersion }),
        refresh: () => invoke<void>("refresh"),
        sendClipboardText: (text) => invoke<string>("send_clipboard_text", { text }),
        sendClipboardPayload: (mimeType, bytes) =>
            invoke<string>("send_clipboard_payload", { mimeType, bytes: [...bytes] }),
        clipboardPayload: async (id) =>
            normalizeClipboardPayload(
                await invoke<RawClipboardPayload>("clipboard_payload", { id }),
            ),
        uploadFileBytes: (filename, mimeType, bytes) =>
            invoke<string>("upload_file_bytes", { filename, mimeType, bytes: [...bytes] }),
        uploadFilePath: (path) => invoke<string>("upload_file_path", { path }),
        downloadFileBytes: async (fileId) =>
            bytesFrom(await invoke<number[]>("download_file_bytes", { fileId })),
        downloadFilePath: (fileId, path) => invoke<void>("download_file_path", { fileId, path }),
        deleteFile: (fileId) => invoke<void>("delete_file", { fileId }),
    };
}

function normalizeClipboardPayload(raw: RawClipboardPayload): ClipboardPayload {
    return {
        mimeType: raw.mimeType,
        bytes: bytesFrom(raw.bytes),
        text: raw.text,
    };
}

function bytesFrom(value: number[] | ArrayBuffer | Uint8Array | unknown): Uint8Array {
    if (value instanceof Uint8Array) return value;
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    if (Array.isArray(value)) return Uint8Array.from(value);
    return Uint8Array.from([]);
}
