import 'dart:async';

import 'package:flutter/material.dart';
import 'platform/web_runtime.dart';
import 'services/background_sync.dart';
import 'src/rust/api/clipper.dart';
import 'src/rust/frb_generated.dart';
import 'screens/login_screen.dart';
import 'screens/home_screen.dart';
import 'widgets/app_status.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();

  final startupError = validateWebRuntime();
  if (startupError == null) {
    try {
      await RustLib.init();
    } catch (error) {
      runApp(ClipperApp(startupError: 'Rust runtime failed to start: $error'));
      return;
    }
  }

  runApp(ClipperApp(startupError: startupError));
}

@pragma('vm:entry-point')
Future<void> backgroundSyncMain() => runAndroidBackgroundSyncService();

class ClipperApp extends StatelessWidget {
  final String? startupError;

  const ClipperApp({super.key, this.startupError});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Clipper',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        brightness: Brightness.dark,
        colorSchemeSeed: Colors.blue,
        useMaterial3: true,
        scaffoldBackgroundColor: const Color(0xFF121212),
        cardTheme: const CardThemeData(color: Color(0xFF1E1E1E)),
        appBarTheme: const AppBarTheme(
          backgroundColor: Color(0xFF1E1E1E),
          elevation: 0,
        ),
      ),
      home: startupError == null
          ? const AppRoot()
          : StartupErrorScreen(message: startupError!),
    );
  }
}

class StartupErrorScreen extends StatelessWidget {
  final String message;

  const StartupErrorScreen({super.key, required this.message});

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: AppStatus(
        icon: Icons.error_outline,
        iconColor: Colors.redAccent,
        title: 'Cannot start Clipper',
        titleStyle: const TextStyle(fontSize: 18, fontWeight: FontWeight.bold),
        message: message,
      ),
    );
  }
}

class AppRoot extends StatefulWidget {
  const AppRoot({super.key});

  @override
  State<AppRoot> createState() => _AppRootState();
}

class _AppRootState extends State<AppRoot> {
  BridgeAppState? _state;
  String? _connectError;
  bool _backgroundSyncRequested = false;

  @override
  void initState() {
    super.initState();
    _run();
  }

  /// Single long-lived loop: connect, watch state, reconnect on failure.
  /// Started exactly once from initState().
  Future<void> _run() async {
    while (mounted) {
      // Attempt connection
      try {
        setState(() => _connectError = null);
        await connectToDaemon();
        final state = await getState();
        if (!mounted) return;
        setState(() => _state = state);
        _configureBackgroundSync(state);
      } catch (e) {
        if (!mounted) return;
        setState(() {
          _state = null;
          _connectError = e.toString();
        });
        // Wait before retrying
        await Future.delayed(const Duration(seconds: 2));
        continue;
      }

      // Watch for state changes until daemon disconnects
      while (mounted) {
        await waitForStateChange();
        if (!mounted) return;
        final state = await getState();
        if (!mounted) return;

        if (state.connectionStatus == BridgeConnectionStatus.daemonNotRunning) {
          setState(() {
            _state = null;
            _connectError = 'Daemon stopped';
          });
          await Future.delayed(const Duration(seconds: 2));
          break; // Back to outer loop to reconnect
        }

        setState(() => _state = state);
        _configureBackgroundSync(state);
      }
    }
  }

  void _configureBackgroundSync(BridgeAppState state) {
    if (state.loggedIn == _backgroundSyncRequested) return;

    _backgroundSyncRequested = state.loggedIn;
    if (state.loggedIn) {
      unawaited(
        startBackgroundSync().catchError((_) {
          _backgroundSyncRequested = false;
          return false;
        }),
      );
    } else {
      unawaited(stopBackgroundSync());
    }
  }

  bool get _isDaemonConnected => _state != null;

  @override
  Widget build(BuildContext context) {
    if (!_isDaemonConnected) {
      return Scaffold(
        body: AppStatus(
          icon: Icons.cloud_off,
          iconColor: Colors.grey,
          title: 'Daemon not running',
          titleStyle: const TextStyle(
            fontSize: 18,
            fontWeight: FontWeight.bold,
          ),
          message: _connectError,
          footer: const Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              SizedBox(
                width: 24,
                height: 24,
                child: CircularProgressIndicator(strokeWidth: 2),
              ),
              SizedBox(height: 8),
              Text(
                'Reconnecting...',
                style: TextStyle(color: Colors.grey, fontSize: 12),
              ),
            ],
          ),
        ),
      );
    }

    if (_state!.loggedIn) {
      return HomeScreen(state: _state!);
    }
    return LoginScreen(initialUserId: _state!.userId);
  }
}
