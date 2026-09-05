export type ConnectionStatus = "Disconnected" | "Connecting" | "Connected" | "DaemonNotRunning";

export type ClipboardItem = {
  id: string;
  text: string;
  mime_type: string;
  payload_size: number;
  created_at: string;
  source_device_id: string;
};

export type FileItem = {
  id: string;
  filename: string;
  mime_type: string;
  blob_size: number;
  created_at: string;
  source_device_id: string;
};

export type CollabItem = {
  id: string;
  // Empty for a doc that has never been renamed; render a placeholder for it.
  title: string;
  share_token: string;
  // The server-built public link. Null when the server has no `public_web_url`
  // configured — clients cannot derive it, since the web frontend and the API
  // are separate origins and the native shells have no web origin at all.
  share_url: string | null;
  created_at: string;
  updated_at: string;
};

// ── Schedule ──
//
// These mirror the serde shapes emitted by `crates/schedule`. That crate's
// `the_wire_format_is_self_describing` test pins the JSON, so if these drift the
// Rust side fails first. Every discriminated union below is tagged, and every
// weekday and month name is lower case on both sides.

export type Weekday = "mon" | "tue" | "wed" | "thu" | "fri" | "sat" | "sun";

export type MonthName =
  | "january" | "february" | "march" | "april" | "may" | "june"
  | "july" | "august" | "september" | "october" | "november" | "december";

/// A day of the month, counted from either end. `from_end` with day 1 is the
/// last day, which is how "last day of the month" works without special-casing
/// February.
export type MonthDay =
  | { from: "from_start"; day: number }
  | { from: "from_end"; day: number };

export type NthWeekday =
  | { from: "from_start"; nth: number }
  | { from: "from_end"; nth: number };

export type MonthlyRule =
  | ({ by: "on_day" } & MonthDay)
  | { by: "on_weekday"; nth: NthWeekday; weekday: Weekday };

export type Frequency =
  | { unit: "daily" }
  | { unit: "weekly"; weekdays: Weekday[]; week_start: Weekday }
  | ({ unit: "monthly" } & MonthlyRule)
  | { unit: "yearly"; month: MonthName; day: MonthDay };

export type RecurrenceEnd =
  | { when: "never" }
  // Total occurrences, counting the first.
  | { when: "after"; value: number }
  // RFC 3339 instant, inclusive.
  | { when: "on"; value: string };

export type Recurrence =
  | { kind: "once" }
  | { kind: "every"; frequency: Frequency; interval: number; end: RecurrenceEnd };

/// A start with a time of day. Floating follows the device — a 07:00 alarm is
/// 07:00 wherever you wake up — while zoned stays pinned to its IANA zone.
export type TimedStart =
  // Local wall-clock, no zone: "2026-06-10T07:00:00".
  | { kind: "floating"; at: string }
  | { kind: "zoned"; at: { local: string; zone: string } };

/// A block's extent. All-day is a date span, not a number of hours: 29 March
/// 2026 is 23 hours long in Berlin.
export type ScheduleSpan =
  | { kind: "timed"; start: TimedStart; duration: number }
  // `start` is a date, "2026-03-29".
  | { kind: "all_day"; start: string; days: number };

export type ObjectRef = {
  kind: "clipboard" | "file" | "collab" | "schedule";
  id: string;
};

/// A series definition. Stored once however often it repeats.
export type ScheduleItem = {
  id: string;
  title: string;
  span: ScheduleSpan;
  recurrence: Recurrence;
  reference?: ObjectRef | null;
};

/// A series as rendered for a list. Built by the Rust side so every shell shows
/// the same wording.
export type ScheduleItemView = {
  id: string;
  title: string;
  recurrence: string;
  time_summary: string;
  all_day: boolean;
  created_at: string;
};

/// One computed instance, ready to place on a grid. Never stored — the client
/// expands the window it is showing and throws the result away.
export type OccurrenceView = {
  item_id: string;
  title: string;
  // RFC 3339 UTC. Half-open: an occurrence does not include its end instant.
  start: string;
  end: string;
  all_day: boolean;
  // An override moved this off its rule position.
  overridden: boolean;
};

