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

export type AuthenticatedSession = {
  username: string;
  device_id: string;
  device_name: string;
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
  error?: string | null;
};

export type ClipboardPayload = {
  mimeType: string;
  bytes: Uint8Array;
  text: string | null;
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
  waitForStateChange: (seenVersion: number) => Promise<number>;
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
};
