import 'package:shorebird_code_push/src/shorebird_updater.dart';
import 'package:shorebird_code_push/src/updater.dart';

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
  Future<Patch?> readPatch(PatchType type) =>
      throw const UnsupportedPlatformException();

  @override
  Future<UpdateStatus> checkForUpdate() =>
      throw const UnsupportedPlatformException();

  @override
  Future<void> update({OnDownloadProgress? onDownloadProgress}) {
    throw const UnsupportedPlatformException();
  }
}
