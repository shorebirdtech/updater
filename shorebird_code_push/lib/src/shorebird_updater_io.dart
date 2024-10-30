import 'dart:async';
import 'dart:isolate';

import 'package:shorebird_code_push/src/shorebird_updater.dart';
import 'package:shorebird_code_push/src/updater.dart';

/// {@template shorebird_updater_io}
/// The Shorebird web updater.
/// {@endtemplate}
class ShorebirdUpdaterImpl implements ShorebirdUpdater {
  /// {@macro shorebird_updater_io}
  const ShorebirdUpdaterImpl(this._updater);

  final Updater _updater;

  @override
  Future<UpdaterState> get state async {
    return Isolate.run(
      () {
        try {
          return UpdaterAvailableState(
            installedPatchNumber: _updater.currentPatchNumber(),
            downloadedPatchNumber: _updater.nextPatchNumber(),
          );
        } catch (_) {
          return const UpdaterUnavailableState();
        }
      },
    );
  }

  @override
  Future<bool> get isUpToDate async {
    return Isolate.run(() => !_updater.checkForUpdate());
  }

  @override
  Future<void> update({OnDownloadProgress? onDownloadProgress}) async {
    final hasUpdate = await Isolate.run(_updater.checkForUpdate);
    if (!hasUpdate) throw const UpdateException('No updates available.');
    await Isolate.run(_updater.downloadUpdate);
    final currentState = await state;

    bool didDownloadSucceed(UpdaterState state) {
      return switch (state) {
        UpdaterAvailableState(
          installedPatchNumber: final installedPatchNumber,
          downloadedPatchNumber: final downloadedPatchNumber
        ) =>
          downloadedPatchNumber != null &&
              installedPatchNumber != downloadedPatchNumber,
        _ => false,
      };
    }

    if (!didDownloadSucceed(currentState)) {
      throw const UpdateException('Failed to download update.');
    }
  }
}
