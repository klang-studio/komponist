# nice-plug source pin

This directory mirrors the ISC-licensed nice-plug wrapper crate from klang-studio/nice-plug
commit `39e9ad41cf84f98762d86b83abdb9c422fcb58a5`. The original license and notices apply.
The unmodified nice-log, nice-plug-core and nice-plug-derive crates resolve from the same
Git revision. Their resolved revisions also appear in the SDK workspace lockfile.

The local `custom-extension.patch` is the reviewed prototype from #180. It preserves standard
extension dispatch and exposes the wrapper's live author-to-CLAP parameter ID mapping.
`source-pin.patch` narrows the vendored workspace to the wrapper and rewires its sibling package
paths to the exact Git pin. `enum-flag.patch` advertises `CLAP_PARAM_IS_ENUM` for the wrapper's
typed `EnumParam` in addition to its existing `STEPPED` flag.
`review-186-wave1.patch` hardens the CLAP factory, parameter metadata and state framing.

To reproduce the source in a scratch directory from any clone of the fork, run the following
commands with `upstream`, `vendor` and `scratch` set to the clone, this directory and an empty
directory respectively.

```sh
git -C "$upstream" archive 39e9ad41cf84f98762d86b83abdb9c422fcb58a5 \
  README.md LICENSE Cargo.toml crates/nice-plug | tar -x -C "$scratch"
patch -d "$scratch" -p1 < "$vendor/custom-extension.patch"
patch -d "$scratch" -p1 < "$vendor/source-pin.patch"
patch -d "$scratch" -p1 < "$vendor/enum-flag.patch"
patch -d "$scratch" -p1 < "$vendor/review-186-wave1.patch"
diff -ru "$scratch/crates" "$vendor/crates"
diff -u "$scratch/Cargo.toml" "$vendor/Cargo.toml"
```
