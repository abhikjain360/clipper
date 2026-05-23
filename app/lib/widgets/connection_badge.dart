import 'package:flutter/material.dart';

import '../src/rust/api/clipper.dart';

class ConnectionBadge extends StatelessWidget {
  final BridgeConnectionStatus status;

  const ConnectionBadge({super.key, required this.status});

  @override
  Widget build(BuildContext context) {
    final (:color, :label) = switch (status) {
      BridgeConnectionStatus.connected => (
        color: Colors.green,
        label: 'Connected',
      ),
      BridgeConnectionStatus.connecting => (
        color: Colors.orange,
        label: 'Connecting...',
      ),
      _ => (color: Colors.red, label: 'Disconnected'),
    };

    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
      decoration: BoxDecoration(
        color: color.withAlpha(40),
        borderRadius: BorderRadius.circular(12),
        border: Border.all(color: color.withAlpha(100)),
      ),
      child: Text(label, style: TextStyle(fontSize: 12, color: color)),
    );
  }
}
