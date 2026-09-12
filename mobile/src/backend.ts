import * as Clipboard from "expo-clipboard";
import * as DocumentPicker from "expo-document-picker";
import { Directory, File, Paths } from "expo-file-system";
import * as SecureStore from "expo-secure-store";
import * as Sharing from "expo-sharing";
import { createMobileBackend } from "@clipper/mobile-bridge/adapter";

export const backend = createMobileBackend({
  dataDir: resolveDataDir(),
  serverUrl: devDefaultServerUrl(),
});

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

// Session persistence uses a revocable bearer and derived keys, never the
// passphrase. SecureStore gates access behind device authentication. Revoking
// the session prevents resume; data-key rotation remains a separate concern.
const CREDENTIALS_KEY = "clipper.session.v2";
const CREDENTIALS_FLAG_KEY = "clipper.session.present.v2";
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
      ...material,
      username: session.username,
      deviceName: session.device_name,
      serverUrl: session.server_url,
    };
    await SecureStore.setItemAsync(CREDENTIALS_KEY, JSON.stringify(saved), SECURE_AUTH_OPTIONS);
    await SecureStore.setItemAsync(CREDENTIALS_FLAG_KEY, "1");
  } catch {
    await clearCredentials();
  }
}

export async function clearCredentials(): Promise<void> {
  await removeLegacyCredentials();
  await SecureStore.deleteItemAsync(CREDENTIALS_KEY, SECURE_AUTH_OPTIONS).catch(() => {});
  await SecureStore.deleteItemAsync(CREDENTIALS_FLAG_KEY).catch(() => {});
}

function isStoredSession(value: unknown): value is StoredSession {
  if (typeof value !== "object" || value === null || "passphrase" in value) return false;
  return ["token", "dataKey", "wrappingKey", "username", "deviceName", "serverUrl"].every(
    (key) =>
      typeof (value as Record<string, unknown>)[key] === "string" &&
      ((value as Record<string, unknown>)[key] as string).length > 0,
  );
}

// A fresh process prompts once. Cancellation falls back to manual login; it
// never replays OPAQUE with a stored passphrase or registers another device.
export async function resumeSession(): Promise<boolean> {
  await removeLegacyCredentials();
  const flag = await SecureStore.getItemAsync(CREDENTIALS_FLAG_KEY).catch(() => null);
  if (flag !== "1") return false;
  let raw: string | null;
  try {
    raw = await SecureStore.getItemAsync(CREDENTIALS_KEY, SECURE_AUTH_OPTIONS);
  } catch {
    return false;
  }
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
    );
  } catch (error) {
    if (
      typeof error === "object" &&
      error !== null &&
      "code" in error &&
      error.code === "SESSION_RESUME_REJECTED"
    ) {
      await clearCredentials();
    }
    throw error;
  }
  return true;
}
