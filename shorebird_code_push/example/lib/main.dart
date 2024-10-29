import 'package:flutter/material.dart';
import 'package:restart_app/restart_app.dart';

import 'package:shorebird_code_push/shorebird_code_push.dart';

void main() => runApp(const MyApp());

class MyApp extends StatelessWidget {
  const MyApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Shorebird Code Push Demo',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: Colors.red),
        useMaterial3: true,
      ),
      home: const MyHomePage(),
    );
  }
}

class MyHomePage extends StatefulWidget {
  const MyHomePage({super.key});

  @override
  State<MyHomePage> createState() => _MyHomePageState();
}

class _MyHomePageState extends State<MyHomePage> {
  final _updater = ShorebirdUpdater();

  bool _loading = false;
  late UpdaterState _state;

  @override
  void initState() {
    super.initState();
    // Check for updates
    _checkForUpdate();
  }

  Future<void> _checkForUpdate() async {
    setState(() => _loading = true);

    final state = await _updater.state;

    if (!mounted) return;

    setState(() {
      _loading = false;
      _state = state;
    });

    _onUpdateCheckComplete(_state);
  }

  void _onUpdateCheckComplete(UpdaterState state) {
    return switch (state) {
      UpdaterAvailableState(isUpToDate: final isUpToDate) when !isUpToDate =>
        _showUpdateAvailableBanner(),
      _ => null,
    };
  }

  void _showDownloadingBanner() {
    ScaffoldMessenger.of(context).showMaterialBanner(
      const MaterialBanner(
        content: Text('Downloading...'),
        actions: [
          SizedBox(
            height: 14,
            width: 14,
            child: CircularProgressIndicator(
              strokeWidth: 2,
            ),
          ),
        ],
      ),
    );
  }

  void _showUpdateAvailableBanner() {
    ScaffoldMessenger.of(context).showMaterialBanner(
      MaterialBanner(
        content: const Text('Update available'),
        actions: [
          TextButton(
            onPressed: () async {
              ScaffoldMessenger.of(context).hideCurrentMaterialBanner();
              await _downloadUpdate();

              if (!mounted) return;
              ScaffoldMessenger.of(context).hideCurrentMaterialBanner();
            },
            child: const Text('Download'),
          ),
        ],
      ),
    );
  }

  void _showRestartBanner() {
    ScaffoldMessenger.of(context).showMaterialBanner(
      const MaterialBanner(
        content: Text('A new patch is ready!'),
        actions: [
          TextButton(
            // Restart the app for the new patch to take effect.
            onPressed: Restart.restartApp,
            child: Text('Restart app'),
          ),
        ],
      ),
    );
  }

  void _showErrorBanner() {
    ScaffoldMessenger.of(context).showMaterialBanner(
      MaterialBanner(
        content: const Text('An error occurred while downloading the update.'),
        actions: [
          TextButton(
            onPressed: () {
              ScaffoldMessenger.of(context).hideCurrentMaterialBanner();
            },
            child: const Text('Dismiss'),
          ),
        ],
      ),
    );
  }

  Future<void> _downloadUpdate() async {
    _showDownloadingBanner();

    try {
      await _updater.update();
      if (!mounted) return;
      ScaffoldMessenger.of(context).hideCurrentMaterialBanner();
      _showRestartBanner();
    } catch (error) {
      _showErrorBanner();
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Scaffold(
      appBar: AppBar(
        backgroundColor: theme.colorScheme.inversePrimary,
        title: const Text('Shorebird Code Push'),
      ),
      body: _MyHomeBody(state: _state),
      floatingActionButton: FloatingActionButton(
        onPressed: _checkForUpdate,
        tooltip: 'Check for update',
        child: _loading ? const _LoadingIndicator() : const Icon(Icons.refresh),
      ),
    );
  }
}

class _MyHomeBody extends StatelessWidget {
  const _MyHomeBody({required this.state});

  final UpdaterState state;

  @override
  Widget build(BuildContext context) {
    return switch (state) {
      UpdaterUnavailableState() => const _ShorebirdUnavailableView(),
      final UpdaterAvailableState state =>
        _ShorebirdAvailableView(state: state),
    };
  }
}

class _ShorebirdUnavailableView extends StatelessWidget {
  const _ShorebirdUnavailableView();

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Center(
      child: Text(
        'Shorebird Engine not available.',
        style: theme.textTheme.bodyLarge?.copyWith(
          color: theme.colorScheme.error,
        ),
      ),
    );
  }
}

class _ShorebirdAvailableView extends StatelessWidget {
  const _ShorebirdAvailableView({required this.state});

  final UpdaterAvailableState state;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final installedPatchNumber = state.installedPatchNumber;
    final heading = installedPatchNumber != null
        ? '$installedPatchNumber'
        : 'No patch installed';

    return Center(
      child: Column(
        mainAxisAlignment: MainAxisAlignment.center,
        children: <Widget>[
          const Text('Current patch version:'),
          Text(
            heading,
            style: theme.textTheme.headlineMedium,
          ),
        ],
      ),
    );
  }
}

class _LoadingIndicator extends StatelessWidget {
  const _LoadingIndicator();

  @override
  Widget build(BuildContext context) {
    return const SizedBox(
      height: 14,
      width: 14,
      child: CircularProgressIndicator(strokeWidth: 2),
    );
  }
}
