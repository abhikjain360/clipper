import 'package:flutter/material.dart';
import '../src/rust/api/clipper.dart';
import '../utils/app_platform.dart';
import '../widgets/clipper_brand.dart';
import '../widgets/loading_filled_button.dart';
import '../widgets/responsive_card_scaffold.dart';

enum _AuthMode { login, register }

class LoginScreen extends StatefulWidget {
  final String? initialUserId;

  const LoginScreen({super.key, this.initialUserId});

  @override
  State<LoginScreen> createState() => _LoginScreenState();
}

class _LoginScreenState extends State<LoginScreen> {
  _AuthMode _mode = _AuthMode.login;
  final _accessKeyController = TextEditingController();
  final _passphraseController = TextEditingController();
  final _confirmPassphraseController = TextEditingController();
  late final TextEditingController _userIdController;
  late final TextEditingController _serverUrlController;
  bool _loading = false;
  String? _error;

  @override
  void initState() {
    super.initState();
    _userIdController = TextEditingController(text: widget.initialUserId ?? '');
    _serverUrlController = TextEditingController(text: defaultServerUrl());
  }

  @override
  void dispose() {
    _accessKeyController.dispose();
    _passphraseController.dispose();
    _confirmPassphraseController.dispose();
    _userIdController.dispose();
    _serverUrlController.dispose();
    super.dispose();
  }

  String get _resolvedServerUrl {
    final url = _serverUrlController.text.trim();
    return url.isNotEmpty ? url : defaultServerUrl();
  }

  void _setMode(_AuthMode mode) {
    if (_mode == mode || _loading) return;
    setState(() {
      _mode = mode;
      _error = null;
    });
  }

  Future<void> _submit() {
    return switch (_mode) {
      _AuthMode.login => _login(),
      _AuthMode.register => _register(),
    };
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
      final userId = _userIdController.text.trim();
      await login(
        passphrase: passphrase,
        userId: userId.isEmpty ? null : userId,
        deviceName: clipperDeviceName(),
        serverUrl: _resolvedServerUrl,
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

  Future<void> _register() async {
    final accessKey = _accessKeyController.text.trim();
    final passphrase = _passphraseController.text;
    final confirmPassphrase = _confirmPassphraseController.text;

    if (accessKey.isEmpty) {
      setState(() => _error = 'Access key is required');
      return;
    }
    if (passphrase.isEmpty) {
      setState(() => _error = 'Passphrase is required');
      return;
    }
    if (passphrase != confirmPassphrase) {
      setState(() => _error = 'Passphrases do not match');
      return;
    }

    setState(() {
      _loading = true;
      _error = null;
    });

    try {
      final userId = await register(
        accessKey: accessKey,
        passphrase: passphrase,
        deviceName: clipperDeviceName(),
        serverUrl: _resolvedServerUrl,
      );
      _userIdController.text = userId;
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
            SizedBox(
              width: double.infinity,
              child: SegmentedButton<_AuthMode>(
                segments: const [
                  ButtonSegment(
                    value: _AuthMode.login,
                    icon: Icon(Icons.login),
                    label: Text('Login'),
                  ),
                  ButtonSegment(
                    value: _AuthMode.register,
                    icon: Icon(Icons.key),
                    label: Text('Register'),
                  ),
                ],
                selected: {_mode},
                onSelectionChanged: _loading
                    ? null
                    : (selection) => _setMode(selection.single),
              ),
            ),
            const SizedBox(height: 24),
            TextField(
              controller: _serverUrlController,
              decoration: const InputDecoration(
                labelText: 'Server URL',
                border: OutlineInputBorder(),
                prefixIcon: Icon(Icons.dns),
              ),
            ),
            const SizedBox(height: 16),
            AnimatedSwitcher(
              duration: const Duration(milliseconds: 180),
              child: _mode == _AuthMode.login
                  ? _buildLoginFields()
                  : _buildRegisterFields(),
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
              onPressed: _submit,
              child: Text(_mode == _AuthMode.login ? 'Login' : 'Register'),
            ),
          ],
        ),
      ),
    );
  }

  Widget _buildLoginFields() {
    return Column(
      key: const ValueKey('login-fields'),
      mainAxisSize: MainAxisSize.min,
      children: [
        TextField(
          controller: _userIdController,
          decoration: const InputDecoration(
            labelText: 'User ID',
            border: OutlineInputBorder(),
            prefixIcon: Icon(Icons.person),
          ),
          textInputAction: TextInputAction.next,
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
      ],
    );
  }

  Widget _buildRegisterFields() {
    return Column(
      key: const ValueKey('register-fields'),
      mainAxisSize: MainAxisSize.min,
      children: [
        TextField(
          controller: _accessKeyController,
          decoration: const InputDecoration(
            labelText: 'Access key',
            border: OutlineInputBorder(),
            prefixIcon: Icon(Icons.vpn_key),
          ),
          textInputAction: TextInputAction.next,
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
          textInputAction: TextInputAction.next,
        ),
        const SizedBox(height: 16),
        TextField(
          controller: _confirmPassphraseController,
          obscureText: true,
          decoration: const InputDecoration(
            labelText: 'Confirm passphrase',
            border: OutlineInputBorder(),
            prefixIcon: Icon(Icons.lock_reset),
          ),
          onSubmitted: (_) => _register(),
        ),
      ],
    );
  }
}
