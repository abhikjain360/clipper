import 'package:flutter/material.dart';

class ClipperIcon extends StatelessWidget {
  final double size;

  const ClipperIcon({super.key, this.size = 24});

  @override
  Widget build(BuildContext context) {
    return Icon(Icons.content_paste_rounded, size: size, color: Colors.blue);
  }
}

class ClipperBrandHeader extends StatelessWidget {
  const ClipperBrandHeader({super.key});

  @override
  Widget build(BuildContext context) {
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        const ClipperIcon(size: 64),
        const SizedBox(height: 16),
        Text('Clipper', style: Theme.of(context).textTheme.headlineMedium),
        const SizedBox(height: 8),
        Text(
          'Encrypted clipboard & file sync',
          style: Theme.of(context).textTheme.bodySmall,
          textAlign: TextAlign.center,
        ),
      ],
    );
  }
}

class ClipperAppTitle extends StatelessWidget {
  final Widget? trailing;

  const ClipperAppTitle({super.key, this.trailing});

  @override
  Widget build(BuildContext context) {
    return Row(
      children: [
        const ClipperIcon(),
        const SizedBox(width: 8),
        const Text('Clipper'),
        if (trailing != null) ...[const SizedBox(width: 12), trailing!],
      ],
    );
  }
}
