# Boot State Machine

## Overview

The Shorebird updater manages over-the-air code updates for Flutter
applications. A critical part of this is the **boot state machine**, which
tracks:
- Which patch should be loaded on app start
- Whether a patch booted successfully
- Automatic rollback if a patch crashes during boot

## State Variables

### Persisted to Disk (`patches_state.json`)

| Variable | Type | Description |
|----------|------|-------------|
| `next_boot_patch` | `Option<PatchMetadata>` | The patch to boot on next app start. Set when a patch is downloaded/installed. |
| `last_booted_patch` | `Option<PatchMetadata>` | The patch that last completed a successful boot cycle. Our "known good" state. |
| `currently_booting_patch` | `Option<PatchMetadata>` | Transient flag: set when boot starts, cleared on success/failure. If set on init, indicates crash. |
| `known_bad_patches` | `HashSet<usize>` | Patches that have failed to boot. Never attempt these again for this release. |

### In-Memory (Config)

| Variable | Description |
|----------|-------------|
| `UpdateConfig` | Global config set once via `init()`. Contains app ID, paths, network hooks, etc. |

## Boot Lifecycle

### Happy Path

```
[Process Start]
       │
       ▼
    init()
       │
       ├─► Check currently_booting_patch
       │   └─► If set: previous boot crashed → mark patch as bad, fall back
       │
       ▼
    Engine gets next_boot_patch path
       │
       ▼
    TryLoadFromPatch() loads patch snapshot
       │
       └─► report_launch_start()  [called once via std::once_flag]
       │   └─► currently_booting_patch = next_boot_patch
       │
       ▼
    Shell::Shell() constructor completes
       │
       └─► report_launch_success()
       │   ├─► last_booted_patch = currently_booting_patch
       │   └─► currently_booting_patch = None
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
4. Marks that patch as failed (adds to `known_bad_patches`)
5. Falls back to `last_booted_patch` or base release

## Implementation Details

### Where Boot Lifecycle Calls Are Made

**`report_launch_start()`** is called from `TryLoadFromPatch()` in
`runtime/shorebird/patch_cache.cc`:

```cpp
std::shared_ptr<const fml::Mapping> TryLoadFromPatch(...) {
  // ... validation and patch loading ...

  // Only report launch_start when we're actually about to use a patch,
  // and only once per process (for the first symbol, isolate data)
  static std::once_flag launch_start_flag;
  if (symbol == kIsolateDataSymbol) {
    std::call_once(launch_start_flag, []() {
      shorebird_report_launch_start();
    });
  }

  // Return the mapping
  // ...
}
```

**`report_launch_success()`** is called from `Shell::Shell()` constructor in
`shell/common/shorebird/shorebird.cc`, after the Dart VM is created
successfully.

### Why This Placement Matters

The boot lifecycle calls are placed at specific points for good reason:

1. **`report_launch_start()` in `TryLoadFromPatch()`**: Called right before the
   patched Dart snapshot is actually loaded. This ensures:
   - FlutterEngineGroup warmup (which doesn't load patches) doesn't trigger
     false crash detection
   - The call only happens when we're actually about to use a patch
   - `std::once_flag` ensures exactly one boot cycle per process

2. **`report_launch_success()` in Shell constructor**: Called after the Dart VM
   is created, indicating the patch loaded successfully.

3. **Crash on patch load failure**: If `TryLoadFromPatch()` fails to load the
   patch, it calls `FML_LOG(FATAL)` which crashes the process. On the next
   launch, crash recovery naturally handles it.

### Invariants

1. **Config is set once per process**: `set_config()` returns error if already
   set
2. **Crash recovery runs once per process**: Only on first successful `init()`
3. **One boot cycle per process**: `std::once_flag` ensures
   `report_launch_start()` is called at most once
4. **State is persisted atomically**: Each state change writes to disk
   immediately
5. **Bad patches are permanent**: Once in `known_bad_patches`, never tried again
   for this release

## API Behavior

### `init()`
- Sets global config (once per process, subsequent calls return
  `AlreadyInitialized`)
- Calls `handle_prior_boot_failure_if_necessary()` only on first init
- Does NOT run crash recovery if config already initialized

### `report_launch_start()`
- If `next_boot_patch` exists, sets `currently_booting_patch`
- In production, called only once per process due to `std::once_flag` in C++

### `report_launch_success()`
- Clears `currently_booting_patch`
- Sets `last_booted_patch`
- Subsequent calls are no-ops if `currently_booting_patch` is None

### `report_launch_failure()`
- Marks `currently_booting_patch` as bad
- Falls back to previous good state
- Queues failure event for server

## Historical Context: FlutterEngineGroup Bug

Prior to the current implementation, `report_launch_start()` was called from
`ConfigureShorebird()` during `FlutterMain::Init()`. This caused a bug:

**Problem**: `FlutterEngineGroup`'s constructor calls
`ensureInitializationComplete()` which triggered `report_launch_start()`, but
does NOT create a Shell (no `report_launch_success()`). If the app was killed
before `createAndRunEngine()` was called, crash recovery incorrectly marked the
patch as bad.

**Solution**: Move `report_launch_start()` to `TryLoadFromPatch()`, which is
only called when actually loading a patch. FlutterEngineGroup never calls
`TryLoadFromPatch()` (no Shell created), so no false positives occur.

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

Unit tests in `library/src/updater.rs` (`multi_engine_tests` module) verify the
boot state machine behavior:

- `multi_engine_false_positive_rollback`: Demonstrates the historical bug where
  multiple `report_launch_start()` calls caused false positive rollbacks
- `interleaved_boot_calls_success_clears_flag`: Verifies that
  `report_launch_success()` properly clears the booting flag

Note: In production, the C++ `std::once_flag` prevents multiple
`report_launch_start()` calls, so the Rust-level tests demonstrate behavior that
can only occur if the C++ guard is bypassed.
