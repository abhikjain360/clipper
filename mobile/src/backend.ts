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

// The public URL a collab doc's share token resolves to on the server's web UI
// (`/s/:share_token`). NOTE: this uses the configured (dev) server origin; a
// shareable public origin for production builds is still to be wired up.
export function collabShareLink(shareToken: string): string {
  const origin = devDefaultServerUrl();
  return origin ? `${origin}/s/${shareToken}` : `/s/${shareToken}`;
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
  file.create({ intermediates: true, overwrite: true });
  file.write(bytes);

  if (await Sharing.isAvailableAsync()) {
    await Sharing.shareAsync(file.uri, { mimeType });
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

// ── Session persistence (Android Keystore, biometric-gated) ──
//
// The passphrase is otherwise never written to disk (the E2E encryption keys
// derive from it). To resume without re-typing it, the login credentials are
// stashed in the Keystore behind device authentication (`requireAuthentication`),
// so they can only be read after a fingerprint/PIN unlock. A separate, non-gated
// flag lets startup detect stored creds WITHOUT triggering a biometric prompt for
// users who have never logged in.

const CREDENTIALS_KEY = "clipper.credentials.v1";
const CREDENTIALS_FLAG_KEY = "clipper.credentials.present.v1";

const SECURE_AUTH_OPTIONS: SecureStore.SecureStoreOptions = {
  requireAuthentication: true,
  authenticationPrompt: "Unlock Clipper",
  keychainAccessible: SecureStore.WHEN_UNLOCKED_THIS_DEVICE_ONLY,
};

type StoredCredentials = {
  passphrase: string;
  username: string;
  deviceName: string;
  serverUrl: string;
};

// Persist credentials behind biometric/PIN. Best-effort: a missing biometric
// enrollment or a cancelled prompt just leaves nothing stored, so the login it
// follows still succeeds and the user simply logs in manually next launch.
export async function saveCredentials(creds: StoredCredentials): Promise<void> {
  try {
    await SecureStore.setItemAsync(CREDENTIALS_KEY, JSON.stringify(creds), SECURE_AUTH_OPTIONS);
    await SecureStore.setItemAsync(CREDENTIALS_FLAG_KEY, "1");
  } catch {
    await clearCredentials();
  }
}

export async function clearCredentials(): Promise<void> {
  await SecureStore.deleteItemAsync(CREDENTIALS_KEY, SECURE_AUTH_OPTIONS).catch(() => {});
  await SecureStore.deleteItemAsync(CREDENTIALS_FLAG_KEY).catch(() => {});
}

// Reads the non-gated flag so it never triggers a biometric prompt on its own.
async function hasStoredCredentials(): Promise<boolean> {
  const flag = await SecureStore.getItemAsync(CREDENTIALS_FLAG_KEY).catch(() => null);
  return flag === "1";
}

// Cold-start resume: if credentials are stored, prompt the biometric unlock and
// replay login. Returns true on success. Never throws for the "nothing stored"
// or "user cancelled" paths — callers fall back to the manual login screen.
export async function resumeSession(): Promise<boolean> {
  if (!(await hasStoredCredentials())) return false;

  let raw: string | null;
  try {
    raw = await SecureStore.getItemAsync(CREDENTIALS_KEY, SECURE_AUTH_OPTIONS);
  } catch {
    // Biometric cancelled/failed, or the Keystore key was invalidated by a
    // biometric-enrollment change. Fall back to manual login.
    return false;
  }
  if (!raw) return false;

  let creds: StoredCredentials;
  try {
    creds = JSON.parse(raw) as StoredCredentials;
  } catch {
    await clearCredentials();
    return false;
  }

  await backend.login(creds.passphrase, creds.username, creds.deviceName, creds.serverUrl);
  return true;
}
