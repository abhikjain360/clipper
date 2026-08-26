// Install `crypto.getRandomValues` before anything that needs it loads.
//
// React Native ships no Web Crypto, and `lib0` (which yjs is built on) reads
// `crypto.getRandomValues` at module scope — yjs calls it for every `Y.Doc`'s
// client id. lib0's own React Native build resolves that through
// `isomorphic-webcrypto`, an unmaintained package we do not depend on, so
// `metro.config.js` points lib0 at its browser build instead and this supplies
// the global that build expects.
//
// Import this FIRST from the app entry point: module evaluation follows import
// order, so anything importing yjs must come after it.
import { getRandomValues } from "expo-crypto";

type PartialCrypto = { getRandomValues?: unknown };

const existing = (globalThis as { crypto?: PartialCrypto }).crypto;

if (existing === undefined) {
  Object.defineProperty(globalThis, "crypto", {
    value: { getRandomValues },
    configurable: true,
    writable: true,
  });
} else if (typeof existing.getRandomValues !== "function") {
  // A `crypto` object exists but lacks the one function we need (some RN
  // versions expose a partial shim). Fill in the gap rather than replacing it.
  Object.defineProperty(existing, "getRandomValues", {
    value: getRandomValues,
    configurable: true,
    writable: true,
  });
}
