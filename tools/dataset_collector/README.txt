VRCT Dataset Collector — Rust / Windows x64
===================================

VRChatのチャットボックス検出モデルを作るための画像収集ツールです。
PythonやVRCT本体のインストールは不要です。

【始め方】
1. ZIPをすべて展開してください。
   デスクトップ等、自分が書き込めるフォルダに置いてください。
2. VRChatを起動します。VRで使う場合はSteamVR経由で起動します。
3. vrct-dataset-collector.exeをダブルクリックします。
   2秒周期・左眼・10分間の自動撮影を開始します。
   native APIが処理中の場合は完了を待ちます。
4. 画像と撮影情報はexeの隣のdataset_collectedフォルダに保存されます。
   実行ごとに日時付きフォルダができます。
5. 終了後、Enterで画面を閉じます。

【操作】
コンソール（このツールの画面）を選択してからキーを押してください。
P: 一時停止／再開、QまたはCtrl+C: 終了、R: 保存枚数の表示。
P/Q/RにEnterは不要です。VRChatへ戻っても自動撮影は続きます。
処理中の1枚が終わるまで停止を待つことがあります。

【撮影方法】
・SteamVRで動作中のVRChatはOpenVRから取得し、VRChatのPCウィンドウを
  最小化しても撮影できます。ヘッドセットの映像が更新されている必要があります。
・DesktopではVRChatウィンドウから取得します。最小化しないでください。
・取得できない間は [waiting] と理由を表示します。枚数が増えていることを
  確認してください。[ERROR] は保存失敗等で停止したことを示します。
・Windows 10/11 x64向けです。SteamVRを経由しないVRは対象外です。

【設定を変える】
exeのあるフォルダでPowerShellを開いて実行します。

  .\vrct-dataset-collector.exe --duration 300 --interval 3
  .\vrct-dataset-collector.exe --backend openvr --eye right --max-frames 100
  .\vrct-dataset-collector.exe --backend hwnd --out "D:\VRChat画像"
  .\vrct-dataset-collector.exe --manual
  .\vrct-dataset-collector.exe --help

--duration は秒（初期値600、0は無制限）、一時停止も時間に含みます。
--interval は秒（初期値2）。保存が間に合わない回は飛ばします。
--max-frames は保存成功枚数で停止（初期値0は枚数制限なし）。
--manual はEnterでpositive、Nでnegative、Uでunlabeledへその場で撮影します。
引数付き起動では終了後のEnter待ちはありません。

【保存データ】
PNGは元の解像度の可逆圧縮です。3000×3276では300枚で約2 GBが目安です。
空き容量を確認してください。JSONには日時、解像度、取得方法等を記録します。
自動撮影はunlabeled（未分類）です。チャットボックスの枠を確認・付与してから
学習に使ってください。未分類画像をそのまま「背景のみ」として学習させないでください。
画像やJSONのアップロード、Geminiへの送信、学習はこのツールでは行いません。

【配布について】
このZIPは未署名です。Pythonのない別PCでの動作検証はまだ行っていません。
配布する際はREADME.txtとlicensesフォルダを含むZIP全体を渡してください。
dataset_collectedに撮影画像が入ったフォルダをそのまま配布しないでください。
BUILD-INFO.jsonにビルド環境とexeのSHA-256を記録しています。

--max-age は取得開始から保存までの許容秒（既定2）。
--wander はOSCでランダムに歩く機能。--osc-port は送信先127.0.0.1のポート（既定9000）。
Pは歩行も停止・再開、終了時は全軸を0に戻します。障害物は検知しません。
セッション名は単一フォルダ名のみ。パス移動・予約名・root外へのjunctionを拒否します。
PNG/JSONの組を完全にpublishした後だけ件数を増やします。失敗した画像を再利用しません。
VRChat.exeのPID/プロセス開始時刻と取得前後のsourceを確認します。
OpenVR DLL v2.15.6と対応ライセンスをZIPに同梱してください。
診断: vrct-capture-probe.exe --frames 5 --interval 1 --eye left
ビルド: src-tauriで cargo build -p vrct-capture-tools --release --bins -j1
Rust版の実GPU/HMD・SteamVR再起動・長時間動作は別途実機確認が必要です。
