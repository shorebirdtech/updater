import 'package:meta/meta.dart';
import 'package:shorebird_code_push/src/shorebird_hot_restart_io.dart'
    if (dart.library.js_interop) './shorebird_hot_restart_web.dart';
import 'package:shorebird_code_push/src/shorebird_updater.dart';

/// {@template shorebird_hot_restart}
/// Applies a downloaded patch without the user relaunching the app.
///
/// Separate from [ShorebirdUpdater] on purpose. The updater's job is to get a
/// patch onto the disk; deciding that now is a moment the user can afford to
/// lose their place is a different job, and the app is the only thing that can
/// do it.
///
/// ```dart
/// final hotRestart = ShorebirdHotRestart(updater: updater);
/// final status = await updater.checkForUpdate();
/// if (status == UpdateStatus.restartRequired) {
///   // Your call. Prompt, wait for an idle screen, or ignore it and let the
///   // next launch pick the patch up.
///   await hotRestart.dangerouslyHotRestart();
/// }
/// ```
/// {@endtemplate}
@experimental
// Matches the factory-backed abstract class shape of ShorebirdUpdater so the
// surface can grow without breaking callers or test mocks.
// ignore: one_member_abstracts
abstract class ShorebirdHotRestart {
  /// {@macro shorebird_hot_restart}
  factory ShorebirdHotRestart({required ShorebirdUpdater updater}) =>
      ShorebirdHotRestartImpl(updater: updater);

  /// Restarts the app's Dart code so the most recently downloaded patch takes
  /// effect immediately, without the user relaunching the app.
  ///
  /// The name says `dangerously` because the cost is paid by whoever is using
  /// the app at the time: everything on screen is thrown away mid-task, with
  /// no prompt and no undo. Only the code that knows what the user is doing
  /// can judge that this moment is a safe one, which is why nothing in
  /// Shorebird ever calls this for you.
  ///
  /// This tears down the running Dart isolate and boots a fresh one from the
  /// next boot patch (the patch reported by [ShorebirdUpdater.readNextPatch],
  /// or the base release if the current patch was rolled back). It is the
  /// production equivalent of a development-time hot restart: all in-memory
  /// Dart state is lost and `main()` runs again, while the surrounding
  /// platform activity/view controller and plugin registrations are preserved.
  ///
  /// Preserved is not the same as reset. Native state a plugin is holding
  /// — an open socket, a subscription, a camera session, a cached handle
  /// to a Dart callback — survives the restart while the Dart half of it
  /// does not, so `main()` runs again against native objects that think
  /// they are still in use. Plugins written only for a cold start may
  /// double-register listeners or hand the new isolate state belonging to
  /// the old one. Test a restart against the plugins your app actually
  /// uses before shipping one behind a user-facing button.
  ///
  /// Returns `true` if the engine accepted the restart request. When this
  /// returns `true` the restart happens asynchronously. Dart code (including
  /// code immediately after this call) may continue to run briefly before the
  /// isolate is torn down, so don't rely on execution stopping here.
  ///
  /// Returns `false` when hot restart is not supported in the current
  /// environment:
  /// * The updater is not available (see [ShorebirdUpdater.isAvailable]).
  /// * The app is running against a Shorebird engine that predates hot
  ///   restart support.
  /// * More than one Flutter engine is alive in the process (for example in
  ///   add-to-app setups, or while a plugin is running a headless background
  ///   `FlutterEngine` such as `workmanager`, `firebase_messaging`, or
  ///   `flutter_background_service`).
  /// * A hot restart failed or crashed earlier in this release.
  /// * The app is running on a desktop platform (Windows, macOS, or Linux).
  ///   Hot restart is supported on Android and iOS only.
  ///
  /// When this returns `false`, the downloaded patch still takes effect on
  /// the next full app launch, so callers can fall back to prompting the user
  /// to restart the app.
  ///
  /// Note: this restarts Dart code even if no new patch is waiting. Check
  /// [ShorebirdUpdater.checkForUpdate] for [UpdateStatus.restartRequired] (or
  /// call this after [ShorebirdUpdater.update] completes) if you only want to
  /// restart when a patch is pending.
  Future<bool> dangerouslyHotRestart();
}
