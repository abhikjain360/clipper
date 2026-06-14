import type { ClipperBackend } from "./types";
import wasmInit, * as wasmModule from "../generated/wasm/clipper_web_wasm";

type WasmModule = ClipperBackend & {
    default: (
        input?: string | URL | Request | Response | BufferSource | WebAssembly.Module,
    ) => Promise<unknown>;
    initClient: () => void;
};

let modulePromise: Promise<WasmModule> | undefined;

export function clipperWasm(): Promise<WasmModule> {
    modulePromise ??= loadModule().catch((error) => {
        // Allow a retry after a transient wasm fetch/instantiate failure rather
        // than caching the rejection for the whole session.
        modulePromise = undefined;
        throw error;
    });
    return modulePromise;
}

async function loadModule(): Promise<WasmModule> {
    const mod = wasmModule as unknown as WasmModule;
    await wasmInit();
    mod.initClient();
    return mod;
}

export async function defaultServerUrl(): Promise<string> {
    return (await clipperWasm()).defaultServerUrl();
}
