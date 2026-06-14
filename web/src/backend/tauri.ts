import { invoke } from "@tauri-apps/api/core";
import type {
    AppState,
    ClipboardPayload,
    ClipperBackend,
    CollabItem,
    DeviceInfo,
} from "@clipper/shared";

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
        sendCurrentClipboardText: () => invoke<string | null>("send_current_clipboard_text"),
        sendClipboardPayload: (mimeType, bytes) =>
            invoke<string>("send_clipboard_payload", { mimeType, bytes: [...bytes] }),
        clipboardPayload: async (id) =>
            normalizeClipboardPayload(
                await invoke<RawClipboardPayload>("clipboard_payload", { id }),
            ),
        writeClipboardItemText: (id) => invoke<void>("write_clipboard_item_text", { id }),
        uploadFileBytes: (filename, mimeType, bytes) =>
            invoke<string>("upload_file_bytes", { filename, mimeType, bytes: [...bytes] }),
        uploadFileFromDialog: () => invoke<string | null>("upload_file_from_dialog"),
        downloadFileBytes: async (fileId) =>
            bytesFrom(await invoke<number[]>("download_file_bytes", { fileId })),
        downloadFileToDialog: (fileId, defaultFilename) =>
            invoke<boolean>("download_file_to_dialog", { fileId, defaultFilename }),
        deleteFile: (fileId) => invoke<void>("delete_file", { fileId }),
        createCollabDoc: () => invoke<CollabItem>("create_collab_doc"),
        deleteCollabDoc: (objectId) => invoke<void>("delete_collab_doc", { objectId }),
        getCollabDocMeta: (objectId) => invoke<CollabItem>("get_collab_doc_meta", { objectId }),
        listDevices: () => invoke<DeviceInfo[]>("list_devices"),
        removeDevice: (deviceId) => invoke<void>("remove_device", { deviceId }),
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
