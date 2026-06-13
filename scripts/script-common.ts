export type Env = Readonly<Record<string, string>>;

const textDecoder = new TextDecoder();
const textEncoder = new TextEncoder();

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

export function useToolchainPath(env: Env, toolchainBin: string): Record<string, string> {
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
    return (await executableFileExists(name)) ? name : undefined;
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
    readonly stdin?: string;
  } = {},
): Promise<void> {
  const child = new Deno.Command(command, {
    args: [...args],
    cwd: options.cwd,
    env: options.env,
    stdin: options.stdin === undefined ? "inherit" : "piped",
    stdout: "inherit",
    stderr: "inherit",
  }).spawn();

  if (options.stdin !== undefined) {
    const writer = child.stdin.getWriter();
    await writer.write(textEncoder.encode(options.stdin));
    await writer.close();
  }

  const status = await child.status;

  if (!status.success) {
    throw new Error(`${command} failed with exit code ${status.code}`);
  }
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
  if (configured !== undefined && (await directoryExists(configured))) {
    return await Deno.realPath(configured);
  }

  const startDirs = [Deno.cwd(), ...fallbackStartDirs];
  const seen = new Set<string>();
  for (const startDir of startDirs) {
    const key = (await directoryExists(startDir)) ? await Deno.realPath(startDir) : startDir;
    if (seen.has(key)) continue;
    seen.add(key);

    const root = await gitRepoRoot(startDir);
    if (root !== undefined) return root;
  }

  throw new Error("Unable to detect Clipper repo root");
}
