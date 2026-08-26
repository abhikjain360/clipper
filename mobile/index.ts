// Must precede every other import: it installs `crypto.getRandomValues`, which
// lib0 (and therefore yjs) reads at module scope. See webcryptoPolyfill.ts.
import "./src/webcryptoPolyfill";

import { registerRootComponent } from "expo";
import App from "./src/App";

registerRootComponent(App);
