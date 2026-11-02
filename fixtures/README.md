# Updater fixture corpus

A set of JSON files that pin down how the updater behaves, case by case:
what it does with each on-disk state it can find at launch, what it sends to
the server and how it handles each answer, how it downloads, resumes and
installs a patch, and how it recovers when a launch crashes or the process is
killed part way.

## Why it exists

- **Changing the updater safely today.** The updater runs inside every
  Shorebird app, and a mistake in it can strand users on a bad patch. The
  corpus is the safety net: if a change alters any behavior it records, a case
  fails, and the PR has to say so.
- **A port has an exact target.** Any rewrite of the updater (the
  planned C++ one, for example) must pass the same files. "Same behavior as
  the Rust updater" then means something checkable rather than a judgment
  call.

## Why JSON through the C API, not Rust unit tests

- **One spec for any implementation.** The cases are data, not Rust code, so
  a C++ (or any other) updater runs the same files through its own small
  runner. Nothing has to be translated, and nothing can drift between two
  copies of the tests.
- **It tests what the engine calls.** Every case drives the updater only
  through the public C API (`updater_engine.h`, `updater_dart.h`), the files
  it leaves on disk, and the HTTP requests it makes: the same surface the
  Flutter engine and `shorebird_code_push` use. Internal refactors don't touch
  the cases; changes to what callers see do.

## Running it

The Rust runner is `library_test_hooks/tests/fixture_corpus.rs`:

```sh
cd library_test_hooks
cargo test --test fixture_corpus                              # every case
FIXTURE_FILTER=boot/ cargo test --test fixture_corpus         # cases whose path contains boot/
```

It also runs as part of `cargo test` and in CI on macOS, Linux and Windows. A
failure names the case and step and shows expected against actual.

## Layout

```
fixtures/
  cases/<group>/<name>.json   scripted scenarios (format below)
  bipatch/<name>/             patch goldens: base.bin + patch.bin -> new.bin
  bases/                      extra base binaries referenced by cases
  keys/                       RSA key pair for signature cases (test only)
```

Groups: `state_load` (reading and migrating on-disk state at init),
`boot` (launch start/success/failure, crash recovery, boot-time validation,
two engines in one process),
`patch_check` (requests, responses, errors, rollbacks, event queue),
`update` (download, resume, inflate, install, failures, overlapping
`update` calls),
`signing` (`patch_public_key` and `patch_verification`, from check through
the next boot).

## Adding a case

1. Copy the nearest case in the same group to
   `cases/<group>/<what_it_pins_down>.json`.
2. Write `description` as the behavior in one or two sentences, then edit
   `files` (the state before the process starts), `steps` (the calls, with
   the HTTP exchanges each should make) and the expectations, using the format
   below.
3. Run it alone with `FIXTURE_FILTER=<group>/<name>`. Check it fails when the
   behavior it describes is broken (change the expectation, or the code,
   briefly), so it isn't passing vacuously.

Every bug fixed in the updater's state handling gets a case.

## Changing a case when behavior changes on purpose

A case that fails after a deliberate change is doing its job. In the same PR:
update the case's expectations (and `description`, if the behavior it
describes changed), and say in the PR description which cases changed and
why. Don't loosen a matcher to `<any>` to make a case pass: that removes the
check instead of updating it.

## How a runner executes a case

Each case runs in a fresh empty temp directory, the **case root**, with this
layout:

| Path under the case root | Passed to the updater as                        |
|--------------------------|-------------------------------------------------|
| `storage/`               | `AppParameters.app_storage_dir`                 |
| `cache/`                 | `AppParameters.code_cache_dir`                  |
| `app/libapp.so`          | `AppParameters.original_libapp_paths[0]`        |

`app/libapp.so` holds the case's base bytes (`base`, default the 19 ASCII
bytes `base release libapp`). The `FileCallbacks` passed to `shorebird_init`
serve the same bytes (Apple platforms read the base through them; other
platforms read the file).

A runner:

1. Creates the case root and writes `app/libapp.so`.
2. Starts a scripted HTTP server on `127.0.0.1` (below).
3. Replaces `{{server}}` (e.g. `http://127.0.0.1:54321`) and `{{public_key}}`
   (contents of `keys/fixture_public_key.b64`) in every string of the case.
4. Writes `files` under the case root.
5. Resets the updater to a just-started process (no config, no running patch).
   Each `init` first registers `EngineCallbacks`, as the engine does: an HTTP
   client and an RSA signature verifier.
