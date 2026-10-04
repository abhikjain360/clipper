import * as Clipboard from "expo-clipboard";
import * as DocumentPicker from "expo-document-picker";
import { Directory, File, Paths } from "expo-file-system";
import * as SecureStore from "expo-secure-store";
import { CryptoDigestAlgorithm, digestStringAsync } from "expo-crypto";
import * as Sharing from "expo-sharing";
import { createMobileBackend } from "@clipper/mobile-bridge/adapter";
import { readWithDeadline } from "./secureRead";
import { itemClipboard } from "../modules/clipper-clipboard";

const nativeBackend = createMobileBackend({
  dataDir: resolveDataDir(),
  serverUrl: devDefaultServerUrl(),
});
let resumedSession = false;

export const backend = {
  ...nativeBackend,
  getState: async () => {
    const state = await nativeBackend.getState();
    if (state.session)
      itemClipboard?.clearDeleted(state.deleted_clipboard_ids, clipboardScope(state.session));
    if (state.session) resumedSession = true;
    if (resumedSession && !state.session) {
      itemClipboard?.reset();
      resumedSession = false;
      await clearCredentials();
    }
    return state;
  },
  logout: async (cancelRunningWork: boolean) => {
    const outcome = await nativeBackend.logout(cancelRunningWork);
    if (outcome.status === "signed_out") {
      itemClipboard?.reset();
      await clearCredentials();
    }
    return outcome;
  },
};

function clipboardScope(session: { server_url: string; username: string }): string {
  return JSON.stringify([session.server_url, session.username]);
}

