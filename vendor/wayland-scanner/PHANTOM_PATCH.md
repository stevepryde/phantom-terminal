# Phantom security patch

This directory starts from the crates.io source for `wayland-scanner 0.31.10`.
Phantom vendors it temporarily because that release requires vulnerable
`quick-xml 0.39` (`RUSTSEC-2026-0194` and `RUSTSEC-2026-0195`).

The local patch ports the following changes from Smithay's `wayland-rs` master
at commit `672e7cb994883f6d77617308f82a16289f70b9d4`:

- update `quick-xml` to `0.41`;
- use `xml10_content()` for XML entity references; and
- preserve upstream handling of entity references and CDATA in descriptions.

The empty `[workspace]` table in `Cargo.toml` only makes the vendored crate
independently testable. Remove this entire patch and the root
`[patch.crates-io]` entry once a crates.io `wayland-scanner` release contains
the same compatibility changes.
