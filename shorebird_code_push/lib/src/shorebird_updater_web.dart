import 'package:shorebird_code_push/src/shorebird_updater.dart';

/// {@template shorebird_updater_io}
/// The Shorebird io updater.
/// {@endtemplate}
class ShorebirdUpdaterImpl implements ShorebirdUpdater {
  @override
  Future<UpdaterState> get state async => const UpdaterUnavailableState();

  @override
  Future<void> update({OnDownloadProgress? onDownloadProgress}) async {
    throw const UpdateException('Shorebird is not available on the web.');
  }
}
