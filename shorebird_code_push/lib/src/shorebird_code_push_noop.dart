import 'package:shorebird_code_push/src/shorebird_code_push_base.dart';

/// {@template shorebird_code_push_noop}
/// A no-op implementation of [ShorebirdCodePushBase].
///
/// This is used when the build does not contain the Shorebird Engine.
/// {@endtemplate}
class ShorebirdCodePushNoop implements ShorebirdCodePushBase {
  /// {@macro shorebird_code_push_noop}
  ShorebirdCodePushNoop() {
    // ignore: avoid_print
    print('''
ShorebirdCodePush: Shorebird Engine not available.
Using no-op implementation. All methods will return null or false.''');
  }
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
