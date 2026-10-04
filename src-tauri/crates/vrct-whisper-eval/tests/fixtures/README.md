# Selection oracle provenance

`selection.json` was generated on 2026-10-04 by running the original Python
selection and seed helpers, with bytecode writes disabled. It contains 18
synthetic Japanese metadata rows, selection of nine rows with seed `20260911`
and minimum three speakers, and `deterministic_seed("clip", "white_snr20")`.
No external corpus text or audio is included.

Original source SHA-256 at generation:

- `tools/whisper_eval/prepare_common_voice.py`:
  `070ac03d09f965249e781281e13fe55334fcfb965a1b00b42322ed699551198e`
- `tools/whisper_eval/audio.py`:
  `e73054e2da1a66748746ba25d33127f1524cdd1412f7500c5cf3e0f95279f6af`

The oracle checks ordering and source selection independently of the Rust
implementation. Noise waveforms use the documented native generator and are
tested for repeatability and SNR rather than NumPy byte equality.
