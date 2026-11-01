#!/usr/bin/env bash
#
# Runs the checks CI runs, over both halves of this repository, under the
# toolchains CI uses.
#
# Usage: tool/agent-verify.sh [rust|dart]
#
# Exit codes: 0 every check passed, 1 a check failed, 2 usage error, 3 a check
# could not run here, so it was skipped and this is not a pass. Every skip is
# named on the last lines, after the output of everything that did run.
#
# Two divergences from CI are deliberate. CI tests through `cargo llvm-cov`,
# an instrumented build, and over macOS, Windows and Linux, where this runs a
# plain `cargo test` on one host. Out of scope entirely, because they need a
# token, a browser or a container: the coverage uploads, the coverage floor,
# the example's web build, the spell check and the semantic-title check.
set -euo pipefail

# An inherited GIT_DIR or GIT_WORK_TREE (a hook, `git rebase --exec`) makes
# --show-toplevel name the working directory rather than the root.
if ! repo_root="$(env -u GIT_DIR -u GIT_WORK_TREE git rev-parse --show-toplevel)"; then
  echo "error: $(pwd) is not in a git checkout, so there is nothing to verify" >&2
  exit 2
fi
if [ ! -f "$repo_root/.fvmrc" ] || [ ! -d "$repo_root/shorebird_code_push" ]; then
  echo "error: $repo_root is not a checkout of shorebirdtech/updater, or is one from before agent-verify" >&2
  exit 2
fi

usage() { echo "usage: tool/agent-verify.sh [rust|dart]" >&2; }

halves=(rust dart)
case "$#" in
  0) ;;
  1)
    case "$1" in
      rust | dart) halves=("$1") ;;
      *)
        usage
        exit 2
        ;;
    esac
    ;;
  # One half or none. Taking the first of several and dropping the rest read
  # as a pass for both.
  *)
    echo "error: one half at a time, got $# arguments" >&2
    usage
    exit 2
    ;;
esac

# The checkout the caller stands in, not the one beside this script.
cd "$repo_root"

failed=()
skipped=()

# Both halves, not just the Rust one: the Dart half's integration suite shells
# out to `cargo build -p library_test_hooks` and reports a skip reason rather
# than a failure when cargo is missing, so three tests read as passing having
# run none of themselves. rustup installs into ~/.cargo/bin and puts it on PATH
# from a shell profile, which a script does not read.
command -v cargo >/dev/null 2>&1 || export PATH="$HOME/.cargo/bin:$PATH"

if [[ " ${halves[*]} " == *" rust "* ]]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo 'error: no cargo found, so the Rust half was not checked' >&2
    echo 'Install it: https://rustup.rs' >&2
    skipped+=('the whole Rust half, no cargo')
  else
    # The workspace members, which is the set .github/workflows/main.yaml
    # builds.
    for crate in library patch library_test_hooks; do
      echo "==> $crate"
      (
        cd "$crate"
        cargo fmt --check
        cargo clippy --all-targets -- -D warnings
        cargo test
      ) || failed+=("$crate")
    done

    # The host build above compiles nothing behind cfg(target_os = "android")
    # or cfg(target_os = "ios"), so CI lints the library for those targets
    # separately and a change to logging.rs or config.rs can go red there
    # having passed everything here. Only the targets this machine has are
    # run, and the rest are named: `rustup target add <target>` installs one,
    # and Android also wants cargo-ndk to point the C compiler at an NDK.
    installed="$(rustup target list --installed 2>/dev/null || true)"
    for target in aarch64-linux-android aarch64-apple-ios aarch64-apple-ios-sim; do
      if ! grep -qx "$target" <<< "$installed"; then
        skipped+=("clippy for $target, target not installed")
        continue
      fi
      if [[ $target == *android* ]] && ! command -v cargo-ndk >/dev/null 2>&1; then
        skipped+=("clippy for $target, no cargo-ndk")
        continue
      fi
      echo "==> library ($target)"
      (
        cd library
        if [[ $target == *android* ]]; then
          cargo ndk --target "$target" clippy -- -D warnings
        else
          cargo clippy --target "$target" -- -D warnings
        fi
      ) || failed+=("library ($target)")
    done

    # Advisories, banned crates and sources, on every PR rather than only on a
    # Rust change: a newly published advisory against a crate already in
    # Cargo.lock fails CI without this repository changing at all.
    if command -v cargo-deny >/dev/null 2>&1; then
      echo '==> cargo deny'
      cargo deny check advisories bans sources || failed+=('cargo deny')
    else
      skipped+=('cargo deny, not installed (cargo install cargo-deny)')
    fi
  fi
