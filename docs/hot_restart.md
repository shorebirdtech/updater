# Hot restart

Hot restart applies a downloaded patch without the user relaunching the app:
the Flutter engine tears down the running Dart isolate and boots a fresh one
from the next boot patch, inside the same OS process.

## Who does what

The updater cannot restart the Dart VM — only the engine can. The updater's
role is to relay the request and to keep its boot bookkeeping correct across
more than one launch cycle per process.

```
package:shorebird_code_push          updater (Rust)              Shorebird engine (C++)
----------------------------         --------------              ----------------------
ShorebirdUpdater.restartApp()
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
  registered — old engines, debug builds, or unsupported configurations.
- `library/src/restart.rs`: the relay between the two.

## The second launch cycle

A hot restart is, from the updater's perspective, simply a second
`report_launch_start` → `report_launch_success`/`report_launch_failure`
cycle in the same process:

- `report_launch_start` re-reads `next_boot_patch`, updates the in-memory
  `running_patch`, and records `currently_booting_patch` on disk. If the
  process dies mid-restart, the next cold boot's crash recovery marks the
  patch bad — the same protection cold boots get.
- `report_launch_failure` marks the patch bad and falls back, so the
  engine's recovery relaunch boots something known-good.
- `report_launch_success` records the patch as last booted and reports the
  install event.

The once-per-process guarding of these calls lives in the *engine*
(`shell/common/shorebird/updater.cc`), not in Rust; the engine's restart
host resets those guards when it intentionally starts a new launch cycle.
See `hot_restart_tests` in `library/src/updater.rs` for the contract.

## Testing

- `library/src/restart.rs` — relay unit tests.
- `library/src/updater.rs` (`hot_restart_tests`) — second-launch-cycle
  contract.
- `shorebird_code_push/test/integration/all_test.dart` — full FFI round
  trip against the real Rust library via `library_test_hooks`' fake
  restart handler.
