# Shorebird CodePush Updater

The rust library that does the actual update work.

## Design

The updater library is built in Rust for safety (and modernity). It's built
as a C-compatible library, so it can be used from any language.

The library is thread-safe, as it needs to be called both from the flutter_main
thread (during initialization) and then later from the Dart/UI thread
(from application Dart code) in Flutter.

The overarching principle with the Updater is "first, do no harm". The updater
should "fail open", terms of continuing to work with the currently installed
or active version of the application even when the network is unavailable.

The updater also needs to handle error cases conservatively, such as partial
downloads from a server, or malformed responses (e.g. a proxy interfering)
and not crash the application or leave the application in a broken state.

Every time the updater runs it needs to verify that the currently installed
patch is compatible with the currently installed base version. If it is not,
it should refuse to return paths to incompatible patches.

The updater also needs to regularly verify that the current state directory
is in a consistent state. If it is not, it should invalidate any installed
patches and return to a clean state.

Not all of the above is implemented yet, but such is the intent.

## Architecture

The updater is split into separate layers. The top layer is the C-compatible
API, which is used by all consumers of the updater. The C-compatible API
is a thin wrapper around the Rust API, which is the main implementation but
only used directly for testing (see the `cli` directory).

Thread safety is handled by a global configuration object that is locked
when accessed. It's possible I've missed cases where this is not sufficient,
and there could be thread safety issues in the library.

- c_api (module) - C-compatible API
  - c_file.rs - a read-seek interface usable by the engine (used to provide access
    to iOS patch files)
  - mod.rs - the implementation of the C API.
- src/lib.rs - Rust API (and crate root)
- src/updater.rs - Core updater logic
- cache (module) - On-disk state management
  - disk-io.rs - Manages reading and writing serializable state to disk
  - mod.rs - Cache management
  - patch_manager.rs - Patch file state management. Owned by UpdaterState.
  - updater_state.rs - Public API for this module. Provides functions that
    trigger updates to the internal Boot State Machine (detailed below).
- src/config.rs - In memory configuration and thread locking
- src/cache.rs - On-disk state management
- src/logging.rs - Logging configuration (for platforms that need it)
- src/network.rs - Logic dealing with network requests and updater server

## Rust

We use normal rust idioms (e.g. Result) inside the library and then bridge those
to C via an explicit stable C API (explicit enums, null pointers for optional
arguments, etc). This lets the Rust code feel natural and also gives us maximum
flexibility in the future for exposing more in the C API without having to
refactor the internals of the library.

