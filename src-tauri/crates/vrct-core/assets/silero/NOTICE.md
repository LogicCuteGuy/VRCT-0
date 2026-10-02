# Silero VAD models

`silero_encoder_v5.onnx` and `silero_decoder_v5.onnx` are the Silero VAD v5
network, split into an encoder and a decoder, exactly as faster-whisper 1.1.1
ships them (`faster_whisper/assets/`). Silero VAD is released under the MIT
licence by the Silero Team (https://github.com/snakers4/silero-vad);
faster-whisper is MIT as well (SYSTRAN). They are the same files the Python
backend uses today, so the Rust code and the Python code run one model.

They are compiled into `vrct-core` (`audio::silero`), so nothing has to be
downloaded or found on disk at run time.
