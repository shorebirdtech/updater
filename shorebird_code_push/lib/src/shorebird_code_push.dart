import 'dart:isolate';

import 'package:meta/meta.dart';
import 'package:shorebird_code_push/src/updater.dart';

/// A logging function for errors arising from interacting with the native code.
///
/// Used to override the default behavior of using [print].
typedef ShorebirdLog = void Function(Object? object, StackTrace? trace);

/// A function that constructs an [Updater] instance. Used for testing.
@visibleForTesting
typedef UpdaterBuilder = Updater Function();

/// A function that prints error object.
///
/// Default use of [ShorebirdLog].
void printError(Object? object, StackTrace? trace) => print(object);

/// {@template shorebird_code_push}
/// Get info about your Shorebird code push app.
/// {@endtemplate}
class ShorebirdCodePush {
  /// {@macro shorebird_code_push}
  ShorebirdCodePush({
    this.logError = printError,
  }) : _buildUpdater = Updater.new;

  /// A test-only constructor that allows overriding the Updater constructor.
  @visibleForTesting
  ShorebirdCodePush.forTest({
    required this.logError,
    required UpdaterBuilder buildUpdater,
  }) : _buildUpdater = buildUpdater;

  /// Logs error messages arising from interacting with the native code.
  ///
  /// Defaults to [print].
  final ShorebirdLog logError;

  final UpdaterBuilder _buildUpdater;
  static const _loggingPrefix = '[ShorebirdCodePush]';

  /// Checks whether a new patch is available for download.
  ///
  /// Runs in a separate isolate to avoid blocking the UI thread.
  Future<bool> isNewPatchAvailableForDownload() async {
    return await _runInIsolate(
      (updater) => updater.checkForUpdate(),
      fallbackValue: false,
    );
  }

  /// The version of the currently-installed patch. Null if no patch is
  /// installed (i.e., the app is running the release version).
  Future<int?> currentPatchNumber() async {
    return await _runInIsolate(
      (updater) {
        final patchNumber = updater.currentPatchNumber();
        return patchNumber == 0 ? null : patchNumber;
      },
      fallbackValue: null,
    );
  }

  /// The version of the patch that will be run on the next app launch. If no
  /// new patch has been downloaded, this will be the same as
  /// [currentPatchNumber].
  Future<int?> nextPatchNumber() async {
    return await _runInIsolate(
      (updater) {
        final patchNumber = updater.nextPatchNumber();
        return patchNumber == 0 ? null : patchNumber;
      },
      fallbackValue: null,
    );
  }

  /// Downloads the latest patch, if available.
  Future<void> downloadUpdateIfAvailable() async {
    await _runInIsolate(
      (updater) => updater.downloadUpdate(),
      fallbackValue: null,
    );
  }

  /// Whether a new patch has been downloaded and is ready to install.
  ///
  /// If true, the patch number returned by [nextPatchNumber] will be run on the
  /// next app launch.
  Future<bool> isNewPatchReadyToInstall() async {
    final patchNumbers =
        await Future.wait([currentPatchNumber(), nextPatchNumber()]);
    final currentPatch = patchNumbers[0];
    final nextPatch = patchNumbers[1];

    return nextPatch != null && currentPatch != nextPatch;
  }

  void _logError(Object error, StackTrace? trace) {
    final logMessage = '$_loggingPrefix $error';
    if (error is ArgumentError) {
      // ffi function lookup failures manifest as ArgumentErrors.
      logError(
        '''
$logMessage
  This is likely because you are not running with the Shorebird Flutter engine (that is, if you ran with `flutter run` instead of `shorebird run`).''',
        trace,
      );
    } else {
      logError(logMessage, trace);
    }
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
    } catch (error, trace) {
      _logError(error, trace);
      return fallbackValue;
    }
  }
}
