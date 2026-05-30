import {
  directoryExists,
  dirname,
  executableFileExists,
  findRepoRoot,
  joinPath,
  moduleDir,
  nonEmpty,
  resolvePath,
} from "./script-common.ts";
import type { Env } from "./script-common.ts";

const unsetBuildEnv = [
  "SDKROOT",
  "SDKROOT_FOR_BUILD",
  "DEVELOPER_DIR",
  "DEVELOPER_DIR_FOR_BUILD",
  "NIX_CFLAGS_COMPILE",
  "NIX_LDFLAGS",
  "NIX_CC",
  "NIX_CC_WRAPPER_TARGET_HOST_arm64_apple_darwin",
  "NIX_BINTOOLS",
  "NIX_BINTOOLS_WRAPPER_TARGET_HOST_arm64_apple_darwin",
  "NIX_PKG_CONFIG_WRAPPER_TARGET_HOST_arm64_apple_darwin",
  "CC",
  "CXX",
  "LD",
] as const;

const hostFlutterCandidates = [
  "/opt/homebrew/bin/flutter",
  "/usr/local/bin/flutter",
] as const;

const hostPathParts = [
  "/usr/bin",
  "/bin",
  "/usr/sbin",
  "/sbin",
  "/opt/homebrew/bin",
  "/opt/homebrew/sbin",
  "/usr/local/bin",
  "/usr/local/sbin",
] as const;

type BuildConfig = {
  readonly appDir: string;
  readonly args: readonly string[];
  readonly cargo: string;
  readonly commandEnv: Record<string, string>;
  readonly flutterSwiftPackageManager: string;
  readonly hostFlutter: string;
  readonly hostFlutterRoot?: string;
  readonly rustc: string;
};

function requireEnv(env: Env, name: string): string {
  const value = nonEmpty(env[name]);
  if (value === undefined) {
    throw new Error(`Required environment variable missing: ${name}`);
  }
  return value;
}

async function realDirname(path: string): Promise<string> {
  return await Deno.realPath(dirname(path));
}

function resolveExecutableFromApp(
  appDir: string,
  rawPath: string,
): string {
  return resolvePath(appDir, rawPath);
}

async function defaultHostFlutter(env: Env, appDir: string): Promise<string> {
  const configured = nonEmpty(env.CLIPPER_HOST_FLUTTER);
  if (configured !== undefined) {
    return resolveExecutableFromApp(appDir, configured);
  }

  for (const candidate of hostFlutterCandidates) {
    if (await executableFileExists(candidate)) return candidate;
  }

  throw new Error(
    "Host Flutter was not found. Install Flutter outside Nix or set CLIPPER_HOST_FLUTTER.",
  );
}

async function hostFlutterRoot(
  env: Env,
  appDir: string,
  hostFlutter: string,
): Promise<string | undefined> {
  const configured = nonEmpty(env.CLIPPER_HOST_FLUTTER_ROOT);
  if (configured !== undefined) return resolvePath(appDir, configured);

  switch (hostFlutter) {
    case "/opt/homebrew/bin/flutter":
      return "/opt/homebrew/share/flutter";
    case "/usr/local/bin/flutter":
      return "/usr/local/share/flutter";
  }

  if (hostFlutter.endsWith("/bin/flutter")) {
    return await Deno.realPath(joinPath(dirname(hostFlutter), ".."));
  }

  return undefined;
}

async function validateHostFlutter(
  hostFlutter: string,
  hostFlutterRootValue: string | undefined,
): Promise<void> {
  if (!(await executableFileExists(hostFlutter))) {
    throw new Error(
      "Host Flutter was not found. Install Flutter outside Nix or set CLIPPER_HOST_FLUTTER.",
    );
  }

  if (hostFlutter.startsWith("/nix/store/")) {
    throw new Error(
      `macOS packaging needs a writable host Flutter SDK, not Nix Flutter: ${hostFlutter}`,
    );
  }

  if (
    hostFlutterRootValue !== undefined &&
    !(await directoryExists(
      joinPath(hostFlutterRootValue, "packages/flutter_tools"),
    ))
  ) {
    throw new Error(
      [
        `Host Flutter root does not look valid: ${hostFlutterRootValue}`,
        "Set CLIPPER_HOST_FLUTTER_ROOT if CLIPPER_HOST_FLUTTER is a wrapper or symlink.",
      ].join("\n"),
    );
  }
}