6. Runs `steps` in order. For each step: write `write_files`, queue `http`,
   make the `call` and compare its result with `expect`, wait until every
   queued exchange has been served (up to 10 s; some requests come from
   background threads after the call returns), wait for the updater's
   background event sends to finish, then check `expect_files` and
   `expect_tree`. Steps marked `background` and `await` steps work as
   described under [Overlapping calls](#overlapping-calls).
7. After the last step, waits 200 ms and fails on any request nobody
   expected. The server keeps listening for the rest of the run: a request
   that reaches it after its case has finished (a background thread that
   started late) fails the run too, after a final 1 s wait.

The runner rejects a case that uses a key this document does not define, or
an `expect` on a call with no result, so a typo cannot quietly turn an
assertion off. A port's runner should be as strict.

## Case file

```json
{
  "description": "What this pins down, in one or two sentences.",
  "yaml": "app_id: fixture-app-id\nbase_url: {{server}}\n",
  "base": {"file": "bipatch/text_edit/base.bin"},
  "files": { "<path>": <file spec>, ... },
  "steps": [ <step>, ... ]
}
```

`yaml` is the default for `init` steps; the value shown is the default when it
is omitted. `init` uses release version `1.0.0+1` unless the step says
otherwise.

### Steps

```json
{
  "call": "update",
  "channel": "beta",
  "write_files": { "<path>": <file spec> },
  "http": [ <exchange>, ... ],
  "expect": <matcher>,
  "expect_files": { "<path>": <file expectation> },
  "expect_tree": ["app/libapp.so", "storage/state.json"],
  "background": true
}
```

Every key is optional. `call` is one of:

| call | C API | result compared with `expect` |
|---|---|---|
| `init` | `shorebird_init`; takes optional `release_version`, `yaml` | bool |
| `restart` | none: the process dies here and a new one starts (all in-memory state gone, disk kept) | none |
| `report_launch_start` | `shorebird_report_launch_start` | none |
| `report_launch_success` | `shorebird_report_launch_success` | none |
| `report_launch_failure` | `shorebird_report_launch_failure` | none |
| `validate_next_boot_patch` | `shorebird_validate_next_boot_patch` | none |
| `should_auto_update` | `shorebird_should_auto_update` | bool |
| `next_boot_patch_number` | `shorebird_next_boot_patch_number` | number (0 = base release) |
| `current_boot_patch_number` | `shorebird_current_boot_patch_number` | number |
| `next_boot_patch_path` | `shorebird_next_boot_patch_path` | `null`, or the path relative to the case root with `/` separators |
| `check_for_downloadable_update` | `shorebird_check_for_downloadable_update(channel or NULL)` | bool |
| `update` | `shorebird_update_with_result(channel or NULL)` | `{"status": <int>, "message": <string or null>}` |
| `await` | none: waits for the `background` call (below) | that call's result |

A step without `call` only writes or checks files.

`restart` is how crashes and kills are written: whatever the updater put on
disk before the `restart` is what the next process finds. A kill in the middle
of an operation that the C API cannot interrupt (between the download and the
install, say) is written as a case whose `files` hold the state that kill
leaves behind.

Two engines in one process (add-to-app) need nothing more: they are one
process making each engine's calls in turn (`init`, `validate_next_boot_patch`,
`next_boot_patch_path`, and the launch reports the engine does not skip).

### Overlapping calls

A case can hold one call open while it makes others, to pin down what the
updater does when calls overlap (an `update()` from Dart while the engine's
update thread is still running, say):

- `"background": true` on a step starts its call on another thread. The
  step's last HTTP exchange must have `"hold": true` in its response, and no
  other may: the server takes that request but holds its response. The step
  ends once the held request has arrived (the call is then mid-flight, at a
  known point) and fails if the call returns first. The held request must be
  one the call sends itself, not one from a background thread (an event
  flush): `await` fails if the call returned before the release. It cannot
  have `expect`. One background call at a time.
- Later steps run while it waits. Each must return within 10 s; a call that
  waits for the background one fails the case.
- `{"call": "await"}` (with only `http`, `write_files` and expectations)
  queues its own `http` (what the background call sends
  after the held request), releases the held response, waits for the
  background call to return, and compares its result with `expect`.

A background call never awaited fails the case.

### HTTP exchanges

The server answers requests strictly in the order they were queued. A request
that does not match the next exchange, or arrives with nothing queued, fails
the case (and is answered 500).

```json
{
  "request": {
    "method": "POST",
    "path": "/api/v1/patches/check",
    "headers": {"range": "bytes=20-"},
    "json": <matcher>
  },
  "response": {
    "status": 200,
    "json": { ... },
    "text": "...",
    "body": <file spec>,
    "headers": {"Content-Range": "bytes 20-62/63"},
    "close_after_bytes": 20,
    "drop_connection": false,
    "hold": true
  }
}
```

Request: `method` (default `GET`) and `path` must be equal. Each entry in
`headers` must match the request header of that name (case-insensitive);
`null` means the header must be absent. `json`, if present, is matched against
the body parsed as JSON.

