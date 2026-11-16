# Fonts

IBM Plex Sans and IBM Plex Mono, unmodified, from the official IBM release on GitHub
(<https://github.com/IBM/plex/releases>). Licence: SIL Open Font License 1.1, see `OFL.txt`
(copyright © 2017 IBM Corp., Reserved Font Name "Plex").

`komponist-tokens` embeds them (`komponist_tokens::font::FACES`); `design/tokens.json` names the
families (`font.sans`, `font.mono`) and the type styles pick the weights. Mono SemiBold (600) is
used by no type style: it sets the track toggle letters M S R I, which the design data sets in
Mono 10 at `fontWeight: 600` (`tgl` in `Designsystem.dc.html`, `Screens.dc.html` and `ks-view.js`); added in WP-02b.

| File | Release | Font version | SHA-256 |
|---|---|---|---|
| `IBMPlexSans-Regular.ttf` (400) | [`@ibm/plex-sans@1.1.0`](https://github.com/IBM/plex/releases/tag/%40ibm%2Fplex-sans%401.1.0), `ibm-plex-sans.zip` → `fonts/complete/ttf/` | 3.005 | `975dcda37d80f038dcd143c22e33ca2d97a0cc5a929aace1c749153b0fe1afa5` |
| `IBMPlexSans-Medium.ttf` (500) | same | 3.005 | `331c8639d7598b2cde62a911a71db195e30cb655cd6bdf2e324a7e984955f907` |
| `IBMPlexSans-SemiBold.ttf` (600) | same | 3.005 | `a20caf8286023a6a7a85e40b1d2a4ae9fc3e3b1f9eda8f4c542dd4986af67bb1` |
| `IBMPlexMono-Regular.ttf` (400) | [`@ibm/plex-mono@2.5.0`](https://github.com/IBM/plex/releases/tag/%40ibm%2Fplex-mono%402.5.0), `ibm-plex-mono.zip` → `fonts/complete/ttf/` | 2.005 | `7c6fbddca4b700be918f5f6183d9bd4464fa427fe435f0b480d77fe2bb8c5a43` |
| `IBMPlexMono-Medium.ttf` (500) | same | 2.005 | `98fbd727aae340b236955879dabed4d991aac9e8e90b3b2a67ce4a59221cc97c` |
| `IBMPlexMono-SemiBold.ttf` (600) | same | 2.005 | `f04d7c488ddf7d1fa99f2574efc3406ea4cbe17bb1af3a1ab960f84d0c96a172` |

`OFL.txt` is `fonts/complete/ttf/license.txt` from the Sans release (the Mono release ships the
same text, byte for byte).

The SemiBold face was taken in WP-02b from the same `ibm-plex-mono.zip` of `@ibm/plex-mono@2.5.0`
(zip SHA-256 `6d23f01257663d8cc49a0d64c22ced630b79e0e2a0ac08a0da86e9a38bbc481c`); the Regular and
Medium files in that zip are identical to the bundled ones.

## Tabular digits (`tnum`)

The design wants tabular numbers everywhere, and egui applies no OpenType features, so the digits
must be tabular in the default glyphs.

Checked with fontTools 4.68 (`hmtx` advance of the glyphs that `cmap` maps `0`–`9` to, and the
`GSUB` feature list):

| Face | Advance of `0`–`9` (units per em 1000) | `tnum` / `pnum` in `GSUB` |
|---|---|---|
| Plex Sans 400 / 500 / 600 | 600 for every digit | neither |
| Plex Mono 400 / 500 / 600 | 600 for every digit | neither |

IBM Plex Sans has **tabular lining digits by default**; there is no proportional set to switch
away from. Plus (`+`), minus (`−`, U+2212) and figure space (U+2007) also advance 600, so signed
values line up too. **No `tnum` freeze was needed**, so the fonts are the original files under
their original names (no OFL renaming).

The test `font_faces_have_tabular_digits` in `crates/komponist-tokens/tests/egui.rs` measures the
digits through egui's own font code to guard this.

To repeat the fontTools check:

```bash
python3 - assets/fonts/*.ttf <<'EOF'
import sys
from fontTools.ttLib import TTFont
for path in sys.argv[1:]:
    f = TTFont(path)
    cmap, hmtx = f.getBestCmap(), f["hmtx"]
    widths = {hmtx[cmap[ord(d)]][0] for d in "0123456789"}
    feats = {r.FeatureTag for r in f["GSUB"].table.FeatureList.FeatureRecord}
    print(path, sorted(widths), "tnum" in feats, "pnum" in feats)
EOF
```

(`nix shell nixpkgs#python3Packages.fonttools --command python3 …` if fontTools is missing.)

## Keyboard symbols (`KomponistKeys.ttf`, macOS shortcut labels)

IBM Plex has no ⌘ (U+2318), ⇧ (U+21E7), ⌥ (U+2325) or ⌃ (U+2303), and egui panics on a glyph no font
has. The macOS shortcut labels (`⌘S`, `⇧⌘Z`, issue #47) need them, so a four-glyph subset of
DejaVu Sans 2.37 (`DejaVuSans.ttf` of the nixpkgs `dejavu_fonts` 2.37 package) is bundled as a
fallback after Plex (`komponist_ui::fonts::definitions`). Made with fontTools 4.63 (`fontTools.subset`,
no layout features, no hinting, name table reduced), family renamed to "Komponist Keys" because the
Bitstream Vera licence forbids a modified version under the original names. Licence: Bitstream Vera
Fonts Copyright plus public-domain DejaVu changes, `LICENSE-DejaVu.txt`, GPL-compatible.
