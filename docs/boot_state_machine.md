# Boot State Machine

## Overview

The Shorebird updater manages over-the-air code updates for Flutter
applications. A critical part of this is the **boot state machine**, which
tracks:
- Which patch should be loaded on app start
- Which patch this process is running
- Whether a patch booted successfully
- Automatic rollback if a patch crashes during boot

## State Variables

### Persisted to Disk

Release-level pointers live in `pointers.json` (`ReleasePointers` in
`cache/lifecycle.rs`):

| Variable | Type | Description |
|----------|------|-------------|
| `next_boot_patch` | `Option<usize>` | The patch to boot on next app start. Set when a patch is installed. |
| `last_booted_patch` | `Option<usize>` | The patch that last completed a successful boot cycle. Our "known good" state. |
| `currently_booting_patch` | `Option<usize>` | Boot breadcrumb: set when boot starts, cleared on success/failure. If set on init, the previous boot crashed. |
| `boot_started_at` | `Option<u64>` | When the breadcrumb was set; diagnostic only. |

Each patch has its own lifecycle state in `patches/{N}/state.json`
(`PatchState`): `Downloading`, `Downloaded`, `Installed`, or `Bad`. A `Bad`
tombstone survives cleanup, so a patch that failed is never retried within the
release.

### In-Memory (Session-Scoped)

| Variable | Description |
|----------|-------------|
| `UpdateConfig` | Global config set once via `init()`. Contains app ID, paths, network hooks, etc. |
| `running_patch` | The patch this process is using. Set at `report_launch_start()`, surfaced to Dart as `shorebird_current_boot_patch_number` and sent on patch checks as `current_patch_number`. Survives a server-driven rollback of the running patch (the process is still using it) and resets on every process start. |

## Boot Lifecycle

### Happy Path

```
[Process Start]
       │
       ▼
    init()                                        ConfigureShorebird()
       │
       ├─► Check currently_booting_patch
       │   └─► If set: previous boot crashed → mark patch Bad, fall back
       │
       ▼
    Engine validates and gets next_boot_patch path
       │
       ▼
    ResolveIsolateData() resolves the isolate snapshot
       │
       └─► report_launch_start()  [once per process, guarded in the engine]
       │   ├─► running_patch = next_boot_patch
       │   └─► currently_booting_patch = next_boot_patch
       │
       ▼
    Shell::Shell() constructor, Dart VM created
       │
       └─► report_launch_success()  [once per process, guarded in the engine]
       │   ├─► last_booted_patch = currently_booting_patch
       │   ├─► currently_booting_patch = None
       │   └─► cleanup of patches older than last_booted_patch
       │
       └─► start_update_thread()  [if auto_update, started by the engine]
       │   └─► patch check reports running_patch
       │
       ▼
    [App Running - Dart code executing]
```

### Crash Recovery

If the app crashes between `report_launch_start()` and
`report_launch_success()`:

1. Process dies with `currently_booting_patch` still set on disk
2. New process starts, calls `init()`
3. `handle_prior_boot_failure_if_necessary()` sees `currently_booting_patch` is
   set
4. Marks that patch `Bad { BootCrash }` and clears the breadcrumb
5. Recomputes `next_boot_patch`: `last_booted_patch` if it is still
   `Installed`, otherwise the base release

### Patch Load Failure

If the engine cannot load the patch snapshot after `report_launch_start()`
(`TryLoadFromPatch()` in `runtime/shorebird/patch_cache.cc`), it calls
`report_launch_failure()` and boots the base image. The patch is marked
`Bad` immediately rather than on the next launch, so the process is not left
believing it is running a patch while base code executes.

## Implementation Details

### Where the Engine Makes These Calls

All calls go through the `Updater` shim in
`shell/common/shorebird/updater.h`. The shim owns the once-per-process guards
and the ordering; the Rust library does not guard these calls itself.

- **`report_launch_start()`**: `ResolveIsolateData()` in
  `runtime/dart_snapshot.cc`, when the engine resolves the isolate snapshot
  it is about to load.
