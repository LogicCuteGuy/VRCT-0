The model manifest was obtained from Hugging Face's model API on 2026-10-03,
using `https://huggingface.co/api/models/<repo>?blobs=true` for the repository
names already selected by VRCT's Python model configuration. No weights are
embedded in the application. The manifest pins the returned repository commit,
filename, length, and content identity for every downloaded artifact.

Hugging Face's documented download identity uses SHA-256 for LFS objects and a
Git blob SHA-1 for ordinary files (including the `blob <length>\0` prefix).
See [download metadata](https://huggingface.co/docs/huggingface_hub/package_reference/file_download)
and [the Hub API](https://huggingface.co/docs/hub/api).

CT2 model repositories retain their complete runtime files and model cards.
Separate Facebook tokenizer repositories contribute the native tokenizer files,
configuration and model card, under each installed model's `tokenizer/` folder.
Whisper repositories use their actual published files, rather than the union of
candidate filenames (some turbo variants have `vocabulary.json` rather than
`vocabulary.txt`). Downloaded model cards and any license files remain in the
installed model directories.

`Manager::available` checks file integrity. Engine loading remains the final
check of native format/compute compatibility; downloading does not allocate a
second model instance merely to test availability.
