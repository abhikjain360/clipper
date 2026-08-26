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
