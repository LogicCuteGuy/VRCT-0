# 多言語チャットの送信と撮影（Rust）

`vrct-chatbox-player.exe` は編集可能な `chatbox_samples/*.json` を読み、実行中のPCの
`127.0.0.1:9000` の `/chatbox/input` に `[本文, true, false]` を送る。
送信役のアカウントのPCで起動する。通知音は無効。撮影側は `vrct-dataset-collector.exe` を使う。
UDPの送信成功はVRChatの表示成功を意味しない。

引数なしではEnterで送信開始、既定は6秒周期のシャッフル・繰り返し。
開始前にQ→Enterで取り消し。開始後はコンソールにフォーカスを置いて
P=一時停止/再開、R=状態表示、Q/Ctrl+C=終了。引数なしの対話起動は終了後Enterで閉じる。
一時停止後の再開と遅れた送信は間隔を取り直し、遅れを取り戻す連続送信をしない。

```powershell
.\vrct-chatbox-player.exe --list
.\vrct-chatbox-player.exe --samples .\chatbox_samples --export-catalogue .\catalogue
.\vrct-chatbox-player.exe --dry-run --max-messages 1
.\vrct-chatbox-player.exe --languages ja,en,ko,zh-Hans --interval 8
.\vrct-chatbox-player.exe --length long --once
.\vrct-chatbox-player.exe --languages mixed --ordered --start --max-messages 50
.\vrct-chatbox-player.exe --samples "D:\サンプル" --seed 42 --port 9000
```

`--samples` はJSONフォルダ、`--languages` はカンマ区切りの言語コード、
`--length` は tiny/short/medium/long。境界は5/30/80/144 UTF-16単位。
`--ordered` はファイル順とJSON内の言語・行順を保持、`--seed` は同じRust版のシャッフルを再現する。
Rustの固定PRNGであり、以前のPython版の同じseedと並び順は一致しない。
`--once` は1周、`--max-messages` は送信件数上限（0=無制限）、`--interval` は最低3秒。
`--port` はローカルのOSC入力ポート。送信先のホストは変更できない。
`--timeout` はUDP送信待ちの上限秒（既定1、最大3600）。`--log-dir` はログ保存先の上書き。
`--start` は開始確認を省略する。非対話端末での実送信にはこの明示指定が必要。
`--dry-run` はソケットを作らず、OSC送信・ログ書込も行わない。`--list` は一覧のみ。
`--export-catalogue <directory>` は送信せず、原本の順に `samples.jsonl`（UTF-8）と
`samples.txt`（UTF-8 BOM）を作る。本文、tags、ID、UTF-16単位、length、明示行数を保持する。
既存の同名生成ファイルは置き換える。シャッフルは行わず、言語/lengthのフィルタは適用する。

すべてのJSONを送信前に検証し、空本文、制御・方向制御文字、144 UTF-16単位超、
9行超を拒否する。本文を切り詰めない。JSONはUTF-8/BOM対応、元の本文と改行を保持する。
VRChatの最大9行には自動折り返しも含まれるため、字形・RTL・絵文字は撮影時に確認する。
[Chatbox OSC](https://docs.vrchat.com/docs/osc-as-input-controller#chatbox)、
[公式Wiki](https://wiki.vrchat.com/wiki/Chatbox)。

実送信はexeの隣の `sent_logs/*.jsonl` にUTC日時、ID、言語、本文、UTF-16単位、
明示行数、宛先、tagsを即時flushして記録する。編集用の原本は `tools/chatbox_samples/`。
配布時にはJSONとライセンスを同梱し、送信ログや収集画像を含めない。

## ビルドと回帰テスト

```powershell
cd src-tauri
cargo build -p vrct-capture-tools --release --bins -j1
cargo test -p vrct-capture-tools -j1 -- --test-threads=1
```

ソースは `src-tauri/crates/vrct-capture-tools`。Python、venv、PyInstallerは不要。
Windows x64の対話キー操作を使用する。既定のJSONはexeの隣から読み、
開発debug版だけはリポジトリの原本も参照する。
テストは画像源を偽物に置換し、OSCはローカルの一時ポートだけで検証する。
VRChatの9000番や外部PCへ送信しない。Rust版の実VRChat表示・全言語の字形は別途実機確認する。
