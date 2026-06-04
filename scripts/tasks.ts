import {
  findRepoRoot,
  joinPath,
  moduleDir,
  requireEnv,
  requireExecutableFromPath,
  runCommand,
  useNightlyToolchain,
  useStableToolchain,
} from "./script-common.ts";
import type { Env } from "./script-common.ts";

type TaskContext = {
  readonly env: Env;
  readonly repoRoot: string;
};

type TaskHandler = (context: TaskContext, args: readonly string[]) => Promise<void>;

const webTaskScripts: Readonly<Record<string, string>> = {
  "web-build": "build",
  "web-serve": "dev",
  "tauri-dev": "tauri:dev",
  "tauri-build": "tauri:build",
};

const mobileTaskScripts: Readonly<Record<string, string>> = {
  "mobile-start": "start",
  "mobile-android": "android",
};

async function command(name: string, env: Env): Promise<string> {
  return await requireExecutableFromPath(name, env);
}

async function runFmt(
  { env: initialEnv, repoRoot }: TaskContext,
  args: readonly string[],
): Promise<void> {
  if (args.length > 0) {
    throw new Error("fmt does not accept arguments");
  }

  const env = useNightlyToolchain(initialEnv);
  await runCommand(await command("nixfmt", env), ["flake.nix"], {
    cwd: repoRoot,
    env,
  });
  await runCommand(await command("cargo", env), ["fmt", "--all"], {
    cwd: repoRoot,
    env,
  });
  await runRootPnpmScript(repoRoot, env, "fmt", []);
}

async function runRustfmt(
  { env: initialEnv, repoRoot }: TaskContext,
  args: readonly string[],
): Promise<void> {
  const env = useNightlyToolchain(initialEnv);
  await runCommand(await command("cargo", env), ["fmt", "--all", ...args], {
    cwd: repoRoot,
    env,
  });
}

async function runAudit(
  { env: initialEnv, repoRoot }: TaskContext,
  args: readonly string[],
): Promise<void> {
  const env = useStableToolchain(initialEnv);
  const scannerArgs = args.length === 0 ? ["scan", "source", "-r", repoRoot] : [...args];

  await runCommand(await command("osv-scanner", env), scannerArgs, {
    cwd: repoRoot,
    env,
  });
}

async function runUdeps(
  { env: initialEnv, repoRoot }: TaskContext,
  args: readonly string[],
): Promise<void> {
  const env = useNightlyToolchain(initialEnv);
  const udepsArgs = args.length === 0 ? ["--workspace", "--all-targets", "--locked"] : [...args];

  await runCommand(await command("cargo", env), ["udeps", ...udepsArgs], {
    cwd: repoRoot,
    env,
  });
}

async function runWasmCheck(
  { env: initialEnv, repoRoot }: TaskContext,
  args: readonly string[],
): Promise<void> {
  const target = requireEnv(initialEnv, "CLIPPER_WASM_TARGET");
  const env = useStableToolchain(initialEnv);

  await runCommand(
    await command("cargo", env),
    ["check", "-p", "clipper-web-wasm", "--target", target, ...args],
    { cwd: repoRoot, env },
  );
}

async function runPnpmInstall(repoRoot: string, env: Env): Promise<string> {
  const pnpm = await command("pnpm", env);

  await runCommand(pnpm, ["install", "--frozen-lockfile"], {
    cwd: repoRoot,
    env: suppressNodeWarnings(env),
  });

  return pnpm;
}

function suppressNodeWarnings(env: Env): Record<string, string> {
  const nodeOptions = env.NODE_OPTIONS?.trim();
  return {
    ...env,
    NODE_OPTIONS:
      nodeOptions === undefined || nodeOptions.length === 0
        ? "--no-warnings"
        : `${nodeOptions} --no-warnings`,
  };
}

async function runRootPnpmScript(
  repoRoot: string,
  env: Env,
  script: string,
  args: readonly string[],
): Promise<void> {
  const pnpm = await runPnpmInstall(repoRoot, env);
  await runCommand(pnpm, ["run", script, ...args], {
    cwd: repoRoot,
    env: suppressNodeWarnings(env),
  });
}

async function runPackagePnpmScript(
  repoRoot: string,
  env: Env,
  packageDir: string,
  script: string,
  args: readonly string[],
): Promise<void> {
  const pnpm = await runPnpmInstall(repoRoot, env);
  await runCommand(pnpm, ["--dir", packageDir, "run", script, ...args], {
    cwd: repoRoot,
    env: suppressNodeWarnings(env),
  });
}

