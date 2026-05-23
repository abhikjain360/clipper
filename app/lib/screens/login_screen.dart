import 'package:flutter/material.dart';
import '../src/rust/api/clipper.dart';
import '../utils/app_platform.dart';
import '../widgets/clipper_brand.dart';
import '../widgets/loading_filled_button.dart';
import '../widgets/responsive_card_scaffold.dart';

class LoginScreen extends StatefulWidget {
  const LoginScreen({super.key});

  @override
  State<LoginScreen> createState() => _LoginScreenState();
}

class _LoginScreenState extends State<LoginScreen> {
  final _passphraseController = TextEditingController();
  late final TextEditingController _serverUrlController;
  bool _loading = false;
  String? _error;

  @override
  void initState() {
    super.initState();
    _serverUrlController = TextEditingController(text: defaultServerUrl());
  }

  @override
  void dispose() {
    _passphraseController.dispose();
    _serverUrlController.dispose();
    super.dispose();
  }

  Future<void> _login() async {
    final passphrase = _passphraseController.text;
    if (passphrase.isEmpty) {
      setState(() => _error = 'Passphrase is required');
      return;
    }

    setState(() {
      _loading = true;
      _error = null;
    });

    try {
      final url = _serverUrlController.text.trim();
      await login(
        passphrase: passphrase,
        deviceName: clipperDeviceName(),
        serverUrl: url.isNotEmpty ? url : defaultServerUrl(),
      );
      // State change will be picked up by AppRoot's watcher
    } catch (e) {
      setState(() => _error = e.toString());
    } finally {
      if (mounted) {
        setState(() => _loading = false);
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: ResponsiveCardScaffold(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const ClipperBrandHeader(),
            const SizedBox(height: 32),
            TextField(
              controller: _serverUrlController,
              decoration: const InputDecoration(
                labelText: 'Server URL',
                border: OutlineInputBorder(),
                prefixIcon: Icon(Icons.dns),
              ),
            ),
            const SizedBox(height: 16),
            TextField(
              controller: _passphraseController,
              obscureText: true,
              decoration: const InputDecoration(
                labelText: 'Passphrase',
                border: OutlineInputBorder(),
                prefixIcon: Icon(Icons.lock),
              ),
              onSubmitted: (_) => _login(),
            ),
            if (_error != null) ...[
              const SizedBox(height: 12),
              Text(
                _error!,
                style: const TextStyle(color: Colors.redAccent),
                textAlign: TextAlign.center,
              ),
            ],
            const SizedBox(height: 24),
            LoadingFilledButton(
              loading: _loading,
              onPressed: _login,
              child: const Text('Login'),
            ),
          ],
        ),
      ),
    );
  }
}