fi

if [[ " ${halves[*]} " == *" dart "* ]]; then
  # One Flutter, named rather than resolved off PATH. CI installs the version
  # in .fvmrc, `fvm use` puts that version at .fvm/flutter_sdk, and FLUTTER_ROOT
  # overrides both. A laptop's own Flutter is usually neither: this one is
  # seven months behind the pin, and `dart format` reformats nine files in
  # shorebird_code_push between the two, which is a whole-tree format dragging
  # files into a branch that never touched them.
  if [[ -z ${FLUTTER_ROOT:-} && -d .fvm/flutter_sdk ]]; then
    FLUTTER_ROOT="$PWD/.fvm/flutter_sdk"
  fi
  export FLUTTER_ROOT="${FLUTTER_ROOT:-$HOME/flutter}"
  if [[ ! -x $FLUTTER_ROOT/bin/flutter ]]; then
    echo "error: no flutter at $FLUTTER_ROOT/bin/flutter, so the Dart half was not checked" >&2
    echo "Point FLUTTER_ROOT at a Flutter SDK, or run \`fvm use\` to get the pinned one" >&2
    skipped+=('the whole Dart half, no flutter')
  else
    export PATH="$FLUTTER_ROOT/bin:$PATH"
    if ! command -v cargo >/dev/null 2>&1; then
      skipped+=('the integration suite, no cargo to build library_test_hooks')
    fi

    # An unreadable pin is a skip, and format does not run without one. jq
    # yields the string `null` for a file with no such key, and the sed
    # fallback yields nothing for a file that is not JSON at all. Formatting
    # under an SDK nothing compared is what this script exists to prevent.
    pin="$(jq -r '.flutter' .fvmrc 2>/dev/null ||
      sed -n 's/.*"flutter"[^"]*"\([^"]*\)".*/\1/p' .fvmrc)"
    [[ $pin == 'null' ]] && pin=''
    here="$(flutter --version --machine |
      sed -n 's/.*"frameworkVersion": *"\([^"]*\)".*/\1/p')"

    format=1
    if [[ -z $pin ]]; then
      echo 'warning: no flutter version readable from .fvmrc, so format is skipped' >&2
      format=0
      skipped+=('dart format, no pin readable from .fvmrc')
    elif [[ -z $here ]]; then
      echo "warning: $FLUTTER_ROOT did not report a version, so format is skipped" >&2
      format=0
      skipped+=('dart format, the local Flutter did not report a version')
    elif [[ $pin != "$here" ]]; then
      echo "warning: this tree runs Flutter $here, CI pins $pin (.fvmrc)." >&2
      echo "dart format's output moves between versions, so format is skipped" >&2
      echo "rather than run against the wrong one. \`fvm use\` installs the pin." >&2
      format=0
      skipped+=("dart format, Flutter $here is not the pinned $pin")
    fi

    # Both CI paths for this package, which disagree with each other:
    # .github/actions/flutter_package analyzes `lib test` with
    # --fatal-warnings, and _shorebird_ci_flutter.yaml runs `flutter analyze .`
    # over the whole package and checks the example as its own job. Running the
    # union means green here implies green in both.
    echo "==> shorebird_code_push (Flutter $here)"
    (
      cd shorebird_code_push
      flutter pub get
      # -o none because dart format otherwise writes in place, and the tree it
      # reformatted would then pass.
      if ((format)); then dart format -o none --set-exit-if-changed .; fi
      dart analyze --fatal-warnings lib test
      flutter analyze .
      flutter test
    ) || failed+=('shorebird_code_push')

    echo '==> shorebird_code_push/example'
    (
      cd shorebird_code_push/example
      flutter pub get
      if ((format)); then dart format -o none --set-exit-if-changed .; fi
      flutter analyze .
    ) || failed+=('shorebird_code_push/example')
  fi
fi

# After the output of every check, because the line saying what was not
# examined is the one worth reading. Both loops are guarded because an
# unquoted-empty expansion under `set -u` errors in the bash macOS ships.
if [[ ${#failed[@]} -gt 0 ]]; then
  for one in "${failed[@]}"; do echo "agent-verify: failed: $one" >&2; done
fi
if [[ ${#skipped[@]} -gt 0 ]]; then
  for one in "${skipped[@]}"; do echo "agent-verify: skipped: $one" >&2; done
fi
if [[ ${#failed[@]} -gt 0 ]]; then exit 1; fi
if [[ ${#skipped[@]} -gt 0 ]]; then exit 3; fi
echo "agent-verify: ${halves[*]} passed"
