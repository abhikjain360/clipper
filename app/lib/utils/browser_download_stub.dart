import 'dart:typed_data';

Future<void> downloadBytes(String filename, Uint8List bytes) async {
  throw UnsupportedError('Browser downloads are only available on web');
}