https://docs.rust-embedded.org/book/interoperability/rust-with-c.html
are docs on how to use Rust from C (what we're doing).

https://github.com/RubberDuckEng/safe_wren has an example of building in Rust
and exposing it with a C api.

## Integration

The updater library is built as a static library, and is linked into the
libflutter.so as part of a custom build of Flutter. We also link libflutter.so
with the correct flags such that updater symbols are exposed to Dart.

## Building for Android

The best way I found was to install:
https://github.com/bbqsrc/cargo-ndk

```
cargo install cargo-ndk
rustup target add \
    aarch64-linux-android \
    armv7-linux-androideabi \
    x86_64-linux-android \
    i686-linux-android
cargo ndk -t armeabi-v7a -t arm64-v8a build --release
```

When building to include with libflutter.so, you need to build with the same
version of the ndk as Flutter is using:

You'll need to have a Flutter engine checkout already setup and synced.
As part of `gclient sync` the Flutter engine repo will pull down a copy of the
ndk into `src/third_party/android_tools/ndk`.

Then you can set the NDK_HOME environment variable to point to that directory.
e.g.:

```
NDK_HOME=$HOME/Documents/GitHub/engine/src/third_party/android_tools/ndk
```

Then you can build the updater library as above. If you don't want to change
your NDK_HOME, you can also set the environment variable for just the one call:

```
NDK_HOME=$HOME/Documents/GitHub/engine/src/third_party/android_tools/ndk cargo ndk -t armeabi-v7a -t arm64-v8a build --release
```

## Imagined Architecture (not all implemented)

### Assumptions (not all enforced yet)

- Updater library is never allowed to crash, except on bad parameters from C.
- Network and Disk are untrusted.
- Running code is trusted.
- Store-installed bundle is trusted (e.g. APK).
- Updates are signed by a trusted key.
- Updates must be applied in order.
- Updates are applied in a single transaction.

### State Machines

#### Boot State Machine

This state machine tracks the process of the engine starting up, with or without
a patch. These state transitions happen whether or not a new patch is available,
but in the context of the updater, we only care about the case where we are
booting from a patch.

The patch boot state is internal to the PatchManager and stored on disk in
`patches_state.json`. It contains three fields, all of which are Optional
PatchMetadata structs:

- last_boot_patch: The last patch that was successfully booted.
- last_attempted_patch: The last patch that we attempted to boot.
- next_boot_patch: The next patch that we will attempt to boot.

This state machine has the following states. It can only move forward through
them.

1. Ready - The engine is initialized but has not started booting yet.
2. Booting - The engine has started booting.
3. Success - The engine successfully booted.
4. Failure - The engine failed to boot.

State is advanced through calls to the following methods of the `ManagePatches`
trait, which PatchManager implements:

- `record_launch_start`: Moves from Ready to Booting
  - next_boot_patch is the patch that we will attempt to boot, i.e., the "current" patch.
  - last_attempted_patch is set to next_boot_patch.
- `record_launch_success`: Moves from Booting to Success
  - last_boot_patch is set to next_boot_patch.
  - Artifacts for patches older than last_boot_patch are deleted.
- `record_launch_failure`: Moves from Booting to Failure
  - next_boot_patch artifacts are deleted.
  - next_boot_patch is set to either:
    - last_boot_patch if it is still valid, or
    - None, if last_boot_patch is None or invalid.

These are effectively no-ops if we are not booting from a patch.

Assumptions (not currently enforced, but should as possible):

- This state machine will have advanced at least as far as the
  Booting state before the Patch Check State Machine (below) is started.
- Calls to mutate state will not come out-of-order. For example,
  `record_launch_failure` will not be called before `record_launch_start`. This
  is important because PatchManager state is implicit - it does not track which
  state it is in.

#### Patch Check/Update State Machine

This state machine tracks the process of checking for new patches. It is managed
by the code in `updater.rs` and does not have any on-disk state. It has the
following states:

1. Ready - Ready to check for updates.
2. Send queued events (e.g., report that a patch succeeded or failed to boot)
   a. Move to checking once events, if any, have been reported.
3. Checking for new patches - A PatchCheckRequest is issued but not completed.
   a. If no patch is available, move back to Ready.
   b. If a new patch is is available, move to Downloading Patch.
4. Downloading Patch - A patch is available, we're downloading it.
   a. If the download fails, move back to Ready.
   b. If the download succeeds, move to Inflating Patch.
5. Inflating Patch - A patch has been downloaded, we're inflating it.
   a. Attempt to inflate the patch (apply a bidiff to the current release).
   b. If the patch is valid, queue a PatchInstallSuccess event and move to Ready.
   c. If the patch is invalid, queue a PatchInstallFailure event and move to Ready.

Changes in this state can be triggered by:

1. The engine via the C API.
2. The user via the C API (using the `shorebird_code_push` package).
3. Network activity.

- Server is authoritative, regarding current update/patch state. Client can
  cache state in memory. Not written to disk.
- Patches are downloaded to a temporary location on disk.
- Client keeps on disk (in state.json):
  - Current release version. Set when the app launches. If the app is updated to
    a new release version, all state is invalidated.
  - Queue of PatchEvents. This is cleared once events are sent to our servers.

### Trust model

- Network and Disk are untrusted.
- Running software (including apk service) is trusted.
- Patch contents are signed, public key is included in the APK.

### Patch Verification Modes

The updater supports two patch verification modes, configured via
`patch_verification` in `shorebird.yaml`. Both modes require a
`patch_public_key` to be configured for signature verification to occur.

#### Strict Mode (default)

```yaml
patch_verification: strict
```

In Strict mode, patch signature verification happens at **boot time**. This
provides the strongest security guarantee because it detects any potential
on-disk tampering to the patch file _after_ installation (e.g., if an attacker
were to modify the patch file on disk between app launches). However the
practical risk to such an attack is very low, since patches are stored within
the app's protected storage. An attacker in this case would need to have already
compromised the app itself, or the system (e.g. via a rooted device). However if
an attacker has compromised the system (rooted) they could already modify the
APK/IPA internals directly. The on-boot protection here is for cases where
developers are concerned that their app might be compromised and they wish to
ensure that such a compromise could not theoretically persist itself via editing
an installed patch file. Such a case is impractical, but we default to the
strongest-possible security stance regardless.

Strict mode is currently default for Shorebird, however some of our large
customers requested that we add an install_only mode, since their applications
were so large (many hundreds of mb) that the hash-verification during boot
was showing up on profiles from older devices.

**Install flow:**

1. Download patch from server
2. Inflate patch (apply bidiff to base release)
3. `check_hash()`: Compute SHA256 of inflated file, verify it matches server-provided hash
4. Store patch file, hash, and signature to disk

**Boot flow:**

1. Verify patch file exists and size matches stored metadata
2. `hash_file()`: Re-compute SHA256 of patch file on disk
3. `check_signature()`: Verify the computed hash has a valid signature using the public key
4. If verification fails, fall back to last known good patch or base release

#### Install Only Mode

```yaml
patch_verification: install_only
```

In Install Only mode, patch signature verification happens at **install time**
only. This provides faster boot times but does not protect against post-install
tampering (extremely uncommon). The only case that this does not protect
against is if _your app itself_ were to accidentally (or through some other
malicious exploit of your app) modify its own data directory and modify the
patch files within such.

**Install flow:**

1. Download patch from server
2. Inflate patch (apply bidiff to base release)
3. `check_hash()`: Compute SHA256 of inflated file, verify it matches server-provided hash
4. `check_signature()`: Verify the server-provided hash has a valid signature using the public key
5. Store patch file, hash, and signature to disk

**Boot flow:**

1. Verify patch file exists and size matches stored metadata
2. (No signature verification - trusted from install time)

#### Without a Public Key

If no `patch_public_key` is configured, signature verification is skipped in
both modes. The `check_hash()` step still runs during install to detect
download corruption, but there is no cryptographic verification that the
patch came from a trusted source.

## TODO:

- Add an async API.
- Write tests for state management.
- Make state management/filesystem management atomic (and tested).

## Later-stage update system design docs

- https://theupdateframework.io/
- https://fuchsia.dev/fuchsia-src/concepts/packages/software_update_system
