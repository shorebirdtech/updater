import 'package:shorebird_code_push/shorebird_code_push.dart';

/// A no-op implementation of [ShorebirdCodePush].
///
/// This is used when the build does not contain the Shorebird Engine.
class ShorebirdCodePushNoop implements ShorebirdCodePush {
  @override
  Future<int?> currentPatchNumber() async => null;

  @override
  Future<void> downloadUpdateIfAvailable() async {}

  @override
  Future<bool> isNewPatchAvailableForDownload() async => false;

  @override
  Future<bool> isNewPatchReadyToInstall() async => false;

  @override
  Future<int?> nextPatchNumber() async => null;
}
