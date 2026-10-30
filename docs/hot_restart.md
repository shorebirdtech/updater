# Hot restart

Hot restart applies a downloaded patch without the user relaunching the app:
the Flutter engine tears down the running Dart isolate and boots a fresh one
from the next boot patch, inside the same OS process.

Nothing in Shorebird starts one. The app subscribes to
`ShorebirdUpdater.patchTransitions`, learns that a patch is waiting, and
calls `dangerouslyHotRestart()` if it judges the moment a safe one. Only the
app knows what the user is in the middle of losing.

## Who does what

The updater cannot restart the Dart VM: only the engine can. The updater's
role is to relay the request and to keep its boot bookkeeping correct across
more than one launch cycle per process.

```
package:shorebird_code_push          updater (Rust)              Shorebird engine (C++)
----------------------------         --------------              ----------------------
ShorebirdUpdater.dangerouslyHotRestart()
  └─ shorebird_restart_app() ──────► restart::request_restart()
                                       └─ registered handler ──► restart host
                                                                   ├─ resolves next boot patch
                                                                   ├─ begins new launch cycle
                                                                   │   (report_launch_start)
                                                                   ├─ relaunches root isolate
                                                                   │   from the patch snapshot
                                                                   └─ report_launch_success /
                                                                      report_launch_failure
```

- `shorebird_set_restart_handler` (engine C surface, `c_api/engine.rs`):
  the engine registers a handler at startup. The handler returns `true` if
  it accepted and scheduled a restart.
- `shorebird_restart_app` (Dart C surface, `c_api/dart.rs`): called by
  `package:shorebird_code_push`. Returns `false` when no handler is
  registered: old engines, debug builds, or unsupported configurations.
- `library/src/restart.rs`: the relay between the two.

## The second launch cycle

A hot restart is, from the updater's perspective, simply a second
`report_launch_start` → `report_launch_success`/`report_launch_failure`
cycle in the same process:

- `report_launch_start` re-reads `next_boot_patch`, updates the in-memory
  `running_patch`, and records `currently_booting_patch` on disk. It also
  flags the breadcrumb as a repeat cycle when this live process already
  reported a successful launch of that same patch. That evidence is a
  session-scoped global set in `report_launch_success`, not the on-disk
  `last_booted_patch`, which survives a reboot and so cannot say whether
  the running process ever got the patch up. If the process then dies,
  the next launch's crash recovery
  (`handle_prior_boot_failure_if_necessary`) drops a repeat-cycle
  breadcrumb and leaves the patch installed, rather than marking it bad.
  A patch that has run is not condemned by a death nothing attributes to
  it. A first boot in a fresh process carries no flag and is still rolled
  back on a crash, so cold-boot protection is unchanged.
- A spared breadcrumb queues no event. `PatchInstallFailure` would be a
  false statement about a patch that booted, and there is no event type
  for a launch cycle that died without implicating its patch. That is a
  gap in field visibility, and closing it needs a server-side contract
  change rather than a change here.
- `report_launch_failure` marks the patch bad and falls back, so the
  engine's recovery relaunch boots something known-good.
- `report_launch_success` records the patch as last booted and reports the
  install event.

The once-per-process guarding of these calls lives in the *engine*
(`shell/common/shorebird/updater.cc`), not in Rust. The engine's restart
host resets those guards when it intentionally starts a new launch cycle.
See `hot_restart_tests` in `library/src/updater.rs` for the contract.

## Testing

- `library/src/restart.rs`: relay unit tests.
- `library/src/updater.rs` (`hot_restart_tests`): second-launch-cycle
  contract.
- `shorebird_code_push/test/integration/all_test.dart`: full FFI round
  trip against the real Rust library via `library_test_hooks`' fake
  restart handler.
