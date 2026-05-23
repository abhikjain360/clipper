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
  bool _connected = false;
  bool _loggedIn = false;
  String? _connectError;

  @override
  void initState() {
    super.initState();
    _connectToDaemon();
  }

  Future<void> _connectToDaemon() async {
    try {
      await connectToDaemon();
      setState(() {
        _connected = true;
        _connectError = null;
      });
      _watchState();
    } catch (e) {
      setState(() {
        _connected = false;
        _connectError = e.toString();
      });
    }
  }

  Future<void> _watchState() async {
    while (mounted) {
      final state = await getState();
      if (!mounted) return;

      final wasLoggedIn = _loggedIn;
      final newLoggedIn = state.loggedIn;

      if (state.connectionStatus == 'daemon_not_running') {
        setState(() {
          _connected = false;
          _connectError = 'Daemon stopped';
        });
        // Try to reconnect after a delay
        await Future.delayed(const Duration(seconds: 2));
        if (mounted) _connectToDaemon();
        return;
      }

      if (wasLoggedIn != newLoggedIn) {
        setState(() => _loggedIn = newLoggedIn);
      }

      await waitForStateChange();
    }
  }

  void _onLoginSuccess() {
    setState(() => _loggedIn = true);
  }

  void _onLogout() {
    setState(() => _loggedIn = false);
  }

  @override
  Widget build(BuildContext context) {
    if (!_connected) {
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
              ElevatedButton.icon(
                onPressed: _connectToDaemon,
                icon: const Icon(Icons.refresh),
                label: const Text('Retry'),
              ),
            ],
          ),
        ),
      );
    }

    if (_loggedIn) {
      return HomeScreen(onLogout: _onLogout);
    }
    return LoginScreen(onLoginSuccess: _onLoginSuccess);
  }
}
