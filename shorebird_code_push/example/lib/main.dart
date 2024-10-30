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
        colorScheme: ColorScheme.fromSeed(seedColor: Colors.green),
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
  var _state = AsyncValue<UpdaterState>.idle();
  var _isUpToDate = AsyncValue<bool>.idle();

  @override
  Future<void> initState() async {
    super.initState();
    setState(() => _state = AsyncValue.loading());
    final state = await _updater.state;
    setState(() => _state = AsyncValue.loaded(state));
  }

  Future<void> _checkForUpdate() async {
    setState(() => _isUpToDate = AsyncValue.loading());
    try {
      final isUpToDate = await _updater.isUpToDate;
      if (!mounted) return;
      setState(() => _isUpToDate = AsyncValue.loaded(isUpToDate));
      if (!isUpToDate) _showUpdateAvailableBanner();
    } catch (error) {
      setState(() => _isUpToDate = AsyncValue.error(error));
    }
  }

  void _showDownloadingBanner() {
    ScaffoldMessenger.of(context).showMaterialBanner(
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
    final state = _state;
    final loading = _isUpToDate is Loading<bool>;

    return Scaffold(
      appBar: AppBar(
        backgroundColor: theme.colorScheme.inversePrimary,
        title: const Text('Shorebird Code Push'),
      ),
      body: Builder(
        builder: (context) {
          return switch (state) {
            Idle<UpdaterState>() =>
              const Center(child: CircularProgressIndicator()),
            Loading() => const Center(child: CircularProgressIndicator()),
            Loaded<UpdaterState>() => _MyHomeBody(state: state.value),
            Error<UpdaterState>() =>
              const Center(child: Text('Something went wrong')),
          };
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

sealed class AsyncValue<T> {
  const AsyncValue._();
  factory AsyncValue.idle() => const Idle();
  factory AsyncValue.loading() => const Loading();
  factory AsyncValue.loaded(T value) => Loaded(value);
  factory AsyncValue.error(Object error) => Error(error);
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
