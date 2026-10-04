# VRCT-0 ビルドガイド

UI のタイ語とテーマ設定は [VRCT-0 guide](vrct-0.md) を参照。

Windowsアプリと補助ツールはRustで実装されている。React UIはTauri内のRust controllerを呼び出す。現行の構成は [native backend](native_pipeline.md)、補助ツールは [native tools](native_tools.md) を参照。

## 必要な環境

- Windows x64、Node.js/npm、Git、RustのMSVC toolchain
- Visual StudioまたはBuild ToolsのC++ workload、Windows SDK、CMake
- MSVCのx64 Redistributables。`xtask prepare` はインストール済みVisual Studioからapp-local DLLと再配布条項を取得する

最初にリポジトリのルートで依存関係を用意する。

```powershell
npm ci
npm run update-version
npm run native:prepare
```

`native:prepare` は固定pinとSHA256を検証したSudachi辞書、OCR認識モデル、ONNX Runtimeを配置する。フォントとOpenVRもhashを検証する。MSVC runtimeはインストール済みtoolchainのversionとx64 DLLを検証してmanifestに記録する。翻訳・Whisperの大容量weightsはアプリから必要時に取得する。準備済み資源の検査だけを行う場合は次を使う。

```powershell
cargo run --manifest-path src-tauri/Cargo.toml -p xtask -- prepare --profile debug --offline
```

## 開発

```powershell
npm run dev
```

バージョン同期とnative資源準備の後、ViteとTauriを起動する。`ct2` featureでCTranslate2/Whisperを同じプロセスに組み込む。自動クリーンアップや他アプリの強制終了は行わない。

`npm run dev-ui` は準備処理を省いてViteとTauriを起動するため、既存資源を用いたUI検証に使える。`npm run vite` はブラウザでUIだけを表示する。`dev-fast` は `dev` の互換alias。`dev-cuda` と `dev-cuda-fast` も現在は同じCPU構成になる。

大容量モデルの自動取得を避けて起動経路を確認するときは、起動前に `VRCT_SKIP_MODEL_DOWNLOAD=1` を設定する。これはローカル推論に必要なweightsを用意する代わりにはならない。

## リリースビルドとZIP

```powershell
npm run build
npm run release
npm run native:verify
```

`build` はバージョン同期、release資源準備、Viteビルド、`tauri build --features ct2` を行う。`release` はその後Rust `xtask package` でZIPを作成して検査する。

- アプリ: `src-tauri/target/release/VRCT.exe`
- NSIS: `src-tauri/target/release/bundle/nsis/VRCT-0_<version>_x64-setup.exe`
- パッケージ: リポジトリ直下の `VRCT-0.zip` と `VRCT-0.zip.sha256`

既にビルド済みのアプリをパッケージする場合は次を使う。

```powershell
cargo run --manifest-path src-tauri/Cargo.toml -p xtask -- package --profile release --output VRCT-0.zip
cargo run --manifest-path src-tauri/Cargo.toml -p xtask -- verify --output VRCT-0.zip
```

ZIPは `VRCT.exe`、native DLL、`licenses/`、`resources/`、整合性manifestを含む。検査はファイル名だけでなく、各entryの長さ・hash、資源manifest、制限付き検出モデルの内容とEXE内への埋め込みも検査する。必要な資源やDLLが不足している場合、パッケージ作成は停止する。

`build-cuda` は現在 `build` と同じCPUビルドの互換alias。CUDA対応の補助学習ツールは [学習ガイド](ocr_yolo_training.md) のfeatureとtoolchain要件に従う。

## 補助ツールのビルドと配布

```powershell
npm run tools:build
npm run tools:package
```

既定はdebug版で、ZIPは `tool-dist/VRCT-native-tools.zip`。release版は次を使う。

```powershell
npm run native:prepare-release
npm run tools:build-release
npm run tools:package-release
```

ZIPにはcollector、sample player、capture probe、annotation、dataset、Whisper評価、YOLOX学習の8 executableと資源・ライセンスを含む。フォルダ全体を展開して使う。sample JSONは `tools/chatbox_samples/` にあり、パッケージ時にplayerの `--export-catalogue` で一覧を生成する。詳細な引数は [native tools](native_tools.md) と各binaryの `--help` を参照。

