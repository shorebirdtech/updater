import 'dart:isolate';

import 'package:shorebird_code_push/src/shorebird_code_push_base.dart';
import 'package:shorebird_code_push/src/updater.dart';

/// {@template shorebird_code_push}
/// Get info about your Shorebird code push app.
/// {@endtemplate}
class ShorebirdCodePushFfi implements ShorebirdCodePushBase {
  /// {@macro shorebird_code_push}
  ShorebirdCodePushFfi({Updater? updater}) : _updater = updater ?? Updater();

  final Updater _updater;

  @override
  Future<bool> isNewPatchAvailableForDownload() {
    return _runInIsolate((updater) {
      final result = updater.checkForUpdate();
      return result;
    });
  }

  @override
  Future<int?> currentPatchNumber() {
    return _runInIsolate((updater) {
      final currentPatchNumber = updater.currentPatchNumber();
      return currentPatchNumber == 0 ? null : currentPatchNumber;
    });
  }

  @override
  Future<int?> nextPatchNumber() {
    return _runInIsolate(
      (updater) {
        final patchNumber = updater.nextPatchNumber();
        return patchNumber == 0 ? null : patchNumber;
      },
    );
  }

  @override
  Future<void> downloadUpdateIfAvailable() async {
    await _runInIsolate((updater) => updater.downloadUpdate());
  }

  @override
  Future<bool> isNewPatchReadyToInstall() async {
    final patchNumbers =
        await Future.wait([currentPatchNumber(), nextPatchNumber()]);
    final currentPatch = patchNumbers[0];
    final nextPatch = patchNumbers[1];

    return nextPatch != null && currentPatch != nextPatch;
  }

  @override
  bool isShorebirdAvailable() => true;

  /// Creates an [Updater] in a separate isolate and runs the given function.
  Future<T> _runInIsolate<T>(T Function(Updater updater) f) async {
    return Isolate.run(() => f(_updater));
  }
}
