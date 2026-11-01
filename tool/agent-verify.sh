#!/usr/bin/env bash
#
# Runs the checks CI runs, over both halves of this repository, under the
# toolchains CI uses.
#
# Usage: tool/agent-verify.sh [rust|dart]
#
# Out of scope, because they need a token, a container or a browser: the
# coverage upload, the coverage floor, the example's web build, the spell check
# and the semantic-title check.
#
# Exit codes: 0 every check passed, 1 a check failed, 2 usage error, 3 a check
# could not run, so it was skipped and this is not a pass.
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

halves=(rust dart)
case "${1-}" in
  '') ;;
  rust | dart) halves=("$1") ;;
  *)
    echo "usage: tool/agent-verify.sh [rust|dart]" >&2
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
    # builds. `cargo fmt`'s output moves between toolchains the same way `dart
    # format`'s does, and nothing here pins one; see the PR that added this
    # script.
    for crate in library patch library_test_hooks; do
      echo "==> $crate"
      (
        cd "$crate"
        cargo fmt --check
        cargo clippy --all-targets -- -D warnings
        cargo test
      ) || failed+=("$crate")
    done
  fi
fi

if [[ " ${halves[*]} " == *" dart "* ]]; then
  # One Flutter, named rather than resolved off PATH. CI installs the version
  # in .fvmrc, `fvm use` puts that version at .fvm/flutter_sdk, and FLUTTER_ROOT
  # overrides both. A laptop's own Flutter is usually neither: this one is
  # seven months behind stable, and `dart format` reformats nine files in
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
    pin="$(jq -r '.flutter' .fvmrc 2>/dev/null ||
      sed -n 's/.*"flutter"[^"]*"\([^"]*\)".*/\1/p' .fvmrc)"
    here="$(flutter --version --machine |
      sed -n 's/.*"frameworkVersion": *"\([^"]*\)".*/\1/p')"

    format=1
    if [[ -n $pin && -n $here && $pin != "$here" ]]; then
      echo "warning: this tree runs Flutter $here, CI pins $pin (.fvmrc)." >&2
      echo "dart format's output moves between versions, so format is skipped" >&2
      echo "rather than run against the wrong one. \`fvm use\` installs the pin." >&2
      format=0
      skipped+=("dart format, Flutter $here is not the pinned $pin")
    fi

    echo "==> shorebird_code_push (Flutter $here)"
    (
      cd shorebird_code_push
      flutter pub get
      # -o none because dart format otherwise writes in place, and the tree it
      # reformatted would then pass.
      if ((format)); then dart format -o none --set-exit-if-changed .; fi
      dart analyze --fatal-warnings lib test
      flutter test
    ) || failed+=('shorebird_code_push')
  fi
fi

# Named one at a time, and after the output of every check, because the line
# that says what was not examined is the one worth reading. Both loops are
# guarded: an empty array under `set -u` is an error in bash 3.2, which is the
# bash macOS ships.
if [[ ${#failed[@]} -gt 0 ]]; then
  for one in "${failed[@]}"; do echo "agent-verify: failed: $one" >&2; done
fi
if [[ ${#skipped[@]} -gt 0 ]]; then
  for one in "${skipped[@]}"; do echo "agent-verify: skipped: $one" >&2; done
fi
if [[ ${#failed[@]} -gt 0 ]]; then exit 1; fi
if [[ ${#skipped[@]} -gt 0 ]]; then exit 3; fi
echo "agent-verify: ${halves[*]} passed"
