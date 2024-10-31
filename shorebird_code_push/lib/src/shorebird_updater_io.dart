import 'dart:async';
import 'dart:isolate';

import 'package:meta/meta.dart';
import 'package:shorebird_code_push/src/shorebird_updater.dart';
import 'package:shorebird_code_push/src/updater.dart';

@visibleForTesting

/// Type definition for [Isolate.run].
typedef IsolateRun = Future<R> Function<R>(
  FutureOr<R> Function(), {
  String? debugName,
});

/// {@template shorebird_updater_io}
/// The Shorebird web updater.
/// {@endtemplate}
class ShorebirdUpdaterImpl implements ShorebirdUpdater {
  /// {@macro shorebird_updater_io}
  ShorebirdUpdaterImpl(this._updater, {IsolateRun? run})
      : _run = run ?? Isolate.run {
    try {
      // If the Shorebird Engine is not available, this will throw an exception.
      _updater.currentPatchNumber();
      _isAvailable = true;
    } catch (_) {
      logShorebirdEngineUnavailableMessage();
      _isAvailable = false;
    }
  }

  late final bool _isAvailable;

  final Updater _updater;

  final IsolateRun _run;

  @override
  bool get isAvailable => _isAvailable;

  @override
  Future<Patch?> readPatch(PatchType type) async {
    if (!_isAvailable) return null;

    return _run(
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
    if (!_isAvailable) return UpdateStatus.unavailable;

    final isUpdateAvailable = await _run(_updater.checkForUpdate);
    if (isUpdateAvailable) return UpdateStatus.outdated;

    final (current, next) =
        await (readPatch(PatchType.current), readPatch(PatchType.next)).wait;
    return next != null && current?.number != next.number
        ? UpdateStatus.restartRequired
        : UpdateStatus.upToDate;
  }

  @override
  Future<void> update() async {
    if (!_isAvailable) return;

    final hasUpdate = await _run(_updater.checkForUpdate);
    if (!hasUpdate) {
      throw const UpdaterException(
        '''
No update available.
update() should only be called when checkForUpdate() returns UpdateStatus.outdated.''',
      );
    }
    await _run(_updater.downloadUpdate);
    final status = await checkForUpdate();
    if (status != UpdateStatus.restartRequired) {
      // TODO(felangel): surface the underlying error reason.
      throw const UpdaterException('Failed to download update.');
    }
  }
}
