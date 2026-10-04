# 収集ツールのRust exe配布

`vrct-dataset-collector.exe`、`vrct-chatbox-player.exe`、`vrct-capture-probe.exe` は
`src-tauri/crates/vrct-capture-tools` から作るWindows x64のnative CLI。
Python、venv、PyInstaller、VRCT本体、CUDAは不要。画像取得にはVRChat、VR取得にはSteamVRが必要。

```powershell
cd src-tauri
cargo build -p vrct-capture-tools --release --bins -j1
cargo test -p vrct-capture-tools -j1 -- --test-threads=1
```

exeの隣へValve OpenVR v2.15.6の `resources/openvr/openvr_api.dll` と対応ライセンスを同梱する。
SDK/DLLのpinとSHA256はnative resource manifestに従う。
HWND取得にはPythonファイルもOCR/音声モデルも不要。
OpenVRを使わないHWND経路、playerのlist/dry-runはOpenVR初期化を行わない。
playerには編集可能な `chatbox_samples/*.json` をexeの隣に置く。
配布物にはREADME、依存ライセンス、ビルド情報とSHA256を付ける。
ビルド/ZIPの出力はVRCTの画面用distと分離した `tool-dist/` を使う。
画像、送信ログ、APIキー、個人設定、保護されたchatbox検出モデルは同梱しない。

collectorの既定保存先はexeの隣の `dataset_collected/`。
日本語・空白を含む、書込可能なフォルダへZIP全体を展開する。
`--out` はcollectorの保存先、`--samples` と `--log-dir` はplayerの原本/ログ保存先を変更する。
操作は [収集手順](ocr_dataset_collection.md) と [送信手順](chatbox_sample_player.md) を参照。

## 起動検査

```powershell
.\vrct-chatbox-player.exe --list
.\vrct-chatbox-player.exe --dry-run --max-messages 1
.\vrct-dataset-collector.exe --help
.\vrct-capture-probe.exe --help
```

これらは画像取得もVRChat送信も伴わない。
collectorの `--manual --duration 0.1 --out <test folder>` は要求キーを押さない限りnative画像取得をしない。
回帰テストはfake sourceと一時loopback UDP portのみ。
source外の日本語/空白パス、署名、Python未導入の別PC、実GPU/HMD、長時間動作は配布時の追加確認対象。
旧Python/PyInstaller版のexe hashと実機記録をRust版の検証証拠として使わない。

[Valve OpenVR v2.15.6 LICENSE](https://github.com/ValveSoftware/openvr/blob/v2.15.6/LICENSE)。
配布を渡すときはライセンスを含むZIP全体を渡す。
