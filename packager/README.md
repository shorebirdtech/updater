# packager command line tool

This is the tool used by the `shorebird` command line to compute the package
file for uploading to the server.

This currently uses the rust `bidiff` crate to compute the package file.
and could just use the `bic` command line tool included in that crate. However
we're explicitly writing our own command line to allow us to change the
underlying compression without affecting the `shorebird` command line callers.

## Usage

    packager <old> <new> <package>


## Generating test expectations

The string_packager target can be used to generate test expectations for testing
the updater.  It takes two strings as arguments and prints the necessary
variables you will need in your test to stdout.

```
% cargo run --bin=string_packager "foo" "bar"
Base: foo
New: bar
Patch: [40, 181, 47, 253, 0, 128, 113, 0, 0, 223, 177, 0, 0, 0, 16, 0, 0, 0, 3, 98, 97, 114, 0]
Hash (new): fcde2b2edba56bf408601fb721fe9b5c338d10ee429ea04fae5511b68fbf8fb9
```

Which will translate into:
```rust
let base = "foo";
let new = "bar";
let package: Vec<u8> = vec![40, 181, 47, 253, 0, 128, 113, 0, 0, 223, 177, 0, 0, 0, 16, 0, 0, 0, 3, 98, 97, 114, 0];
let hash = "fcde2b2edba56bf408601fb721fe9b5c338d10ee429ea04fae5511b68fbf8fb9";
```
