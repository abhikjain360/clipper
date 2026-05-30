export type Env = Readonly<Record<string, string>>;

const textDecoder = new TextDecoder();

export function nonEmpty(value: string | undefined): string | undefined {
  return value === undefined || value.length === 0 ? undefined : value;
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
