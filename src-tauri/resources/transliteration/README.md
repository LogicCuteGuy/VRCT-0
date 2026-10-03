# Native Japanese readings

VRCT uses the Rust Sudachi library at revision
`7e2f287bbfffc036421cf960802e41a696727747` (v0.6.10) and the **full
20250825** dictionary, matching the former SudachiPy dependency. The library
uses its built-in plugins; these resources need no Python interpreter,
Python package, native Python extension, or separately installed tokenizer.

Before creating an installer, run `pwsh -File resources/transliteration/prepare.ps1`
from `src-tauri`. This downloads the pinned official archive and checks both
archive and extracted dictionary SHA-256. The ignored `system.dic` is
359,725,440 bytes; include it and the other resources from this directory in
the installer. Do not ship a reduced dictionary: it changes compound and
proper-noun readings. A missing, wrong-size or corrupt dictionary is an
explicit initialization error. Tests also require this preparation step.

The source archive URL, sizes and checksums are in `manifest.json`. They were
verified against the official downloadable archive. The checked-in config,
`char.def`, `rewrite.def` and `unk.def` were copied unchanged from the pinned
Sudachi Rust source. `LEGAL` and `LICENSE-2.0.txt` were copied unchanged from
the full 20250825 dictionary distribution and **must travel with it**.
Sudachi is Copyright (c) 2021-2024 Works Applications Co., Ltd., Apache-2.0.
The dictionary is Apache-2.0 with UniDic/NEologd/other third-party notices
in `LEGAL`.

`vrct-core/tests/fixtures/transliteration.json` records 164 flag/input cases
from the existing Python implementation (SudachiPy 0.6.10 / full 20250825),
using `analyze(..., use_macron=False)` and exactly the model's payload key
filter. Test generation is development evidence; the shipped code uses only
Rust. Tests cover contextual 何, compounds, okurigana, prolonged vowels,
half-width text, combining marks, emoji and mixed scripts. Chinese/Korean,
Thai and other scripts retain the old Japanese tokenizer behavior; this
feature does not add pinyin or a separate multilingual transliterator.

Sources: [Sudachi Rust](https://github.com/WorksApplications/sudachi.rs/tree/7e2f287bbfffc036421cf960802e41a696727747),
[SudachiDict](https://github.com/WorksApplications/SudachiDict),
[original package version](https://pypi.org/project/SudachiDict-full/20250825/).
