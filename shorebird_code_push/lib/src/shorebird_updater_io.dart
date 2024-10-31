import 'dart:async';
import 'dart:ffi';
import 'dart:isolate';

import 'package:ffi/ffi.dart';
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
  Future<Patch?> readCurrentPatch() async {
    if (!_isAvailable) return null;

    return _run(
      () {
        try {
          final patchNumber = _updater.currentPatchNumber();
          return patchNumber > 0 ? Patch(number: patchNumber) : null;
        } catch (error) {
          throw UpdaterException('$error');
        }
      },
    );
  }

  @override
  Future<Patch?> readNextPatch() async {
    if (!_isAvailable) return null;

    return _run(
      () {
        try {
          final patchNumber = _updater.nextPatchNumber();
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

    final (current, next) = await (readCurrentPatch(), readNextPatch()).wait;
    return next != null && current?.number != next.number
        ? UpdateStatus.restartRequired
        : UpdateStatus.upToDate;
  }

  @override
  Future<void> update() async {
    if (!_isAvailable) return;

    final error = await _run(_updater.downloadUpdateWithError);
    if (error != nullptr) {
      final reason = error.toDartString();
      _updater.freeString(error);
      // TODO: use a struct from rust instead of a string
      throw UpdaterException(reason);
    }
  }
}
