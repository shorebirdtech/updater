import 'package:shorebird_code_push/src/shorebird_code_push_base.dart';

/// A no-op implementation of [ShorebirdCodePushBase].
///
/// This is used when the build does not contain the Shorebird Engine.
class ShorebirdCodePushNoop implements ShorebirdCodePushBase {
  @override
  Future<int?> currentPatchNumber() async => null;

  @override
  Future<void> downloadUpdateIfAvailable() async {}

  @override
  Future<bool> isNewPatchAvailableForDownload() async => false;

  @override
  Future<bool> isNewPatchReadyToInstall() async => false;

  @override
  bool isShorebirdAvailable() => false;

  @override
  Future<int?> nextPatchNumber() async => null;
}
