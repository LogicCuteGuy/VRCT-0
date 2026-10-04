# VRCT-0

[English](/docs/readmes/README.en.md) · [日本語](/docs/readmes/README.ja.md) · [한국어](/docs/readmes/README.ko.md) · [繁體中文](/docs/readmes/README.zh-Hant.md) · [简体中文](/docs/readmes/README.zh-Hans.md) · [ไทย](/docs/readmes/README.th.md)

## このforkでの変更

[VRCT](https://github.com/misyaguziya/VRCT) をベースにしたforkです。翻訳・音声認識の流れを引き継ぎ、以下を変更しています。

- Python sidecarをネイティブRustバックエンドに置き換え。
- マイクとスピーカーのHost/Device選択、WASAPI・ASIO入力、ドライバー設定パネル。
- キャプチャ、データセット、アノテーション、学習・評価用のネイティブツール。
- VRCT-0の名称、アイコン、明暗のロゴ。
- 保存できるダーク・ライト・システムテーマとUI翻訳の拡充。
- Rust xtaskによるビルド・パッケージ検証と、このforkのリリースからの更新。

[Documentation](/docs/README.md) · [Releases](https://github.com/LogicCuteGuy/0-VRCT/releases)

Upstream store and supporter links below belong to the original project. Original credits are retained.

<div align="center">

<picture>
    <source srcset="/docs/img/vrct_logo_white.png" media="(prefers-color-scheme: dark)">
    <img src="/docs/img/vrct_logo_black.png" alt="VRCT-0 — VRChat Chatbox Translator &amp; Transcription" width="50%">
</picture>

<br>
<br>

[![GitHub release](https://img.shields.io/github/v/release/misyaguziya/VRCT.svg)](https://github.com/misyaguziya/VRCT/releases)
[![Downloads](https://img.shields.io/github/downloads/misyaguziya/VRCT/total)](https://github.com/misyaguziya/VRCT/releases)
[![Licence](https://img.shields.io/github/license/misyaguziya/VRCT)](https://github.com/misyaguziya/VRCT/blob/master/LICENSE)
[![Booth](https://img.shields.io/badge/Store-Booth.pm-red)](https://misyaguziya.booth.pm/items/5155325)
[![Github Sponsors](https://img.shields.io/badge/GitHub%20Sponsors-30363D?&logo=GitHub-Sponsors&logoColor=EA4AAA)](https://github.com/sponsors/misyaguziya)

<h3>
Become a VRCT-0 Supporter on:
</h3>

<a href="https://vrct-dev.fanbox.cc">
    <picture>
        <source srcset="/docs/img/pixiv_fanbox_white.png" media="(prefers-color-scheme: dark)" height="18px">
        <source srcset="/docs/img/pixiv_fanbox_black.png" media="(prefers-color-scheme: light)" height="18px">
        <img src="/docs/img/pixiv_fanbox_black.png" alt="PIXIV FANBOX" height="18px">
    </picture>
</a>&emsp;&nbsp;

<a href="https://patreon.com/vrct_dev">
    <picture>
        <source srcset="/docs/img/patreon_logo_white.png" media="(prefers-color-scheme: dark)" height="22px">
        <source srcset="/docs/img/patreon_logo_black.png" media="(prefers-color-scheme: light)" height="22px">
        <img src="/docs/img/patreon_logo_black.png" alt="Patreon" height="22px">
    </picture>
</a>&emsp;&nbsp;

<br>

<picture>
    <source srcset="/docs/img/supporter_section_border_d.png" media="(prefers-color-scheme: dark)">
    <source srcset="/docs/img/supporter_section_border_l.png" media="(prefers-color-scheme: light)">
    <img src="/docs/img/supporter_section_border_d.png" alt="Supporter Section Border">
</picture>

<br>
<br>

[English](/docs/readmes/README.en.md) · [日本語](/docs/readmes/README.ja.md) · [한국어](/docs/readmes/README.ko.md) · [繁體中文](/docs/readmes/README.zh-Hant.md) · [简体中文](/docs/readmes/README.zh-Hans.md) · [ไทย](/docs/readmes/README.th.md)
<h3>
VRCTは翻訳や文字起こしでVRChatの会話をサポートするソフトウェアです。
</h3>

![](/docs/img/main_window.png)

<div align="left">

# ダウンロード＆インストール
好きな場所からダウンロードしてください。
- [Github.com](https://github.com/misyaguziya/VRCT/releases/)
- [BOOTH.pm](https://misyaguziya.booth.pm/items/5155325)

ダウンロードしてexeを起動するだけです。

# VRCTってなに？
VRCTは話す言語の異なる人同士が会話を行うためにチャットもしくは音声の翻訳を行うことで会話をサポートするソフトウェアです。
これらの機能はVRChat内で使用するために設計されています。
※サポート対象外ですがその他の用途として映画鑑賞等でも使用されています。

VRCTはあなたの会話を以下でサポートをします。
- 💬 **VRChatへのチャット送信機能**
- 🌐 **翻訳機能**
- 🎙 **マイクの文字起こし機能**
- 🔈 **スピーカーの文字起こし機能**

# ドキュメント
初期設定や基本機能、その他の機能についても記載してあります。
- [Documents Link](https://misyaguziya.github.io/VRCT-Docs/)

# 使い方(Youtube)
<div align="center">

[![](https://img.youtube.com/vi/rUTad037n8Q/0.jpg)](https://www.youtube.com/watch?v=rUTad037n8Q)

<div align="left">

## Author
- [みしゃ(misyaguzi)](https://github.com/misyaguziya) (メイン開発)
- [しいな(Shiina_12siy)](https://twitter.com/Shiina_12siy) (UI/UX, UI多言語対応)
- [レラ](https://github.com/soumt-r) (テクニカルサポート)
- [どね](https://twitter.com/done_vrc) (ロゴデザイン)

## テレメトリー（利用統計情報）

VRCTは[Aptabase](https://aptabase.com)を通じて、アプリの改善のために匿名のテレメトリーデータを収集しています。収集されるデータは、起動回数、起動時間、使用機能です。個人を特定できる情報は一切収集されません。

テレメトリーはアプリの設定からいつでも無効化できます。詳細は[Aptabaseプライバシーポリシー](https://aptabase.com/legal/privacy)をご確認ください。

## ネイティブ開発

WindowsバックエンドはRustで動作します。Node/npm、Rust、MSVC/Windows SDK、CMakeを用意して
`npm run dev`、`npm run build`、`npm run release` を使います。資源準備とパッケージ検査はRust
`xtask` が行います。構成と検証の範囲は [native backend](../native_pipeline.md)、
補助ツールは [native tools](../native_tools.md) を参照してください。

## ライセンス

VRCT-0 は [MIT License](../../LICENSE) で公開しています。ただし OCR 機能が使うチャットボックス
検出モデル（`weights/ocr/chatbox_yolox_tiny.onnx`）は例外で、**VRCT-0 専用の
利用許諾**が適用されます。VRCT-0 として、また **VRCT-0 の開発・修正・検証のためのフォークとして**
実行することは自由で、リポジトリをフォークしてモデルを含んだまま持っていて構いません。
できないのは、フォーク独自のリリース版にモデルを同梱すること、VRCT-0 以外のソフトウェアへ
持ち出すこと、単体での再配布、派生モデルの作成です。
条文は [LICENSE.txt](../licenses/chatbox/LICENSE.txt)、範囲は [NOTICE.md](../../NOTICE.md) を
確認してください。このフォークの配布物にはモデルを同梱しません。権限のある外部モデルは
`VRCT_OCR_BUBBLE_MODEL` で指定します。モデルがなくてもアプリはビルド・実行できますが、
OCR開始時に不足を通知します（自前で学習する手順は [学習ガイド](../ocr_yolo_training.md)）。

## Thanks to our contributors
<a href="https://github.com/misyaguziya/VRCT/graphs/contributors" target="_blank">
  <img src="https://contrib.rocks/image?repo=misyaguziya/VRCT" />
</a>

---

VRCT-0 は VRChat によって承認されておらず、VRChat または VRChat の開発もしくは管理に公式に関与する者の見解や意見が反映されたものではありません。VRChat および関連するすべての財産は 米国VRChat, Incの商標または登録商標です。

## Fork credits

VRCT-0 includes rebranding, UI localization, and appearance changes by LogicCuteGuy. Original VRCT developer and contributor credits are retained.
