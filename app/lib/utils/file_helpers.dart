import 'package:flutter/material.dart';
import 'package:path/path.dart' as p;

IconData fileIconForMimeType(String mimeType) {
  if (mimeType.startsWith('image/')) return Icons.image;
  if (mimeType.startsWith('video/')) return Icons.videocam;
  if (mimeType.startsWith('audio/')) return Icons.audiotrack;
  if (mimeType.startsWith('text/')) return Icons.description;
  if (mimeType.contains('pdf')) return Icons.picture_as_pdf;
  if (mimeType.contains('zip') ||
      mimeType.contains('tar') ||
      mimeType.contains('gz')) {
    return Icons.archive;
  }
  return Icons.insert_drive_file;
}

String safeDownloadFilename(String filename) {
  final name = p.basename(filename).replaceAll(RegExp(r'[\x00-\x1F\x7F]'), '_');
  if (name.isEmpty || name == '.' || name == '..') {
    return 'download';
  }
  return name;
}
