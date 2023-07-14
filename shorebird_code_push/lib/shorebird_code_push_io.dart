import 'package:shorebird_code_push/src/shorebird_code_push_base.dart';
import 'package:shorebird_code_push/src/shorebird_code_push_ffi.dart';
import 'package:shorebird_code_push/src/shorebird_code_push_noop.dart';
import 'package:shorebird_code_push/src/updater.dart';

/// {@template shorebird_code_push}
/// Get info about your Shorebird code push app.
/// {@endtemplate}
class ShorebirdCodePush implements ShorebirdCodePushBase {
  /// {@macro shorebird_code_push}
  ShorebirdCodePush() {
    try {
      // If the Shorebird Engine is not available, this will throw an exception.
      Updater().currentPatchNumber();
      _delegate = ShorebirdCodePushFfi();
    } catch (error) {
      _delegate = ShorebirdCodePushNoop();
    }
  }

  late final ShorebirdCodePushBase _delegate;

  /// Whether the Shorebird Engine is available.
  @override
  bool isShorebirdAvailable() => _delegate.isShorebirdAvailable();

  /// Checks whether a new patch is available for download.
  ///
  /// Runs in a separate isolate to avoid blocking the UI thread.
  @override
  Future<bool> isNewPatchAvailableForDownload() {
    return _delegate.isNewPatchAvailableForDownload();
  }

  /// The version of the currently-installed patch. `0` if no patch is
  /// installed (i.e., the app is running the release version).
  ///
  /// This will return `null` if Shorebird is not available.
  @override
  Future<int?> currentPatchNumber() => _delegate.currentPatchNumber();

  /// The version of the patch that will be run on the next app launch. If no
  /// new patch has been downloaded, this will be the same as
  /// [currentPatchNumber].
  @override
  Future<int?> nextPatchNumber() => _delegate.nextPatchNumber();

  /// Downloads the latest patch, if available.
  @override
  Future<void> downloadUpdateIfAvailable() =>
      _delegate.downloadUpdateIfAvailable();

  /// Whether a new patch has been downloaded and is ready to install.
  ///
  /// If true, the patch number returned by [nextPatchNumber] will be run on the
  /// next app launch.
  @override
  Future<bool> isNewPatchReadyToInstall() =>
      _delegate.isNewPatchReadyToInstall();
}
