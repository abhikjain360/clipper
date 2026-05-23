import 'package:flutter/material.dart';
import 'src/rust/api/clipper.dart';
import 'src/rust/frb_generated.dart';
import 'screens/login_screen.dart';
import 'screens/home_screen.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();

  runApp(const ClipperApp());
}

class ClipperApp extends StatelessWidget {
  const ClipperApp({super.key});

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
        cardTheme: const CardThemeData(
          color: Color(0xFF1E1E1E),
        ),
        appBarTheme: const AppBarTheme(
          backgroundColor: Color(0xFF1E1E1E),
          elevation: 0,
        ),
      ),
      home: const AppRoot(),
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

        if (state.connectionStatus ==
            BridgeConnectionStatus.daemonNotRunning) {
          setState(() {
            _state = null;
            _connectError = 'Daemon stopped';
          });
          await Future.delayed(const Duration(seconds: 2));
          break; // Back to outer loop to reconnect
        }

        setState(() => _state = state);
      }
    }
  }

  bool get _isDaemonConnected => _state != null;

  @override
  Widget build(BuildContext context) {
    if (!_isDaemonConnected) {
      return Scaffold(
        body: Center(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              const Icon(Icons.cloud_off, size: 64, color: Colors.grey),
              const SizedBox(height: 16),
              const Text(
                'Daemon not running',
                style: TextStyle(fontSize: 18, fontWeight: FontWeight.bold),
              ),
              if (_connectError != null) ...[
                const SizedBox(height: 8),
                Text(
                  _connectError!,
                  style: const TextStyle(color: Colors.grey),
                  textAlign: TextAlign.center,
                ),
              ],
              const SizedBox(height: 24),
              const SizedBox(
                width: 24,
                height: 24,
                child: CircularProgressIndicator(strokeWidth: 2),
              ),
              const SizedBox(height: 8),
              const Text(
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
    return const LoginScreen();
  }
}
