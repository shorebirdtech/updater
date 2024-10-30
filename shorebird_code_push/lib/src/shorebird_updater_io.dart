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
  Future<Patch?> get currentPatch async {
    return Isolate.run(
      () {
        try {
          final currentPatchNumber = _updater.currentPatchNumber();
          return currentPatchNumber > 0
              ? Patch(number: currentPatchNumber)
              : null;
        } catch (_) {
          return null;
        }
      },
    );
  }

  @override
  Future<Patch?> get nextPatch async {
    return Isolate.run(
      () {
        try {
          final nextPatchNumber = _updater.nextPatchNumber();
          return nextPatchNumber > 0 ? Patch(number: nextPatchNumber) : null;
        } catch (_) {
          return null;
        }
      },
    );
  }

  @override
  Future<UpdateStatus> get updateStatus async {
    if (!_isAvailable) return UpdateStatus.unsupported;

    final isUpdateAvailable = await Isolate.run(_updater.checkForUpdate);
    if (isUpdateAvailable) return UpdateStatus.outdated;

    final (current, next) = await (currentPatch, nextPatch).wait;
    return next != null && current?.number != next.number
        ? UpdateStatus.restartRequired
        : UpdateStatus.upToDate;
  }

  @override
  Future<void> update({OnDownloadProgress? onDownloadProgress}) async {
    final hasUpdate = await Isolate.run(_updater.checkForUpdate);
    if (!hasUpdate) throw const UpdateException('No updates available.');
    await Isolate.run(_updater.downloadUpdate);
    final status = await updateStatus;
    if (status != UpdateStatus.restartRequired) {
      throw const UpdateException('Failed to download update.');
    }
  }
}
