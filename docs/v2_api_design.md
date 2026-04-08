# `shorebird_code_push` v2 API Design

Status: **Draft / RFC**
Tracking: shorebirdtech/shorebird#3684
Related private context: shorebirdtech/_shorebird#2015

## Motivation

The v1 `ShorebirdUpdater` surface has accumulated enough sharp edges that
further incremental fixes would just be rearranging the same wrong shape:

1. **Exceptions are load-bearing for expected outcomes.** `update()` throws
   `UpdateException` for "no update available", "update already in progress",
   and other non-pathological states. Apps that pipe exceptions into telemetry
   see large volumes of noise that isn't actionable. Fixes #3681, #3682, #3683
   each reclassified one such outcome — but the underlying API shape still
   encourages new ones.
2. **Two-call dance.** The idiomatic flow is `checkForUpdate()` then `update()`,
   with a network round-trip in each. Callers forget, or call them in the wrong
   order, or race them. The "right" shape is one call.
3. **No progress, no cancellation.** `update()` is a black box — callers can't
   show a progress bar and can't back out if the user navigates away. Larger
   patches have made this increasingly visible.
4. **Opaque failure taxonomy.** `UpdateFailureReason` mixes retryable
   (`downloadFailed`) and terminal (`installFailed`) states under the same
   exception type with no guidance on retry behavior. Agents and humans both
   guess.
5. **Platform-unavailable is an exception-adjacent special case.** `isAvailable`
   is a separate bool; forgetting to check it means the other methods return
   stubs or `null`s that look like errors.

This document proposes a v2 surface — a coordinated change across the Rust
C API, the generated Dart FFI bindings, and the public `shorebird_code_push`
package — shipped behind a Dart package major version bump (2.0.0).

## Goals

- **Result-typed, not exception-typed.** Expected outcomes are values; only
  programmer errors throw.
- **Agent-friendly.** One obvious entry point, one obvious verb, exhaustive
  `switch` on a sealed result. Easy to discover via grep and completion. Hard
  to use in a way that produces silent footguns.
- **Observable by construction.** Every terminal state has a stable,
  machine-readable discriminator suitable for metrics keys.
- **Progress and cancellation.** First-class, not bolted on.
- **Retry guidance in the type system.** A failure says whether the caller
  should retry, and roughly when.
- **Version-skew tolerant.** The Dart package ships on its own cadence, and
  may run against an older Rust updater embedded in an older engine. New
  Dart code must degrade gracefully on old Rust.

## Non-goals

- Changing the patch format, the network protocol, or the server.
- Replacing the engine-initiated automatic update path. (v2 is the caller-
  driven API; the engine auto-update continues to exist and is orthogonal.)
- Multi-track orchestration beyond the existing `UpdateTrack` extension type.
- Pause/resume. Cancel + re-run is sufficient for the foreseeable use cases.

## Design overview

### One verb: `checkAndInstall`

The primary entry point collapses `checkForUpdate` + `update` into a single
operation that returns a handle:

```dart
final op = updater.checkAndInstall(track: UpdateTrack.stable);
// Optional: observe progress.
op.progress.listen((p) => setState(() => _percent = p.fraction));
// Await the terminal result.
final result = await op.done;
```

`op.done` is a `Future<UpdateOutcome>` where `UpdateOutcome` is a sealed
class. The caller exhaustively switches on it. There is no `throw` on the
happy path, on the "nothing to do" path, or on the "try again later" path.

Callers who only want the terminal result can write:

```dart
final result = await updater.checkAndInstall().done;
```

Callers who want to fire-and-forget (matching most current usage):

```dart
unawaited(updater.checkAndInstall().done);
```

### Result taxonomy

```dart
sealed class UpdateOutcome {
  const UpdateOutcome();
}

/// App is already running the newest patch on this track. No work done.
final class UpToDate extends UpdateOutcome { ... }

/// A patch was downloaded and staged. Takes effect on next app launch.
final class Installed extends UpdateOutcome {
  final Patch patch;
  ...
}

/// The updater declined to act right now, but this is expected and the
/// caller should try again later. Includes the reason for telemetry.
final class Deferred extends UpdateOutcome {
  final DeferralReason reason;   // e.g. alreadyInProgress, storageLocked, offline
  final Duration? retryAfter;    // hint if the updater knows
  ...
}

/// The updater cannot run in this environment at all. Stable, not retryable.
/// Replaces `isAvailable` + stub methods.
final class Unavailable extends UpdateOutcome {
  final UnavailableReason reason; // debugBuild, notShorebirdBuild, unsupportedPlatform
  ...
}

/// Something actually went wrong. Structured error for telemetry + UI.
final class Failed extends UpdateOutcome {
  final FailureCategory category; // network, server, disk, integrity, internal
  final bool retryable;
  final String message;           // human-readable, not for programmatic use
  final String code;              // stable metrics key, e.g. "download.timeout"
  ...
}
```

