import 'package:flutter/material.dart';

import 'package:shorebird_code_push/shorebird_code_push.dart';

final ShorebirdCodePush shorebirdCodePush = ShorebirdCodePush();

void main() {
  runApp(const MyApp());
}

class MyApp extends StatelessWidget {
  const MyApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Shorebird Code Push Demo',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: Colors.blue),
        useMaterial3: true,
      ),
      home: const MyHomePage(title: 'Shorebird Code Push'),
    );
  }
}

class MyHomePage extends StatefulWidget {
  const MyHomePage({super.key, required this.title});

  final String title;

  @override
  State<MyHomePage> createState() => _MyHomePageState();
}

class _MyHomePageState extends State<MyHomePage> {
  int? _patchVersion;
  bool? _isUpdateAvailable;

  @override
  void initState() {
    super.initState();
    setState(() {
      _patchVersion = shorebirdCodePush.currentPatchVersion();
    });
  }

  Future<void> _checkForUpdate() async {
    _isUpdateAvailable = await shorebirdCodePush.checkForUpdate();
    setState(() {});
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
              _patchVersion != null ? _patchVersion.toString() : 'none',
              style: Theme.of(context).textTheme.headlineMedium,
            ),
            if (_isUpdateAvailable != null)
              Text(
                'Is update available: ${_isUpdateAvailable! ? 'Yes' : 'No'}',
              ),
            ElevatedButton(
              onPressed: () => _checkForUpdate(),
              child: const Text('Check for update'),
            ),
          ],
        ),
      ),
    );
  }
}
