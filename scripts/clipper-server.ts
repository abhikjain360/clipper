#!/usr/bin/env -S deno run --allow-env --allow-read --allow-write --allow-run

import {
  fileExists,
  findRepoRoot,
  joinPath,
  moduleDir,
  nonEmpty,
  requireExecutableFromPath,
  runCommand,
  useToolchainPath,
} from "./script-common.ts";

const SERVER_SECRET_BYTES = 32;
const SERVER_SECRET_FILE = "data/clipper-server.secret";
const SERVER_DATA_DIR = "data/clipper-server";
const SERVER_RUST_LOG = "clipper_server=debug,tower_http=debug,info";

function randomBytes(length: number): Uint8Array {
  const bytes = new Uint8Array(length);
  crypto.getRandomValues(bytes);
  return bytes;
}

function base64(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }

  return btoa(binary);
}

function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function commandEnv(
  initialEnv: Readonly<Record<string, string>>,
  repoRoot: string,
  secretFile: string,
): Record<string, string> {
  const stableBin = nonEmpty(initialEnv.CLIPPER_STABLE_BIN);
  const env = {
    ...initialEnv,
    CLIPPER_REPO_ROOT: repoRoot,
    CLIPPER_SERVER_SECRET_FILE: secretFile,
  };

  return stableBin === undefined ? env : useToolchainPath(env, stableBin);
}

async function writeSecretFile(path: string): Promise<void> {
  await Deno.writeTextFile(path, `${base64(randomBytes(SERVER_SECRET_BYTES))}\n`);
  if (Deno.build.os !== "windows") {
    await Deno.chmod(path, 0o600);
  }
}

async function main(): Promise<void> {
  const initialEnv = Deno.env.toObject();
  const scriptDir = moduleDir(import.meta.url);
  const repoRoot = await findRepoRoot(initialEnv, [joinPath(scriptDir, "..")]);
  const dataRoot = joinPath(repoRoot, "data");
  const secretFile = joinPath(repoRoot, SERVER_SECRET_FILE);
  const dataDir = joinPath(repoRoot, SERVER_DATA_DIR);
  const env = commandEnv(initialEnv, repoRoot, secretFile);
  const cargo = await requireExecutableFromPath("cargo", env);

  await Deno.mkdir(dataRoot, { recursive: true });
  if (!(await fileExists(secretFile))) {
    await writeSecretFile(secretFile);
  } else if (Deno.build.os !== "windows") {
    await Deno.chmod(secretFile, 0o600);
  }

  await runCommand(cargo, ["run", "-p", "clipper-server", "--", "init", "--data-dir", dataDir], {
    cwd: repoRoot,
    env,
  });

  const accessKey = hex(randomBytes(32));
  await runCommand(
    cargo,
    [
      "run",
      "-p",
      "clipper-server",
      "--",
      "add-access-key",
      "--data-dir",
      dataDir,
      "--access-key",
      accessKey,
    ],
    { cwd: repoRoot, env },
  );

  console.log(`\nAccess key:\n${accessKey}\n`);

  await runCommand(
    cargo,
    ["run", "-p", "clipper-server", "--", "serve", "--data-dir", dataDir, ...Deno.args],
    {
      cwd: repoRoot,
      env: {
        ...env,
        RUST_LOG: SERVER_RUST_LOG,
      },
    },
  );
}

try {
  await main();
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  Deno.exit(1);
}
