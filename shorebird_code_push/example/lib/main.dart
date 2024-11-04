import 'package:flutter/material.dart';
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
  late final bool _isUpdaterAvailable;
  var _isCheckingForUpdates = false;
  Patch? _currentPatch;

  @override
  void initState() {
    super.initState();
    setState(() => _isUpdaterAvailable = _updater.isAvailable);
    _updater.readCurrentPatch().then((currentPatch) {
      setState(() => _currentPatch = currentPatch);
    }).catchError((Object error) {
      debugPrint('Error reading current patch: $error');
    });
  }

  Future<void> _checkForUpdate() async {
    if (_isCheckingForUpdates) return;

    try {
      setState(() => _isCheckingForUpdates = true);
      final status = await _updater.checkForUpdate();
      if (!mounted) return;
      if (status == UpdateStatus.outdated) _showUpdateAvailableBanner();
    } catch (error) {
      debugPrint('Error checking for update: $error');
    } finally {
      setState(() => _isCheckingForUpdates = false);
    }
  }

  void _showDownloadingBanner() {
    ScaffoldMessenger.of(context)
      ..hideCurrentMaterialBanner()
      ..showMaterialBanner(
        const MaterialBanner(
          content: Text('Downloading...'),
          actions: [
            SizedBox(
              height: 14,
              width: 14,
              child: CircularProgressIndicator(),
            ),
          ],
        ),
      );
  }

  void _showUpdateAvailableBanner() {
    ScaffoldMessenger.of(context)
      ..hideCurrentMaterialBanner()
      ..showMaterialBanner(
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
    ScaffoldMessenger.of(context)
      ..hideCurrentMaterialBanner()
      ..showMaterialBanner(
        MaterialBanner(
          content: const Text('A new patch is ready! Please restart your app.'),
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

  void _showErrorBanner(Object error) {
    ScaffoldMessenger.of(context)
      ..hideCurrentMaterialBanner()
      ..showMaterialBanner(
        MaterialBanner(
          content: Text(
            'An error occurred while downloading the update: $error.',
          ),
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
      _showRestartBanner();
    } on UpdateException catch (error) {
      _showErrorBanner(error.message);
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
      body: _isUpdaterAvailable
          ? _PatchInfo(patch: _currentPatch)
          : const _MissingShorebirdUpdater(),
      floatingActionButton: FloatingActionButton(
        onPressed: _isCheckingForUpdates ? null : _checkForUpdate,
        tooltip: 'Check for update',
        child: _isCheckingForUpdates
            ? const _LoadingIndicator()
            : const Icon(Icons.refresh),
      ),
    );
  }
}

class _MissingShorebirdUpdater extends StatelessWidget {
  const _MissingShorebirdUpdater();

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

class _PatchInfo extends StatelessWidget {
  const _PatchInfo({required this.patch});

  final Patch? patch;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Center(
      child: Column(
        mainAxisAlignment: MainAxisAlignment.center,
        children: <Widget>[
          const Text('Current patch version:'),
          Text(
            patch != null ? '${patch!.number}' : 'No patch installed',
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
