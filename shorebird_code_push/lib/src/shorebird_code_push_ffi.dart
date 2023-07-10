import 'dart:isolate';

import 'package:meta/meta.dart';
import 'package:shorebird_code_push/shorebird_code_push.dart';
import 'package:shorebird_code_push/src/updater.dart';

/// A logging function for errors arising from interacting with the native code.
///
/// Used to override the default behavior of using [print].
typedef ShorebirdLog = void Function(Object? object);

/// A function that constructs an [Updater] instance. Used for testing.
@visibleForTesting
typedef UpdaterBuilder = Updater Function();

/// {@template shorebird_code_push}
/// Get info about your Shorebird code push app.
/// {@endtemplate}
class ShorebirdCodePushFfi implements ShorebirdCodePush {
  /// {@macro shorebird_code_push}
  ShorebirdCodePushFfi() : _buildUpdater = Updater.new;

  /// A test-only constructor that allows overriding the Updater constructor.
  @visibleForTesting
  ShorebirdCodePushFfi.forTest({
    required UpdaterBuilder buildUpdater,
  }) : _buildUpdater = buildUpdater;

  final UpdaterBuilder _buildUpdater;

  @override
  Future<bool> isNewPatchAvailableForDownload() {
    return _runInIsolate(
      (updater) => updater.checkForUpdate(),
      fallbackValue: false,
    );
  }

  @override
  Future<int?> currentPatchNumber() {
    return _runInIsolate(
      (updater) {
        final patchNumber = updater.currentPatchNumber();
        return patchNumber == 0 ? null : patchNumber;
      },
      fallbackValue: null,
    );
  }

  @override
  Future<int?> nextPatchNumber() {
    return _runInIsolate(
      (updater) {
        final patchNumber = updater.nextPatchNumber();
        return patchNumber == 0 ? null : patchNumber;
      },
      fallbackValue: null,
    );
  }

  @override
  Future<void> downloadUpdateIfAvailable() async {
    await _runInIsolate(
      (updater) => updater.downloadUpdate(),
      fallbackValue: null,
    );
  }

  @override
  Future<bool> isNewPatchReadyToInstall() async {
    final patchNumbers =
        await Future.wait([currentPatchNumber(), nextPatchNumber()]);
    final currentPatch = patchNumbers[0];
    final nextPatch = patchNumbers[1];

    return nextPatch != null && currentPatch != nextPatch;
  }

  /// Creates an [Updater] in a separate isolate and runs the given function. If
  /// an error occurs, the error is logged and [fallbackValue] is returned.
  Future<T> _runInIsolate<T>(
    T Function(Updater updater) f, {
    required T fallbackValue,
  }) async {
    try {
      // Create a new Updater in the new isolate.
      return await Isolate.run(() => f(_buildUpdater()));
    } catch (error) {
      return fallbackValue;
    }
  }
}
