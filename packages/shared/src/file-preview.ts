import { clipboardImage } from "./clipboard.ts";

type PreviewFile = { mime_type: string; blob_size: number };

export function filePreviewType(file: PreviewFile): "text" | "image" | null {
  const mime = file.mime_type.toLowerCase().split(";")[0]?.trim() ?? "";
  if (file.blob_size < 0 || !Number.isSafeInteger(file.blob_size)) return null;
  if (mime.startsWith("text/") && file.blob_size <= 1024 * 1024) return "text";
  if (
    ["image/png", "image/jpeg", "image/gif", "image/webp"].includes(mime) &&
    file.blob_size <= 4 * 1024 * 1024
  )
    return "image";
  return null;
}

export async function filePreview(
  file: PreviewFile,
  download: () => Promise<Uint8Array>,
  decode: (bytes: Uint8Array) => string,
): Promise<{ content: string; image: string | null } | null> {
  const type = filePreviewType(file);
  if (!type) return null;
  const bytes = await download();
  if (!filePreviewType({ ...file, blob_size: bytes.length })) return null;
  return {
    content: type === "text" ? decode(bytes) : "",
    image:
      type === "image"
        ? clipboardImage(file.mime_type.toLowerCase().split(";")[0]!.trim(), bytes)
        : null,
  };
}