export type AuthenticatedSession = {
  username: string;
  device_id: string;
  device_name: string;
  // The server this session is with. The collab Y-sync WebSocket is opened from
  // the UI rather than through the engine, so it needs the URL the user actually
  // logged in to — not the compiled-in default.
  server_url: string;
};

export type DeviceInfo = {
  id: string;
  name: string;
  platform: string;
  created_at: string;
  last_seen_at: string;
  is_current: boolean;
};

export type SavedProfile = {
  username: string;
  device_name: string;
};

export type AppState = {
  session?: AuthenticatedSession | null;
  saved_profile?: SavedProfile | null;
  connection_status: ConnectionStatus;
  clipboard_items: ClipboardItem[];
  files: FileItem[];
  collab_docs: CollabItem[];
  // Series definitions only. Occurrences depend on the window being shown, so
  // they come from expandSchedule rather than from state.
  schedule_items: ScheduleItemView[];
  error?: string | null;
};

export type ClipboardPayload = {
  mimeType: string;
  bytes: Uint8Array;
  text: string | null;
};

export type SessionResumeMaterial = {
  token: string;
  dataKey: string;
  wrappingKey: string;
};

export type ClipperBackend = {
  connect: () => Promise<void>;
  defaultServerUrl: () => string | Promise<string>;
  login: (
    passphrase: string,
    username: string,
    deviceName: string,
    serverUrl: string,
  ) => Promise<void>;
  register: (
    accessKey: string,
    username: string,
    passphrase: string,
    deviceName: string,
    serverUrl: string,
  ) => Promise<string>;
  logout: () => Promise<void>;
  getState: () => Promise<AppState>;
  stateVersion: () => number | Promise<number>;
  waitForStateChange: (seenVersion: number, signal?: AbortSignal) => Promise<number>;
  refresh: () => Promise<void>;
  sendClipboardText: (text: string) => Promise<string>;
  sendCurrentClipboardText?: () => Promise<string | null>;
  sendClipboardPayload: (mimeType: string, bytes: Uint8Array) => Promise<string>;
  clipboardPayload: (id: string) => Promise<ClipboardPayload>;
  writeClipboardItemText?: (id: string) => Promise<void>;
  uploadFileBytes: (filename: string, mimeType: string, bytes: Uint8Array) => Promise<string>;
  uploadFileFromDialog?: () => Promise<string | null>;
  downloadFileBytes: (fileId: string) => Promise<Uint8Array>;
  downloadFileToDialog?: (fileId: string, defaultFilename: string) => Promise<boolean>;
  deleteFile: (fileId: string) => Promise<void>;
  createScheduleItem: (item: ScheduleItem) => Promise<string>;
  deleteScheduleObject: (objectId: string) => Promise<void>;
  // `observerZone` is an IANA name; it resolves floating and all-day spans,
  // which carry no zone of their own.
  expandSchedule: (from: string, to: string, observerZone: string) => Promise<OccurrenceView[]>;
  createCollabDoc: () => Promise<CollabItem>;
  deleteCollabDoc: (objectId: string) => Promise<void>;
  renameCollabDoc: (objectId: string, title: string) => Promise<CollabItem>;
  getCollabDocMeta: (objectId: string) => Promise<CollabItem>;
  listDevices: () => Promise<DeviceInfo[]>;
  removeDevice: (deviceId: string) => Promise<void>;
  // Browser session resume. `sessionResumeMaterial` snapshots the bearer token
  // and OPAQUE-derived keys (never the passphrase) after login; `resume`
  // re-mounts the session from them on reload without an OPAQUE login. Under
  // Tauri these are inert — the desktop daemon owns the session.
  resume: (
    token: string,
    dataKey: string,
    wrappingKey: string,
    username: string,
    deviceName: string,
    serverUrl: string,
  ) => Promise<void>;
  sessionResumeMaterial: () => Promise<SessionResumeMaterial | null>;
};