// The native engine persists its SQLite store and blobs under this path.
// expo-file-system reports locations as `file://` URIs, but the Rust side
// (`resolve_data_dir`) needs a bare absolute path, so strip the scheme. The
// document directory is app-private and survives across launches (unlike the
// cache directory), and has no platform default on Android — hence we must pass
// it explicitly rather than relying on the native fallback.
function resolveDataDir(): string {
  const dir = new Directory(Paths.document, "clipper-mobile");
  return decodeURIComponent(dir.uri.replace(/^file:\/\//, ""));
}

// Dev builds pin a loopback server for convenience; production ships no default,
// so the login field starts empty and the user enters their own server. The
// native client only permits plain HTTP to loopback hosts (the emulator's
// 10.0.2.2 alias is rejected), so on Android run `adb reverse tcp:8787 tcp:8787`
// to make the device's localhost reach the host server. The native client fixes
// its base URL at construction, so this single value seeds both the client and
// the login form (they must match).
export function devDefaultServerUrl(): string {
  return __DEV__ ? "http://127.0.0.1:8787" : "";
}

export async function readClipboardText(): Promise<string> {
  return await Clipboard.getStringAsync();
}

export async function writeClipboardText(text: string): Promise<void> {
  await Clipboard.setStringAsync(text);
}

export async function captureClipboardItem(): Promise<string | null> {
  const session = (await nativeBackend.getState()).session;
  if (!session) throw new Error("Not logged in");
  const captured = itemClipboard?.read();
  const text = captured?.text ?? (await readClipboardText());
  if (!text) return null;
  const id = await nativeBackend.sendClipboardText(text);
  if (captured)
    itemClipboard?.claim(id, captured.timestamp, clipboardScope(session), captured.token);
  await backend.getState();
  return id;
}

export async function copyClipboardItem(id: string): Promise<void> {
  const session = (await nativeBackend.getState()).session;
  if (!session) throw new Error("Not logged in");
  const payload = await nativeBackend.clipboardPayload(id);
  if (payload.text == null)
    throw new Error(`Cannot copy ${payload.mimeType} to the text clipboard`);
  if (itemClipboard) itemClipboard.install(id, payload.text, clipboardScope(session));
  else await writeClipboardText(payload.text);
  await backend.getState();
}

export async function pickUploadFile(): Promise<{
  bytes: Uint8Array;
  filename: string;
  mimeType: string;
} | null> {
  const result = await DocumentPicker.getDocumentAsync({
    copyToCacheDirectory: true,
    multiple: false,
  });
  if (result.canceled) return null;

  const file = result.assets[0];
  if (!file) return null;

  const pickedFile = new File(file.uri);
  return {
    bytes: await pickedFile.bytes(),
    filename: file.name,
    mimeType: file.mimeType ?? "application/octet-stream",
  };
}

export async function shareDownloadedFile(
  filename: string,
  mimeType: string,
  bytes: Uint8Array,
): Promise<void> {
  const file = new File(Paths.cache, safeCacheFilename(filename));
  try {
    file.create({ intermediates: true, overwrite: true });
    file.write(bytes);

    if (await Sharing.isAvailableAsync()) {
      await Sharing.shareAsync(file.uri, { mimeType });
    }
  } finally {
    try {
      file.delete();
    } catch {
      // The file may not exist when creation fails.
    }
  }
}

export function formatBackendError(error: unknown): string {
  if (error instanceof Error) return error.message;

  if (typeof error === "object" && error !== null) {
    const message = (error as { message?: unknown }).message;
    if (typeof message === "string" && message.length > 0) return message;
  }

  return String(error);
}

function safeCacheFilename(filename: string): string {
  const trimmed = filename.trim();
  const safe = trimmed.length > 0 ? trimmed : "clipper-download";
  return safe.replaceAll(/[^A-Za-z0-9._-]/g, "_");
}

const CREDENTIALS_KEY = "clipper.session.v2";
const CREDENTIALS_FLAG_KEY = "clipper.session.present.v2";
const CONFIRMATION_KEY = "clipper.session.confirmation.v1";
const CONFIRMATION_OPTIONS: SecureStore.SecureStoreOptions = {
  keychainAccessible: SecureStore.WHEN_UNLOCKED_THIS_DEVICE_ONLY,
};
let credentialGeneration = 0;
let confirmationWrite = Promise.resolve();
const SECURE_AUTH_OPTIONS: SecureStore.SecureStoreOptions = {
  requireAuthentication: true,
  authenticationPrompt: "Unlock Clipper",
  keychainAccessible: SecureStore.WHEN_UNLOCKED_THIS_DEVICE_ONLY,
};

type StoredSession = {
  token: string;
  dataKey: string;
  wrappingKey: string;
  username: string;
  deviceName: string;
  serverUrl: string;
};

async function removeLegacyCredentials(): Promise<void> {
  // Retry on every launch, independently of the flag. Do not declare the old
  // secret absent if deleting the protected value itself failed.
  try {
    await SecureStore.deleteItemAsync("clipper.credentials.v1", SECURE_AUTH_OPTIONS);
    await SecureStore.deleteItemAsync("clipper.credentials.present.v1");
  } catch {
    /* retried on the next launch */
  }
}

// Best-effort: missing enrollment or a cancelled prompt must not fail login.
export async function saveCredentials(): Promise<void> {
  try {
    await removeLegacyCredentials();
    const material = await backend.sessionResumeMaterial();
    const session = (await backend.getState()).session;
    if (!material || !session) {
      await clearCredentials();
      return;
    }
    const saved: StoredSession = {
      token: material.token,
      dataKey: material.dataKey,
      wrappingKey: material.wrappingKey,
      username: session.username,
      deviceName: session.device_name,
      serverUrl: session.server_url,
    };
    await SecureStore.setItemAsync(CREDENTIALS_KEY, JSON.stringify(saved), SECURE_AUTH_OPTIONS);
    await saveSessionConfirmation();
    await SecureStore.setItemAsync(CREDENTIALS_FLAG_KEY, "1");
  } catch {
    await clearCredentials();
  }
}

export async function clearCredentials(): Promise<void> {
  resumedSession = false;
  credentialGeneration += 1;
  await confirmationWrite.catch(() => {});
  await removeLegacyCredentials();
  await SecureStore.deleteItemAsync(CREDENTIALS_KEY, SECURE_AUTH_OPTIONS).catch(() => {});
  await SecureStore.deleteItemAsync(CREDENTIALS_FLAG_KEY).catch(() => {});
  await SecureStore.deleteItemAsync(CONFIRMATION_KEY).catch(() => {});
}

export async function saveSessionConfirmation(): Promise<void> {
  const generation = credentialGeneration;
  const write = confirmationWrite.then(async () => {
    const material = await backend.sessionResumeMaterial();
    if (!material?.lastConfirmedAt || generation !== credentialGeneration) return;
    const sessionHash = await digestStringAsync(CryptoDigestAlgorithm.SHA256, material.token);
    if (generation !== credentialGeneration) return;
    if ((await lastSessionConfirmation(material.token)) === material.lastConfirmedAt) return;
    if (generation !== credentialGeneration) return;
    await SecureStore.setItemAsync(
      CONFIRMATION_KEY,
      JSON.stringify({ sessionHash, lastConfirmedAt: material.lastConfirmedAt }),
      CONFIRMATION_OPTIONS,
    );
  });
  confirmationWrite = write.catch(() => {});
  await write;
}

async function lastSessionConfirmation(token: string): Promise<number> {
  try {
    const raw = await SecureStore.getItemAsync(CONFIRMATION_KEY);
    if (!raw) return 0;
    const saved = JSON.parse(raw) as { sessionHash?: unknown; lastConfirmedAt?: unknown };
    const sessionHash = await digestStringAsync(CryptoDigestAlgorithm.SHA256, token);
    return saved.sessionHash === sessionHash &&
      typeof saved.lastConfirmedAt === "number" &&
      Number.isSafeInteger(saved.lastConfirmedAt)
      ? saved.lastConfirmedAt
      : 0;
  } catch {
    return 0;
  }
}

async function readResumeCredentials(): Promise<string | null> {
  return readWithDeadline(() => SecureStore.getItemAsync(CREDENTIALS_KEY, SECURE_AUTH_OPTIONS));
}

function isStoredSession(value: unknown): value is StoredSession {
  if (typeof value !== "object" || value === null || "passphrase" in value) return false;
  return ["token", "dataKey", "wrappingKey", "username", "deviceName", "serverUrl"].every(
    (key) =>
      typeof (value as Record<string, unknown>)[key] === "string" &&
      ((value as Record<string, unknown>)[key] as string).length > 0,
  );
}

export function isResumeRejected(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    "code" in error &&
    error.code === "SESSION_RESUME_REJECTED"
  );
}

export async function resumeSession(): Promise<boolean> {
  await removeLegacyCredentials();
  const flag = await SecureStore.getItemAsync(CREDENTIALS_FLAG_KEY).catch(() => null);
  if (flag !== "1") return false;
  const raw = await readResumeCredentials();
  if (!raw) return false;
  let saved: unknown;
  try {
    saved = JSON.parse(raw);
  } catch {
    await clearCredentials();
    return false;
  }
  if (!isStoredSession(saved)) {
    await clearCredentials();
    return false;
  }
  try {
    await backend.resume(
      saved.token,
      saved.dataKey,
      saved.wrappingKey,
      saved.username,
      saved.deviceName,
      saved.serverUrl,
      await lastSessionConfirmation(saved.token),
    );
    resumedSession = true;
    if (!(await backend.getState()).session) return false;
    await saveSessionConfirmation();
  } catch (error) {
    if (isResumeRejected(error)) {
      await clearCredentials();
    }
    throw error;
  }
  return true;
}
