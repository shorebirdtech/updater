import 'package:shorebird_code_push/src/shorebird_updater_io.dart'
    if (dart.library.js_interop) './shorebird_updater_web.dart';
import 'package:shorebird_code_push/src/updater.dart';

/// Signature for a function that reports download progress.
typedef OnDownloadProgress = void Function(int received, int total);

/// {@template updater_exception}
/// Thrown when an error occurs while performing an update.
/// {@endtemplate}
class UpdaterException implements Exception {
  /// {@macro updater_exception}
  const UpdaterException(this.message);

  /// The error message.
  final String message;

  @override
  String toString() => 'UpdaterException: $message';
}

/// {@template unsupported_platform_exception}
/// Thrown when an operation is not supported on the current platform.
/// {@endtemplate}
class UnsupportedPlatformException extends UpdaterException {
  /// {@macro unsupported_platform_exception}
  const UnsupportedPlatformException()
      : super('Shorebird is not supported on the current platform.');
}

/// {@template updater_unavailable_exception}
/// Thrown when an operation is not supported on the current platform.
/// {@endtemplate}
class UpdaterUnavailableException extends UpdaterException {
  /// {@macro updater_unavailable_exception}
  const UpdaterUnavailableException() : super('''
The Shorebird updater is not available.
This occurs when using package:shorebird_code_push in an app that does not
contain the Shorebird Engine. Most commonly this is due to building with
`flutter build` or `flutter run` instead of `shorebird release`.
''');
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
}

/// The types of patches.
enum PatchType {
  /// The patch which is currently installed and running on the device.
  current,

  /// The next patch which was downloaded and is ready to be installed upon a
  /// restart (see [UpdateStatus.restartRequired]).
  next,
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

  /// Returns information about the specified [PatchType].
  /// Returns `null` if no patch exists for the provided [type].
  Future<Patch?> readPatch(PatchType type);

  /// Checks for available updates and returns the [UpdateStatus].
  /// This method should be used to determine the update status before calling
  /// [update].
  Future<UpdateStatus> checkForUpdate();

  /// Updates the app to the latest patch (if available).
  /// Note: The app must be restarted for the update to take effect.
  ///
  /// [onDownloadProgress] is called with the number of bytes received and the
  /// total number of bytes to be received.
  ///
  /// Throws an [UpdaterException] if an error occurs while updating or if no new
  /// patch is available.
  ///
  /// See also:
  /// * [checkForUpdate], which should be called to check if an update is
  ///   available before calling this method.
  Future<void> update({OnDownloadProgress? onDownloadProgress});
}
