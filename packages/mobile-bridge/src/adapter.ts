import "./index";

import {
  ConnectionStatus as NativeConnectionStatus,
  type AppState as NativeAppState,
  type ClipboardPayload as NativeClipboardPayload,
  type DecryptedClipboardItem,
  type DecryptedFileItem,
  type DeviceInfo as NativeDeviceInfo,
} from "./generated/clipper_app_types";
import {
  MobileClipperClient,
  type MobileClipperClientLike,
} from "./generated/clipper_mobile_uniffi";
import type {
  AppState,
  ClipboardItem,
  ClipboardPayload,
  ClipperBackend,
  ConnectionStatus,
  DeviceInfo,
  FileItem,
} from "@clipper/shared";

export function createMobileBackend(
  client: MobileClipperClientLike = MobileClipperClient.newWithDefaultServer(),
): ClipperBackend {
  return {
    clipboardPayload: async (id) => mapClipboardPayload(client.clipboardPayload(id)),
    connect: async () => client.connect(),
    defaultServerUrl: () => client.defaultServerUrl(),
    deleteFile: async (fileId) => client.deleteFile(fileId),
    downloadFileBytes: async (fileId) => new Uint8Array(client.downloadFileBytes(fileId)),
    getState: async () => mapAppState(client.getState()),
    listDevices: async () => client.listDevices().map(mapDeviceInfo),
    login: async (passphrase, username, deviceName, serverUrl) =>
      client.login(passphrase, username, deviceName, serverUrl),
    logout: async () => client.logout(),
    refresh: async () => client.refresh(),
    register: async (accessKey, username, passphrase, deviceName, serverUrl) =>
      client.register(accessKey, username, passphrase, deviceName, serverUrl),
    removeDevice: async (deviceId) => client.removeDevice(deviceId),
    sendClipboardPayload: async (mimeType, bytes) =>
      client.sendClipboardPayload(mimeType, arrayBufferFrom(bytes)),
    sendClipboardText: async (text) => client.sendClipboardText(text),
    stateVersion: () => client.stateVersion(),
    uploadFileBytes: async (filename, mimeType, bytes) =>
      client.uploadFileBytes(filename, mimeType, arrayBufferFrom(bytes)),
    waitForStateChange: async (seenVersion) => {
      for (;;) {
        const version = client.stateVersion();
        if (version !== seenVersion) return version;
        await delay(250);
      }
    },
  };
}

export default createMobileBackend;

function mapAppState(state: NativeAppState): AppState {
  return {
    clipboard_items: state.clipboardItems.map(mapClipboardItem),
    connection_status: mapConnectionStatus(state.connectionStatus),
    error: state.error ?? null,
    files: state.files.map(mapFileItem),
    saved_profile: state.savedProfile
      ? {
          device_name: state.savedProfile.deviceName,
          username: state.savedProfile.username,
        }
      : null,
    session: state.session
      ? {
          device_id: state.session.deviceId,
          device_name: state.session.deviceName,
          username: state.session.username,
        }
      : null,
  };
}

function mapClipboardItem(item: DecryptedClipboardItem): ClipboardItem {
  return {
    created_at: item.createdAt,
    id: item.id,
    mime_type: item.mimeType,
    payload_size: numberFromBigInt(item.payloadSize),
    source_device_id: item.sourceDeviceId,
    text: item.text,
  };
}

function mapFileItem(item: DecryptedFileItem): FileItem {
  return {
    blob_size: numberFromBigInt(item.blobSize),
    created_at: item.createdAt,
    filename: item.filename,
    id: item.id,
    mime_type: item.mimeType,
    source_device_id: item.sourceDeviceId,
  };
}

function mapDeviceInfo(device: NativeDeviceInfo): DeviceInfo {
  return {
    created_at: device.createdAt,
    id: device.id,
    is_current: device.isCurrent,
    last_seen_at: device.lastSeenAt,
    name: device.name,
    platform: device.platform,
  };
}

function mapClipboardPayload(payload: NativeClipboardPayload): ClipboardPayload {
  return {
    bytes: new Uint8Array(payload.bytes),
    mimeType: payload.mimeType,
    text: payload.text ?? null,
  };
}

function mapConnectionStatus(status: NativeConnectionStatus): ConnectionStatus {
  switch (status) {
    case NativeConnectionStatus.Connected:
      return "Connected";
    case NativeConnectionStatus.Connecting:
      return "Connecting";
    case NativeConnectionStatus.DaemonNotRunning:
      return "DaemonNotRunning";
    case NativeConnectionStatus.Disconnected:
      return "Disconnected";
  }
}

function numberFromBigInt(value: bigint): number {
  // Defensive: a non-conformant or buggy same-user device could encode an
  // absurd (>= 2^53) or negative size in AEAD-authenticated metadata. Clamp
  // per item instead of throwing so a single malformed item cannot poison the
  // whole getState() mapping and brick the UI.
  const numberValue = Number(value);
  if (!Number.isFinite(numberValue) || numberValue < 0) {
    return 0;
  }
  if (numberValue > Number.MAX_SAFE_INTEGER) {
    return Number.MAX_SAFE_INTEGER;
  }
  return numberValue;
}

function arrayBufferFrom(bytes: Uint8Array): ArrayBuffer {
  const copy = new Uint8Array(bytes.byteLength);
  copy.set(bytes);
  return copy.buffer;
}

async function delay(milliseconds: number): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, milliseconds));
}
