import "./index";

import {
  ConnectionStatus as NativeConnectionStatus,
  type AppState as NativeAppState,
  type ClipboardPayload as NativeClipboardPayload,
  type CollabItem as NativeCollabItem,
  type DecryptedClipboardItem,
  type DecryptedFileItem,
  type DeviceInfo as NativeDeviceInfo,
  type ActualView as NativeActualView,
  type AlarmView as NativeAlarmView,
  type CalendarSourceView as NativeCalendarSourceView,
  type ScheduleItemView as NativeScheduleItemView,
} from "./generated/clipper_app_types";
import {
  MobileClipperClient,
  MobileError,
  type MobileClipperClientLike,
} from "./generated/clipper_mobile_uniffi";
import type {
  ActualView,
  AlarmView,
  AppState,
  ClipboardItem,
  ClipboardPayload,
  ClipperBackend,
  CollabItem,
  ConnectionStatus,
  DeviceInfo,
  CalendarSourceView,
  FileItem,
  ScheduleItemView,
} from "@clipper/shared";

export interface CreateMobileBackendOptions {
  /**
   * Absolute filesystem path for the engine's local store. Required on Android:
   * the native `dirs::data_dir()` returns `None` there, so the Rust side cannot
   * resolve a relative/empty path and fails with `DataDirUnavailable`. The
   * caller (which owns the platform filesystem API) must supply this.
   */
  dataDir?: string;
  /**
   * Server base URL the native client is pinned to at construction. The engine
   * fixes its base URL at init, so this must match the URL later passed to
   * `login`/`register`; the app sources both from one place.
   */
  serverUrl?: string;
  /** Inject a pre-built native client (tests); takes precedence over the above. */
  client?: MobileClipperClientLike;
}

export function createMobileBackend(options: CreateMobileBackendOptions = {}): ClipperBackend {
  // Empty strings let the Rust constructor apply its own defaults (server URL,
  // device name) via `non_empty_or_default`; `dataDir` must be a real absolute
  // path on Android.
  const dataDir = options.dataDir ?? "";
  const injected = options.client != null;
  let currentServerUrl = options.serverUrl ?? "";
  let client =
    options.client ??
    new MobileClipperClient(currentServerUrl, dataDir, "Android-Clipper", "android");

  // The native client pins its server URL at construction: the engine fixes its
  // base URL at init and `login`/`register` reject a different URL. Production
  // ships no default, so the user supplies the server at auth time — re-point the
  // client by reconstructing it (a fresh engine over the same data dir, the
  // in-process analog of the daemon respawning) the first time the URL differs.
  // The caller restarts its state-watch loop when the session changes, so it
  // follows the new client. An injected client (tests) is never replaced.
  function clientFor(serverUrl: string): MobileClipperClientLike {
    const target = serverUrl.trim();
    if (!injected && target.length > 0 && target !== currentServerUrl) {
      client = new MobileClipperClient(target, dataDir, "Android-Clipper", "android");
      currentServerUrl = target;
    }
    return client;
  }
  // Every networked method on the native client is now an async UniFFI export
  // mapped to a JS Promise, so awaiting it yields the React Native JS thread for
  // the whole call instead of blocking it (the former busy-poll/`block_on`
  // shape caused ANRs on slow/hostile servers).
  return {
    clipboardPayload: async (id) => mapClipboardPayload(await client.clipboardPayload(id)),
    connect: async () => client.connect(),
    createCollabDoc: async () => mapCollabItem(await client.createCollabDoc()),
    defaultServerUrl: () => client.defaultServerUrl(),
    deleteCollabDoc: async (objectId) => client.deleteCollabDoc(objectId),
    deleteFile: async (fileId) => client.deleteFile(fileId),
    downloadFileBytes: async (fileId) => new Uint8Array(await client.downloadFileBytes(fileId)),
    getCollabDocMeta: async (objectId) => mapCollabItem(await client.getCollabDocMeta(objectId)),
    getState: async () => mapAppState(await client.getState()),
    listDevices: async () => (await client.listDevices()).map(mapDeviceInfo),
    login: async (passphrase, username, deviceName, serverUrl) =>
      clientFor(serverUrl).login(passphrase, username, deviceName, serverUrl),
    logout: async () => client.logout(),
    refresh: async () => client.refresh(),
    register: async (accessKey, username, passphrase, deviceName, serverUrl) =>
      clientFor(serverUrl).register(accessKey, username, passphrase, deviceName, serverUrl),
    removeDevice: async (deviceId) => client.removeDevice(deviceId),
    renameCollabDoc: async (objectId, title) =>
      mapCollabItem(await client.renameCollabDoc(objectId, title)),
    // Schedule editing is not on mobile yet: the web and desktop grid comes
    // first (docs/schedule-plan.md, D11), and these three carry the schedule
    // domain types, which UniFFI cannot express without flattening them into
    // strings. Series still *sync* to this device and appear in
    // `state.scheduleItems` — only creating and expanding are missing. These
    // throw rather than silently no-op so a premature caller is obvious.
    nextAlarms: async (withinHours, observerZone) =>
      (await client.nextAlarms(withinHours, observerZone)).map(mapAlarmView),
    addCalendarSource: async () => {
      throw new Error("Adding a calendar source is not available on mobile yet");
    },
    syncCalendarSource: async () => {
      throw new Error("Syncing a calendar source is not available on mobile yet");
    },
    startActual: async () => {
      throw new Error("The timer is not available on mobile yet");
    },
    stopActual: async () => {
      throw new Error("The timer is not available on mobile yet");
    },
    actualsBetween: async () => [],
    updateScheduleItem: async () => {
      throw new Error("Editing schedule items is not available on mobile yet");
    },
    createScheduleItem: async () => {
      throw new Error("Creating schedule items is not available on mobile yet");
    },
    deleteScheduleObject: async () => {
      throw new Error("Deleting schedule items is not available on mobile yet");
    },
    expandSchedule: async () => {
      throw new Error("Expanding the schedule is not available on mobile yet");
    },
    resume: async (token, dataKey, wrappingKey, username, deviceName, serverUrl) => {
      try {
        await clientFor(serverUrl).resume(
          token,
          dataKey,
          wrappingKey,
          username,
          deviceName,
          serverUrl,
        );
      } catch (error) {
        if (
          MobileError.SessionResumeRejected.instanceOf(error) ||
          MobileError.InvalidResumeKey.instanceOf(error)
        ) {
          throw Object.assign(new Error("Saved session is no longer valid; sign in again"), {
            code: "SESSION_RESUME_REJECTED",
          });
        }
        throw error;
      }
    },
    sendClipboardPayload: async (mimeType, bytes) =>
      client.sendClipboardPayload(mimeType, arrayBufferFrom(bytes)),
    sendClipboardText: async (text) => client.sendClipboardText(text),
    sessionResumeMaterial: async () => {
      const material = await client.sessionResumeMaterial();
      return material
        ? { token: material.token, dataKey: material.dataKey, wrappingKey: material.wrappingKey }
        : null;
    },
    stateVersion: () => client.stateVersion(),
    uploadFileBytes: async (filename, mimeType, bytes) =>
      client.uploadFileBytes(filename, mimeType, arrayBufferFrom(bytes)),
    // Suspends on the engine's state `watch` channel native-side until the
    // version actually advances — no 250 ms polling loop. Forward the optional
    // AbortSignal so a teardown can cancel the in-flight UniFFI future instead
    // of leaking it past unmount.
    waitForStateChange: async (seenVersion, signal) =>
      client.waitForStateChange(seenVersion, signal ? { signal } : undefined),
  };
}

