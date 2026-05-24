import 'package:web/web.dart' as web;

String? validateWebRuntime() {
  if (web.window.crossOriginIsolated) {
    return null;
  }

  return 'This web build was served without cross-origin isolation headers. '
      'Serve it with Cross-Origin-Opener-Policy: same-origin and '
      'Cross-Origin-Embedder-Policy: require-corp so the Rust WebAssembly '
      'worker can start.';
}
