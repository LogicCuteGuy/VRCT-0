# 収集画像からGeminiの仮アノテーションを作る

Rust製の `vrct-annotator` は画像抽出、Gemini RESTへの送信、再開、Label Studio向け書き出しを行う。PythonやGoogle SDKは不要。Label Studioは別途導入する。

入力はDataset Collectorが生成した `session/unlabeled/*.png` と同名のJSON。`positive/negative/` も入力できるが、撮影時の分類を正解bboxとして扱わない。

## ビルドと基本操作

```powershell
cargo build --manifest-path src-tauri/Cargo.toml -p vrct-annotator --release
src-tauri/target/release/vrct-annotator.exe check
src-tauri/target/release/vrct-annotator.exe prepare D:/captures --out D:/annotation_jobs/job1 --limit 100 --seed 42
$env:GEMINI_API_KEY = "your-api-key"
src-tauri/target/release/vrct-annotator.exe annotate D:/annotation_jobs/job1 --limit 100
src-tauri/target/release/vrct-annotator.exe status D:/annotation_jobs/job1
src-tauri/target/release/vrct-annotator.exe export D:/annotation_jobs/job1
```

`check` はオフライン検査でAPIを呼ばない。引数なしで起動すると対話ウィザードになる。新しいジョブはexe隣の `annotation_jobs/` に作り、既存の `manifest.json` を持つジョブを選ぶと再開する。環境変数がない場合、ウィザードはキーを画面に表示せず入力する。パイプやCIでは明示的なサブコマンドを使う。APIキーをコマンド引数に渡すオプションはない。

`prepare --limit 0` は全件を抽出する。既定モデルは旧ジョブとの互換性を維持する `gemini-2.5-flash`。`--model` で変更できるが、既存ジョブのモデル・プロンプト・スキーマは変更できない。別モデルを使う場合は新しいジョブを作る。モデルの提供状況はGoogleのモデル一覧で確認する。

`annotate` の既定値は `--limit 100 --interval 6 --retries 2`。`--limit 0` は対象全件、間隔は1秒以上、再試行回数は0〜5。`--retry-failed` を付けた場合のみ失敗・不明の画像を再送する。

## ジョブと検証

- 抽出はsession/backend別に分散し、Python版と同じ整数seedのMT19937シャッフルを使う。入力は変更せず、新しいジョブにPNGをコピーする。ジョブの出力先を入力内に置くことはできない。
- メタデータのファイル名、寸法、PNG形式、画像の実寸法と向きを検査する。パスの親参照、絶対パス、シンボリックリンクやジャンクションを経由したジョブ内アクセスを拒否する。
- `manifest.json` は画像SHA256とモデル・プロンプト・スキーマのpolicy hashを固定する。送信前に全画像を検証し、送信直前も当該画像のハッシュを検証する。
- Gemini RESTにPNGと固定プロンプトを送信する。構造化JSON、temperature 0、candidate 1、最大出力8192 tokens、要求タイムアウト120秒を使用する。
- bboxは `box_2d: [ymin,xmin,ymax,xmax]` の整数0〜1000と `label: chat_box` のみ。正の面積、重複なし、余分なキーなし、finish reason `STOP` を要求する。軸を推測した入れ替え、clamp、一部の不正boxだけを捨てる処理はしない。
- 書き出しは `exports/<日時>/` の追加スナップショット。人が確認済みの既存書き出しを上書きしない。仮bboxはLabel Studioの **predictions** に入れ、確定済みannotationsとしては書かない。

## 再開・失敗・利用量

結果の状態は `pending`、`unknown`、`detected`、`no_detection`、`invalid_response`、`api_error`。送信直前に `unknown` をatomic書き込みし、完了結果と全attempt履歴を保存する。`detected/no_detection` は再送しない。通常の再開では `pending` のみ処理し、失敗・不明を再送するときは明示的に `--retry-failed` を付ける。

HTTP処理中のCtrl+Cではジョブロックを解放して書き出しを作り、exit 130になる。強制終了や接続断では `unknown` が残ることがある。ジョブはOSファイルロックで並行更新を防ぐ。ロックファイルが残っていても、停止済みプロセスのロックは解除される。

429/500/502/503/504だけを指定回数まで再試行する。`Retry-After` の秒数とHTTP日時を尊重し、各API呼び出し間の最小間隔も守る。400/401/403/404、または再試行を使い切った429では残り画像の送信を止める。HTTP/1のみを使い、idle接続の再利用を無効にして、HTTP/2や再利用接続に伴う隠れた再試行を避ける。リダイレクトも無効。キーはヘッダーに渡し、URL、エラー応答本文、キーをログや結果ファイルに保存しない。

`status` は全attempt履歴のtoken使用量を合計する。未知の接続結果について課金やAPI実行の有無を確定することはできず、再送のexactly-onceは保証できない。間隔指定はGoogle側のquotaや課金額を保証しない。

## Label Studioで人が確定する

1. 書き出しごとに新しいプロジェクトを作り、`label_config.xml` を読み込む。
2. Local Filesのdocument rootをジョブのルートに設定する。`START_HERE.txt` に必要な環境変数とパスを出力している。
3. Local Files Storageを保存するときは **Saveのみ** を使う。Save & Syncによる重複取り込みを避ける。
4. `tasks.json` を一度だけimportし、predictionsを表示してannotationへコピーする。
5. bboxを修正してSubmitする。`no_detection`、失敗、未送信の画像も全件確認し、吹き出しなしと判断した画像は空のannotationをSubmitする。背景画像をSkipで済ませない。
6. 確定結果をYOLOでexportする。chat_boxはclass 0。空ラベルは人が確認した負例として保持する。

座標は元画像の寸法を保持し、0〜1000のyxyxからLabel Studioの百分率xywhへ変換する。ジョブを移動したらLocal Filesのdocument rootを更新する。追加書き出しを同じプロジェクトへ再importすると重複するので、新しいプロジェクトで扱う。

学習用のscene分割、固定validation、COCO変換は [native dataset手順](native_dataset.md) を参照。

## 回帰検査と外部仕様

```powershell
cargo test --manifest-path src-tauri/Cargo.toml -p vrct-annotator
```

検査は合成PNGとlocalhostの模擬HTTPを使用する。有料APIは呼ばない。実APIでの検出精度、quota、課金額、Label Studio UIの動作は、この検査では確認していない。

外部仕様: [Gemini REST](https://ai.google.dev/api/generate-content)、[構造化出力](https://ai.google.dev/gemini-api/docs/structured-output)、[画像入力](https://ai.google.dev/gemini-api/docs/image-understanding)、[rate limits](https://ai.google.dev/gemini-api/docs/rate-limits)、[モデル](https://ai.google.dev/gemini-api/docs/models)、[価格](https://ai.google.dev/gemini-api/docs/pricing)、[利用規約](https://ai.google.dev/gemini-api/terms)、[Label Studio Local Storage](https://labelstud.io/guide/storage.html#Local-storage)。
