import 'dart:async';
import 'dart:isolate';

import 'package:shorebird_code_push/src/shorebird_updater.dart';
import 'package:shorebird_code_push/src/updater.dart';

/// {@template shorebird_updater_io}
/// The Shorebird web updater.
/// {@endtemplate}
class ShorebirdUpdaterImpl implements ShorebirdUpdater {
  /// {@macro shorebird_updater_io}
  ShorebirdUpdaterImpl(this._updater) {
    try {
      // If the Shorebird Engine is not available, this will throw an exception.
      _updater.currentPatchNumber();
      _isAvailable = true;
    } catch (_) {
      _isAvailable = false;
    }
  }

  late final bool _isAvailable;

  final Updater _updater;

  @override
  bool get isAvailable => _isAvailable;

  @override
  Future<PatchState> get patchState async {
    return Isolate.run(
      () {
        try {
          final currentPatchNumber = _updater.currentPatchNumber();
          final nextPatchNumber = _updater.nextPatchNumber();
          return PatchState(
            current: currentPatchNumber > 0
                ? Patch(number: currentPatchNumber)
                : null,
            next: nextPatchNumber > 0 ? Patch(number: nextPatchNumber) : null,
          );
        } catch (_) {
          return const PatchState();
        }
      },
    );
  }

  @override
  Future<UpdateState> get updateState async {
    if (!_isAvailable) return UpdateState.unsupported;

    final isUpdateAvailable = await Isolate.run(_updater.checkForUpdate);
    if (isUpdateAvailable) return UpdateState.outdated;

    final patch = await patchState;
    return patch.next != null && patch.current?.number != patch.next?.number
        ? UpdateState.restartRequired
        : UpdateState.upToDate;
  }

  @override
  Future<void> update({OnDownloadProgress? onDownloadProgress}) async {
    final hasUpdate = await Isolate.run(_updater.checkForUpdate);
    if (!hasUpdate) throw const UpdateException('No updates available.');
    await Isolate.run(_updater.downloadUpdate);
    final currentUpdateState = await updateState;
    if (currentUpdateState != UpdateState.restartRequired) {
      throw const UpdateException('Failed to download update.');
    }
  }
}
