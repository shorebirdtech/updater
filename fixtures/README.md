# Updater fixture corpus

Language-neutral description of how the updater behaves, as plain files. The
Rust updater passes all of it (`library_test_hooks/tests/fixture_corpus.rs`,
run by `cargo test` in `library_test_hooks/`); any other implementation of the
updater's C API is expected to pass the same files through its own runner.

Everything is observed through the public C API (`updater_engine.h`,
`updater_dart.h`), the files the updater leaves on disk, and the HTTP requests
it makes. Nothing here depends on how the updater is built inside.

```
fixtures/
  cases/<group>/<name>.json   scripted scenarios (format below)
  bipatch/<name>/             patch goldens: base.bin + patch.bin -> new.bin
  bases/                      extra base binaries referenced by cases
  keys/                       RSA key pair for signature cases (test only)
```

Groups: `state_load` (reading and migrating on-disk state at init),
`boot` (launch start/success/failure, crash recovery, boot-time validation),
`patch_check` (requests, responses, errors, rollbacks, event queue),
`update` (download, resume, inflate, install, failures).

Run one subset: `FIXTURE_FILTER=boot/ cargo test --test fixture_corpus`.

## Running a case

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
6. Runs `steps` in order. For each step: write `write_files`, queue `http`,
   make the `call` and compare its result with `expect`, wait until every
   queued exchange has been served (up to 10 s; some requests come from
   background threads after the call returns), then check `expect_files` and
   `expect_tree`.
7. After the last step, waits briefly and fails on any request nobody
   expected.

## Case file

```json
{
  "description": "What this pins down, in one or two sentences.",
  "release_version": "1.0.0+1",
  "yaml": "app_id: fixture-app-id\nbase_url: {{server}}\n",
  "base": {"file": "bipatch/text_edit/base.bin"},
  "files": { "<path>": <file spec>, ... },
  "steps": [ <step>, ... ]
}
```

`release_version` and `yaml` are the defaults for `init` steps; the values
shown are the defaults when they are omitted.

### Steps

```json
{
  "call": "update",
  "channel": "beta",
  "write_files": { "<path>": <file spec> },
  "http": [ <exchange>, ... ],
  "expect": <matcher>,
  "expect_files": { "<path>": <file expectation> },
  "expect_tree": ["app/libapp.so", "storage/state.json"]
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

A step without `call` only writes or checks files.

`restart` is how crashes and kills are written: whatever the updater put on
disk before the `restart` is what the next process finds. A kill in the middle
of an operation that the C API cannot interrupt (between the download and the
install, say) is written as a case whose `files` hold the state that kill
leaves behind.

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
    "omit_content_length": false,
    "drop_connection": false
  }
}
```

Request: `method` (default `GET`) and `path` must be equal. Each entry in
`headers` must match the request header of that name (case-insensitive);
`null` means the header must be absent. `json`, if present, is matched against
the body parsed as JSON.

Response: the body is `json` (sent with `Content-Type: application/json`),
`text`, or the bytes of a `body` file spec, else empty. `Content-Length` is
the full body length unless `omit_content_length`. Extra `headers` are added.
The connection is always closed after the response. `close_after_bytes` sends
the headers for the full body but only the first N body bytes, then closes (an
interrupted download). `drop_connection` closes without sending anything.

### File specs (what to write)

| spec | bytes |
|---|---|
| `{"json": <value>}` | the value as pretty-printed JSON |
| `{"text": "..."}` | the UTF-8 string |
| `{"file": "<path under fixtures/>", "offset": n, "length": n}` | that file, or a slice of it |
| `{"dir": true}` | an empty directory |
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

`expect_tree` lists every regular file under the case root, sorted, with `/`
separators, and must match exactly. Cases use it only where leftover files are
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
`patch.bin` to `base.bin` must give exactly `new.bin`. The Rust runner replays
each one through the C API: base release `base.bin`, the server offers
`patch.bin` with hash `sha256(new.bin)`, `update` must install it and
`storage/patches/1/dlc.vmcode` must equal `new.bin`. A port can also feed them
straight to its patch-apply code.

| golden | what it covers |
|---|---|
| `tiny_base` | one-byte base, all new content |
| `text_edit` | small in-place edits to a text file (also the payload of the `update` cases) |
| `identical` | new equals base |
| `to_empty` | new is empty |
| `binary_edits` | 64 KiB random base with an overwrite, an insertion, a deletion and a longer tail |
| `unrelated` | base and new share nothing |

Regenerate one with `cargo run -p patch --release -- base.bin new.bin patch.bin`.
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

## Changing the corpus

These files are the contract the C++ port is held to, so a change here is a
behavior change: say so in the PR. Add a case for every bug fixed in the
updater's state handling. Not yet covered: `patch_verification: install_only`
does not check the signature at install time today (fix in progress), so
there is no install-time signature case yet; Android's split-APK base lookup
(the runner uses the desktop path); concurrent `update` calls
(`UpdateInProgress`).
