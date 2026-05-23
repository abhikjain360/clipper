import 'package:flutter/material.dart';

class AppStatus extends StatelessWidget {
  final IconData icon;
  final String title;
  final String? message;
  final Widget? footer;
  final Color iconColor;
  final TextStyle? titleStyle;
  final TextStyle? messageStyle;

  const AppStatus({
    super.key,
    required this.icon,
    required this.title,
    this.message,
    this.footer,
    this.iconColor = Colors.white24,
    this.titleStyle,
    this.messageStyle,
  });

  @override
  Widget build(BuildContext context) {
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(icon, size: 64, color: iconColor),
            const SizedBox(height: 16),
            Text(
              title,
              style: titleStyle ?? const TextStyle(color: Colors.white38),
              textAlign: TextAlign.center,
            ),
            if (message != null) ...[
              const SizedBox(height: 8),
              Text(
                message!,
                style: messageStyle ?? const TextStyle(color: Colors.grey),
                textAlign: TextAlign.center,
              ),
            ],
            if (footer != null) ...[const SizedBox(height: 24), footer!],
          ],
        ),
      ),
    );
  }
}
