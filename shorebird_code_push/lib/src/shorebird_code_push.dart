import 'dart:isolate';

import 'package:shorebird_code_push/src/updater.dart';

/// {@template shorebird_code_push}
/// Get info about your Shorebird code push app.
/// {@endtemplate}
class ShorebirdCodePush {
  /// {@macro shorebird_code_push}
  ShorebirdCodePush({
    Updater Function()? initUpdater, // for testing
  }) : _initUpdater = initUpdater ?? Updater.init {
    _updater = _initUpdater();
  }

  final Updater Function() _initUpdater;
  late final Updater _updater;

  /// Checks whether a new patch is available for download.
  ///
  /// Runs in a separate isolate to avoid blocking the UI thread.
  Future<bool> checkForUpdate() async {
    return Isolate.run(() {
      // Re-initialize the Updater in the new isolate.
      final updater = _initUpdater();
      return updater.checkForUpdate();
    });
  }

  /// The version of the currently-installed patch. Null if no patch is
  /// installed (i.e., the app is running the release version).
  int? currentPatchVersion() {
    return _updater.currentPatchNumber();
  }
}
