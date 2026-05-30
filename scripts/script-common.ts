export type Env = Readonly<Record<string, string>>;

const textDecoder = new TextDecoder();
const rustcWasmWarningFilterScript =
  `#!/usr/bin/env -S deno run --quiet --ext=js --allow-env=CLIPPER_REAL_RUSTC --allow-run
const decoder = new TextDecoder();
const encoder = new TextEncoder();

function removeUnstableAtomicsJsonDiagnostics(text) {
  return text.replace(
    /^\\{"\\$message_type":"diagnostic","message":"unstable feature specified for \`-Ctarget-feature\`: \`atomics\`".*\\n?/gm,
    "",
  );
}

function removeJsonWarningSummariesIfClean(text) {
  const withoutSummary = text.replace(
    /^\\{"\\$message_type":"diagnostic","message":"\\d+ warnings? emitted".*\\n?/gm,
    "",
  );

  return /"\\$message_type":"diagnostic".*"level":"warning"/s.test(withoutSummary)
    ? text
    : withoutSummary;
}

function removeTextWarningSummariesIfClean(text) {
  const withoutSummary = text
    .replace(
      /^\\{"\\$message_type":"diagnostic","message":"\\d+ warnings? emitted".*\\n?/gm,
      "",
    )
    .replace(/warning: \\d+ warnings? emitted\\n\\n?/g, "");

  return withoutSummary.includes("warning:") ||
      /"\\$message_type":"diagnostic".*"level":"warning"/s.test(withoutSummary)
    ? text
    : withoutSummary;
}

function filterStdout(text) {
  return removeJsonWarningSummariesIfClean(
    removeUnstableAtomicsJsonDiagnostics(text),
  );
}

function filterStderr(text) {
  return removeTextWarningSummariesIfClean(
    removeUnstableAtomicsJsonDiagnostics(text).replace(
      /warning: unstable feature specified for \`-Ctarget-feature\`: \`atomics\`\\n  \\|\\n  = note: this feature is not stably supported; its behavior can change in the future\\n\\n/g,
      "",
    ),
  );
}

const realRustc = Deno.env.get("CLIPPER_REAL_RUSTC");
if (realRustc === undefined || realRustc.length === 0) {
  console.error("CLIPPER_REAL_RUSTC is required");
  Deno.exit(1);
}

const output = await new Deno.Command(realRustc, {
  args: Deno.args,
  stdin: "inherit",
  stdout: "piped",
  stderr: "piped",
}).output();

await Deno.stdout.write(encoder.encode(filterStdout(decoder.decode(output.stdout))));
await Deno.stderr.write(encoder.encode(filterStderr(decoder.decode(output.stderr))));
Deno.exit(output.code);
`;

export function nonEmpty(value: string | undefined): string | undefined {
  return value === undefined || value.length === 0 ? undefined : value;
}

export function requireEnv(env: Env, name: string): string {
  const value = nonEmpty(env[name]);
  if (value === undefined) {
    throw new Error(`Required environment variable missing: ${name}`);
  }
  return value;
}

export function moduleDir(metaUrl: string): string {
  const url = new URL(".", metaUrl);
  if (url.protocol !== "file:") {
    throw new Error(`expected file module URL, got ${metaUrl}`);
  }

  const path = decodeURIComponent(url.pathname);
  return path.length > 1 ? path.replace(/\/+$/, "") : path;
}

export function isAbsolutePath(path: string): boolean {
  return path.startsWith("/");
}

export function dirname(path: string): string {
  const trimmed = path.length > 1 ? path.replace(/\/+$/, "") : path;
  const slash = trimmed.lastIndexOf("/");
  if (slash < 0) return ".";
  if (slash === 0) return "/";
  return trimmed.slice(0, slash);
}

export function joinPath(first: string, ...rest: readonly string[]): string {
  let path = first;

  for (const part of rest) {
    if (part.length === 0) continue;
    if (path.length === 0) {
      path = part;
      continue;
    }

    const left = path === "/" ? "" : path.replace(/\/+$/, "");
    const right = part.replace(/^\/+/, "");
    path = `${left}/${right}`;
  }

  return path.length === 0 ? "." : path;
}

export function resolvePath(baseDir: string, path: string): string {
  return isAbsolutePath(path) ? path : joinPath(baseDir, path);
}

export function prependPath(env: Env, path: string): Record<string, string> {
  return {
    ...env,
    PATH: `${path}${env.PATH === undefined ? "" : `:${env.PATH}`}`,
  };
}

export function useToolchainPath(
  env: Env,
  toolchainBin: string,
): Record<string, string> {
  if (toolchainBin.length === 0) {
    throw new Error("toolchain path is required");
  }

  return prependPath(env, toolchainBin);
}

export function useStableToolchain(env: Env): Record<string, string> {
  return useToolchainPath(env, requireEnv(env, "CLIPPER_STABLE_BIN"));
}

export function useNightlyToolchain(env: Env): Record<string, string> {
  return useToolchainPath(env, requireEnv(env, "CLIPPER_RUST_NIGHTLY_BIN"));
}

