# Release Checklist

Copperline is currently released from source. The crate is marked
`publish = false` because it depends on a patched vendored copy of `m68k`;
resolve that dependency story before attempting a crates.io release.

## Before Creating the Public Repository

1. Create the public repository from a clean tree with rewritten history.
2. Confirm the tracked tree has no copyrighted ROM, disk, hard-disk, or CD
   images:

   ```sh
   git status --short
   git ls-files | rg -n '\.(rom|ROM|adf|ADF|adz|ADZ|dms|DMS|hdf|HDF|scp|SCP|cue|CUE|bin|BIN|iso|ISO|u12|U12|u13|U13|png|jpg|jpeg|gif|pdf|zip|7z|lha|lzx)$'
   ```

   Expected tracked binary files are:

   - `assets/brand/*.png`
   - `docs/images/*.png`; review provenance before release when these change
   - `timing-test/boot.bin`, built from `timing-test/boot.asm`
   - `vendor/m68k/tests/fixtures/extra/**/bin/*.bin`, built from the
     adjacent assembly sources under sibling `src/` directories

3. Confirm local assets are still ignored:

   ```sh
   git check-ignore -v KICK13.ROM AmigaTestKit.adf cdtv_single.bin
   ```

## Checks

Run these before tagging a source release:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
```

Build the documentation:

```sh
cd docs
myst build --html --ci --strict --check-links
myst build --pdf --ci --strict
test -s _build/exports/copperline.pdf
```

`cargo package --no-verify --offline` can be used to inspect the source
archive layout after the dependencies are cached. Do not use full package
verification as a release gate until the vendored `m68k` path dependency has
a crates.io-compatible replacement.
