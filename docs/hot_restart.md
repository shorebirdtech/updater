# Hot restart

Hot restart applies a downloaded patch without the user relaunching the app:
the Flutter engine tears down the running Dart isolate and boots a fresh one
from the next boot patch, inside the same OS process.

Nothing in Shorebird starts one. The app checks `ShorebirdUpdater.checkForUpdate`
(or completes `ShorebirdUpdater.update`), learns that a patch is waiting, and
calls `ShorebirdHotRestart.dangerouslyHotRestart()` if it judges the moment a
safe one. Only the app knows what the user is in the middle of losing.

## Who does what

The updater cannot restart the Dart VM: only the engine can. The updater's
role is to relay the request and to track the restart lifecycle separately from
cold-boot patch evaluation.

```
package:shorebird_code_push          updater (Rust)              Shorebird engine (C++)
----------------------------         --------------              ----------------------
ShorebirdHotRestart.dangerouslyHotRestart()
  └─ shorebird_restart_app() ──────► restart::request_restart()
                                       └─ registered handler ──► restart host
                                                                   ├─ report_restart_start
                                                                   ├─ resolves next boot patch
                                                                   ├─ relaunches root isolate
                                                                   │   from the patch snapshot
                                                                   └─ report_restart_success
                                                                      (after launch window) /
                                                                      report_restart_failure
```

- `shorebird_set_restart_handler` (engine C surface, `c_api/engine.rs`):
  the engine registers a handler at startup. The handler returns `true` if
  it accepted and scheduled a restart.
- `shorebird_restart_app` (Dart C surface, `c_api/dart.rs`): called by
  `package:shorebird_code_push`. Returns `false` when no handler is
  registered (old engines, debug builds, or unsupported configurations) or
  when a prior hot restart failed or crashed earlier in this release.
- `library/src/restart.rs`: the relay between the two.

## Only a cold boot judges a patch

A hot restart runs inside a process that already has native plugin state,
static initializers, and threads from the prior isolate. A crash or failure
during a hot restart is not clean evidence against the patch, and a survival is
not clean evidence that the patch can cold-boot. Hot restart therefore uses its
own reporting surface (`shorebird_report_restart_start`,
`shorebird_report_restart_success`, `shorebird_report_restart_failure`) and
never touches `currently_booting_patch`, `last_booted_patch`, `mark_bad`,
`PatchInstallSuccess`, or `PatchInstallFailure`:

- `report_restart_start` reads `next_boot_patch`, updates the in-memory
  `running_patch`, and writes `currently_restarting_patch` to `pointers.json`.
- `report_restart_success` (called by the engine after the relaunched isolate's
  launch window closes) clears `currently_restarting_patch`. It does not
  promote `last_booted_patch` or run `cleanup_older_than`, so the prior
  cold-booted patch stays on disk as the rollback target until the new patch
  survives a cold boot.
- `report_restart_failure` clears `currently_restarting_patch`, sets
  `restart_failed = true` in `pointers.json` (so `dangerouslyHotRestart()`
  returns `false` for the rest of this release), excludes the patch in memory
  for the rest of the current process so the engine's recovery relaunch falls
  back to `last_booted_patch` (or the base release), and queues a
  `PatchRestartFailure` (`__patch_restart_failure__`) event.
- If the process crashes while `currently_restarting_patch` is set, the next
  cold boot's `init` clears the marker, sets `restart_failed = true` for the
  rest of the release, and queues a `PatchRestartCrash`
  (`__patch_restart_crash__`) event without marking the patch bad. The cold
  boot then tries the patch under normal cold-boot crash protection.

## Testing

- `library/src/restart.rs`: relay unit tests.
- `library/src/updater.rs` (`hot_restart_tests`): restart lifecycle contract.
- `shorebird_code_push/test/integration/all_test.dart`: full FFI round
  trip against the real Rust library via `library_test_hooks`' fake
  restart handler.
