const path = require("path");
const { mergeConfig } = require("@expo/metro/metro-config");
const { getDefaultConfig } = require("expo/metro-config");

const projectRoot = __dirname;
const workspaceRoot = path.resolve(projectRoot, "..");
const config = getDefaultConfig(projectRoot);

// On macOS, Metro's multi-process transform pool intermittently aborts Node on
// teardown during the release bundle ("Assertion failed: (errno == EINTR) ...
// uv__io_poll, kqueue.c" / "EBADF: bad file descriptor, write"), which fails
// Gradle's createBundleReleaseJsAndAssets non-deterministically. Transforming
// in a single in-process worker removes the worker-pipe fd churn that triggers
// it, at a small bundle-time cost. The bug is macOS/kqueue-specific, so other
// platforms keep full parallelism. See https://github.com/nodejs/node/issues/47241.
const maxWorkers = process.platform === "darwin" ? 1 : config.maxWorkers;

// lib0 (yjs's utility layer) publishes a `react-native` export for its Web
// Crypto shim that imports `isomorphic-webcrypto`, an unmaintained package this
// app does not depend on — Metro prefers that condition and the release bundle
// fails to resolve it. Point the module at lib0's browser build instead, which
// just reads the global `crypto`; `src/webcryptoPolyfill.ts` provides that
// global from expo-crypto before anything touches yjs.
// lib0's `exports` map does not publish `./webcrypto.js` as a subpath, so
// locate the package root (`./package.json` is always exported) and join it.
const lib0Webcrypto = path.join(
  path.dirname(
    require.resolve("lib0/package.json", {
      paths: [
        path.resolve(projectRoot, "node_modules"),
        path.resolve(workspaceRoot, "node_modules"),
      ],
    }),
  ),
  "webcrypto.js",
);
const defaultResolveRequest = config.resolver.resolveRequest;

module.exports = mergeConfig(config, {
  watchFolders: [workspaceRoot],
  maxWorkers,
  resolver: {
    nodeModulesPaths: [
      path.resolve(projectRoot, "node_modules"),
      path.resolve(workspaceRoot, "node_modules"),
    ],
    resolveRequest: (context, moduleName, platform) => {
      if (moduleName === "lib0/webcrypto" || moduleName === "lib0/webcrypto.js") {
        return { type: "sourceFile", filePath: lib0Webcrypto };
      }
      return (defaultResolveRequest ?? context.resolveRequest)(context, moduleName, platform);
    },
  },
});
