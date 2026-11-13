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

Kerno's wordmark (`assets/brand/kerno-wordmark.svg` and the horizontal lockups) is Inter SemiBold converted
to outlines, under the same licence.

SHA-256 of the release archives the files were taken from:

- `Inter-4.1.zip`: `9883fdd4a49d4fb66bd8177ba6625ef9a64aa45899767dde3d36aa425756b11e`
- `JetBrainsMono-2.304.zip`: `6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf`

## Icons

The interface icons are the Lucide set (ISC licence, https://lucide.dev), bundled through the
`gpui-kit-assets` crate.

## Database engine logos

Kerno shows the logo of a database engine next to its name (engine picker, saved connections) to identify
the engine a connection talks to. This is nominative use: the logos are not part of Kerno's own branding,
and their presence does not mean that their owners endorse, sponsor or are affiliated with Kerno. The files
in `crates/kerno/assets/engines/` are byte-for-byte what the sources below serve; Kerno scales them and does
not alter them.

| File | Mark | Taken from | SHA-256 |
| --- | --- | --- | --- |
| `postgresql.svg` | PostgreSQL elephant ("Slonik"), official three-colour artwork | https://wiki.postgresql.org/images/a/a4/PostgreSQL_logo.3colors.svg (listed on https://wiki.postgresql.org/wiki/Logo) | `51f93e19516081fc7d6fe6ab9bbab07abe5f7819e28016eefacea6dea691bc54` |
| `mysql.svg` | MySQL logo (dolphin and wordmark), official colours | https://www.mysql.com/common/logos/mysql-logo.svg (the artwork of https://www.mysql.com/about/legal/logos.html) | `974bf440a8a42e7783d3896cf9b326f3d73bc7305a27febf45cef0e91b53e595` |
| `mariadb.svg` | MariaDB sea lion, single colour | Simple Icons 15.22.0, `icons/mariadb.svg` (https://simpleicons.org, artwork files released under CC0-1.0) | `477435746bb2024048c7b71529f615fb70d7e679618f37bac49f78fd07e67dfe` |

The PostgreSQL and MySQL logos keep their official colours. The MariaDB mark is single-colour and is drawn in
one flat colour, dark on light surfaces and light on dark ones, as the vendor's own black and reversed
variants are.

Trademark notices:

- Postgres, PostgreSQL and the Slonik Logo are trademarks or registered trademarks of the PostgreSQL
  Community Association of Canada, and used with their permission. Policy:
  https://www.postgresql.org/about/policies/trademarks/
- MySQL is a registered trademark of Oracle and/or its affiliates. Logo policy:
  https://www.mysql.com/about/legal/logos.html
- MariaDB is a registered trademark of MariaDB plc or its affiliates. Logo page: https://mariadb.com/about-us/logos/

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
