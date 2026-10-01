# -*- mode: python ; coding: utf-8 -*-

import os

_use_upx = os.environ.get("VRCT_PYINSTALLER_UPX") == "1"


# The upstream chat-bubble detector (and the licence files that describe it) must
# not ship in a fork's release build -- see NOTICE.md. Everything else placed in
# that folder (e.g. a detector trained independently) is still bundled.
_OCR_ONNX_DIR = os.path.join(SPECPATH, '..', 'src-python', 'models', 'ocr', 'onnx')
_NOT_REDISTRIBUTABLE = {'chatbox_yolox_tiny.onnx', 'LICENSE.txt', 'LICENSE.en.txt', 'NOTICE.txt'}
_ocr_onnx_datas = [
    (os.path.join(_OCR_ONNX_DIR, file_name), 'ocr_onnx/')
    for file_name in sorted(os.listdir(_OCR_ONNX_DIR))
    if file_name not in _NOT_REDISTRIBUTABLE
    and os.path.isfile(os.path.join(_OCR_ONNX_DIR, file_name))
]


a = Analysis(
    ['..\\src-python\\mainloop.py'],
    pathex=[],
    binaries=[],
    datas=[
        ('./../src-python/models/overlay/fonts', 'fonts/'),
        ('./../src-python/models/translation/translation_settings/prompt', 'translation_settings/prompt/'),
        ('./../src-python/models/translation/translation_settings/languages', 'translation_settings/languages/'),
        ('./../.venv_cuda/Lib/site-packages/zeroconf', 'zeroconf/'),
        ('./../.venv_cuda/Lib/site-packages/openvr', 'openvr/'),
        ('./../.venv_cuda/Lib/site-packages/faster_whisper', 'faster_whisper/'),
        ('./../.venv/Lib/site-packages/hf_xet', 'hf_xet/'),
        ('./../.venv_cuda/Lib/site-packages/rapidocr', 'rapidocr/'),
        ] + _ocr_onnx_datas,
    # nvidia.cublas / nvidia.cudnn は ctranslate2 が GPU 実行時に
    # LoadLibrary で遅延ロードするDLLの提供元で、Python からは import
    # されないので依存解析に掛からない。ここで明示して
    # pyinstaller-hooks-contrib の hook-nvidia.* に _internal/nvidia/<lib>/bin/
    # へ収集させる (2026-09-18 に torch を落とすまでは、torch が同梱していた
    # 同じDLL群が torch 経由で収集されていた)。実行時のDLL検索パス登録は
    # src-python/utils.py の _registerBundledCudaLibraries が行う。
    hiddenimports=['faster_whisper.vad', 'models.transcription.audio_pipeline', 'rapidocr', 'cv2', 'models.ocr',
                   'nvidia.cublas', 'nvidia.cudnn'],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=['pandas', 'matplotlib', 'PyQt5'],
    noarchive=False,
    optimize=0,
)
pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    [],
    exclude_binaries=True,
    name='VRCT-sidecar-x86_64-pc-windows-msvc',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=_use_upx,
    console=True,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    icon=[],
)
coll = COLLECT(
    exe,
    a.binaries,
    a.datas,
    strip=False,
    upx=_use_upx,
    upx_exclude=[],
    name='.',
)
