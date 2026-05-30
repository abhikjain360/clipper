import {
  findRepoRoot,
  joinPath,
  moduleDir,
  requireEnv,
  requireExecutableFromPath,
  runCommand,
  useNightlyToolchain,
  useWasmRustcWarningFilter,
  wasmSharedMemoryRustflags,
} from "./script-common.ts";

async function main(): Promise<void> {
  const initialEnv = Deno.env.toObject();
  requireEnv(initialEnv, "FLUTTER_ROOT");

  const scriptDir = moduleDir(import.meta.url);
  const repoRoot = await findRepoRoot(initialEnv, [joinPath(scriptDir, "..")]);
  const appDir = joinPath(repoRoot, "app");

  let env = useNightlyToolchain(initialEnv);
  const warningFilter = await useWasmRustcWarningFilter(env);

  try {
    env = warningFilter.env;
    const rustc = await requireExecutableFromPath("rustc", env);
    const codegen = await requireExecutableFromPath(
      "flutter_rust_bridge_codegen",
      env,
    );

    await runCommand(
      codegen,
      [
        "build-web",
        "--cargo-build-args",
        "--config",
        "--cargo-build-args",
        `build.rustc="${rustc}"`,
        "--wasm-pack-rustflags",
        wasmSharedMemoryRustflags(),
        ...Deno.args,
      ],
      { cwd: appDir, env },
    );
  } finally {
    await warningFilter.cleanup();
  }
}

try {
  await main();
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  Deno.exit(1);
}
