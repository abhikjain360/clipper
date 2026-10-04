import assert from "node:assert/strict";
import { test } from "node:test";
import { filePreview } from "../../packages/shared/src/file-preview.ts";

function decode(bytes: Uint8Array): string {
    return new TextDecoder().decode(bytes);
}

test("large and unsupported files open details without downloading bytes", async () => {
    await Promise.all(
        [
            { mime_type: "video/mp4", blob_size: 512 * 1024 * 1024 },
            { mime_type: "application/pdf", blob_size: 100 },
            { mime_type: "text/plain", blob_size: 1024 * 1024 + 1 },
            { mime_type: "image/png", blob_size: 4 * 1024 * 1024 + 1 },
            { mime_type: "image/svg+xml", blob_size: 100 },
        ].map(async (file) => {
            let downloads = 0;
            assert.equal(
                await filePreview(
                    file,
                    async () => {
                        downloads++;
                        return new Uint8Array();
                    },
                    () => "",
                ),
                null,
            );
            assert.equal(downloads, 0);
        }),
    );
});

test("small text and supported images render their content", async () => {
    const bytes = new TextEncoder().encode("full text");
    const download = async () => bytes;
    assert.deepEqual(
        await filePreview(
            { mime_type: "text/plain; charset=utf-8", blob_size: bytes.length },
            download,
            decode,
        ),
        { content: "full text", image: null },
    );
    const image = await filePreview(
        { mime_type: "image/png", blob_size: bytes.length },
        download,
        decode,
    );
    assert.equal(image?.content, "");
    assert.equal(image?.image, `data:image/png;base64,${Buffer.from(bytes).toString("base64")}`);
});
