import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

const _secureClipboardChannel = MethodChannel(
  'com.clipper.app/secure_clipboard',
);

class DeviceClipboardEntry {
  final String mimeType;
  final Uint8List bytes;
  final String? text;

  const DeviceClipboardEntry({
    required this.mimeType,
    required this.bytes,
    this.text,
  });

  factory DeviceClipboardEntry.text(String text) {
    return DeviceClipboardEntry(
      mimeType: 'text/plain',
      bytes: Uint8List.fromList(utf8.encode(text)),
      text: text,
    );
  }

  bool get isText =>
      mimeType.toLowerCase().split(';').first.startsWith('text/');

  bool get isImage =>
      mimeType.toLowerCase().split(';').first.startsWith('image/');
}

/// Centralizes clipboard writes so platform-specific privacy handling has one
/// call site.
Future<void> setSecureClipboardText(String text) async {
  await setSecureClipboardEntry(DeviceClipboardEntry.text(text));
}

Future<void> setSecureClipboardEntry(DeviceClipboardEntry entry) async {
  if (!kIsWeb &&
      (defaultTargetPlatform == TargetPlatform.android ||
          defaultTargetPlatform == TargetPlatform.macOS)) {
    try {
      await _secureClipboardChannel.invokeMethod<void>('setEntry', {
        'mimeType': entry.mimeType,
        'bytes': entry.bytes,
        'text': entry.text,
      });
      return;
    } on MissingPluginException {
      // Widget tests and non-embedded runs do not have the platform channel.
    }
  }

  if (entry.text != null) {
    await Clipboard.setData(ClipboardData(text: entry.text!));
    return;
  }

  throw UnsupportedError(
    'Copying ${entry.mimeType} to the platform clipboard is not supported here',
  );
}

Future<DeviceClipboardEntry?> readDeviceClipboardEntry() async {
  if (!kIsWeb &&
      (defaultTargetPlatform == TargetPlatform.android ||
          defaultTargetPlatform == TargetPlatform.macOS)) {
    try {
      final raw = await _secureClipboardChannel
          .invokeMapMethod<String, Object?>('getEntry');
      return _entryFromPlatformMap(raw);
    } on MissingPluginException {
      // Fall through to text-only Flutter clipboard for tests and other shells.
    }
  }

  final data = await Clipboard.getData('text/plain');
  final text = data?.text;
  if (text == null || text.isEmpty) return null;
  return DeviceClipboardEntry.text(text);
}

DeviceClipboardEntry? _entryFromPlatformMap(Map<String, Object?>? raw) {
  if (raw == null) return null;
  final mimeType = raw['mimeType'] as String?;
  if (mimeType == null || mimeType.isEmpty) return null;

  final text = raw['text'] as String?;
  final bytes =
      _bytesFromPlatformValue(raw['bytes']) ??
      (text == null ? null : Uint8List.fromList(utf8.encode(text)));
  if (bytes == null || bytes.isEmpty) return null;

  return DeviceClipboardEntry(mimeType: mimeType, bytes: bytes, text: text);
}

Uint8List? _bytesFromPlatformValue(Object? raw) {
  if (raw == null) return null;
  if (raw is Uint8List) return raw;
  if (raw is List) return Uint8List.fromList(raw.cast<int>());
  return null;
}
