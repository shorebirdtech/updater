- Issues for this repo live in `shorebirdtech/shorebird`, labels `updater` and `package_code_push`. PRs come here.
- Rust tests that call `shorebird_init` need `#[serial]`: the config is a global.
- `library/include/*.h` come from `cbindgen` in `library/build.rs`; `updater_bindings.g.dart` comes from `ffigen`. Don't hand-edit either.
- Fail open: any error leaves the app on its installed version.
- `fixtures/` freezes updater behavior as JSON cases replayed through the C API by `library_test_hooks/tests/fixture_corpus.rs`; see `fixtures/README.md`. An intended behavior change updates its case in the same PR.
- Keep PRs reviewable. A small PR can carry a little cleanup next to its change; once a PR gets large, split it along its seams: a refactor with no behavior change, general improvements (fixes, cleanups, tests of existing code, simplifications), and each separate behavior change. Improvements land first: we always want them, and may not want the feature. A change keeps its own tests and fixture updates in its PR; a feature that needs an improvement builds on that PR's branch.

## Shorebird is a guest in the app

The updater runs inside every customer's app, beside code and plugins we can't see. Shorebird never claims a resource that has one slot per process or app where the last writer wins, such as Android's process state summary (`setProcessStateSummary`): we'd overwrite the app's or a plugin's, or they ours, and neither would know. A design that needs one finds another way or drops the feature, and says so. Signal handlers and uncaught-exception or crash handlers can be shared, as crash reporters share them: keep the previous handler and call it, never replace it. Any we add must chain.

## FFI compatibility

The Dart package and the Rust library in the engine ship independently, in either skew, with no version negotiation; the pubspec's Flutter SDK floor bounds only the oldest engine. Every change to `library/src/c_api/dart.rs` states its fallback for both skews. `c_api/engine.rs` ships with the engine and is exempt.

- Dart probes a new C symbol (`DynamicLibrary.providesSymbol` or a guarded lookup) before calling it and falls back when absent; ffigen bindings throw on a missing symbol.
- Existing structs, `UpdateResult` included, have frozen layouts. New data goes in a new struct from a new symbol.
- Existing symbols never change behavior.
- Status codes are append-only, never renumbered. Add only error codes: old Dart treats any code outside its success allowlist as failure. `SHOREBIRD_UPDATE_IN_PROGRESS` (4) is the one existing exception.
- Failure categories stay internal until a structured-result symbol exposes them; see `FailureCategory` in `library/src/updater.rs` and `NetworkError` in `library/src/network.rs`.

## Landing a PR

Land with `--admin` once, at the head commit:

- a separate agent's adversarial review of the full diff is posted as a PR comment, its blockers fixed or answered;
- a maintainer has LGTM'd (`lgtm` comment, approval, or they opened the PR). Agents never post `lgtm` or approve. Rebases and small fixes keep an LGTM; a scope change needs a new one;
- `gh pr checks <N> --required` is all green, nothing pending, no review thread open. Check right before merging.

```sh
gh pr merge <N> --admin --squash --match-head-commit <full sha>
```
