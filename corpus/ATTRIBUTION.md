# Corpus attribution

The synthetic categories (`gradient`, `pixelart`, `texture`, `synthetic`)
are generated procedurally by `cargo run -p cafe-bench --bin gen-corpus`
(see `AGENTS.md` phase 2) and require no attribution — they contain no
third-party content.

This file documents the source, author, and license of every real-content
image under `corpus/photo/`, `corpus/screenshot/`, `corpus/illustration/`,
and `corpus/lineart/`, plus the HDR fixtures under `corpus/hdr/`. All
images were downscaled to fit within 256x256 (preserving aspect ratio,
never upscaled) and re-encoded as RGBA8 PNG by
`cargo run -p cafe-bench --bin import-corpus`; this page attributes the
*original* source image, not the resized derivative.

## `corpus/photo/`

Source: the [Kodak Lossless True Color Image Suite](https://r0k.us/graphics/kodak/),
a long-standing public-domain photographic test set widely used in image
compression research (unrestricted for research/technical use; no formal
license statement is attached by the host, consistent with how the set has
been used across the image-compression literature for decades).

| File | Original | Original size |
|---|---|---|
| `kodim01.png` | `kodim01.png` | 768x512 |
| `kodim05.png` | `kodim05.png` | 768x512 |
| `kodim15.png` | `kodim15.png` | 768x512 |
| `kodim23.png` | `kodim23.png` | 768x512 |

## `corpus/lineart/` and `corpus/illustration/`

Source: [Wikimedia Commons, Category:PD-ScottForesman](https://commons.wikimedia.org/wiki/Category:PD-ScottForesman) —
line-art illustrations from Pearson Scott Foresman, donated to the public
domain and hosted by the Wikimedia Foundation. License: **Public domain**
(no attribution legally required; credited here for traceability).

| File | Original title | Category |
|---|---|---|
| `lineart/blip-psf.png` | *Blip (PSF).png* | lineart |
| `lineart/abacus-psf.png` | *Abacus (PSF).png* | lineart |
| `lineart/abdomen-psf.png` | *ABDOMEN (PSF).png* | lineart |
| `lineart/aardvark2-psf-colourised.png` | *Aardvark2 (PSF) colourised.png* | lineart |
| `illustration/abstract-art-psf.png` | *Abstract art (PSF).png* | illustration |
| `illustration/hr-chart-psf.png` | *A Hertzsprung–Russell chart of stellar evolution (PSF).png* | illustration |

Source URLs (original, full-resolution files):
- <https://upload.wikimedia.org/wikipedia/commons/3/3b/Blip_%28PSF%29.png>
- <https://upload.wikimedia.org/wikipedia/commons/a/af/Abacus_%28PSF%29.png>
- <https://upload.wikimedia.org/wikipedia/commons/4/46/ABDOMEN_%28PSF%29.png>
- <https://upload.wikimedia.org/wikipedia/commons/b/bb/Aardvark2_%28PSF%29_colourised.png>
- <https://upload.wikimedia.org/wikipedia/commons/6/66/Abstract_art_%28PSF%29.png>
- <https://upload.wikimedia.org/wikipedia/commons/e/ef/A_Hertzsprung%E2%80%93Russell_chart_of_stellar_evolution_%28PSF%29.png>

## `corpus/screenshot/`

Source: captured locally for this project on 2026-09-10, Windows 11,
showing only built-in system utilities with placeholder/neutral content
(no personal data, file paths, network shares, or account information).
Author: Daniel Secco Ferreira e Silva (project author) — original work,
licensed the same as the rest of this repository (BSD-3-Clause, see root
`Cargo.toml`'s `license.workspace`).

| File | Application | Content |
|---|---|---|
| `screenshot/calculator.png` | Windows Calculator | default/blank state |
| `screenshot/charmap.png` | Windows Character Map | default Arial glyph table |
| `screenshot/notepad.png` | Windows Notepad | placeholder Lorem-ipsum-style text authored for this capture |
| `screenshot/paint.png` | Windows Paint | blank canvas, default toolbar |

## `corpus/hdr/`

Source: [`openexr-images`](https://github.com/AcademySoftwareFoundation/openexr-images),
the official OpenEXR sample image set maintained by the Academy Software
Foundation (formerly Industrial Light & Magic / Lucasfilm). License:
BSD-3-Clause-style, per the repository's own `LICENSE` file (Copyright
Contributors to the OpenEXR Project). These files are staged for a future
CLI pass (see `AGENTS.md`'s HDR deferral) and are **not** referenced by
`corpus/manifest.json`, since `cafe-cli`/`cafe-bench` only support 8-bit
PNG I/O today.

| File | Original path in `openexr-images` |
|---|---|
| `hdr/Blobbies.exr` | `ScanLines/Blobbies.exr` |
| `hdr/Cannon.exr` | `ScanLines/Cannon.exr` |

Source URL: <https://github.com/AcademySoftwareFoundation/openexr-images/tree/main/ScanLines>
