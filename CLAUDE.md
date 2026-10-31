- Issues for this repo live in `shorebirdtech/shorebird`, labels `updater` and `package_code_push`. PRs come here.
- Rust tests that call `shorebird_init` need `#[serial]`: the config is a global.
- `include/*.h` come from `cbindgen` in `library/build.rs`; `updater_bindings.g.dart` comes from `ffigen`. Don't hand-edit either.
- Fail open: any error leaves the app on its installed version.

## FFI compatibility

The Dart package and the Rust library in the engine ship independently, in either skew, with no version negotiation. Every change to `library/src/c_api/dart.rs` states its fallback for both skews. `c_api/engine.rs` ships with the engine and is exempt.

- Dart probes a new C symbol with `DynamicLibrary.providesSymbol` before calling it and falls back when absent; ffigen bindings throw on a missing symbol.
- Existing structs, `UpdateResult` included, have frozen layouts. New data goes in a new struct from a new symbol.
- Existing symbols never change behavior.
- Status codes are append-only. Add only error codes: old Dart treats any code outside its success allowlist as failure.
- Failure categories stay internal until a structured-result symbol exposes them; see `FailureCategory` in `library/src/updater.rs` and `NetworkError` in `library/src/network.rs`.

## Landing a PR

Land with `--admin` once, at the head commit:

- a separate agent's adversarial review of the full diff is posted as a PR comment, its blockers fixed or answered;
- a maintainer has LGTM'd (`lgtm` comment, approval, or they opened the PR). Agents never post `lgtm` or approve. Rebases and small fixes keep an LGTM; a scope change needs a new one;
- `gh pr checks <N> --required` is all green, nothing pending, no review thread open. Check right before merging.

```sh
gh pr merge <N> --admin --squash --match-head-commit <full sha>
```
