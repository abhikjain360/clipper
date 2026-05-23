import 'package:flutter/material.dart';

class LoadingIconButton extends StatelessWidget {
  final bool loading;
  final IconData icon;
  final String tooltip;
  final VoidCallback? onPressed;

  const LoadingIconButton({
    super.key,
    required this.loading,
    required this.icon,
    required this.tooltip,
    required this.onPressed,
  });

  @override
  Widget build(BuildContext context) {
    return IconButton(
      icon: loading
          ? const SizedBox(
              width: 20,
              height: 20,
              child: CircularProgressIndicator(strokeWidth: 2),
            )
          : Icon(icon),
      onPressed: loading ? null : onPressed,
      tooltip: tooltip,
    );
  }
}
