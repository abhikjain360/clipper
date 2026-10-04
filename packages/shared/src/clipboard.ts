export function clipboardImage(mimeType: string, bytes: Uint8Array): string | null {
  if (!mimeType.startsWith("image/")) return null;
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  let encoded = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const a = bytes[i] ?? 0;
    const b = bytes[i + 1] ?? 0;
    const c = bytes[i + 2] ?? 0;
    encoded += alphabet[a >> 2];
    encoded += alphabet[((a & 3) << 4) | (b >> 4)];
    encoded += i + 1 < bytes.length ? alphabet[((b & 15) << 2) | (c >> 6)] : "=";
    encoded += i + 2 < bytes.length ? alphabet[c & 63] : "=";
  }
  return `data:${mimeType};base64,${encoded}`;
}

export function clipboardBytes(bytes: Uint8Array): string {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join(" ");
}
