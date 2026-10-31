import 'package:shorebird_code_push/src/shorebird_hot_restart.dart';
import 'package:shorebird_code_push/src/shorebird_updater.dart';

/// {@template shorebird_hot_restart_web}
/// The Shorebird web hot restart surface.
/// {@endtemplate}
class ShorebirdHotRestartImpl implements ShorebirdHotRestart {
  /// {@macro shorebird_hot_restart_web}
  // Takes the updater only to match the IO signature the factory calls.
  // ignore: avoid_unused_constructor_parameters
  ShorebirdHotRestartImpl({required ShorebirdUpdater updater});

  @override
  Stream<PatchTransition> get patchTransitions => const Stream.empty();

  @override
  Future<bool> dangerouslyHotRestart() async => false;
}