export function wasmSharedMemoryRustflags(): string {
  return [
    "-C target-feature=+atomics,+bulk-memory,+mutable-globals",
    "-C link-arg=--shared-memory",
    "-C link-arg=--import-memory",
    "-C link-arg=--max-memory=4294967296",
    "-C link-arg=--export=__heap_base",
    "-C link-arg=--export=__wasm_init_tls",
    "-C link-arg=--export=__tls_size",
    "-C link-arg=--export=__tls_align",
    "-C link-arg=--export=__tls_base",
  ].join(" ");
}

export function useWasmSharedMemoryRustflags(
  env: Env,
): Record<string, string> {
  const existing = nonEmpty(env.RUSTFLAGS);
  const sharedFlags = wasmSharedMemoryRustflags();
  return {
    ...env,
    RUSTFLAGS: existing === undefined
      ? sharedFlags
      : `${existing} ${sharedFlags}`,
  };
}

async function statPath(path: string): Promise<Deno.FileInfo | undefined> {
  try {
    return await Deno.stat(path);
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) return undefined;
    throw error;
  }
}

export async function fileExists(path: string): Promise<boolean> {
  return (await statPath(path))?.isFile ?? false;
}

export async function directoryExists(path: string): Promise<boolean> {
  return (await statPath(path))?.isDirectory ?? false;
}

export async function executableFileExists(path: string): Promise<boolean> {
  const stat = await statPath(path);
  if (!stat?.isFile) return false;
  if (Deno.build.os === "windows") return true;
  return ((stat.mode ?? 0) & 0o111) !== 0;
}

export async function executableFromPath(
  name: string,
  env: Env = Deno.env.toObject(),
): Promise<string | undefined> {
  if (name.includes("/")) {
    return await executableFileExists(name) ? name : undefined;
  }

  for (const dir of (env.PATH ?? "").split(":")) {
    if (dir.length === 0) continue;
    const candidate = joinPath(dir, name);
    if (await executableFileExists(candidate)) return candidate;
  }

  return undefined;
}

export async function requireExecutableFromPath(
  name: string,
  env: Env = Deno.env.toObject(),
): Promise<string> {
  const executable = await executableFromPath(name, env);
  if (executable === undefined) {
    throw new Error(`executable not found on PATH: ${name}`);
  }

  return executable;
}

export async function runCommand(
  command: string,
  args: readonly string[],
  options: {
    readonly cwd?: string;
    readonly env?: Record<string, string>;
  } = {},
): Promise<void> {
  const status = await new Deno.Command(command, {
    args: [...args],
    cwd: options.cwd,
    env: options.env,
    stdin: "inherit",
    stdout: "inherit",
    stderr: "inherit",
  }).spawn().status;

  if (!status.success) {
    throw new Error(`${command} failed with exit code ${status.code}`);
  }
}

export async function useWasmRustcWarningFilter(
  env: Env,
): Promise<{
  readonly cleanup: () => Promise<void>;
  readonly env: Record<string, string>;
}> {
  const realRustc = await requireExecutableFromPath("rustc", env);
  const wrapperDir = await Deno.makeTempDir({
    prefix: "clipper-rustc-wasm-filter.",
  });
  const wrapper = joinPath(wrapperDir, "rustc");

  await Deno.writeTextFile(wrapper, rustcWasmWarningFilterScript);
  if (Deno.build.os !== "windows") {
    await Deno.chmod(wrapper, 0o755);
  }

  return {
    env: prependPath({ ...env, CLIPPER_REAL_RUSTC: realRustc }, wrapperDir),
    cleanup: async () => {
      try {
        await Deno.remove(wrapperDir, { recursive: true });
      } catch (error) {
        if (!(error instanceof Deno.errors.NotFound)) throw error;
      }
    },
  };
}

async function gitRepoRoot(startDir: string): Promise<string | undefined> {
  if (!(await directoryExists(startDir))) return undefined;

  const output = await new Deno.Command("git", {
    args: ["-C", startDir, "rev-parse", "--show-toplevel"],
    stdout: "piped",
    stderr: "null",
  }).output();

  if (!output.success) return undefined;

  const root = textDecoder.decode(output.stdout).trim();
  return root.length === 0 ? undefined : root;
}

export async function findRepoRoot(
  env: Env,
  fallbackStartDirs: readonly string[],
): Promise<string> {
  const configured = nonEmpty(env.CLIPPER_REPO_ROOT);
  if (configured !== undefined && await directoryExists(configured)) {
    return await Deno.realPath(configured);
  }

  const startDirs = [Deno.cwd(), ...fallbackStartDirs];
  const seen = new Set<string>();
  for (const startDir of startDirs) {
    const key = await directoryExists(startDir)
      ? await Deno.realPath(startDir)
      : startDir;
    if (seen.has(key)) continue;
    seen.add(key);

    const root = await gitRepoRoot(startDir);
    if (root !== undefined) return root;
  }

  throw new Error("Unable to detect Clipper repo root");
}
