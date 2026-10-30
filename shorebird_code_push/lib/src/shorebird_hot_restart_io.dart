import 'dart:async';

import 'package:shorebird_code_push/src/patch_transitions.dart';
import 'package:shorebird_code_push/src/shorebird_hot_restart.dart';
import 'package:shorebird_code_push/src/shorebird_updater.dart';
import 'package:shorebird_code_push/src/updater.dart';

/// {@template shorebird_hot_restart_io}
/// The Shorebird IO hot restart surface.
/// {@endtemplate}
class ShorebirdHotRestartImpl implements ShorebirdHotRestart {
  /// {@macro shorebird_hot_restart_io}
  ShorebirdHotRestartImpl({required ShorebirdUpdater updater, Updater? ffi})
      : _updater = updater,
        _ffi = ffi ?? const Updater();

  final ShorebirdUpdater _updater;

  final Updater _ffi;

  @override
  Stream<PatchTransition> get patchTransitions {
    if (!_updater.isAvailable) return const Stream.empty();
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
      final subscription = patchTransitionPublisher.stream.listen((transition) {
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

  Future<PatchTransition> _readTransition() async {
    final (current, next) =
        await (_updater.readCurrentPatch(), _updater.readNextPatch()).wait;
    return PatchTransition(current: current, next: next);
  }

  @override
  Future<bool> dangerouslyHotRestart() async {
    if (!_updater.isAvailable) return false;

    // Intentionally not run in a separate isolate, because a helper isolate
    // could be torn down mid-call once the restart begins. The updater invokes
    // the engine's registered restart handler synchronously, so this relies on
    // that handler scheduling the restart and returning without waiting for it
    // to complete.
    try {
      return _ffi.restartApp();
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
