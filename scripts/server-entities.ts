import {
  findRepoRoot,
  joinPath,
  moduleDir,
  nonEmpty,
  requireExecutableFromPath,
  runCommand,
  useStableToolchain,
} from "./script-common.ts";

// SQLite has no UUID type. Our migrations declare UUID columns as `UUID`, which
// SQLite records verbatim as a declared type it does not know, so schema
// discovery reports it as an opaque custom type and every sea-orm-cli release
// generates `pub x: String` with `column_type = "custom(\"UUID\")"` — never
// `sea_orm::Uuid`. sea-orm-cli 2.x also tags those columns `ignore` ("not a
// column"), which does not even compile when the column is a primary key.
//
// The server's handlers are typed on `Uuid` throughout, and that is the point:
// an id that is a `Uuid` cannot be confused with the many other strings in these
// rows. So the generated files are rewritten here rather than by hand — that way
// `nix run .#server-entities` stays a true no-op on an unchanged schema, and the
// CLI is free to track nixpkgs.
// A `column_type` naming SQLite's unrecognised `UUID` declared type. Matched by
// pattern rather than by literal text so the Rust source's escaped quotes
// (`custom(\"UUID\")`) do not have to be reproduced here exactly.
const UUID_COLUMN_TYPE = /column_type\s*=\s*"custom\(\\?"UUID\\?"\)"/u;

// One generated field: its `#[sea_orm(...)]` attribute (optional, and possibly
// wrapped over several lines) plus its declaration. Anchored per line so a field
// is only ever rewritten as a whole.
const GENERATED_FIELD =
  /^(?<indent>[ \t]*)(?:#\[sea_orm\((?<attrs>[^)]*(?:\)[^)\]]*)*?)\)\]\n[ \t]*)?pub (?<name>\w+): (?<type>[\w:<>]+),$/gmu;

// Attributes that exist only to describe the degraded mapping, and so go with
// it. `nullable` is among them here: it is only ever emitted alongside an
// `Option<...>` field, from which sea-orm already infers nullability.
function describesDegradedMapping(attribute: string): boolean {
  return (
    attribute === "ignore" ||
    attribute === "nullable" ||
    attribute.startsWith("column_type") ||
    attribute.startsWith("select_as")
  );
}

// Restore `Uuid` typing on every column the codegen degraded to a custom-typed
// `String`.
function restoreUuidColumns(source: string): string {
  return source.replaceAll(GENERATED_FIELD, (match, ...args) => {
    const groups = args.at(-1) as {
      indent: string;
      attrs?: string;
      name: string;
      type: string;
    };
    if (groups.attrs === undefined || !UUID_COLUMN_TYPE.test(groups.attrs)) return match;

    if (groups.type !== "String" && groups.type !== "Option<String>") {
      throw new Error(
        `entity codegen: expected a String column for custom UUID field \`${groups.name}\`, got \`${groups.type}\``,
      );
    }

    const kept = groups.attrs
      .split(",")
      .map((attribute) => attribute.trim())
      .filter((attribute) => attribute.length > 0 && !describesDegradedMapping(attribute));

    const type = groups.type === "Option<String>" ? "Option<Uuid>" : "Uuid";
    const attribute = kept.length > 0 ? `${groups.indent}#[sea_orm(${kept.join(", ")})]\n` : "";
    return `${attribute}${groups.indent}pub ${groups.name}: ${type},`;
  });
}

async function restoreUuidColumnsInDir(dir: string): Promise<void> {
  for await (const entry of Deno.readDir(dir)) {
    if (!entry.isFile || !entry.name.endsWith(".rs")) continue;
    const path = joinPath(dir, entry.name);
    const source = await Deno.readTextFile(path);
    const restored = restoreUuidColumns(source);
    if (restored !== source) await Deno.writeTextFile(path, restored);
  }
}

const SERVER_SECRET_ENV = "CLIPPER_SERVER_SECRET";
const SERVER_SECRET_FILE_ENV = "CLIPPER_SERVER_SECRET_FILE";
const SERVER_SECRET_BYTES = 32;
const ENTITY_DIR = "crates/server/src/entity";

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
        ENTITY_DIR,
        "--with-prelude",
        "none",
      ],
      { cwd: repoRoot, env },
    );
    await restoreUuidColumnsInDir(joinPath(repoRoot, ENTITY_DIR));
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
