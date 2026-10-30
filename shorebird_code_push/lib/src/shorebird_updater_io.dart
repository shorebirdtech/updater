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
  ShorebirdUpdaterImpl({Updater? updater, IsolateRun? run})
      : _updater = updater ?? const Updater(),
        _run = run ?? Isolate.run {
    try {
      // If the Shorebird Engine is not available, this will throw an exception.
      // FIXME: Run this in an isolate or refactor the updater to avoid risking
      // a hang. If another thread is also calling into Shorebird at the same
      // time the underlying Rust code could block getting the config lock.
      _updater.currentPatchNumber();
      _isAvailable = true;
      // We explicitly catch all errors and exceptions to ensure we notify users
      // when the Shorebird Updater is unavailable.
      // ignore: avoid_catches_without_on_clauses
    } catch (_) {
      logShorebirdEngineUnavailableMessage();
      _isAvailable = false;
    }
  }

  late final bool _isAvailable;

  final Updater _updater;

  final IsolateRun _run;

  final _transitions = StreamController<PatchTransition>.broadcast();

  @override
  bool get isAvailable => _isAvailable;

  @override
  Stream<PatchTransition> get patchTransitions {
    if (!_isAvailable) return const Stream.empty();
    return Stream.multi((controller) {
      PatchTransition? last;
      void send(PatchTransition transition) {
        // The same pending transition read twice is one restart seen twice,
        // not two.
        if (transition == last) return;
        last = transition;
        controller.add(transition);
      }

      var sawLive = false;
      final subscription = _transitions.stream.listen((transition) {
        sawLive = true;
        send(transition);
      });
      controller.onCancel = subscription.cancel;

      // A patch can already be waiting from before this listener existed, so
      // a stream carrying only what it saw live would never mention the
      // update() that landed during startup.
      //
      // The catch-up read and a live event race, and each patch read is its
      // own isolate spawn, so the read can resolve after an install that has
      // already been published. A live event is never older than the read
      // this listener started with, so one arriving retires the catch-up
      // rather than being corrected by it.
      //
      // A read that fails says nothing about whether a patch is waiting, and
      // the app's own readCurrentPatch/readNextPatch still report it, so the
      // catch-up stays silent rather than tearing down the subscription.
      _readTransition().then(
        (transition) {
          if (controller.isClosed || sawLive) return;
          if (transition.isPending) send(transition);
        },
        onError: (_, __) {},
      );
    });
  }

  @override
  Future<Patch?> readCurrentPatch() => _readPatch(_updater.currentPatchNumber);

  @override
  Future<Patch?> readNextPatch() => _readPatch(_updater.nextPatchNumber);

  Future<Patch?> _readPatch(int Function() fn) async {
    if (!_isAvailable) return null;
    return _run(
      () {
        try {
          final patchNumber = fn();
          return patchNumber > 0 ? Patch(number: patchNumber) : null;
        } catch (error) {
          throw ReadPatchException(message: '$error');
        }
      },
    );
  }

  @override
  Future<UpdateStatus> checkForUpdate({UpdateTrack? track}) async {
    if (!_isAvailable) return UpdateStatus.unavailable;

    // First, check to see whether an update is available for download.
    final isUpdateAvailable =
        await _run(() => _updater.checkForDownloadableUpdate(track: track));
    if (isUpdateAvailable) return UpdateStatus.outdated;

    // If no new update is available for download, see if a new patch exists
    // on disk that requires a restart. A restart is required when the current
    // and next patches differ, which covers both the "new patch downloaded"
    // case and the "current patch was rolled back" case, where next is null
    // and the app has to restart to revert to the base release.
    final transition = await _readTransition();
    if (!transition.isPending) return UpdateStatus.upToDate;
    _transitions.add(transition);
    return UpdateStatus.restartRequired;
  }

  Future<PatchTransition> _readTransition() async {
    final (current, next) = await (readCurrentPatch(), readNextPatch()).wait;
    return PatchTransition(current: current, next: next);
  }

  @override
  Future<void> update({UpdateTrack? track}) async {
    if (!_isAvailable) return;

    await _install(track: track);

    // Best-effort: the install already succeeded, and a patch that cannot be
    // read is not a reason to report that it failed.
    try {
      final transition = await _readTransition();
      if (transition.isPending) _transitions.add(transition);
      // A ReadPatchException is the expected failure, but the read crosses an
      // isolate boundary, so catch everything rather than let an unforeseen
      // error reach a caller whose install worked.
      // ignore: avoid_catches_without_on_clauses
    } catch (_) {}
  }

  Future<void> _install({UpdateTrack? track}) async {
    final result = await _run(() => _updater.update(track: track));

    const unknownErrorMessage = 'An unknown error occurred.';

    try {
      if (result == nullptr) {
        throw const UpdateException(
          reason: UpdateFailureReason.unknown,
          message: unknownErrorMessage,
        );
      }

      final status = result.ref.status;

      // Successful outcomes of update():
      // - SHOREBIRD_UPDATE_INSTALLED: a new patch was downloaded and installed.
      // - SHOREBIRD_NO_UPDATE: the app is already running the latest patch.
      // - SHOREBIRD_UPDATE_IN_PROGRESS: another update (typically the automatic
      //   updater thread) was already running; the caller did not start a new
      //   one. This is benign and must not surface as an exception.
      if (status == SHOREBIRD_UPDATE_INSTALLED ||
          status == SHOREBIRD_NO_UPDATE ||
          status == SHOREBIRD_UPDATE_IN_PROGRESS) {
        return;
      }

      final reason = status.toFailureReason();
      final message = result.ref.message != nullptr
          ? result.ref.message.cast<Utf8>().toDartString()
          : unknownErrorMessage;
      throw UpdateException(message: message, reason: reason);
    } finally {
      _updater.freeUpdateResult(result);
    }
  }

  @override
  Future<bool> dangerouslyHotRestart() async {
    if (!_isAvailable) return false;

    // Intentionally not run in a separate isolate, because a helper isolate
    // could be torn down mid-call once the restart begins. The updater invokes
    // the engine's registered restart handler synchronously, so this relies on
    // that handler scheduling the restart and returning without waiting for it
    // to complete.
    try {
      return _updater.restartApp();
      // Catch everything: engines that predate hot restart support don't
      // export the symbol, which surfaces as an ArgumentError from the
      // late-bound FFI lookup.
      // ignore: avoid_catches_without_on_clauses
    } catch (error) {
      logRestartUnavailableMessage(error);
      return false;
    }
  }
}

extension on int {
  UpdateFailureReason toFailureReason() {
    switch (this) {
      case SHOREBIRD_NO_UPDATE:
        return UpdateFailureReason.noUpdate;
      case SHOREBIRD_UPDATE_HAD_ERROR:
        return UpdateFailureReason.downloadFailed;
      case SHOREBIRD_UPDATE_IS_BAD_PATCH:
        return UpdateFailureReason.installFailed;
      case SHOREBIRD_UPDATE_ERROR:
        return UpdateFailureReason.unknown;
      default:
        return UpdateFailureReason.unknown;
    }
  }
}
