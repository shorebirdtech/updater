# Updater library

This is the C/Rust side of the Shorebird code push system.  This is built
in Rust with a C API for easy calling from other languages, most notably
for linking into libflutter.so.

See cli/README.md for more documentation on the library.

## Parts
* cli: Test the updater library via the Rust API (for development).
* dart_cli: Test ffi wrapping of updater library.
* library: The rust library that does the actual update work.
* dart_bindings: The Dart bindings for the updater library.

All of the interesting code is in the `library` directory.  There is also
a README.md in that directory explaining the design.


## Developing

It's best to edit this repository from within an engine checkout.  See [BUILDING_ENGINE.md](BUILDING_ENGINE.md) for instructions on how to set up an engine checkout.

The workflow I use involves 2-3 VSC windows:

1. Opening the engine 'src'.

In that terminal I:
```
cd third_party/updater
```

2. To build the updater as part of the engine:
```
cargo ndk --target aarch64-linux-android build --release && ninja -C ../../out/android_release_arm64 && say "done"
```

The cargo part *should not* be needed, but I haven't yet done the work to integrate the Rust code into the gn files for the Flutter engine yet.

I add `say "done"` to the end as linking can take several minutes for release Android builds.

3.  In a second window, I open `code third_party/updater`.  I do this because otherwise the rust_analyzer can't seem to find the rust code.  We could fix this by adding the directory to the VSC workspace, but I'm not sure where we would put the workspace file in the first place.  `src` is actually shorebirdtech/buildroot and is controlled via gclient by shorebirdtech/engine/DEPS.

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
flutter run --release  --local-engine-src-path ~/Documents/GitHub/engine/src --local-engine android_release_arm64
```

You may also need to build `out/host_release` as `flutter` looks for some Dart .dill files there and if they're not there can fail to build.

## Note:

`--local-engine-src-path` doesn't work on ARM Macs at the moment due to: https://github.com/flutter/flutter/issues/124620

Alternatives being considered include
* pushing artifacts to a dev server: https://github.com/shorebirdtech/shorebird/pull/277
* Or we could build a local artifact proxy mode for `shorebird`, but would require teaching `flutter` how to use an alternative artifacts directory (or just making a new checkout of flutter) and telling it how to override the version in engine.version.  This is needed to make sure that gradel requests artifacts of a different version than existing so as to avoid poluting the gradle cache.
