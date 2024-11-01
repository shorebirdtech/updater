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
  late bool _isUpdaterAvailable;
  var _currentPatch = AsyncValue<Patch?>.idle();

  @override
  Future<void> initState() async {
    super.initState();
    setState(() {
      _isUpdaterAvailable = _updater.isAvailable;
      _currentPatch = AsyncValue.loading();
    });
    final currentPatch = await _updater.readCurrentPatch();
    setState(() => _currentPatch = AsyncValue.loaded(currentPatch));
  }

  Future<void> _checkForUpdate() async {
    try {
      final status = await _updater.checkForUpdate();
      if (!mounted) return;
      if (status == UpdateStatus.outdated) _showUpdateAvailableBanner();
    } catch (_) {}
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
    final loading = _currentPatch is Loading<bool>;

    return Scaffold(
      appBar: AppBar(
        backgroundColor: theme.colorScheme.inversePrimary,
        title: const Text('Shorebird Code Push'),
      ),
      body: Builder(
        builder: (context) {
          if (!_isUpdaterAvailable) return const _MissingShorebirdUpdater();
          return _currentPatch.when(
            idle: () => const SizedBox.shrink(),
            loading: () => const Center(child: CircularProgressIndicator()),
            loaded: (patch) => _PatchInfo(patch: patch),
            error: (error) => Center(
              child: Text('Oops something went wrong: $error'),
            ),
          );
        },
      ),
      floatingActionButton: FloatingActionButton(
        onPressed: loading ? null : _checkForUpdate,
        tooltip: 'Check for update',
        child: loading ? const _LoadingIndicator() : const Icon(Icons.refresh),
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

sealed class AsyncValue<T> {
  const AsyncValue._();
  factory AsyncValue.idle() => const Idle();
  factory AsyncValue.loading() => const Loading();
  factory AsyncValue.loaded(T value) => Loaded(value);
  factory AsyncValue.error(Object error) => Error(error);

  R when<R>({
    required R Function() idle,
    required R Function() loading,
    required R Function(T value) loaded,
    required R Function(Object error) error,
  }) {
    final value = this;
    if (value is Idle<T>) return idle();
    if (value is Loading<T>) return loading();
    if (value is Loaded<T>) return loaded(value.value);
    if (value is Error<T>) return error(value.error);
    throw AssertionError();
  }
}

class Idle<T> extends AsyncValue<T> {
  const Idle() : super._();
}

class Loading<T> extends AsyncValue<T> {
  const Loading() : super._();
}

class Loaded<T> extends AsyncValue<T> {
  const Loaded(this.value) : super._();
  final T value;
}

class Error<T> extends AsyncValue<T> {
  const Error(this.error) : super._();
  final Object error;
}
