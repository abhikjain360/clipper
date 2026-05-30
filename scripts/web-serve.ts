import {
  fileExists,
  findRepoRoot,
  joinPath,
  moduleDir,
  nonEmpty,
  resolvePath,
} from "./script-common.ts";
import type { Env } from "./script-common.ts";

type ServeArgs = {
  readonly root: string;
  readonly preferredPort: number;
};

const HOST = "127.0.0.1";
const DEFAULT_ROOT = "app/build/web";
const DEFAULT_PORT = 53880;
const PORT_ATTEMPTS = 100;

const crossOriginHeaders: Readonly<Record<string, string>> = {
  "Cross-Origin-Opener-Policy": "same-origin",
  "Cross-Origin-Embedder-Policy": "require-corp",
  "Cross-Origin-Resource-Policy": "same-origin",
};

const contentTypes: Readonly<Record<string, string>> = {
  ".css": "text/css; charset=utf-8",
  ".gif": "image/gif",
  ".html": "text/html; charset=utf-8",
  ".ico": "image/x-icon",
  ".jpeg": "image/jpeg",
  ".jpg": "image/jpeg",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".map": "application/json; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".otf": "font/otf",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".ttf": "font/ttf",
  ".txt": "text/plain; charset=utf-8",
  ".wasm": "application/wasm",
  ".webp": "image/webp",
  ".woff": "font/woff",
  ".woff2": "font/woff2",
};

function usage(): never {
  throw new Error(
    [
      "usage: web-serve.ts [port]",
      "       web-serve.ts [--root <path>] [--port <port>]",
      "       web-serve.ts <root> <port>",
    ].join("\n"),
  );
}

function parsePort(rawPort: string): number {
  const port = Number(rawPort);
  if (!Number.isInteger(port) || port < 1 || port > 65_535) {
    throw new Error(`invalid port: ${rawPort}`);
  }
  return port;
}

function looksLikePort(value: string): boolean {
  return /^[0-9]+$/.test(value);
}

function parseArgs(args: readonly string[], env: Env): ServeArgs {
  let root = nonEmpty(env.CLIPPER_WEB_ROOT) ?? DEFAULT_ROOT;
  let preferredPort = DEFAULT_PORT;
  let rootSet = false;
  let portSet = false;
  const positional: string[] = [];

  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];

    if (arg === "--root") {
      const value = args[index + 1];
      if (value === undefined) usage();
      root = value;
      rootSet = true;
      index += 1;
      continue;
    }

    if (arg.startsWith("--root=")) {
      root = arg.slice("--root=".length);
      rootSet = true;
      continue;
    }

    if (arg === "--port" || arg === "-p") {
      const value = args[index + 1];
      if (value === undefined) usage();
      preferredPort = parsePort(value);
      portSet = true;
      index += 1;
      continue;
    }

    if (arg.startsWith("--port=")) {
      preferredPort = parsePort(arg.slice("--port=".length));
      portSet = true;
      continue;
    }

    if (arg.startsWith("-")) usage();

    positional.push(arg);
  }

  for (const value of positional) {
    if (!rootSet && (portSet || !looksLikePort(value))) {
      root = value;
      rootSet = true;
      continue;
    }

    if (!portSet) {
      preferredPort = parsePort(value);
      portSet = true;
      continue;
    }

    usage();
  }

  return { root, preferredPort };
}

function readEnv(): Env {
  return {
    CLIPPER_REPO_ROOT: Deno.env.get("CLIPPER_REPO_ROOT") ?? "",
    CLIPPER_WEB_ROOT: Deno.env.get("CLIPPER_WEB_ROOT") ?? "",
  };
}

function responseHeaders(contentType?: string): Headers {
  const headers = new Headers(crossOriginHeaders);
  if (contentType) headers.set("Content-Type", contentType);
  return headers;
}

function errorResponse(status: number, message: string): Response {
  return new Response(`${message}\n`, {
    status,
    headers: responseHeaders("text/plain; charset=utf-8"),
  });
}

function extension(path: string): string {
  const name = path.slice(path.lastIndexOf("/") + 1);
  const dot = name.lastIndexOf(".");
  return dot === -1 ? "" : name.slice(dot).toLowerCase();
}

function contentTypeFor(path: string): string {
  return contentTypes[extension(path)] ?? "application/octet-stream";
}

function requestedFilePath(root: string, url: URL): string | undefined {
  let pathname: string;
  try {
    pathname = decodeURIComponent(url.pathname);
  } catch {
    return undefined;
  }

  if (pathname.includes("\0")) return undefined;

  const segments = pathname.split("/").filter((segment) => segment.length > 0);
  if (segments.some((segment) => segment === "." || segment === "..")) {
    return undefined;
  }

  if (pathname.endsWith("/")) segments.push("index.html");
  return joinPath(root, ...segments);
}

async function serveStaticFile(
  root: string,
  request: Request,
): Promise<Response> {
  if (request.method !== "GET" && request.method !== "HEAD") {
    return errorResponse(405, "method not allowed");
  }

  const path = requestedFilePath(root, new URL(request.url));
  if (!path) return errorResponse(400, "bad request");

  let file: Deno.FsFile;
  try {
    file = await Deno.open(path, { read: true });
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) {
      return errorResponse(404, "not found");
    }
    throw error;
  }

  const stat = await file.stat();
  if (!stat.isFile) {
    file.close();
    return errorResponse(404, "not found");
  }

  const headers = responseHeaders(contentTypeFor(path));
  headers.set("Content-Length", stat.size.toString());

  if (request.method === "HEAD") {
    file.close();
    return new Response(null, { headers });
  }

  return new Response(file.readable, { headers });
}

async function serve(root: string, preferredPort: number): Promise<void> {
  let lastError: unknown;

  for (
    let port = preferredPort;
    port < preferredPort + PORT_ATTEMPTS;
    port += 1
  ) {
    try {
      const server = Deno.serve(
        {
          hostname: HOST,
          port,
          onListen: ({ port }) => {
            console.log(`Serving ${root} at http://${HOST}:${port}/`);
            console.log(
              "Sending COOP/COEP headers required by the Rust wasm worker.",
            );
          },
        },
        (request) => serveStaticFile(root, request),
      );
      await server.finished;
      return;
    } catch (error) {
      if (!(error instanceof Deno.errors.AddrInUse)) throw error;
      lastError = error;
    }
  }

  throw lastError instanceof Error
    ? lastError
    : new Error("could not bind local web server");
}

async function resolveRoot(rawRoot: string): Promise<string> {
  const scriptDir = moduleDir(import.meta.url);
  const repoRoot = await findRepoRoot(readEnv(), [
    joinPath(scriptDir, ".."),
  ]);
  const root = resolvePath(repoRoot, rawRoot);

  if (!(await fileExists(joinPath(root, "index.html")))) {
    throw new Error(
      `missing ${root}/index.html; run nix run .#web-build first`,
    );
  }

  return await Deno.realPath(root);
}

async function main(): Promise<void> {
  const args = parseArgs(Deno.args, readEnv());
  await serve(await resolveRoot(args.root), args.preferredPort);
}

try {
  await main();
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  Deno.exit(1);
}