async function runWebTask(
  name: string,
  { env: initialEnv, repoRoot }: TaskContext,
  args: readonly string[],
): Promise<void> {
  requireEnv(initialEnv, "CLIPPER_WASM_TARGET");
  const env = useStableToolchain(initialEnv);
  const script = webTaskScripts[name];

  if (script === undefined) {
    throw new Error(`unknown web task: ${name}`);
  }

  await runPackagePnpmScript(repoRoot, env, "web", script, args);
}

async function runWebCheck(
  { env: initialEnv, repoRoot }: TaskContext,
  args: readonly string[],
): Promise<void> {
  if (args.length > 0) {
    throw new Error("web-check does not accept arguments");
  }

  requireEnv(initialEnv, "CLIPPER_WASM_TARGET");
  const env = useStableToolchain(initialEnv);
  const pnpm = await runPnpmInstall(repoRoot, env);
  await runCommand(pnpm, ["--dir", "web", "run", "lint"], {
    cwd: repoRoot,
    env: suppressNodeWarnings(env),
  });
  await runCommand(pnpm, ["--dir", "web", "run", "check"], {
    cwd: repoRoot,
    env: suppressNodeWarnings(env),
  });
}

async function runMobileCheck(
  { env: initialEnv, repoRoot }: TaskContext,
  args: readonly string[],
): Promise<void> {
  if (args.length > 0) {
    throw new Error("mobile-check does not accept arguments");
  }

  const env = useStableToolchain(initialEnv);
  const pnpm = await runPnpmInstall(repoRoot, env);
  for (const packageDir of ["packages/mobile-bridge", "mobile"]) {
    await runCommand(pnpm, ["--dir", packageDir, "run", "lint"], {
      cwd: repoRoot,
      env: suppressNodeWarnings(env),
    });
    await runCommand(pnpm, ["--dir", packageDir, "run", "check"], {
      cwd: repoRoot,
      env: suppressNodeWarnings(env),
    });
  }
}

async function runMobileTask(
  name: string,
  { env: initialEnv, repoRoot }: TaskContext,
  args: readonly string[],
): Promise<void> {
  const env = useStableToolchain(initialEnv);
  const script = mobileTaskScripts[name];

  if (script === undefined) {
    throw new Error(`unknown mobile task: ${name}`);
  }

  await runPackagePnpmScript(repoRoot, env, "mobile", script, args);
}

async function runMobileUniffiAndroid(
  { env: initialEnv, repoRoot }: TaskContext,
  args: readonly string[],
): Promise<void> {
  const env = useStableToolchain(initialEnv);
  await runPackagePnpmScript(repoRoot, env, "packages/mobile-bridge", "ubrn:android", args);
}

async function runJsCheck({ env, repoRoot }: TaskContext, args: readonly string[]): Promise<void> {
  if (args.length > 0) {
    throw new Error("js-check does not accept arguments");
  }

  const pnpm = await runPnpmInstall(repoRoot, env);
  for (const script of ["lint", "check"]) {
    await runCommand(pnpm, ["run", script], {
      cwd: repoRoot,
      env: suppressNodeWarnings(env),
    });
  }
}

const taskHandlers: Readonly<Record<string, TaskHandler>> = {
  fmt: runFmt,
  rustfmt: runRustfmt,
  audit: runAudit,
  "js-check": runJsCheck,
  udeps: runUdeps,
  "wasm-check": runWasmCheck,
  "web-check": runWebCheck,
  "web-build": (context, args) => runWebTask("web-build", context, args),
  "web-serve": (context, args) => runWebTask("web-serve", context, args),
  "tauri-dev": (context, args) => runWebTask("tauri-dev", context, args),
  "tauri-build": (context, args) => runWebTask("tauri-build", context, args),
  "mobile-check": runMobileCheck,
  "mobile-start": (context, args) => runMobileTask("mobile-start", context, args),
  "mobile-android": (context, args) => runMobileTask("mobile-android", context, args),
  "mobile-uniffi-android": runMobileUniffiAndroid,
};

function usage(): never {
  const names = Object.keys(taskHandlers).toSorted().join(", ");
  throw new Error(`usage: tasks.ts <task> [args...]\navailable tasks: ${names}`);
}

async function main(): Promise<void> {
  const taskName = Deno.args[0] ?? usage();
  const handler = taskHandlers[taskName] ?? usage();
  const initialEnv = Deno.env.toObject();
  const scriptDir = moduleDir(import.meta.url);
  const repoRoot = await findRepoRoot(initialEnv, [joinPath(scriptDir, "..")]);
  const env = {
    ...initialEnv,
    CLIPPER_REPO_ROOT: repoRoot,
  };

  await handler({ env, repoRoot }, Deno.args.slice(1));
}

try {
  await main();
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  Deno.exit(1);
}
