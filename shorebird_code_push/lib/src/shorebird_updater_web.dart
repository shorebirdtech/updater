import 'package:shorebird_code_push/src/shorebird_updater.dart';
import 'package:shorebird_code_push/src/updater.dart';

class UnsupportedPlatformException extends UpdateException {
  const UnsupportedPlatformException()
      : super('Shorebird is not supported on the web.');
}

/// {@template shorebird_updater_web}
/// The Shorebird web updater.
/// {@endtemplate}
class ShorebirdUpdaterImpl implements ShorebirdUpdater {
  /// {@macro shorebird_updater_web}
  const ShorebirdUpdaterImpl(this._updater);

  // ignore: unused_field
  final Updater _updater;

  @override
  Future<UpdaterState> get state async => const UpdaterUnavailableState();

  @override
  Future<bool> get isUpToDate => throw const UnsupportedPlatformException();

  @override
  Future<void> update({OnDownloadProgress? onDownloadProgress}) {
    throw const UnsupportedPlatformException();
  }
}