Response: the body is `json` (sent with `Content-Type: application/json`),
`text`, or the bytes of a `body` file spec, else empty. `Content-Length` is
always the full body length (a case may not set it). Extra `headers` are
added.
The connection is always closed after the response. `close_after_bytes` sends
the headers for the full body but only the first N body bytes, then closes (an
interrupted download). `drop_connection` closes without sending anything.
`hold` answers only once an `await` step releases it (see
[Overlapping calls](#overlapping-calls)).

### File specs (what to write)

| spec | bytes |
|---|---|
| `{"json": <value>}` | the value as pretty-printed JSON |
| `{"text": "..."}` | the UTF-8 string |
| `{"file": "<path under fixtures/>", "offset": n, "length": n}` | that file, or a slice of it |
| `{"delete": true}` | removes the file or directory |

### File expectations (what to check)

All keys present must hold:

| key | meaning |
|---|---|
| `"absent": true` | the path (file or directory) does not exist |
| `"exists": true` | the path exists; nothing else is checked |
| `"json": <matcher>` | the file parses as JSON and matches |
| `"text": "..."` | the file is exactly these bytes |
| `"size": n` | the file is n bytes |
| `"same_as": <file spec>` | the file is exactly those bytes |

`expect_tree` lists every regular file under the case root, plus every empty
directory with a trailing `/`, sorted, with `/` separators, and must match
exactly. Cases use it only where leftover files are
the point (cleanup, wipes).

### Matchers

Expected JSON values match actual values exactly (objects must have the same
keys, so a missing key, an extra key and `null` are all different), except
strings of the form `<...>`:

| matcher | matches |
|---|---|
| `<any>` | anything |
| `<number>` | any integer |
| `<string>` | any string |
| `<uuid>` | a UUID string (8-4-4-4-12 hex) |
| `<platform>` | the host platform as the updater reports it: `macos`, `linux`, `windows` (`android`, `ios` on devices) |
| `<arch>` | the host architecture: `aarch64`, `x86_64`, `x86`, `arm` |
| `<prefix:TEXT>` | a string starting with TEXT |
| `<contains:TEXT>` | a string containing TEXT |

Error messages from `update` are matched with `<any>`: the status is the
contract, the wording is not. Event messages that the server parses
(`crash_recovery: ...`, `engine_report: ...`, `update_failure: ...`) are
matched on their stable prefix.

## Bipatch goldens

Each `bipatch/<name>/` holds `base.bin`, `patch.bin` (zstd-compressed bipatch,
as the `patch` tool in this repo writes them) and `new.bin`. Applying
`patch.bin` to `base.bin` must give exactly `new.bin`. The runner replays
each one as this case (with `NAME` and `HASH` = hex `sha256(new.bin)` filled
in):

```json
{
  "base": {"file": "bipatch/NAME/base.bin"},
  "files": {"storage/state.json": {"json": {
    "client_id": "00000000-0000-4000-8000-000000000001",
    "release_version": "1.0.0+1", "queued_events": []}}},
  "steps": [
    {"call": "init", "expect": true},
    {"call": "update",
     "http": [
       {"request": {"method": "POST", "path": "/api/v1/patches/check"},
        "response": {"status": 200, "json": {"patch_available": true,
          "patch": {"number": 1, "hash": "HASH", "download_url": "{{server}}/patch.bin"}}}},
       {"request": {"method": "GET", "path": "/patch.bin"},
        "response": {"status": 200, "body": {"file": "bipatch/NAME/patch.bin"}}},
       {"request": {"method": "POST", "path": "/api/v1/patches/events"},
        "response": {"status": 200, "text": ""}}],
     "expect": {"status": 1, "message": "Update installed"},
     "expect_files": {"storage/patches/1/dlc.vmcode": {"same_as": {"file": "bipatch/NAME/new.bin"}}}}
  ]
}
```

A port can also feed the goldens straight to its patch-apply code.

| golden | what it covers |
|---|---|
| `tiny_base` | one-byte base, all new content |
| `text_edit` | small in-place edits to a text file (also the payload of the `update` cases) |
| `identical` | new equals base |
| `to_empty` | new is empty |
| `binary_edits` | 64 KiB random base with an overwrite, an insertion, a deletion and a longer tail |
| `unrelated` | base and new share nothing |

`fixtures/.gitattributes` turns off line-ending conversion: several goldens are
text, and a CRLF checkout would change their hashes.

Regenerate one with `cargo run -p patch --bin patch --release -- base.bin new.bin patch.bin`.
The `patch` tool cannot diff against an empty base, so there is no
empty-base golden.

## Keys

`keys/fixture_private_key.pem` / `keys/fixture_public_key.b64` exist only to
sign fixtures; they protect nothing. The public key is the base64 DER
`RSAPublicKey` that `patch_public_key` takes. A signature is
`base64(RSA-PKCS1v1.5-SHA256(hex sha256 of the artifact))`:

```sh
printf '%s' "$HASH" | openssl dgst -sha256 -sign keys/fixture_private_key.pem | base64
```

Cases that need a signature from another key use one made the same way with
a throwaway key (`openssl genrsa 2048`) that was then discarded.

## Known oddities

Some of what the corpus pins down is current behavior rather than a choice
anyone would defend. A port must match it; changing it is its own PR that
edits the case:

- `state_load/state_json_missing_field_resets`: a `state.json` that fails to
  parse for any reason resets, including its `client_id`.
- `boot/second_launch_start_in_process_boots_new_patch`: a second
  `report_launch_start` in one process, after an install, records the new
  patch as booting though the first engine still runs the old one. The
  engine reports launches once per process so this never happens; the case
  pins why it must.

Not covered: Android's split-APK base lookup (the runner uses the desktop path).
