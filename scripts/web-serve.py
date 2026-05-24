#!/usr/bin/env python3
import errno
import functools
import http.server
import socketserver
import sys


class Handler(http.server.SimpleHTTPRequestHandler):
    def end_headers(self):
        self.send_header("Cross-Origin-Opener-Policy", "same-origin")
        self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
        self.send_header("Cross-Origin-Resource-Policy", "same-origin")
        super().end_headers()

    def guess_type(self, path):
        if path.endswith(".wasm"):
            return "application/wasm"
        return super().guess_type(path)


class Server(socketserver.TCPServer):
    allow_reuse_address = True


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("usage: web-serve.py <root> <port>")

    root = sys.argv[1]
    preferred_port = int(sys.argv[2])

    handler = functools.partial(Handler, directory=root)
    server = None
    last_error = None

    for port in range(preferred_port, preferred_port + 100):
        try:
            server = Server(("127.0.0.1", port), handler)
            break
        except OSError as error:
            if error.errno != errno.EADDRINUSE:
                raise
            last_error = error

    if server is None:
        raise SystemExit(last_error or RuntimeError("could not bind local web server"))

    with server:
        _, port = server.server_address
        print(f"Serving {root} at http://127.0.0.1:{port}/")
        print("Sending COOP/COEP headers required by the Rust wasm worker.")
        sys.stdout.flush()
        server.serve_forever()


if __name__ == "__main__":
    main()
