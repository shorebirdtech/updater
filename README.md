# Updater library

[![codecov](https://codecov.io/gh/shorebirdtech/updater/branch/main/graph/badge.svg)](https://codecov.io/gh/shorebirdtech/updater)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE-MIT)
[![License: Apache](https://img.shields.io/badge/license-Apache-orange.svg)](./LICENSE-APACHE)

This is the Rust side of the Shorebird code push system. This is built
in Rust with a C API for easy calling from other languages, most notably
for linking into `libflutter.so`.

The primary modification Shorebird makes to the stock Flutter engine
is adding support for the updater library (this repo). The updater library is
written in Rust and is used to update the code running in the Flutter
app. The updater library is built as a static library and is linked
into the Flutter engine during build time.

## Parts

- `library`: Runtime library linked into Flutter Engine to unpack and apply
  updates. Build into libupdater.a and linked into libflutter.so.
- `patch`: Developer tooling to package Shorebird updates ("patches"). Built
  into `patch.exe` and downloaded and run by `shorebird` command line tool:
  https://github.com/shorebirdtech/shorebird/tree/main/packages/shorebird_cli.
- `shorebird_code_push`: The Dart bindings for communicating with the updater
  library from within a Flutter app. Published to pub.dev, usage is optional by
  developers.

Most interesting code is in the `library` directory. There is also
a [README.md](library/README.md) in that directory explaining the design.

## Developing

It's best to edit this repository from within an engine checkout. See
[BUILDING_ENGINE.md](BUILDING_ENGINE.md) for instructions on how to set up an
engine checkout.

The workflow I use involves 2 to 3 VSC windows:

1. Opening the engine `src`.

In that terminal I:

```
cd third_party/updater
```

2. To build the updater as part of the engine:

```
cargo ndk --target aarch64-linux-android build --release && \
    ninja -C ../../out/android_release_arm64 && say "done"
```

The cargo part _should not_ be needed, but I haven't yet done the work to
integrate the Rust code into the gn files for the Flutter engine yet.

I add `say "done"` to the end as linking can take several minutes for release
Android builds.

3.  In a second window, I open `code third_party/updater`. I do this because
    otherwise the `rust_analyzer` can't seem to find the rust code. We could
    fix this by adding the directory to the VSC workspace, but I'm not sure
    where we would put the workspace file in the first place. `src` is actually
    `shorebirdtech/buildroot` and is controlled via `gclient` by
    `shorebirdtech/engine/DEPS`.

4.  In a third window I open my test app. e.g.:

```
flutter create test_app
cd test_app
shorebird init
shorebird release
code .
```

5. To run the test app with my local engine I use:

```
shorebird run --local-engine-src-path $HOME/Documents/GitHub/engine/src \
    --local-engine android_release_arm64
```

You may also need to build `out/host_release` once as `flutter build` looks for
some Dart `.dill` files in `host_release`.

## Coverage

We'd like to get to 100% coverage but aren't there yet.

https://github.com/taiki-e/cargo-llvm-cov
is the best tool I've found for generating coverage reports.

Install:
https://github.com/taiki-e/cargo-llvm-cov#installation

`cargo llvm-cov` will then generate the report.
