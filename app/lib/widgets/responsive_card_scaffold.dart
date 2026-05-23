import 'package:flutter/material.dart';

class ResponsiveCardScaffold extends StatelessWidget {
  final Widget child;
  final double maxWidth;

  const ResponsiveCardScaffold({
    super.key,
    required this.child,
    this.maxWidth = 420,
  });

  @override
  Widget build(BuildContext context) {
    return SafeArea(
      child: Center(
        child: SingleChildScrollView(
          padding: const EdgeInsets.all(24),
          child: ConstrainedBox(
            constraints: BoxConstraints(maxWidth: maxWidth),
            child: Card(
              child: Padding(padding: const EdgeInsets.all(28), child: child),
            ),
          ),
        ),
      ),
    );
  }
}
