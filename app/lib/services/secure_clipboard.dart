import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

const _secureClipboardChannel = MethodChannel(
  'com.clipper.app/secure_clipboard',
);

/// Centralizes clipboard writes so platform-specific privacy handling has one
/// call site.
Future<void> setSecureClipboardText(String text) async {
  if (defaultTargetPlatform == TargetPlatform.android) {
    try {
      await _secureClipboardChannel.invokeMethod<void>('setText', {
        'text': text,
      });
      return;
    } on MissingPluginException {
      // Widget tests and non-embedded runs do not have the Android channel.
    }
  }

  await Clipboard.setData(ClipboardData(text: text));
}
