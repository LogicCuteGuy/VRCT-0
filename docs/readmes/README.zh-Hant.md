# VRCT-0

[English](/docs/readmes/README.en.md) · [日本語](/docs/readmes/README.ja.md) · [한국어](/docs/readmes/README.ko.md) · [繁體中文](/docs/readmes/README.zh-Hant.md) · [简体中文](/docs/readmes/README.zh-Hans.md) · [ไทย](/docs/readmes/README.th.md)

## 此 fork 的變更

這是基於 [VRCT](https://github.com/misyaguziya/VRCT) 的 fork，保留翻譯與語音辨識流程，並調整以下部分。

- 以原生 Rust 後端取代 Python sidecar。
- 麥克風與喇叭各自的 Host/Device 選擇、WASAPI／ASIO 輸入與驅動控制面板。
- 擷取、資料集、標註、訓練與評估用的原生工具。
- VRCT-0 名稱、圖示與明暗標誌。
- 可儲存的深色／淺色／系統主題與 UI 在地化擴充。
- Rust xtask 建置與套件驗證，並從此 fork 的發行版本更新。

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
VRCT-0 是一個支援 VRChat 對話翻譯和紀錄的軟體。
</h3>

![](/docs/img/main_window.png)

<div align="left">

# 下載 & 安裝
你可以從這些地方下載 VRCT-0：
- [Github.com](https://github.com/misyaguziya/VRCT/releases/)
- [BOOTH.pm](https://misyaguziya.booth.pm/items/5155325)

你只需要下載並啟動 exe 文件。

# 什麼是 VRCT-0？
VRCT-0 是一種透過提供聊天或語音翻譯來幫助語言不通的人對話的軟體。
這些功能專為在 VRChat 中使用而設計，但你也可以拿來看電影。

VRCT-0 可以：
- 💬 **傳送訊息至遊戲內 Chatbox**
- 🌐 **自動翻譯**
- 🎙 **麥克風轉文字**
- 🔈 **喇叭轉文字**

# 文件
解釋了初始設定、基本功能以及其他功能。
- [Documents Link](https://misyaguziya.github.io/VRCT-Docs/)

# YouTube 教學（日語、英文字幕）
<div align="center">

[![](https://img.youtube.com/vi/rUTad037n8Q/0.jpg)](https://www.youtube.com/watch?v=rUTad037n8Q)

<div align="left">

## 作者
- [みしゃ(misyaguzi)](https://github.com/misyaguziya) (主要開發)
- [しいな(Shiina_12siy)](https://twitter.com/Shiina_12siy) (UI/UX, UI 多語系支援)
- [レラ](https://github.com/soumt-r) (技術支援)
- [どね](https://twitter.com/done_vrc) (Logo 設計)

## 遙測（使用統計）

VRCT-0 透過 [Aptabase](https://aptabase.com) 收集匿名遙測資料以協助改善應用程式。收集的資料包括應用程式啟動次數、使用時長、功能使用。不會收集任何可識別個人身份的資訊。

您可以隨時在應用程式設定中停用遙測。詳情請參閱 [Aptabase 隱私權政策](https://aptabase.com/legal/privacy)。

## 原生開發

Windows 後端以 Rust 執行。準備 Node/npm、Rust、MSVC/Windows SDK、CMake 後，使用
`npm run dev`、`npm run build`、`npm run release`。資源準備與封裝檢查由 Rust `xtask` 執行。
架構及驗證範圍請參閱 [native backend](../native_pipeline.md)，
輔助工具請參閱 [native tools](../native_tools.md)。

## 授權條款

VRCT-0 以 [MIT License](../../LICENSE) 發布，但有一項例外：OCR 功能所使用的聊天氣泡偵測模型
（`weights/ocr/chatbox_yolox_tiny.onnx`）適用 **VRCT-0 專用授權**。
您可以將它作為 VRCT-0 的一部分執行，也可以在為了開發、修正或驗證 VRCT-0 而建立的分支中執行；
分叉儲存庫並保留模型檔案沒有問題。不可以的是：把模型放進分支自己的發行版、用於 VRCT-0 以外的
軟體、單獨再散布，或製作衍生模型。
條款請見 [LICENSE.en.txt](../licenses/chatbox/LICENSE.en.txt)，適用範圍請見
[NOTICE.md](../../NOTICE.md)。本分支的發行檔不包含此模型。可透過 `VRCT_OCR_BUBBLE_MODEL`
指定已獲授權的外部模型路徑。沒有模型仍可建置及執行應用程式；啟動 OCR 時會回報缺少模型
（自行訓練的步驟見 [訓練指南](../ocr_yolo_training.md)）。

## Thanks to our contributors
<a href="https://github.com/misyaguziya/VRCT/graphs/contributors" target="_blank">
  <img src="https://contrib.rocks/image?repo=misyaguziya/VRCT" />
</a>

---

VRCT-0 未得到 VRChat 的認可，也不反映 VRChat 或正式參與製作或管理 VRChat 財產的任何人的觀點或意見。VRChat 和所有相關財產均為 VRChat Inc. 的商標或註冊商標。

## Fork credits

VRCT-0 includes rebranding, UI localization, and appearance changes by LogicCuteGuy. Original VRCT developer and contributor credits are retained.
