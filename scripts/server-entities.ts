import {
  findRepoRoot,
  joinPath,
  moduleDir,
  nonEmpty,
  requireExecutableFromPath,
  runCommand,
  useStableToolchain,
} from "./script-common.ts";

const SERVER_SECRET_ENV = "CLIPPER_SERVER_SECRET";
const SERVER_SECRET_FILE_ENV = "CLIPPER_SERVER_SECRET_FILE";
const SERVER_SECRET_BYTES = 32;

function generateServerSecret(): string {
  const bytes = new Uint8Array(SERVER_SECRET_BYTES);
  crypto.getRandomValues(bytes);

  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }

  return btoa(binary);
}

function withEphemeralServerSecret(env: Readonly<Record<string, string>>): Record<string, string> {
  if (
    nonEmpty(env[SERVER_SECRET_ENV]) !== undefined ||
    nonEmpty(env[SERVER_SECRET_FILE_ENV]) !== undefined
  ) {
    return { ...env };
  }

  return {
    ...env,
    [SERVER_SECRET_ENV]: generateServerSecret(),
  };
}

async function main(): Promise<void> {
  const initialEnv = Deno.env.toObject();
  const scriptDir = moduleDir(import.meta.url);
  const repoRoot = await findRepoRoot(initialEnv, [joinPath(scriptDir, "..")]);
  const env = withEphemeralServerSecret(
    useStableToolchain({
      ...initialEnv,
      RUST_LOG: nonEmpty(initialEnv.RUST_LOG) ?? "warn",
    }),
  );
  const cargo = await requireExecutableFromPath("cargo", env);
  const seaOrmCli = await requireExecutableFromPath("sea-orm-cli", env);
  const tempDir = await Deno.makeTempDir({
    prefix: "clipper-server-entities.",
  });

  try {
    const dataDir = joinPath(tempDir, "data");
    await runCommand(cargo, ["run", "-q", "-p", "clipper-server", "--", "init", "-d", dataDir], {
      cwd: repoRoot,
      env,
    });
    await runCommand(
      seaOrmCli,
      [
        "generate",
        "entity",
        "-u",
        `sqlite:${joinPath(dataDir, "clipper.db")}`,
        "-o",
        "crates/server/src/entity",
        "--with-prelude",
        "none",
      ],
      { cwd: repoRoot, env },
    );
  } finally {
    await Deno.remove(tempDir, { recursive: true });
  }
}

try {
  await main();
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  Deno.exit(1);
}
