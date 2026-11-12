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
