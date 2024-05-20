# Updater Tools

[![License: MIT][license_badge]][license_link]

Tools to create artifacts used by the Updater.

## Usage

### package_patch

The `package_patch` command accepts two archives (a base/release and a patch)
produced by `flutter build` (only aabs are currently supported) and creates
patch artifacts for every architecture contained in both archives.

This command accepts the following arguments:

- `archive-type`: The type of the archives to process. Currently, only `aab`
  is supported.
- `release`: The path to the base/release archive.
- `patch`: The path to the patch archive.
- `patch-executable`: The path to the `patch` executable.
- `output`: The path to the directory where the patch artifacts will be created.

Sample usage:

```
dart run updater_tools package_patch \
  --archive-type=aab \
  --release=release.aab \
  --patch=patch.aab \
  --patch-executable=path/to/patch \
  --output=patch_output
```

If `release.aab` contains the default architectures produced by `flutter build`
(`arm64-v8a`, `armeabi-v7a`, and `x86_64`), this will produce the following in
the `patch_output` directory:

```
patch_output/
  ├── arm64-v8a.zip
  │   ├── dlc.vmcode
  │   └── hash
  ├── armeabi-v7a.zip
  │   ├── dlc.vmcode
  │   └── hash
  └── x86_64.zip
      ├── dlc.vmcode
      └── hash
```

In each .zip:

- dlc.vmcode: the bidiff file produced by the `patch` executable
- hash: the sha256 digest of the fully constituted (aka pre-diff) patch file
  (libapp.so on Android).
