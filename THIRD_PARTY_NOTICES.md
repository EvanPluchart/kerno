# Third-party notices

Kerno is licensed under MIT OR Apache-2.0. It includes or builds on the work below. The licences of every
Rust dependency are checked with `cargo deny check` (see `deny.toml`).

## Bundled fonts

Both typefaces are embedded in the application binary, unmodified, under the SIL Open Font License 1.1.
The licence texts ship next to the font files.

| Typeface | Version | Source | Licence text |
| --- | --- | --- | --- |
| Inter (Regular, Italic, Medium, SemiBold) | 4.1 | https://github.com/rsms/inter/releases/tag/v4.1 | `crates/kerno/assets/fonts/inter/LICENSE.txt` |
| JetBrains Mono (Regular, Italic, Bold) | 2.304 | https://github.com/JetBrains/JetBrainsMono/releases/tag/v2.304 | `crates/kerno/assets/fonts/jetbrains-mono/OFL.txt` |

Copyright 2016 The Inter Project Authors. Copyright 2020 The JetBrains Mono Project Authors.

SHA-256 of the release archives the files were taken from:

- `Inter-4.1.zip`: `9883fdd4a49d4fb66bd8177ba6625ef9a64aa45899767dde3d36aa425756b11e`
- `JetBrainsMono-2.304.zip`: `6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf`

## Icons

The interface icons are the Lucide set (ISC licence, https://lucide.dev), bundled through the
`gpui-kit-assets` crate.

## UI framework

- GPUI, by Zed Industries (Apache-2.0), consumed as the `gpui-pre` snapshot crates.
- GPUI Kit / gpui-component, by Longbridge (Apache-2.0).

## Reference implementations

Kerno was written with two open-source database clients as references for behaviour. No code, icon,
screenshot, name, colour or wording of either is included.

- **DBeaver Community** (https://github.com/dbeaver/dbeaver), Copyright DBeaver Corp and others, licensed
  under the Apache License, Version 2.0 (http://www.apache.org/licenses/LICENSE-2.0). Kerno files that follow
  DBeaver's logic say so in a comment naming the DBeaver source file. At present:
  - `crates/kerno-postgres/src/ddl.rs`: the order of the parts of a reconstructed table script follows
    `SQLTableManager.java`.
- **Beekeeper Studio** (https://github.com/beekeeper-studio/beekeeper-studio, GPL-3.0) was read to understand
  behaviour and user experience only. None of its code was translated or copied, and its commercially
  licensed directories were never opened.
