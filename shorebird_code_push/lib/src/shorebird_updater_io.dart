import 'dart:async';
import 'dart:ffi';
import 'dart:isolate';

import 'package:ffi/ffi.dart';
import 'package:meta/meta.dart';
import 'package:shorebird_code_push/src/generated/updater_bindings.g.dart';
import 'package:shorebird_code_push/src/shorebird_updater.dart';
import 'package:shorebird_code_push/src/updater.dart';

@visibleForTesting

/// Type definition for [Isolate.run].
typedef IsolateRun = Future<R> Function<R>(
  FutureOr<R> Function(), {
  String? debugName,
});

/// {@template shorebird_updater_io}
/// The Shorebird IO Updater.
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
          throw ReadPatchException(message: '$error');
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
          throw ReadPatchException(message: '$error');
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

    late final Pointer<UpdateResult> result;

    try {
      result = await _run(_updater.update);

      if (result == nullptr) {
        const reason = UpdateFailureReason.unknown;
        final message = reason.toFailureMessage();
        throw UpdateException(reason: reason, message: message);
      }

      final status = result.ref.status;

      if (status == 1) return; // SHOREBIRD_UPDATE_SUCCESS

      final reason = status.toFailureReason();
      final details = result.ref.message != nullptr
          ? result.ref.message.cast<Utf8>().toDartString()
          : 'unknown';
      final message = reason.toFailureMessage(details);
      throw UpdateException(message: message, reason: reason);
    } finally {
      _updater.freeUpdateResult(result);
    }
  }
}

extension on int {
  UpdateFailureReason toFailureReason() {
    switch (this) {
      case 0: // SHOREBIRD_NO_UPDATE
        return UpdateFailureReason.noUpdate;
      case 2: // SHOREBIRD_UPDATE_HAD_ERROR
        return UpdateFailureReason.downloadFailed;
      case 3: // SHOREBIRD_UPDATE_IS_BAD_PATCH
        return UpdateFailureReason.badPatch;
      case 4: // SHOREBIRD_UPDATE_ERROR
        return UpdateFailureReason.unknown;
      default:
        return UpdateFailureReason.unknown;
    }
  }
}

extension on UpdateFailureReason {
  String toFailureMessage([String details = '']) {
    switch (this) {
      case UpdateFailureReason.noUpdate:
        return 'No update available.';
      case UpdateFailureReason.badPatch:
        return 'Update available but previously failed to install.';
      case UpdateFailureReason.downloadFailed:
        return 'An error occurred while downloading the patch: $details';
      case UpdateFailureReason.unknown:
        return 'An unknown error occurred.';
    }
  }
}

/*

pub const SHOREBIRD_UPDATE_IS_BAD_PATCH: i32 = 3;
pub const SHOREBIRD_UPDATE_ERROR: i32 = 4;
*/
