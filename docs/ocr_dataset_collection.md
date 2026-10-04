# チャットボックス学習画像の収集（Rust）

`vrct-dataset-collector.exe` はWindows x64の独立CLI。PythonやVRCT本体を起動せず、
ローカルにPNGとJSONを保存する。アップロードや自動ラベル付けは行わない。
配布は [exe配布手順](dataset_collector_distribution.md) を参照。

引数なしで自動撮影を開始する。既定は **auto・左眼・2秒周期・10分間**。
保存先はexeの隣の `dataset_collected/session_日時/unlabeled/`。
コンソールでP=一時停止/再開、R=件数と状態、Q/Ctrl+C=終了。Enterは不要。
引数なしの対話起動は終了後Enterで閉じる。停止は処理中のnative APIの完了と所有スレッドでの解放を待つ。

```powershell
.\vrct-dataset-collector.exe night_world --backend openvr
.\vrct-dataset-collector.exe desktop --backend hwnd --duration 300
.\vrct-dataset-collector.exe right_eye --eye right --interval 3 --max-frames 100
.\vrct-dataset-collector.exe --duration 0 --out "D:\VRChat画像"
.\vrct-dataset-collector.exe manual_session --manual
.\vrct-dataset-collector.exe --wander --osc-port 9000
```

`--backend auto/openvr/hwnd`、`--eye left/right`、`--interval` 秒（最低0.1）、
`--duration` 秒（既定600、0=無制限、一時停止も含む実時間）、`--max-frames` 成功枚数
（0=無制限）、`--max-age` 取得開始から保存までの許容秒（既定2）を指定できる。
intervalは開始の目標周期。処理が遅れた回は飛ばし、追いつくための連写はしない。

手動モードはEnter=positive、N=negative、U=unlabeled。未知のキーは撮影しない。
要求後に新しく画像を取得し、最新キャッシュ画像を保存する動作は使用しない。
一時停止中は保存しない。自動撮影はすべてunlabeledであり、背景画像として直接学習させない。
枠の作成とレビューは [仮アノテーション手順](gemini_annotation_workflow.md) を参照。

`--wander` はこのPCのVRChatへOSC `/input/Vertical`、`/input/LookHorizontal`、
`/input/Jump` を送りランダムに歩かせる。障害物を検知しない。
Pは撮影と移動を一緒に停止/再開する。停止・エラー時はすべての軸を0に戻してからsocketを閉じる。
送信先は127.0.0.1、`--osc-port` は1..65535（既定9000）。指定しなければOSC socketを作らない。

## 取得と保存の保証

- HWNDはタイトルだけで選ばず、VRChat.exeが所有する可視のVRChatウィンドウ1個を選ぶ。
  PrintWindowは対象のsurfaceを読み、重なった別アプリの画面を取得しない。
  最小化、空画像、PID/プロセス開始時刻/handle/矩形/状態の変化は拒否する。
- OpenVR D3D11は選択した眼を読む。PIDとプロセス開始時刻、focus、rendererを前後で照合し、
  compositor frame timingが更新していない画像を拒否する。
  VRChatのVR sceneを一度検出したら、そのプロセスが終了するまでHWNDへ黙って切り替えない。
  VR取得ではVRChatウィンドウを最小化できる。Desktop取得ではできない。
- 捕捉/保存/解放は同一workerが所有する。失敗時に前の画像を使い回さない。
  native APIが返らない場合、終了はその完了を待つ。
- RGBを保持した可逆PNG（Fast圧縮）と同名JSONを一時ファイルへ書き、JSON→PNGの順に
  overwriteなしでpublishする。PNGがcommit marker。通常の失敗は所有する途中ファイルと
  publish済み片方だけを戻し、成功した組だけ件数を増やす。
  OS強制終了/電源断ではJSONや`.part`が残る可能性があり、PNG/JSON両方のある組だけを使う。
- セッションは単一の有効なフォルダ名。`../`、Windows予約名、末尾の空白/点を拒否し、
  既存のsymlink/junctionが保存先root外へ向く場合も拒否する。同じsessionの再実行はrun_idで区別する。

JSONには取得開始UTC、backend、eye、PID/開始時刻、window状態、解像度、session、run_id、
label、imageを記録する。VR取得は読出し前後のcompositor frame indexも記録する。
取得できない間は `[waiting]`、書込や初期化の致命的な失敗は `[ERROR]` で停止する。

## Probeと検証

```powershell
.\vrct-capture-probe.exe --frames 5 --interval 1 --eye right --out tmp\openvr_probe
cd src-tauri
cargo test -p vrct-capture-tools -j1 -- --test-threads=1
```

Probeはmirrorを診断するため、VRChat以外のsceneも記録する。`CAPTURED` は画像読出し成功を示し、
VRChatの検証成功を意味しない。HMD、推奨サイズ、DXGI adapter、view/texture形式、row pitch、
PID/focus、frame index、RGB SHA256、平均・標準偏差、変更pixel割合、window状態と画像をreportへ保存する。
取得に失敗しても `report.json` に理由を残す。既定は5枚/1秒/左眼、上限120枚、間隔0.1..10秒。

回帰テストはfake sourceとローカル一時UDPのみを使い、実際の画像取得は行わない。
Rust版のGPU/HMD/SteamVR再起動・長時間動作・VRChat実画像は別途実機確認が必要。
旧Python版の実機記録はRust版の検証結果として扱わない。
