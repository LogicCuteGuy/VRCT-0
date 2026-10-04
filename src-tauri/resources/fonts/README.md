# Native overlay fonts

These four files are unmodified copies of VRCT's previously bundled Noto Sans
fonts (preserved from commit `16cb286c`). The native application loads this
directory directly; it does not install fonts.

The font name-table license entries identify SIL Open Font License 1.1 and
`(c) 2014-2021 Adobe (http://www.adobe.com/), with Reserved Font Name 'Source'.`
The matching full [OFL license](https://github.com/google/fonts/blob/main/ofl/notosansjp/OFL.txt)
is included as `OFL.txt` and must be distributed with the fonts.

| File | SHA-256 |
| --- | --- |
| NotoSansJP-Regular.ttf | fb3df01b4182734d021d79ec5bac17903bb681e926a059c59ed81a373d612241 |
| NotoSansKR-Regular.ttf | 9db318b65ee9c575a43e7efd273dbdd1afef26e467eea3e1073a50e1a6595f6d |
| NotoSansSC-Regular.ttf | ae82f4e2a55e1316a55bcc1d05e9555ce08d8bda07e893b486896b626fd852ff |
| NotoSansTC-Regular.ttf | 6b137e2eb57a2d1e4cf391c886ab1b783a0e5ddb5c75254748bde00c15cb8ff5 |

Rust uses `rustybuzz` to shape glyphs and `fontdue` to rasterize them, with the
same language-to-font selection, overlay dimensions, palette, rounded panel,
message alignment, five-message history and per-token ruby layout. Rasterizer
antialiasing may differ from Pillow; pixel-identical rendering is not claimed.
