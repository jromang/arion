# WDSP source patches

These patches are applied at build time by [`build.rs`](../build.rs) on top of a
fresh copy of the upstream WDSP sources from
[`thetis-upstream/Project Files/Source/wdsp/`](../../../thetis-upstream/).

They are kept as plain unified diffs (format: `patch -p1`) so every
modification of upstream C code stays visible and reviewable.

## Convention

- `NNNN-short-name.patch` — numbered in application order.
- First line: one-line summary.
- Empty line, then a paragraph explaining *why* the change is needed.
- Then the diff.

The entire porting shim lives in [`../shim/`](../shim/) and does **not** go
here — shim headers intercept `<Windows.h>` / `<process.h>` / `<intrin.h>` /
`<avrt.h>` without touching upstream source at all.

## Current patches

| #    | Name                                     | Purpose                                         |
|------|------------------------------------------|-------------------------------------------------|
| 0001 | eq: fix eq_mults declaration mismatch    | Align 8-arg decl in `eq.h` with 9-arg def in `eq.c`. Upstream bug latent under MSVC, rejected by gcc/clang. |

## Adding a new patch

1. Let the upstream copy land in `$OUT_DIR/wdsp/` by running `cargo build -p
   wdsp-sys` once.
2. Edit the file in place under `$OUT_DIR/wdsp/`, rebuild until it works.
3. `diff -u <upstream-file> <OUT_DIR/wdsp/file>` to get the diff.
4. Save it here as `NNNN-description.patch` (next free number).
5. Add a row to the table above.
6. Clean the build: `cargo clean -p wdsp-sys`. On the next build the patch is
   applied on top of a fresh upstream copy and everything recompiles.
