import 'package:shorebird_code_push/src/shorebird_updater_io.dart'
    if (dart.library.js_interop) './shorebird_updater_web.dart';
import 'package:shorebird_code_push/src/updater.dart';

/// Signature for a function that reports download progress.
typedef OnDownloadProgress = void Function(int received, int total);

/// {@template update_exception}
/// Thrown when an error occurs while performing an update.
/// {@endtemplate}
class UpdateException implements Exception {
  /// {@macro update_exception}
  const UpdateException(this.message);

  /// The error message.
  final String message;

  @override
  String toString() => 'UpdateException: $message';
}

/// {@template patch_state}
/// Information about the current (installed) and next (downloaded) patches.
/// {@endtemplate}
class PatchState {
  /// {@macro patch_state}
  const PatchState({this.current, this.next});

  /// The patch number of the currently installed patch.
  /// This is the patch that the app is currently running.
  /// Returns null if no patch is installed.
  final Patch? current;

  /// The patch number of the patch that has been most recently downloaded.
  /// If no patch has been downloaded, this will be null.
  /// See also:
  /// * [ShorebirdUpdater.updateStatus] to determine whether a new patch is
  ///   available.
  /// * [ShorebirdUpdater.update] to download a new patch.
  final Patch? next;
}

/// {@template patch}
/// An object representing a single patch (over-the-air update).
/// {@endtemplate}
class Patch {
  /// {@macro patch}
  const Patch({required this.number});

  /// The patch number.
  final int number;
}

/// The current status of the app in terms of whether its up-to-date.
enum UpdateStatus {
  /// The app is up to date (e.g. running the latest patch.)
  upToDate,

  /// A new update is available for download.
  outdated,

  /// The app is up to date, but a restart is required for the update to take
  /// effect.
  restartRequired,

  /// The current status is unsupported because the updater is not available.
  unsupported,
}

/// {@template shorebird_updater}
/// Manage updates for a Shorebird app.
/// {@endtemplate}
abstract class ShorebirdUpdater {
  /// {@macro shorebird_updater}
  factory ShorebirdUpdater() => ShorebirdUpdaterImpl(const Updater());

  /// Whether the updater is available on the current platform.
  /// The most common reasons for this returning false are:
  /// 1. The app is running in debug mode (Shorebird only supports release
  ///    mode).
  /// 2. The app was *NOT* built using `shorebird release` and does *NOT*
  ///    contain the Shorebird engine.
  bool get isAvailable;

  /// The current state of the updater which includes the currently installed
  /// and downloaded patches.
  Future<Patch?> get currentPatch;

  /// The current state of the updater which includes the currently installed
  /// and downloaded patches.
  Future<Patch?> get nextPatch;

  /// Returns the current [UpdateStatus].
  Future<UpdateStatus> get updateStatus;

  /// Updates the app to the latest patch.
  /// Note: The app must be restarted for the update to take effect.
  ///
  /// [onDownloadProgress] is called with the number of bytes received and the
  /// total number of bytes to be received.
  ///
  /// Throws an [UpdateException] if an error occurs while updating or if no new
  /// patch is available.
  Future<void> update({OnDownloadProgress? onDownloadProgress});
}
