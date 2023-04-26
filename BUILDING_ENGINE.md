# Building the Shorebird Flutter Engine

Shorebird uses a modified version of the Flutter engine.  Normally
when you use Shorebird, you would use the pre-built engine binaries
that we provide.  However, if you want to build the engine yourself,
this document describes how to do that.

The primary modification Shorebird makes to the stock Flutter engine
is adding support for the updater library.  The updater library is
written in Rust and is used to update the code running in the Flutter
app.  The updater library is built as a static library and is linked
into the Flutter engine during build time.

## Building the Updater Library

### Installing Rust

The updater library is written in Rust.  You can install Rust using
rustup.  See https://rustup.rs/ for details.

## Building for Android

Rust Android tooling *mostly* works out of the box, but needs a little
of configuration to get it to work.

The best way I found was to install:
https://github.com/bbqsrc/cargo-ndk

```
cargo install cargo-ndk
rustup target add \
    aarch64-linux-android \
    armv7-linux-androideabi \
    x86_64-linux-android \
    i686-linux-android
```

Once you have cargo-ndk installed, you can build the updater library:

```
cargo ndk --target aarch64-linux-android --target armv7-linux-androideabi build --release
```

### Setting up to build the Flutter Engine:

https://github.com/flutter/flutter/wiki/Setting-up-the-Engine-development-environment
https://github.com/flutter/flutter/wiki/Compiling-the-engine

The .gclient file I recommend is:
```
solutions = [
  {
    "managed": False,
    "name": "src/flutter",
    "url": "git@github.com:shorebirdtech/engine.git",
    "custom_deps": {},
    "deps_file": "DEPS",
    "safesync_url": "",
  },
]
```
(We should probably just check that in somewhere.)

Once you have that set up and `gclient sync` has run, you will need
to switch your flutter checkout to the `codepush` branch:

```
cd src/flutter
git checkout codepush
```

And then `gclient sync` again.

The `updater` source should now be in `src/third_party/updater`.

## Building Flutter Engine

We have scripts to perform the builds for all Android targets in:
https://github.com/shorebirdtech/build_engine/blob/main/build_engine/build.sh

But you can also do so manually.  Here is building for Android arm64:

```
./flutter/tools/gn --android --android-cpu arm64 --runtime-mode=release
ninja -C out/android_release_arm64
```

The linking step for android_release_arm64 is _much_ longer than other platforms
we may need to use unopt or debug builds for faster iteration.

I also add `&& say "done"` to the end of the ninja command so I know when it's
done (because it takes minutes).  Often I'm editing/buiding from within the updater
directory so my command is:

```
cargo ndk --target aarch64-linux-android build --release && ninja -C ../../out/android_release_arm64 && say "done"
```

## Running with your local engine

`shorebird` commands support `--local-engine-src-path` and `--local-engine` just like `flutter` commands do.

When testing on my machine I use something like:

```shorebird --local-engine-src-path=$HOME/Documents/GitHub/engine --local-engine=android_release_arm64 run```
