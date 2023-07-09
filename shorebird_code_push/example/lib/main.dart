import 'package:flutter/material.dart';
import 'package:restart_app/restart_app.dart';

import 'package:shorebird_code_push/shorebird_code_push.dart';

// Create an instance of ShorebirdCodePush. Because this example only contains
// a single widget, we create it here, but you will likely only need to create
// a single instance of ShorebirdCodePush in your app.
late final ShorebirdCodePush? _shorebirdCodePush;

void main() async {
  _shorebirdCodePush = await ShorebirdCodePush.initialize();

  runApp(const MyApp());
}

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
      home: const MyHomePage(title: 'Shorebird Code Push'),
    );
  }
}

class MyHomePage extends StatefulWidget {
  const MyHomePage({required this.title, super.key});

  final String title;

  @override
  State<MyHomePage> createState() => _MyHomePageState();
}

class _MyHomePageState extends State<MyHomePage> {
  int? _currentPatchVersion;
  bool _isCheckingForUpdate = false;
  bool get _isShorebirdAvailable => _shorebirdCodePush != null;

  @override
  void initState() {
    super.initState();
    if (_isShorebirdAvailable) {
      // Request the current patch number.
      _shorebirdCodePush!.currentPatchNumber().then((currentPatchVersion) {
        if (!mounted) return;
        setState(() {
          _currentPatchVersion = currentPatchVersion;
        });
      }).onError((error, __) {
        _showErrorDialog(message: error.toString());
      });
    } else {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        _showShorebirdNotFoundDialog();
      });
    }
  }

  Future<void> _checkForUpdate() async {
    setState(() {
      _isCheckingForUpdate = true;
    });

    bool isUpdateAvailable;
    try {
      // Ask the Shorebird servers if there is a new patch available.
      isUpdateAvailable =
          await _shorebirdCodePush!.isNewPatchAvailableForDownload();
    } catch (error) {
      _showErrorDialog(message: error.toString());
      return;
    } finally {
      if (mounted) {
        setState(() {
          _isCheckingForUpdate = false;
        });
      }
    }

    if (!mounted) return;

    if (isUpdateAvailable) {
      _showUpdateAvailableBanner();
    } else {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(
          content: Text('No update available'),
        ),
      );
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
            child: CircularProgressIndicator(
              strokeWidth: 2,
            ),
          )
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

  void _showShorebirdNotFoundDialog() {
    showDialog<void>(
      context: context,
      builder: (context) {
        return AlertDialog(
          title: const Text('Shorebird not available'),
          content: const Text(
            '''This is likely because you are not running with the Shorebird Flutter engine. This is expected if you are running in debug mode or if you used 'flutter run' instead of 'shorebird run'.''',
          ),
          actions: <Widget>[
            TextButton(
              child: const Text('Ok'),
              onPressed: () {
                Navigator.of(context).pop();
              },
            ),
          ],
        );
      },
    );
  }

  void _showErrorDialog({required String message}) {
    showDialog<void>(
      context: context,
      builder: (context) {
        return AlertDialog(
          title: const Text('An error occurred'),
          content: Text(message),
          actions: <Widget>[
            TextButton(
              child: const Text('Ok'),
              onPressed: () {
                Navigator.of(context).pop();
              },
            ),
          ],
        );
      },
    );
  }

  Future<void> _downloadUpdate() async {
    _showDownloadingBanner();
    try {
      await _shorebirdCodePush!.downloadUpdateIfAvailable();
    } catch (error) {
      _showErrorDialog(message: error.toString());
      return;
    } finally {
      if (mounted) {
        ScaffoldMessenger.of(context).hideCurrentMaterialBanner();
      }
    }

    if (!mounted) return;

    _showRestartBanner();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        backgroundColor: Theme.of(context).colorScheme.inversePrimary,
        title: Text(widget.title),
      ),
      body: Center(
        child: Column(
          mainAxisAlignment: MainAxisAlignment.center,
          children: <Widget>[
            const Text('Current patch version:'),
            Text(
              _currentPatchVersion != null
                  ? _currentPatchVersion.toString()
                  : 'No patch installed',
              style: Theme.of(context).textTheme.headlineMedium,
            ),
            const SizedBox(
              height: 20,
            ),
            ElevatedButton(
              onPressed: !_isShorebirdAvailable || _isCheckingForUpdate
                  ? null
                  : _checkForUpdate,
              child: _isCheckingForUpdate
                  ? const SizedBox(
                      height: 14,
                      width: 14,
                      child: CircularProgressIndicator(
                        strokeWidth: 2,
                      ),
                    )
                  : const Text('Check for update'),
            ),
          ],
        ),
      ),
    );
  }
}
