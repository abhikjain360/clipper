import 'package:flutter/services.dart';

/// Centralizes clipboard writes so platform-specific privacy handling has one
/// call site. Android implementation should switch this to a native channel
/// that marks synced text as sensitive with ClipDescription.EXTRA_IS_SENSITIVE.
Future<void> setSecureClipboardText(String text) {
  return Clipboard.setData(ClipboardData(text: text));
}
