# Updater library

[![codecov](https://codecov.io/gh/shorebirdtech/updater/branch/main/graph/badge.svg)](https://codecov.io/gh/shorebirdtech/updater)

This is the Rust side of the Shorebird code push system.  This is built
in Rust with a C API for easy calling from other languages, most notably
for linking into `libflutter.so`.

## Parts
* `dart_cli`: Test ffi wrapping of updater library.
* `library`: The rust library that does the actual update work.
* `dart_bindings`: The Dart bindings for the updater library.

All of the interesting code is in the `library` directory.  There is also
a [README.md](library/README.md) in that directory explaining the design.


## Developing

It's best to edit this repository from within an engine checkout.  See
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

The cargo part *should not* be needed, but I haven't yet done the work to
integrate the Rust code into the gn files for the Flutter engine yet.

I add `say "done"` to the end as linking can take several minutes for release
Android builds.

3.  In a second window, I open `code third_party/updater`.  I do this because
    otherwise the `rust_analyzer` can't seem to find the rust code.  We could
    fix this by adding the directory to the VSC workspace, but I'm not sure
    where we would put the workspace file in the first place.  `src` is actually
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
some  Dart  `.dill` files in `host_release`.

## Coverage

We'd like to get to 100% coverage but aren't there yet.

https://github.com/taiki-e/cargo-llvm-cov
is the best tool I've found for generating coverage reports.

Install:
https://github.com/taiki-e/cargo-llvm-cov#installation

`cargo llvm-cov` will then generate the report.