Why this shape:

- **Four of five branches are not errors.** The old API made `UpToDate`
  and `Deferred` throw; v2 makes them ordinary values.
- **`Failed.category` is small and stable.** Fine-grained reason lives in
  `code`, a string that telemetry can group on without the Dart enum having
  to enumerate every Rust error.
- **`retryable` is explicit.** Callers and retry frameworks don't have to
  pattern-match on strings.
- **`Unavailable` is a result, not a separate query.** `isAvailable` goes
  away; callers that want to short-circuit write
  `if (result case Unavailable()) return;`.

### Progress and cancellation

```dart
abstract class UpdateOperation {
  /// Terminal result. Never throws on expected outcomes; only throws
  /// `StateError` for programmer errors (e.g. double-cancel after dispose).
  Future<UpdateOutcome> get done;

  /// Broadcast stream of progress events. Completes when `done` completes.
  /// Safe to ignore entirely.
  Stream<UpdateProgress> get progress;

  /// Request cancellation. Idempotent. `done` will complete with a
  /// `Deferred(reason: cancelled)` once the Rust side has unwound.
  void cancel();
}

class UpdateProgress {
  final UpdatePhase phase;       // checking, downloading, verifying, installing
  final int bytesCompleted;      // 0 if unknown
  final int bytesTotal;          // 0 if unknown
  double get fraction;           // 0..1, clamped; 0 if unknown
}
```

Progress events are delivered on a broadcast stream so that late subscribers
(e.g. a progress UI shown after `checkAndInstall` was already called) see
subsequent events without crashing. Events are emitted on the root isolate
so callers don't need to think about isolate boundaries.

### Reading current state

```dart
Future<PatchState> readPatchState();

final class PatchState {
  final Patch? current;   // running now
  final Patch? next;      // staged for next launch
  final UpdateTrack track;
}
```

Returns a value even on `Unavailable` platforms (all fields null, track
defaulted) so callers don't have to special-case. No exceptions for "nothing
installed" — that's just `current: null`.

### The one-liner case

For the very common "fire off an update check on app start, don't care about
the result" pattern:

```dart
await ShorebirdUpdater.instance.checkAndInstall().done;
```

There is no cheaper or simpler form — that's the point. No `isAvailable`
guard, no `checkForUpdate`-then-`update` dance, no try/catch. An agent that
reads the one-line example in the README and copy/pastes it gets correct
behavior, including on debug builds and unsupported platforms.

## Rust / C API changes

The v1 C API exposes status codes as `int32` (`SHOREBIRD_UPDATE_INSTALLED`,
etc.) and treats `update()` as a blocking, opaque call. v2 needs:

1. **A capabilities query.** `shorebird_updater_abi_version() -> u32`. Dart
   bindings check this at startup and fall back to v1 paths when running
   against an old Rust that doesn't understand v2 calls. Unknown function
   symbols are resolved with `dlsym`-equivalent and gated on the ABI version,
   not on symbol presence alone (since partial rollouts exist).
2. **A structured result.** `shorebird_check_and_install(...)` returns a
   `ShorebirdResult` C struct with:
   - `outcome` discriminator (enum: up_to_date, installed, deferred,
     unavailable, failed)
   - `reason_code` (stable string, pointer into static table — no alloc)
   - `retryable` (bool, only meaningful for `failed`)
   - `retry_after_ms` (u64, only meaningful for `deferred`)
   - `patch_number` (u64, only meaningful for `installed`)
3. **Progress callback.** `shorebird_check_and_install` takes a
   `ProgressCallback` function pointer + opaque `user_data`. Callback is
   invoked from the updater worker thread; Dart side marshals onto the root
   isolate via `NativeCallable.listener`.
4. **Cancellation token.** `shorebird_cancel_token_t` created by the caller,
   passed into `shorebird_check_and_install`, signalled by
   `shorebird_cancel_token_signal`. The Rust updater checks at safe points
   (after each network chunk, before hash verification, before install).
   Cancellation surfaces as `outcome = deferred, reason_code = "cancelled"`.