## バージョン管理

配布versionの編集箇所は `package.json` の `version`。`npm run update-version` はRust `xtask version` を呼び、`src-tauri/tauri.conf.json` に同期する。アプリbuildとreleaseコマンドでも実行される。

タグを付ける前に同期した両ファイルをcommitする。GitHub Actionsはタグの `v` を除いたversionと両ファイルの一致を検査する。`3.5.1-beta.1` のような `-beta` / `-rc` 識別子を含むタグはprereleaseとして公開する。

## GitHub Actionsとインストーラー

実際の設定は [release.yml](../.github/workflows/release.yml)。`v*` タグでWindows jobを起動し、Node/Rust/C++環境、native資源準備、テスト、release作成、ZIP検査を実行する。配布assetはsetup.exe、`VRCT-0.zip`、それぞれの `.sha256`。このガイド自体はCI、配布先、実機、CUDAの検証完了を示す記録ではない。

NSISは同じリリースタグから `VRCT-0.zip` を取得するダウンローダー形式。特定の既存versionへのインストール・ロールバックは `/VERSION=` で指定できる。

```powershell
VRCT_setup.exe /VERSION=3.4.2
```

`/CHANNEL=` と `/EDITION=` は互換のため受け付けるが、取得するパッケージはversionで決まる。指定リリースにZIPがない場合はインストールを中断する。現在のreleaseはSHA256 sidecarを提供する。旧releaseへのロールバックでsidecarが存在しない場合の互換動作は [NSIS template](../src-tauri/nsis/template.nsi) を参照。

## ソースと編集対象

| 場所 | 内容 |
|---|---|
| `src-tauri/src/` | Tauri、controllerとの接続、起動と終了 |
| `src-tauri/crates/vrct-core/` | controller、モデル管理、音声、翻訳、OCR、overlay、各sink |
| `src-tauri/crates/xtask/` | 資源準備、version同期、アプリ/補助ツールZIP検査 |
| `src-tauri/crates/vrct-*` | 各補助ツール |
| `src-ui/` | React UI、設定、ロケール |
| `resources/` | native資源とmanifest |
| `docs/readmes/README.*.md` | READMEの言語別原稿 |
| `docs/licenses/chatbox/` | 上流検出モデルの独自利用許諾とNOTICE |

READMEを編集するときは英語・日本語・韓国語・繁体字中国語の原稿を揃え、root READMEの参照先も確認する。UI文言は `src-ui/` のロケールとendpoint設定に合わせる。sampleカタログの文章をREADMEへ転載するときは元JSONと生成一覧を同期する。

## 検証とトラブルシューティング

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --workspace -j1
cargo clippy --manifest-path src-tauri/Cargo.toml --workspace --all-targets -j1 -- -D warnings
npm run vite-build
```

対象を限定する場合は `-p vrct-annotator` 等でcrateを指定する。モック・localhost検査と実機/有料API検査の境界は [native tools](native_tools.md) を参照。

資源の不一致は `native:prepare` / `native:prepare-release` で再検証する。MSVC runtimeが見つからない場合はインストール済みC++ workloadとRedistributablesを確認する。DLLを任意のSystem32から集める運用は使わない。

`npm run clean` はCargo成果物全体を、`npm run clean-soft` はアプリpackageのCargo成果物を削除する。通常はincremental buildを使い、変更・失敗の根拠があるときにcleanする。Node依存関係は `npm ci` で復元できる。

上流の制限付き `chatbox_yolox_tiny.onnx` はこのフォークの配布物に含まれない。OCRには権限のある外部検出モデルを `VRCT_OCR_BUBBLE_MODEL` で指定する。利用条件は [日本語条文](licenses/chatbox/LICENSE.txt)、[英語訳](licenses/chatbox/LICENSE.en.txt)、[NOTICE](../NOTICE.md)、独自モデルの学習は [学習ガイド](ocr_yolo_training.md) を参照。

プロジェクト全体の利用条件は [LICENSE](../LICENSE) を参照。