- **`report_launch_success()`** / **`report_launch_failure()`**: the
  `Shell::Shell()` constructor in `shell/common/shell.cc`, depending on whether
  the Dart VM was created. `report_launch_failure()` is also called from
  `TryLoadFromPatch()` on a patch load failure.
- **`start_update_thread()`**: from the shim's `ReportLaunchSuccess()`, after
  the success report, when `init()` succeeded and auto-update is enabled.

### Why This Placement Matters

`ConfigureShorebird()` means an engine might boot; the launch reports mean an
engine did boot. Every lifecycle side effect keys off the launch reports:

1. **`report_launch_start()` at snapshot resolution**: A `FlutterEngineGroup`
   or add-to-app host that calls `ConfigureShorebird()` without creating a
   Shell never resolves a snapshot, so it does not record a boot that never
   happened and cannot trigger false crash detection.

2. **`report_launch_success()` in the Shell constructor**: Called after the
   Dart VM is created, indicating the patch loaded successfully.

3. **The update thread after `report_launch_success()`**: The thread must not
   run while a boot is in progress. Its patch check reports `running_patch`,
   which is only set at `report_launch_start()`; a check sent earlier omits
   `current_patch_number`. And an install that completes during a first boot
   would retire the patch being booted (`promote_to_next_boot` only protects
   `last_booted_patch`) and leave the breadcrumb naming a patch that never
   ran. After success the booted patch is `last_booted_patch`, so installs
   are safe. There is no reason to check earlier: an update only applies at
   the next launch.

### Invariants

1. **Config is set once per process**: `set_config()` returns error if already
   set
2. **Crash recovery runs once per process**: Only on first successful `init()`
3. **One boot cycle per process**: The engine's `Updater` shim calls
   `report_launch_start()` and `report_launch_success()` /
   `report_launch_failure()` at most once each, so a second engine in the same
   process (add-to-app) cannot re-report a boot or relabel the running patch
4. **One update thread per process**: Started from the once-guarded success
   report, not per engine
5. **State is persisted atomically**: Each state change writes to disk
   immediately
6. **Bad patches are permanent**: A `Bad` tombstone is never retried for this
   release

## API Behavior

### `init()`
- Sets global config (once per process, subsequent calls return
  `AlreadyInitialized`)
- Calls `handle_prior_boot_failure_if_necessary()` only on first init
- Does NOT run crash recovery if config already initialized

### `report_launch_start()`
- Sets `running_patch` to `next_boot_patch` (or the base release when there is
  none)
- If `next_boot_patch` exists, sets `currently_booting_patch`
- In production, called only once per process; the engine guards it

### `report_launch_success()`
- Clears `currently_booting_patch`
- Sets `last_booted_patch` and cleans up older patches
- Subsequent calls are no-ops if `currently_booting_patch` is None

### `report_launch_failure()`
- Marks `currently_booting_patch` as `Bad`
- Falls back to previous good state
- Queues failure event for server

## Multiple Processes (Android)

If multiple processes access the same state file:
- Each process has its own Java `initCalled` flag
- Each process has its own Rust global config
- BUT they share the same on-disk state files
- No cross-process locking exists

This could cause issues if:
1. Process A sets `currently_booting_patch`
2. Process B also sets `currently_booting_patch` (it's the first engine in THAT
   process)
3. Process A completes, clears flag
4. Process B is killed before completing
5. On restart: crash recovery sees flag set (by Process B) and marks patch as
   bad

## Testing

Unit tests in `library/src/updater.rs` verify the boot state machine:

- `multi_engine_tests::multi_engine_false_positive_rollback`: Demonstrates what
  happens if `report_launch_start()` is called twice in one process (a false
  positive rollback), which is why the engine guards the call
- `multi_engine_tests::interleaved_boot_calls_success_clears_flag`: Verifies
  that `report_launch_success()` properly clears the booting flag
- `state_recovery_tests`: Crash recovery on init
- `patch_check_current_patch_number_tests`: Patch checks carry `running_patch`

The Rust API does not guard against multiple `report_launch_start()` calls, so
the Rust-level tests demonstrate behavior that can only occur if the engine's
guard is bypassed. Engine-side ordering (one thread per process, thread only
after success) is tested in the engine's `Updater` unit tests in `shell/common/shorebird/`.
