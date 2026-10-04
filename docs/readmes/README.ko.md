# VRCT-0

[English](/docs/readmes/README.en.md) · [日本語](/docs/readmes/README.ja.md) · [한국어](/docs/readmes/README.ko.md) · [繁體中文](/docs/readmes/README.zh-Hant.md) · [简体中文](/docs/readmes/README.zh-Hans.md) · [ไทย](/docs/readmes/README.th.md)

## 이 포크의 변경 사항

[VRCT](https://github.com/misyaguziya/VRCT)를 기반으로 한 포크입니다. 번역과 음성 인식 흐름을 유지하며 다음을 변경했습니다.

- Python 사이드카를 네이티브 Rust 백엔드로 교체.
- 마이크와 스피커별 Host/Device 선택, WASAPI·ASIO 입력과 드라이버 제어판.
- 캡처, 데이터셋, 주석, 학습 및 평가용 네이티브 도구.
- VRCT-0 이름, 아이콘, 밝은/어두운 로고.
- 저장되는 Dark/Light/System 테마와 UI 현지화 확장.
- Rust xtask 빌드·패키지 검증과 이 포크의 릴리스에서 업데이트.

[Documentation](/docs/README.md) · [Releases](https://github.com/LogicCuteGuy/VRCT-0/releases)

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
VRCT는 음성인식 및 번역 기능을 통해 VRChat의 대화를 지원하는 소프트웨어입니다.
</h3>

![](/docs/img/main_window.png)

<div align="left">

# 다운로드 및 설치
원하는 곳에서 다운로드 할 수 있어요.
- [Github.com](https://github.com/misyaguziya/VRCT/releases/)
- [BOOTH.pm](https://misyaguziya.booth.pm/items/5155325)

다운로드 후 exe를 실행하기만 하면 됩니다.

# VRCT가 뭔가요？
VRCT는 서로 다른 언어를 사용하는 사람들끼리 대화를 할 수 있도록 채팅이나 음성 번역을 통해 대화를 지원하는 소프트웨어에요.
이 기능들은 VRChat 내에서 사용하도록 설계되었어요.
※ 지원 대상에서 제외되지만, 영화 감상 등 다른 용도로도 사용되고 있습니다.

VRCT는 다음과 같이 당신의 대화를 도와드려요.
- 💬 **VRChat으로의 채팅 전송 기능**
- 🌐 **번역 기능**
- 🎙 **마이크 음성인식 기능**
- 🔈 **스피커 음성인식 기능**

# 문서 (일본어)
초기 설정과 기본 기능 및 기타 기능에 대해 설명되어 있어요.
- [Documents Link](https://misyaguziya.github.io/VRCT-Docs/)

# 사용법 (Youtube)
<div align="center">

[![](https://img.youtube.com/vi/rUTad037n8Q/0.jpg)](https://www.youtube.com/watch?v=rUTad037n8Q)

<div align="left">

## Author
- [みしゃ(misyaguzi)](https://github.com/misyaguziya) (주요 개발)
- [しいな(Shiina_12siy)](https://twitter.com/Shiina_12siy) (UI/UX, UI 다국어 지원)
- [レラ](https://github.com/soumt-r) (기술 지원)
- [どね](https://twitter.com/done_vrc) (로고 디자인)

## 텔레메트리 (사용 통계)

VRCT는 [Aptabase](https://aptabase.com)를 통해 앱 개선을 위한 익명 텔레메트리 데이터를 수집합니다. 수집되는 데이터는 앱 시작 횟수, 세션 시간, 기능 사용입니다. 개인 식별 정보는 수집되지 않습니다.

앱 설정에서 언제든지 텔레메트리를 비활성화할 수 있습니다. 자세한 내용은 [Aptabase 개인정보 처리방침](https://aptabase.com/legal/privacy)을 참조하세요.

## 네이티브 개발

Windows 백엔드는 Rust로 실행됩니다. Node/npm, Rust, MSVC/Windows SDK, CMake를 준비한 뒤
`npm run dev`, `npm run build`, `npm run release`를 사용합니다. 리소스 준비와 패키지 검사는
Rust `xtask`가 수행합니다. 구성과 검증 범위는 [native backend](../native_pipeline.md),
보조 도구는 [native tools](../native_tools.md)를 참조하세요.

## 라이선스

VRCT는 [MIT License](../../LICENSE)로 배포됩니다. 단 한 가지 예외가 있습니다. OCR 기능이 사용하는
채팅 말풍선 검출 모델(`weights/ocr/chatbox_yolox_tiny.onnx`)에는 **VRCT-0 전용
라이선스**가 적용됩니다. VRCT로 실행하는 것, 그리고 **VRCT의 개발·수정·검증을 위한 포크에서
실행하는 것**은 자유이며, 저장소를 포크해 모델을 그대로 포함해도 됩니다.
불가능한 것은 포크 자체의 릴리스 빌드에 모델을 동봉하는 것, VRCT-0 외의 소프트웨어로 가져가는 것,
단독 재배포, 파생 모델 제작입니다.
조문은 [LICENSE.en.txt](../licenses/chatbox/LICENSE.en.txt), 범위는
[NOTICE.md](../../NOTICE.md)를 확인하세요. 이 포크의 배포 파일에는 모델을 포함하지 않습니다.
사용 권한이 있는 외부 모델 경로는 `VRCT_OCR_BUBBLE_MODEL`로 지정합니다. 모델 없이도 앱의
빌드와 실행은 가능하지만 OCR을 시작하면 누락 오류를 알립니다
(직접 학습하는 절차는 [학습 가이드](../ocr_yolo_training.md)).

## Thanks to our contributors
<a href="https://github.com/misyaguziya/VRCT/graphs/contributors" target="_blank">
  <img src="https://contrib.rocks/image?repo=misyaguziya/VRCT" />
</a>

---

VRCT는 VRChat의 어떠한 승인도 받지 않았으며, VRChat 또는 VRChat의 개발 또는 관리에 공식적으로 관여하는 사람의 견해나 의견을 반영하지 않습니다. VRChat 및 모든 관련 재산은 미국 VRChat, Inc의 상표 또는 등록상표입니다.

## Fork credits

VRCT-0 includes rebranding, UI localization, and appearance changes by LogicCuteGuy. Original VRCT developer and contributor credits are retained.
