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

  @override
  bool isShorebirdAvailable() => _delegate.isShorebirdAvailable();

  @override
  Future<bool> isNewPatchAvailableForDownload() {
    return _delegate.isNewPatchAvailableForDownload();
  }

  @override
  Future<int?> currentPatchNumber() => _delegate.currentPatchNumber();

  @override
  Future<int?> nextPatchNumber() => _delegate.nextPatchNumber();

  @override
  Future<void> downloadUpdateIfAvailable() {
    return _delegate.downloadUpdateIfAvailable();
  }

  @override
  Future<bool> isNewPatchReadyToInstall() {
    return _delegate.isNewPatchReadyToInstall();
  }
}
