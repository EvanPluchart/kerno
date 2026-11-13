# Kerno brand

**Idea.** "Stack K": the stem of the K is a stack of three data disks, the arms are a chevron. Data plus
speed, without the database cylinder every other client uses. Kerno means "core" in Esperanto.

## Files

| File | Use |
|---|---|
| `kerno-symbol.svg` | Master mark, ink on transparent |
| `kerno-symbol-black.svg`, `-white.svg` | One-colour versions |
| `kerno-symbol-mono-c8f03c.svg` | Lime mark on dark surfaces |
| `kerno-symbol-mono-4d6b00.svg` | Olive mark on light surfaces |
| `kerno-horizontal.svg`, `-reversed.svg` | Mark + wordmark (README header, about window, website) |
| `kerno-wordmark.svg` | Wordmark alone |
| `kerno-app-icon.svg` | Lime tile, full bleed (Linux, web) |
| `kerno-app-icon-dark.svg` | Night tile with lime mark (alternate, not the app icon) |
| `kerno-macos-1024.svg`, `kerno-icon-1024.png` | macOS icon on Apple's grid (824 px tile, 100 px inset) |
| `../icon/kerno.icns`, `../icon/kerno-<size>.png` | Built icons used by `scripts/bundle-*.sh` |

The wordmark is Inter SemiBold (OFL) converted to outlines, tracking -2 %.

## Colours

| Token | Hex | Role |
|---|---|---|
| Lime | `#C8F03C` | Brand colour, app icon tile, primary buttons and selection accent in the dark theme |
| Ink | `#111111` | Mark and text on lime |
| Night | `#0E1116` | Dark surfaces |
| Olive | `#4D6B00` | Accent for text, links and focus rings on light surfaces (6.1:1 on white) |
| Lime wash | `#F1F8D6` | Selected row and soft accent background in the light theme |

Contrast: ink on lime 14.4:1. Lime on white is 1.3:1, so lime never carries text or a focus ring on a light
surface; use olive there. Lime buttons always carry ink text, in both themes.

## Rules

- Clear space around the mark: one disk height (44 units on the 256 grid).
- Minimum size: 16 px for the tile icon, 20 px for the bare mark, 96 px wide for the horizontal lockup.
- Do not recolour the mark outside the files above, stretch it, add effects, or rebuild the wordmark in
  another font.
- Engine logos (PostgreSQL, MySQL, MariaDB) never sit inside a Kerno lockup.