async function buildPath(
  env: Env,
  hostFlutter: string,
  stableBin: string,
): Promise<string> {
  const inheritedPath = nonEmpty(env.PATH);
  const pathParts = [
    ...hostPathParts,
    await realDirname(hostFlutter),
    stableBin,
    ...(inheritedPath === undefined ? [] : [inheritedPath]),
  ];

  return pathParts.join(":");
}

function buildCommandEnv(
  env: Env,
  options: {
    readonly cargo: string;
    readonly flutterRoot?: string;
    readonly flutterSwiftPackageManager: string;
    readonly path: string;
    readonly rustc: string;
    readonly stableBin: string;
  },
): Record<string, string> {
  const commandEnv: Record<string, string> = { ...env };

  for (const name of unsetBuildEnv) {
    delete commandEnv[name];
  }

  commandEnv.PATH = options.path;
  commandEnv.CARGOKIT_CARGO = options.cargo;
  commandEnv.CARGOKIT_RUSTC = options.rustc;
  commandEnv.CLIPPER_STABLE_BIN = options.stableBin;
  commandEnv.FLUTTER_SWIFT_PACKAGE_MANAGER = options.flutterSwiftPackageManager;

  if (options.flutterRoot === undefined) {
    delete commandEnv.FLUTTER_ROOT;
  } else {
    commandEnv.FLUTTER_ROOT = options.flutterRoot;
  }

  return commandEnv;
}

async function buildConfig(args: readonly string[]): Promise<BuildConfig> {
  if (Deno.build.os !== "darwin") {
    throw new Error(
      "macOS app builds require Darwin/Xcode; run this on macOS.",
    );
  }

  const env = Deno.env.toObject();
  const scriptDir = moduleDir(import.meta.url);
  const repoRoot = await findRepoRoot(env, [joinPath(scriptDir, "..")]);
  const appDir = joinPath(repoRoot, "app");
  const stableBin = resolvePath(appDir, requireEnv(env, "CLIPPER_STABLE_BIN"));
  const cargo = resolveExecutableFromApp(
    appDir,
    nonEmpty(env.CARGOKIT_CARGO) ?? joinPath(stableBin, "cargo"),
  );
  const rustc = resolveExecutableFromApp(
    appDir,
    nonEmpty(env.CARGOKIT_RUSTC) ?? joinPath(stableBin, "rustc"),
  );

  if (!(await executableFileExists(cargo))) {
    throw new Error(`CARGOKIT_CARGO is not executable: ${cargo}`);
  }

  if (!(await executableFileExists(rustc))) {
    throw new Error(`CARGOKIT_RUSTC is not executable: ${rustc}`);
  }

  const hostFlutter = await defaultHostFlutter(env, appDir);
  const hostFlutterRootValue = await hostFlutterRoot(env, appDir, hostFlutter);
  await validateHostFlutter(hostFlutter, hostFlutterRootValue);

  const flutterSwiftPackageManager = nonEmpty(
    env.CLIPPER_FLUTTER_SWIFT_PACKAGE_MANAGER,
  ) ?? "false";
  const commandPath = await buildPath(env, hostFlutter, stableBin);
  const buildArgs = args.length === 0 ? ["--debug"] : [...args];
  const commandEnv = buildCommandEnv(env, {
    cargo,
    flutterRoot: hostFlutterRootValue,
    flutterSwiftPackageManager,
    path: commandPath,
    rustc,
    stableBin,
  });

  return {
    appDir,
    args: buildArgs,
    cargo,
    commandEnv,
    flutterSwiftPackageManager,
    hostFlutter,
    hostFlutterRoot: hostFlutterRootValue,
    rustc,
  };
}

async function runBuild(config: BuildConfig): Promise<number> {
  console.log(`Using host Flutter: ${config.hostFlutter}`);
  if (config.hostFlutterRoot !== undefined) {
    console.log(`Using host Flutter root: ${config.hostFlutterRoot}`);
  }
  console.log(
    `Using Flutter Swift Package Manager: ${config.flutterSwiftPackageManager}`,
  );
  console.log(`Using Nix cargo for Rust: ${config.cargo}`);
  console.log(`Using Nix rustc for Rust: ${config.rustc}`);

  const command = new Deno.Command(config.hostFlutter, {
    args: ["build", "macos", ...config.args],
    clearEnv: true,
    cwd: config.appDir,
    env: config.commandEnv,
    stdin: "inherit",
    stdout: "inherit",
    stderr: "inherit",
  });

  const status = await command.spawn().status;
  return status.code;
}

try {
  const config = await buildConfig(Deno.args);
  Deno.exit(await runBuild(config));
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  Deno.exit(1);
}
