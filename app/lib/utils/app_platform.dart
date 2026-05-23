import 'package:flutter/foundation.dart';

String clipperDeviceName([TargetPlatform? platform]) {
  return switch (platform ?? defaultTargetPlatform) {
    TargetPlatform.android => 'Android-Clipper',
    TargetPlatform.iOS => 'iOS-Clipper',
    TargetPlatform.macOS => 'macOS-Clipper',
    TargetPlatform.linux => 'Linux-Clipper',
    TargetPlatform.windows => 'Windows-Clipper',
    TargetPlatform.fuchsia => 'Clipper',
  };
}

String defaultServerUrl([TargetPlatform? platform]) {
  return switch (platform ?? defaultTargetPlatform) {
    TargetPlatform.android => 'http://10.0.2.2:8787',
    _ => 'http://127.0.0.1:8787',
  };
}
