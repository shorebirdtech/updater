import 'package:shorebird_code_push/src/shorebird_code_push_ffi.dart';
import 'package:shorebird_code_push/src/shorebird_code_push_noop.dart';
import 'package:shorebird_code_push/src/updater.dart';

/// {@template shorebird_code_push}
/// Get info about your Shorebird code push app.
/// {@endtemplate}
abstract class ShorebirdCodePush {
  /// Creates a [ShorebirdCodePush] instance.
  ///
  /// If the Shorebird Engine is not available, this will return a no-op
  /// implementation.
  ///
  /// If the Shorebird Engine is available, this will return an implementation
  /// that uses ffi to communicate with the Shorebird Engine.
  factory ShorebirdCodePush() {
    try {
      // If the Shorebird Engine is not available, this will throw an exception.
      Updater().currentPatchNumber();
      return ShorebirdCodePushFfi();
    } catch (error) {
      return ShorebirdCodePushNoop();
    }
  }

  /// Whether the Shorebird Engine is available.
  bool isShorebirdAvailable();

  /// Checks whether a new patch is available for download.
  ///
  /// Runs in a separate isolate to avoid blocking the UI thread.
  Future<bool> isNewPatchAvailableForDownload();

  /// The version of the currently-installed patch. `0` if no patch is
  /// installed (i.e., the app is running the release version).
  ///
  /// This will return `null` if Shorebird is not available.
  Future<int?> currentPatchNumber();

  /// The version of the patch that will be run on the next app launch. If no
  /// new patch has been downloaded, this will be the same as
  /// [currentPatchNumber].
  Future<int?> nextPatchNumber();

  /// Downloads the latest patch, if available.
  Future<void> downloadUpdateIfAvailable();

  /// Whether a new patch has been downloaded and is ready to install.
  ///
  /// If true, the patch number returned by [nextPatchNumber] will be run on the
  /// next app launch.
  Future<bool> isNewPatchReadyToInstall();
}
