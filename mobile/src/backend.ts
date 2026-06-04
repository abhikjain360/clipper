import * as Clipboard from "expo-clipboard";
import * as DocumentPicker from "expo-document-picker";
import { File, Paths } from "expo-file-system";
import * as Sharing from "expo-sharing";
import { createMobileBackend } from "@clipper/mobile-bridge/adapter";

export const backend = createMobileBackend();

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
