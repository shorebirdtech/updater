import 'package:shorebird_code_push/src/shorebird_updater.dart';
import 'package:shorebird_code_push/src/updater.dart';

/// {@template unsupported_platform_exception}
/// Thrown when an operation is not supported on the current platform.
/// {@endtemplate}
class UnsupportedPlatformException extends UpdateException {
  /// {@macro unsupported_platform_exception}
  const UnsupportedPlatformException()
      : super('Shorebird is not supported on the web.');
}

/// {@template shorebird_updater_web}
/// The Shorebird web updater.
/// {@endtemplate}
class ShorebirdUpdaterImpl implements ShorebirdUpdater {
  /// {@macro shorebird_updater_web}
  ShorebirdUpdaterImpl(this._updater);

  // ignore: unused_field
  final Updater _updater;

  @override
  bool get isAvailable => false;

  @override
  Future<Patch?> get currentPatch => throw const UnsupportedPlatformException();

  @override
  Future<Patch?> get nextPatch => throw const UnsupportedPlatformException();

  @override
  Future<UpdateStatus> get updateStatus =>
      throw const UnsupportedPlatformException();

  @override
  Future<void> update({OnDownloadProgress? onDownloadProgress}) {
    throw const UnsupportedPlatformException();
  }
}
