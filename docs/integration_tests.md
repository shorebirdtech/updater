# Integration Tests for the Updater

Status: design exploration. This is a proposal, not a committed plan. The goal
is to agree on the shape before writing harness code.

## Why we are talking about this

Most recent updater bugs have lived at the seam between Rust core, FFI, and
the `shorebird_code_push` Dart wrapper. A non-exhaustive recent sample:

- `checkForUpdate` returns `upToDate` after a patch-to-release rollback
  (shorebirdtech/shorebird#3728, originally #3206). The Rust unit test
  passed because it stubbed `currentPatchNumber`. The real FFI path cleared
  `last_booted_patch` and made the Dart side compare `null != null`.
- `update()` re-downloads bytes that are deterministically bad on this
  device (updater #351; design follow-on shorebirdtech/shorebird#3737). The
  Rust-side network hooks made it easy to test the resume logic in
  isolation, but not the cross-cycle "we just installed this and it
  hash-failed" loop.
- `update()` throwing `UpdateException` for `noUpdate` (shorebird #3681,
  updater #334) and for benign `UpdateInProgress` (shorebird #3682, updater
  #335). Both reproduced cleanly only when the Dart layer was driving real
  Rust state.
- `patches_state.json` write failures on iOS (shorebird #3683). Not all of
  this is reproducible on desktop, but the on-disk round-trip part is.

What these have in common: each one was discoverable only when Dart, FFI,
and on-disk Rust state were all participating. Today we have:

- **Rust unit tests** with `NetworkHooks::default` swapped for fakes — good
  coverage of branching logic, no FFI, no `DynamicLibrary`.
- **Dart unit tests** with `_MockUpdater` swapped in for the FFI wrapper —
  good coverage of `ShorebirdUpdaterImpl`, never loads `libupdater`.

There is no test that drives the Dart `ShorebirdUpdater` API and ends up
reading and writing real bytes via the real Rust network and cache code.

## Proposal

Add a desktop-only integration suite that runs:

```
ShorebirdUpdater (Dart)
  → DynamicLibrary.open("libupdater.dylib")  ← real cdylib build
  → c_api::dart + c_api::engine               ← real FFI
  → updater::* + cache::* + network::*        ← real Rust
  → 127.0.0.1:<port>                          ← FakePatchServer (Dart shelf)
```

Each test gets a fresh tempdir for `app_storage_dir` and `code_cache_dir`,
boots a fresh fake server, calls `shorebird_init`, and then exercises the
Dart `ShorebirdUpdater` API. The `cdylib` build target already exists
(`library/Cargo.toml` lists `cdylib`); today nothing actually loads it.

This is intentionally narrower than a true end-to-end test. We are *not*
spinning up a Flutter app, *not* exercising the engine integration in
`flutter::shorebird::Updater`, and *not* running on a phone. Those remain
the job of `shorebird preview`, engine smoke tests, and production
telemetry. What we get here is the layer that has been quietly producing
the most bugs.

## Components to build

### 1. A test-hooks library target

Add a new workspace crate, e.g. `library_test_hooks/`, with
`crate-type = ["cdylib"]`. It depends on the `updater` crate as a path
dependency with a `test-hooks` Cargo feature enabled. The crate's job is
to add a small set of test-only `#[no_mangle] pub extern "C"` symbols
that wrap *internal Rust functions* in `updater` — not new C entry
points exposed by the updater crate itself.

Guiding principle: production packages stay clean. The production C
API in `c_api::dart` and `c_api::engine` does not gain any test-only
symbols. Where the test_hooks cdylib needs to reach behind the C API
(reset the global config, seed an installed patch, force a clock
value), it does so via *Rust-internal* `pub` items in `updater` that
are gated behind the `test-hooks` Cargo feature. Those items already
exist for unit tests (`testing_reset_config`, `test_utils::*`); the
feature flag is what makes them visible to a sibling crate without
also exposing them to production builds.

Concretely, `library/Cargo.toml` grows:

```toml
[features]
test-hooks = []
```

and items like `library/src/config.rs:200` move from
`#[cfg(test)] pub fn testing_reset_config(...)` to
`#[cfg(any(test, feature = "test-hooks"))] pub fn testing_reset_config(...)`.

The test_hooks cdylib then wraps them:

```rust
#[no_mangle]
pub extern "C" fn shorebird_test_reset() {
    updater::testing_reset_config();
}
```

Plus seeding helpers as we need them
(`shorebird_test_install_fake_patch(usize)`,
`shorebird_test_corrupt_patches_state()`, etc.).

The production C symbols (`shorebird_init`, `shorebird_update_with_result`,
…) come along automatically: the `updater` crate is consumed as an rlib
here, and `#[no_mangle]` exports from an rlib propagate into the
dependent's cdylib. One artifact, `libupdater_test_hooks.{dylib,so,dll}`,
exposes both surfaces.

Why this way:

- No test-only C symbols in the cdylib/staticlib that ships inside the
  engine. The `test-hooks` feature is opt-in and only the test_hooks
  crate enables it.
- All scenarios run in a single Dart process. Reset between tests with
  one C call instead of forking a subprocess each time.
- A natural place to add diagnostic / fault-injection hooks later
  without touching production code.

The test_hooks crate also owns the engine-side symbols we need for tests
(`shorebird_init`, `shorebird_report_launch_*`, etc.). They are part of
the engine API today; since this crate's cdylib is never linked into a
Flutter engine build, exposing them here doesn't widen what production
ships.

### 2. Loading the library: use the existing test seam

`Updater.bindings` at `shorebird_code_push/lib/src/updater.dart:20` is
already a `@visibleForTesting` static field, and the existing unit
tests reassign it (`shorebird_code_push/test/src/updater_test.dart:25`).
We use the same seam:

```dart
setUpAll(() async {
  final path = await buildTestHooksCdylib();         // cargo build -p library_test_hooks
  testHooksLib = DynamicLibrary.open(path);          // single handle
  Updater.bindings = UpdaterBindings(testHooksLib);  // production path uses our lib
  testHooks = TestHooksBindings(testHooksLib);       // engine + reset symbols
});
```

This is zero changes to `shorebird_code_push`, works the same on Linux,
macOS, and Windows, and produces no `RTLD_GLOBAL` / `LoadLibrary`
platform-specific code in the harness. Static-field initializers in
Dart are lazy, so as long as the override happens in `setUpAll` before
any production code path touches `Updater.bindings`,
`DynamicLibrary.process()` is never called and there is no question of
"will it find the symbols."

We deliberately keep using the same `Updater` / `UpdaterBindings` types
that production uses. The only Dart-side test-specific code is the
auxiliary `TestHooksBindings` (a separate ffigen-generated file in the
integration-test tree) that exposes the `shorebird_test_*` and
`shorebird_init` / `shorebird_report_launch_*` symbols.

Locating the artifact: a `tool/build_test_hooks.dart` invoked from
`setUpAll` shells out to `cargo build -p library_test_hooks` and returns
`target/debug/libupdater_test_hooks.{dylib,so,dll}`. Cached across tests
in the same run.

### 3. Engine + test-hooks bindings live under `test/integration/`

`library_test_hooks` emits its own header (cbindgen, same pattern as
`updater`), and ffigen generates a `TestHooksBindings` Dart file
inside `shorebird_code_push/test/integration/generated/` — **not**
inside `shorebird_code_push/lib/`. The published package keeps shipping
only the Dart-stable surface; the test-only bindings are part of the
test tree, where their dev-time-only nature is obvious and they don't
contribute to the package's public API.

### 4. `FakePatchServer`

A small `package:shelf` server that the test drives. Sketch:

```dart
final server = await FakePatchServer.start();
server.enqueuePatch(number: 1, libapp: <bytes>, releaseLibapp: <bytes>);
server.respondToCheckWith(PatchCheckResponse(...));
server.failNextDownload(after: 1024); // cut off mid-stream
server.serveCorruptedHash();
```

Endpoints we need (today):

- `POST /api/v1/patches/check` — returns `PatchCheckResponse` JSON.
- `GET <patch_url>` — serves patch bytes, with `Range` header support so
  resume tests are real (`network.rs` already sends `Range: bytes=N-`).
- `POST /api/v1/patches/events` — accepts and records events for assertion.

`base_url` is read from `shorebird.yaml`, so each test writes a yaml
pointing at the test server's `127.0.0.1:<ephemeral-port>`.

### 5. Patch fixtures

Patches are zstd-compressed bipatch files keyed to a specific base
`libapp.so` (or its iOS equivalent). The fake server has to serve real
bytes: a hand-rolled `[0,1,2,3]` will fail `bipatch::Reader::new` and tell
us nothing useful.

Two options:

**Option A:** Pre-bake a few patch fixtures and check them in. Simple,
fast tests, but binary diffs are unreviewable and we are stuck with
whatever scenarios we baked in.

**Option B:** Build patches on the fly using the `patch` workspace crate.
`setUpAll` shells out to `cargo run -p patch` (or links the crate as a
build dependency) to produce a real patch from controlled inputs. Slower
setup; tests stay readable; we can synthesize hash-mismatch scenarios by
swapping bytes after generation.

Lean B. Cache the compiled `patch` binary across tests.

### 6. Storage scaffolding

Each test boots in a fresh tempdir:

```dart
final tmp = await Directory.systemTemp.createTemp('updater-it-');
addTearDown(() => tmp.delete(recursive: true));
final storageDir = Directory('${tmp.path}/storage')..createSync();
final cacheDir   = Directory('${tmp.path}/cache')..createSync();
```

`shorebird_init` is given those paths; we never touch the developer's real
state. Tests can also pre-seed `patches_state.json` to simulate "device
arrives in state X" scenarios without running through the install path.

## First-cut scenarios

Each maps to a real bug or a known fragile path:

| Scenario | Asserts | Maps to |
|---|---|---|
| Server returns no patch | `checkForUpdate` returns `upToDate`, no disk writes | baseline |
| Check + update + report_launch_success | `nextPatchNumber` advances; `current_boot_patch` set after launch cycle | baseline |
| Patch-to-release rollback during running session | `current_boot_patch_number` does not silently go to 0 | shorebird #3728, #3206 |
| Hash mismatch on freshly downloaded patch | Patch is marked bad; subsequent `update()` does not re-fetch | updater #351, shorebird #3737 |
| Download cut off mid-stream | Resume on next `update()` succeeds; bytes match | updater resume tests today, but at the FFI level |
| Concurrent `update()` calls | Second call returns `UpdateInProgress`; does not throw | updater #335, shorebird #3682 |
| `update()` when nothing to do | Returns `noUpdate`; does not throw | updater #334, shorebird #3681 |
| Storage dir becomes unwritable mid-cycle | Defers state write; later cycle recovers | updater #336, #344 |

Each test is small; the value is having them all run together against the
same harness so future regressions show up here first.

## Non-goals

- Phone or emulator coverage. Stays with `shorebird preview` and engine
  smoke tests.
- Engine integration (`flutter::shorebird::Updater`). Tested in
  `shorebirdtech/flutter`.
- Replacing existing Rust unit tests. They are faster and exhaustively
  cover branch logic; this suite is wider, not deeper.
- Network-level fuzzing or fault injection beyond what the fake server
  exposes.

## Open questions

- **Process model.** All integration tests live in a single file
  (e.g. `test/integration/all_test.dart`). `package:test` runs
  *files* in parallel isolates by default but *tests within a file*
  serially in the same isolate, and `dlopen` loads the test_hooks
  cdylib exactly once per process — so every isolate would share the
  Rust `OnceCell<UpdateConfig>`. Single file = single isolate = serial
  = no contention. The unit tests in the same package keep their
  parallelism since they only ever touch `_MockUpdaterBindings` and
  never load the real library.

  Add a comment at the top of the integration test file explaining
  why everything lives in one file, so a future contributor doesn't
  "tidy up" by splitting it without also adding a `dart_test.yaml`
  concurrency override. Between tests, `shorebird_test_reset()`
  clears the global config; subprocess-per-test stays in the back
  pocket only if state leaks turn out to be hard to plug.

  If the suite grows past what's comfortable in one file, the next
  step is splitting across files behind a `dart_test.yaml`
  concurrency override scoped to `test/integration/`. Not stage 1.
- **Where does the suite live?** Inside `shorebird_code_push/test/integration/`,
  as regular `package:test` tests. Two reasons this beats a sibling
  workspace package:

  1. dev_dependencies are private to the package — adding `shelf`,
     `path`, etc. has no effect on consumers, so there is no real
     "production package contamination" cost.
  2. `Updater.bindings` is `@visibleForTesting`, and that annotation is
     package-scoped: the analyzer's `invalid_use_of_visible_for_testing_member`
     would fire if a sibling package reached in. We could blanket-`ignore`
     it, but that defeats the point of the annotation. Same-package tests
     use the seam cleanly.

  Toolchain prerequisites: handle at runtime, not via tags. `setUpAll`
  shells out to `cargo build -p library_test_hooks`; if cargo is
  missing or the build fails, store a skip reason and have `setUp`
  call `markTestSkipped(reason)`. `dart test` runs the suite by
  default everywhere — present-and-working machines exercise the
  integration tests, environments without a Rust toolchain see them
  reported as skipped rather than failed. CI installs Rust on all
  three OSes and expects no skips.
- **CI matrix.** Linux, macOS, Windows. All three are required —
  Windows is where we have already shipped FFI bugs (updater #344's
  parent issue mentioned EACCES paths) and the loading approach above
  is platform-neutral, so there is no reason to skip it.
- **Test-only contamination of `shorebird_code_push`.** Goal is zero.
  The plan above achieves zero by reusing the existing
  `@visibleForTesting` `Updater.bindings` setter that the package's
  own unit tests already use. If something in the integration suite
  forces a real change to `shorebird_code_push`, that's a flag to
  rethink rather than just patch through.
- **Patch fixture build cost.** Will adding `cargo run -p patch` to test
  setup make the suite annoyingly slow on cold checkouts? Worth measuring
  with one real fixture before committing.
- **Existing tracker?** Searched all Eric-authored issues across
  `shorebird` and `_shorebird` from the last week (and broader). No
  dedicated tracker. Closest neighbors: shorebird #3341 (on-device
  integration tests via shorebird ci, customer-facing — different
  thing) and #3737 (per-patch state machine refactor, which would be
  much safer to land with this suite in place). This doc is the
  tracking artifact.

## Phasing

Three deliverables, each independently shippable:

1. **Test-hooks crate + harness.** New `library_test_hooks` cdylib,
   `test-hooks` feature on `updater` to expose `testing_reset_config`
   to a sibling crate, ffigen of the test-hooks header, tempdir
   scaffolding, the `Updater.bindings = ...` override in `setUpAll`.
   One trivial test: init, read `current_boot_patch_number`, reset,
   init again. CI green on Linux, macOS, Windows.
2. **Fake server + golden path.** `FakePatchServer`, patch fixture
   pipeline, and a single check-then-update-then-launch scenario.
3. **Adversarial scenarios.** Walk down the table above. Each test
   should reference the bug it would have caught.

Stage 1 is the riskiest piece (rlib `#[no_mangle]` propagation across
the three OSes and the reset-between-tests story). Stage 3 is the part
that pays back the investment.

## Out of scope for this doc

- Test data hygiene if patch fixtures end up checked in.
- Coverage reporting (CI uses `cargo llvm-cov` for Rust today; Dart-side
  coverage from this suite is a bonus, not the goal).
- Performance benchmarking. This is a correctness suite.
