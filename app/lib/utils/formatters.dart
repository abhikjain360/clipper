String formatByteSize(Object bytes) {
  final byteCount = switch (bytes) {
    final int value => value,
    final BigInt value => value.toInt(),
    _ => 0,
  };

  final bytesValue = byteCount < 0 ? 0 : byteCount;
  if (bytesValue < 1024) return '$bytesValue B';
  if (bytesValue < 1024 * 1024) {
    return '${(bytesValue / 1024).toStringAsFixed(1)} KB';
  }
  if (bytesValue < 1024 * 1024 * 1024) {
    return '${(bytesValue / (1024 * 1024)).toStringAsFixed(1)} MB';
  }
  return '${(bytesValue / (1024 * 1024 * 1024)).toStringAsFixed(1)} GB';
}

String formatRelativeTimestamp(String rfc3339, {DateTime? now}) {
  try {
    final dt = DateTime.parse(rfc3339).toLocal();
    final diff = (now ?? DateTime.now()).difference(dt);

    if (diff.inMinutes < 1) return 'Just now';
    if (diff.inHours < 1) return '${diff.inMinutes}m ago';
    if (diff.inDays < 1) return '${diff.inHours}h ago';
    if (diff.inDays < 7) return '${diff.inDays}d ago';
    return '${dt.month}/${dt.day}/${dt.year}';
  } catch (_) {
    return rfc3339;
  }
}
