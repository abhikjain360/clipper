import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

import '../src/rust/api/clipper.dart';
import '../src/rust/frb_generated.dart';

const _backgroundSyncChannel = MethodChannel('com.clipper.app/background_sync');
const _backgroundSyncServiceChannel = MethodChannel(
  'com.clipper.app/background_sync_service',
);

bool get _supportsAndroidBackgroundSync =>
    !kIsWeb && defaultTargetPlatform == TargetPlatform.android;

Future<bool> startBackgroundSync() async {
  if (!_supportsAndroidBackgroundSync) return false;

  try {
    return await _backgroundSyncChannel.invokeMethod<bool>('start') ?? false;
  } on MissingPluginException {
    return false;
  }
}

Future<void> stopBackgroundSync() async {
  if (!_supportsAndroidBackgroundSync) return;

  try {
    await _backgroundSyncChannel.invokeMethod<void>('stop');
  } on MissingPluginException {
    return;
  }
}

Future<void> runAndroidBackgroundSyncService() async {
  WidgetsFlutterBinding.ensureInitialized();

  try {
    await RustLib.init();
    await connectToDaemon();
  } catch (_) {
    await _stopNativeService();
    return;
  }

  while (true) {
    final state = await getState();
    if (!state.loggedIn) {
      await _stopNativeService();
      return;
    }

    try {
      await waitForStateChange().timeout(const Duration(minutes: 5));
    } on TimeoutException {
      continue;
    } catch (_) {
      await Future<void>.delayed(const Duration(seconds: 5));
    }
  }
}

Future<void> _stopNativeService() async {
  try {
    await _backgroundSyncServiceChannel.invokeMethod<void>('stopSelf');
  } on MissingPluginException {
    return;
  }
}
