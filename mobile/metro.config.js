const path = require("path");
const { getDefaultConfig } = require("expo/metro-config");

const projectRoot = __dirname;
const workspaceRoot = path.resolve(projectRoot, "..");
const config = getDefaultConfig(projectRoot);

config.watchFolders = [workspaceRoot];
config.resolver.nodeModulesPaths = [
  path.resolve(projectRoot, "node_modules"),
  path.resolve(workspaceRoot, "node_modules"),
];

// On macOS, Metro's multi-process transform pool intermittently aborts Node on
// teardown during the release bundle ("Assertion failed: (errno == EINTR) ...
// uv__io_poll, kqueue.c" / "EBADF: bad file descriptor, write"), which fails
// Gradle's createBundleReleaseJsAndAssets non-deterministically. Transforming
// in a single in-process worker removes the worker-pipe fd churn that triggers
// it, at a small bundle-time cost. The bug is macOS/kqueue-specific, so other
// platforms keep full parallelism. See https://github.com/nodejs/node/issues/47241.
if (process.platform === "darwin") {
  config.maxWorkers = 1;
}

module.exports = config;
