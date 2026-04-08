// ignore_for_file: one_member_abstracts
//
// ILLUSTRATIVE SKETCH — not wired up, not exported, not tested.
// Accompanies docs/v2_api_design.md. Names and signatures will move.
// Do not import this file from application code.

/// Top-level entry point. One instance per process.
///
/// ```dart
/// await ShorebirdUpdater.instance.checkAndInstall().done;
/// ```
abstract class ShorebirdUpdater {
  /// Process-wide singleton. No constructor; no DI ceremony; no setup call.
  /// If the updater is unavailable in this environment (debug build, non-
  /// Shorebird build, unsupported platform), the returned instance still
  /// works — every operation resolves to [Unavailable] immediately.
  static ShorebirdUpdater get instance => throw UnimplementedError();

  /// Check for and install the latest patch on [track]. Returns a handle
  /// that exposes progress and cancellation; `await handle.done` for the
  /// terminal [UpdateOutcome].
  ///
  /// Calling this while an operation is already in flight joins the
  /// existing operation (same handle semantics, same progress stream).
  /// See design doc open question #1.
  UpdateOperation checkAndInstall({UpdateTrack track = UpdateTrack.stable});

  /// Check whether a patch is available without downloading it.
  ///
  /// Callers that want to install should prefer [checkAndInstall] — this
  /// method exists for UIs that show an "update available" badge.
  Future<CheckOutcome> check({UpdateTrack track = UpdateTrack.stable});

  /// Information about what patch is running now and what's staged for the
  /// next launch. Always returns; never throws.
  Future<PatchState> readPatchState();
}

/// Handle for an in-flight [ShorebirdUpdater.checkAndInstall] call.
abstract class UpdateOperation {
  /// Terminal result. Completes exactly once. Does not throw on expected
  /// outcomes — only on programmer errors (e.g. misuse after dispose).
  Future<UpdateOutcome> get done;

  /// Progress events. Broadcast stream; safe to subscribe late. Closes
  /// when [done] completes.
  Stream<UpdateProgress> get progress;

  /// Request cancellation. Idempotent. [done] will complete with a
  /// [Deferred] whose `reason` is [DeferralReason.cancelled].
  void cancel();
}

// ---------------------------------------------------------------------------
// Terminal outcomes.
// ---------------------------------------------------------------------------

sealed class UpdateOutcome {
  const UpdateOutcome();
}

/// App is already running the newest patch on this track. No work was done
/// beyond the check. This is a success, not an error.
final class UpToDate extends UpdateOutcome {
  const UpToDate({required this.current});

  /// The patch the app is currently running, or `null` if the app is on
  /// the base release with no patches applied.
  final Patch? current;
}

/// A new patch was downloaded and staged. Takes effect on next app launch.
final class Installed extends UpdateOutcome {
  const Installed({required this.patch});
  final Patch patch;
}

/// The updater declined to act right now, but this is expected and the
/// caller should try again later. Always a non-error outcome.
final class Deferred extends UpdateOutcome {
  const Deferred({required this.reason, this.retryAfter});

  final DeferralReason reason;

  /// Hint from the updater about when to retry, if known. `null` means
  /// "no hint — use your own backoff".
  final Duration? retryAfter;
}

enum DeferralReason {
  /// Another update is already running. Joining it wasn't possible
  /// (e.g. the caller used a separate isolate).
  alreadyInProgress,

  /// State storage is currently unwritable (e.g. iOS Data Protection
  /// before first unlock). Try again later.
  storageLocked,

  /// Network is unreachable. Not a hard error — retry when online.
  offline,

  /// The caller requested cancellation.
  cancelled,
}

/// The updater cannot run in this environment. Stable, not retryable.
/// Replaces v1's `isAvailable` bool.
final class Unavailable extends UpdateOutcome {
  const Unavailable({required this.reason});
  final UnavailableReason reason;
}

enum UnavailableReason {
  /// Running under `flutter run` or a debug build.
  debugBuild,

  /// Release build, but not built with `shorebird release` — no engine.
  notShorebirdBuild,

  /// Platform the updater doesn't support (e.g. web, desktop in 2.0).
  unsupportedPlatform,
}

/// Something went wrong. Structured for telemetry and UI.
final class Failed extends UpdateOutcome {
  const Failed({
    required this.category,
    required this.code,
    required this.message,
    required this.retryable,
  });

  final FailureCategory category;

  /// Stable metrics key — safe to group on in dashboards. Examples:
  /// `network.timeout`, `network.dns`, `server.5xx`, `disk.full`,
  /// `integrity.hash_mismatch`, `internal.panic`.
  final String code;

  /// Human-readable message. Not stable; do not parse.
  final String message;

  /// Whether the caller should retry with the same parameters.
  final bool retryable;
}

enum FailureCategory { network, server, disk, integrity, internal }

// ---------------------------------------------------------------------------
// Check-only variant.
// ---------------------------------------------------------------------------

sealed class CheckOutcome {
  const CheckOutcome();
}

final class CheckUpToDate extends CheckOutcome {
  const CheckUpToDate({required this.current});
  final Patch? current;
}

final class UpdateAvailable extends CheckOutcome {
  const UpdateAvailable({required this.patch, required this.sizeBytes});
  final Patch patch;
  final int sizeBytes;
}

final class CheckDeferred extends CheckOutcome {
  const CheckDeferred({required this.reason});
  final DeferralReason reason;
}

final class CheckUnavailable extends CheckOutcome {
  const CheckUnavailable({required this.reason});
  final UnavailableReason reason;
}

final class CheckFailed extends CheckOutcome {
  const CheckFailed({
    required this.category,
    required this.code,
    required this.message,
    required this.retryable,
  });
  final FailureCategory category;
  final String code;
  final String message;
  final bool retryable;
}

// ---------------------------------------------------------------------------
// Progress.
// ---------------------------------------------------------------------------

class UpdateProgress {
  const UpdateProgress({
    required this.phase,
    this.bytesCompleted = 0,
    this.bytesTotal = 0,
  });

  final UpdatePhase phase;
  final int bytesCompleted;
  final int bytesTotal;

  /// 0..1 fraction, or `0` if unknown. Never NaN, never negative.
  double get fraction {
    if (bytesTotal <= 0) return 0;
    final f = bytesCompleted / bytesTotal;
    if (f.isNaN || f < 0) return 0;
    if (f > 1) return 1;
    return f;
  }
}

enum UpdatePhase {
  /// Talking to the server to see if a patch is available.
  checking,

  /// Downloading the patch blob.
  downloading,

  /// Verifying the downloaded blob (hash, signature).
  verifying,

  /// Applying the patch to staged files.
  installing,

  /// Emulated path (old engine without v2 symbols). Coarse-grained.
  running,
}

// ---------------------------------------------------------------------------
// State and tracks.
// ---------------------------------------------------------------------------

class PatchState {
  const PatchState({
    required this.current,
    required this.next,
    required this.track,
    required this.transport,
  });

  /// Currently running patch, or `null` if on the base release.
  final Patch? current;

  /// Patch staged for next launch, or `null`.
  final Patch? next;

  final UpdateTrack track;

  /// Whether the Dart side is talking to a v2-capable native updater or
  /// emulating over the v1 C API. Useful for telemetry.
  final UpdateTransport transport;
}

enum UpdateTransport { native, legacyEmulated }

class Patch {
  const Patch({required this.number});
  final int number;
}

extension type const UpdateTrack(String value) {
  static const staging = UpdateTrack('staging');
  static const beta = UpdateTrack('beta');
  static const stable = UpdateTrack('stable');
  String get name => value;
}
