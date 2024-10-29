import 'package:shorebird_code_push/src/shorebird_updater.dart';

/// {@template shorebird_updater_web}
/// The Shorebird web updater.
/// {@endtemplate}
class ShorebirdUpdaterImpl implements ShorebirdUpdater {
  @override
  Future<UpdaterState> get state async => const UpdaterUnavailableState();

  @override
  Future<void> update({OnDownloadProgress? onDownloadProgress}) async {
    await Future<void>.delayed(const Duration(seconds: 1));
  }
}
