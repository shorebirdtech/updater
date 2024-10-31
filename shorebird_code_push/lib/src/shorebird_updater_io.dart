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
  Future<Patch?> readPatch(PatchType type) async {
    if (!_isAvailable) throw const UpdaterUnavailableException();

    return Isolate.run(
      () {
        try {
          late final int patchNumber;
          switch (type) {
            case PatchType.current:
              patchNumber = _updater.currentPatchNumber();
            case PatchType.next:
              patchNumber = _updater.nextPatchNumber();
          }
          return patchNumber > 0 ? Patch(number: patchNumber) : null;
        } catch (error) {
          throw UpdaterException('$error');
        }
      },
    );
  }

  @override
  Future<UpdateStatus> checkForUpdate() async {
    if (!_isAvailable) throw const UpdaterUnavailableException();

    final isUpdateAvailable = await Isolate.run(_updater.checkForUpdate);
    if (isUpdateAvailable) return UpdateStatus.outdated;

    final (current, next) =
        await (readPatch(PatchType.current), readPatch(PatchType.next)).wait;
    return next != null && current?.number != next.number
        ? UpdateStatus.restartRequired
        : UpdateStatus.upToDate;
  }

  @override
  Future<void> update({OnDownloadProgress? onDownloadProgress}) async {
    final hasUpdate = await Isolate.run(_updater.checkForUpdate);
    if (!hasUpdate) {
      throw const UpdaterException(
        '''
No update available.
update() should only be called when checkForUpdate() returns UpdateStatus.outdated.''',
      );
    }
    // TODO(felangel): report download progress.
    await Isolate.run(_updater.downloadUpdate);
    final status = await checkForUpdate();
    if (status != UpdateStatus.restartRequired) {
      // TODO(felangel): surface the underlying error reason.
      throw const UpdaterException('Failed to download update.');
    }
  }
}