export default createMobileBackend;

function mapAppState(state: NativeAppState): AppState {
  return {
    clipboard_items: state.clipboardItems.map(mapClipboardItem),
    collab_docs: state.collabDocs.map(mapCollabItem),
    connection_status: mapConnectionStatus(state.connectionStatus),
    error: state.error ?? null,
    files: state.files.map(mapFileItem),
    calendar_sources: state.calendarSources.map(mapCalendarSourceView),
    running_actual: state.runningActual ? mapActualView(state.runningActual) : null,
    schedule_items: state.scheduleItems.map(mapScheduleItemView),
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
          server_url: state.session.serverUrl,
          username: state.session.username,
        }
      : null,
  };
}

function mapCollabItem(item: NativeCollabItem): CollabItem {
  return {
    created_at: item.createdAt,
    id: item.id,
    share_token: item.shareToken,
    share_url: item.shareUrl ?? null,
    title: item.title,
    updated_at: item.updatedAt,
  };
}

function mapAlarmView(alarm: NativeAlarmView): AlarmView {
  return {
    // UniFFI maps Rust's i64 to bigint. Epoch milliseconds sit far inside
    // Number.MAX_SAFE_INTEGER, and the platform alarm APIs want a plain number.
    fire_at_millis: Number(alarm.fireAtMillis),
    item_id: alarm.itemId,
    label: alarm.label,
    occurrence_key: alarm.occurrenceKey,
    occurrence_start_millis: Number(alarm.occurrenceStartMillis),
  };
}

function mapActualView(actual: NativeActualView): ActualView {
  return {
    end: actual.end,
    id: actual.id,
    item_id: actual.itemId,
    running: actual.running,
    start: actual.start,
    title: actual.title,
  };
}

function mapCalendarSourceView(source: NativeCalendarSourceView): CalendarSourceView {
  return {
    enabled: source.enabled,
    event_count: source.eventCount,
    id: source.id,
    location: source.location,
    name: source.name,
    protocol: source.protocol,
  };
}

function mapScheduleItemView(item: NativeScheduleItemView): ScheduleItemView {
  return {
    all_day: item.allDay,
    created_at: item.createdAt,
    definition_json: item.definitionJson,
    has_alarm: item.hasAlarm,
    id: item.id,
    recurrence: item.recurrence,
    time_summary: item.timeSummary,
    title: item.title,
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