5. **Error-to-reason-code mapping lives in Rust.** No more `anyhow::Error`
   strings crossing the FFI boundary. The mapping table is the source of
   truth for telemetry keys; the Dart side never parses messages.

Backwards compatibility: the existing `shorebird_update`, `shorebird_check_*`
symbols remain. v2 is additive. Old engines ship with only v1; new Dart
packages detect this via `shorebird_updater_abi_version` and emulate v2 on
top of v1 where possible:

- `UpToDate` ← v1 `SHOREBIRD_NO_UPDATE`
- `Installed` ← v1 `SHOREBIRD_UPDATE_INSTALLED`
- `Deferred(alreadyInProgress)` ← v1 `SHOREBIRD_UPDATE_IN_PROGRESS` (#335)
- `Deferred(storageLocked)` ← v1 `SHOREBIRD_UPDATE_DEFERRED` (#336)
- `Failed(...)` ← v1 `SHOREBIRD_UPDATE_ERROR` / `SHOREBIRD_UPDATE_HAD_ERROR`
- `Unavailable` ← Dart side detects missing engine (no change)

Progress and cancellation are simply unavailable on the emulation path;
progress stream emits a single `UpdatePhase.unknown` event and cancel is a
no-op that documents itself as such. This is acceptable because progress and
cancel are opt-in — the happy path (`.done`) works identically on old and
new engines.

## Version-skew strategy

The constraint: a user may upgrade the `shorebird_code_push` Dart package
without upgrading their Flutter engine (and therefore the embedded Rust
updater). We must not break those users.

- The Dart FFI bindings use `DynamicLibrary.lookup` for each new v2 symbol
  and cache whether it resolved. Missing symbols are not an error — they
  just flip the "v2 path available" bit to `false`.
- On the v1 fallback path, `UpdateOperation` is backed by a v1 `update()`
  call wrapped in a `Completer`. Progress emits a synthetic
  `UpdatePhase.running` event at start. Cancel is a no-op.
- Telemetry distinguishes the two paths via `UpdateProgress.phase` and a
  `PatchState.transport` enum (`.native` vs `.legacyEmulated`) so we can
  measure how much of our population is on the fallback.

## Migration from v1

v1 stays compilable in 2.x for one release under a deprecated import:

```dart
// ignore_for_file: deprecated_member_use_from_same_package
import 'package:shorebird_code_push/v1.dart';
```

The old names (`ShorebirdUpdater`, `UpdateException`, `UpdateStatus`, ...)
re-export from `v1.dart` with `@Deprecated` annotations pointing at the
corresponding v2 type. Code mods are provided as a `dart fix` rule in the
package.

Timeline:

- 2.0.0: v2 is the default `package:shorebird_code_push` export; v1 still
  available via `package:shorebird_code_push/v1.dart`.
- 2.x: deprecation warnings, `dart fix` migration.
- 3.0.0: v1 entry point removed.

## Open questions

1. **Single operation at a time?** Should `checkAndInstall()` while one is
   already in flight return a handle to the existing operation, or return a
   new handle that immediately completes with
   `Deferred(alreadyInProgress)`? Proposal: the former — the second caller
   joins the first operation and sees the same progress stream. Matches user
   intent ("I asked for an update, give me the update") and avoids the
   telemetry noise of #3682.
2. **Where does `UpdateTrack` belong?** Keep as-is (extension type), or
   promote to a real class with server-side validation? Leaning keep as-is
   — it's already ergonomic and the extension type costs nothing at runtime.
3. **Progress stream on emulation.** Emit zero events, or emit a coarse
   start/end pair? Leaning start/end so UI code doesn't need an
   "is-stream-empty" special case.
4. **`checkOnly()` escape hatch.** Some callers genuinely want "tell me if
   an update exists without downloading it" (e.g. for a settings screen
   that shows a badge). Should v2 expose this as a separate method or as
   a parameter on `checkAndInstall`? Proposal: separate method
   `Future<CheckResult> check({UpdateTrack? track})` returning a smaller
   union (`UpToDate | UpdateAvailable | Deferred | Unavailable | Failed`).
   Install is not called. Keeps `checkAndInstall` unambiguous.

## What this document is not

This is a proposal, not a plan. Before implementing: agree on the shape,
name-check against the SDK API council conventions, prototype the Rust C
struct return + callback marshalling (the most technically uncertain part),
and validate on a real app that the progress events feel right. The Dart
sketch in `shorebird_code_push/lib/src/v2/` is illustrative — names and
signatures will move.
