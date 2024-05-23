# patch command line tool

This is the tool used by the `shorebird` command line to package up a patch
file for uploading to Shorebird's servers.

## Usage

    patch <old> <new> <patch>

## Context, design and future thoughts.

Originally `patch` was written on top of the `bidiff` tool, but really no longer
needs to be. For expediency we used the `comde` crate to have a generic api
across multiple compression algorithms. However, we've since decided to use the
`zstd` crate for compression and decompression and could remove our `comde` and
`bidiff` dependencies eventually.

Because `bidiff` does not check what it's patching, it's possible to apply a
patch to the wrong file. To avoid this we currently have a separate hash
which we save in Shorebird's database and then `library` validates that the
patched file matches what we expected after inflation. This is the wrong design
and we intend to move to a system whereby `patch` is responsible for including
its own hash in the patch file and validating during application.

In that world we might end up with two hashes. One who's purpose is to validate
the patch file itself and another to validate the resulting inflated file or
the original file (both are equivalent). Both of these hashes could be stored
inside the patch container (.vmcode) and validated by the `library` code.

We should also probably rename `patch` to `packager` or similar since it should
do more than just bidiff. Also some of the apply/inflate code in library might
want to move into this directory to be more of the same place.

We also probably eventually want to move to something like sigstore.dev rather
than rolling our own packaging/signing system.

## Generating test expectations

The `string_patch` target can be used to generate test expectations for testing
the updater. It takes two strings as arguments and prints the necessary
variables you will need in your test to stdout.

```
% cargo run --bin=string_patch "foo" "bar"
Base: foo
New: bar
Patch: [40, 181, 47, 253, 0, 128, 113, 0, 0, 223, 177, 0, 0, 0, 16, 0, 0, 0, 3, 98, 97, 114, 0]
Hash (new): fcde2b2edba56bf408601fb721fe9b5c338d10ee429ea04fae5511b68fbf8fb9
```

Which will translate into:

```rust
let base = "foo";
let new = "bar";
let patch: Vec<u8> = vec![40, 181, 47, 253, 0, 128, 113, 0, 0, 223, 177, 0, 0, 0, 16, 0, 0, 0, 3, 98, 97, 114, 0];
let hash = "fcde2b2edba56bf408601fb721fe9b5c338d10ee429ea04fae5511b68fbf8fb9";
```
