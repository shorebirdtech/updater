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

/// {@template updater_state}
/// Information about the current state of the updater.
/// {@endtemplate}
sealed class UpdaterState {
  /// {@macro updater_state}
  const UpdaterState();
}

/// {@template updater_unavailable_state}
/// The state of the updater when the updater is not available.
///
/// The most common reasons for being in this state are:
/// 1. The app is running in debug mode (Shorebird only supports release
///    mode).
/// 2. The app was *NOT* built using `shorebird release` and does *NOT* contain
///    the Shorebird engine.
/// {@endtemplate}
class UpdaterUnavailableState extends UpdaterState {
  /// {@macro updater_unavailable_state}
  const UpdaterUnavailableState();
}

/// {@template updater_available_state}
/// Information about the current state of the updater when the
/// updater is available (e.g. the app was built with Shorebird).
/// {@endtemplate}
class UpdaterAvailableState extends UpdaterState {
  /// {@macro updater_available_state}
  const UpdaterAvailableState({
    required this.installedPatchNumber,
    required this.downloadedPatchNumber,
  });

  /// The patch number of the currently installed patch.
  /// This is the patch that the app is currently running.
  /// Returns null if no patch is installed.
  final int? installedPatchNumber;

  /// The patch number of the patch that has been most recently downloaded.
  /// If no patch has been downloaded, this will be null.
  /// See also:
  /// * [ShorebirdUpdater.patchStatus] to determine whether a new patch is
  ///   available.
  /// * [ShorebirdUpdater.update] to download a new patch.
  final int? downloadedPatchNumber;
}

/// The current status of the updater.
enum PatchStatus {
  /// The app is up to date (e.g. running the latest patch.)
  upToDate,

  /// A new update is available for download.
  outdated,

  /// The app is up to date, but a restart is required for the update to take
  /// effect.
  restartRequired,

  /// The current status is unknown because the updater is not available.
  unsupported,
}

/// {@template shorebird_updater}
/// Manage updates for a Shorebird app.
/// {@endtemplate}
abstract class ShorebirdUpdater {
  /// {@macro shorebird_updater}
  factory ShorebirdUpdater() => const ShorebirdUpdaterImpl(Updater());

  /// The current state of the updater which includes the currently installed
  /// and downloaded patches.
  Future<UpdaterState> get state;

  /// Returns the current [PatchStatus].
  Future<PatchStatus> get patchStatus;

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
