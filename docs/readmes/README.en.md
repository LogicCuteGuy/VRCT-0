<div align="center">
<picture>
    <source srcset="/docs/img/vrct_logo_white.png" media="(prefers-color-scheme: dark)">
    <img src="/docs/img/vrct_logo_black.png" alt="VRCT-0 — VRChat Chatbox Translator &amp; Transcription" width="50%">
</picture>

# VRCT-0

VRChat translation and transcription.

[English](/docs/readmes/README.en.md) · [日本語](/docs/readmes/README.ja.md) · [한국어](/docs/readmes/README.ko.md) · [繁體中文](/docs/readmes/README.zh-Hant.md) · [简体中文](/docs/readmes/README.zh-Hans.md) · [ไทย](/docs/readmes/README.th.md)

[Downloads](https://github.com/LogicCuteGuy/0-VRCT/releases) · [Issues](https://github.com/LogicCuteGuy/0-VRCT/issues) · [Documentation](/docs/README.md)
</div>

## What changed in this fork

VRCT-0 is a fork of [VRCT](https://github.com/misyaguziya/VRCT). It keeps the translation and transcription workflow while changing the desktop implementation and appearance:

- **Native Rust backend:** replaces the Python sidecar and bundled Python runtime, with native settings, translation providers, model management, OSC, and overlays. See [native pipeline](/docs/native_pipeline.md).
- **Windows audio controls:** separate microphone and speaker Host/Device selectors, WASAPI input and playback-loopback sources, ASIO capture, and driver control panels. See [Windows audio](/docs/windows_audio.md).
- **Native development tools:** capture diagnostics, dataset collection/preparation, annotation, detector training/export, and speech evaluation. See [native tools](/docs/native_tools.md).
- **Branding:** VRCT-0 name, new application icons, and light/dark logo variants.
- **Appearance:** persistent Dark, Light, and System themes, plus expanded UI localization.
- **Build and distribution:** Rust `xtask` prepares resources and verifies packages; the updater uses this fork’s releases. The restricted upstream chat-bubble detector is excluded from distributed packages.

## Features

- Translate typed messages and send them to the VRChat OSC chatbox.
- Transcribe microphone audio with Voice2Chatbox and speaker audio with Speaker2Log.
- Read VRChat chat bubbles with OCR and display translations in the message log and SteamVR overlay.
- Use local AI models or supported translation providers.
- Switch between **Dark**, **Light**, and **System** themes in **Settings → Appearance**. The choice is saved on this device; System follows Windows appearance changes immediately.

## Install

Get builds from [this fork’s Releases](https://github.com/LogicCuteGuy/0-VRCT/releases). Extract the complete portable package before launching the application. The current portable payload retains the compatibility filename `VRCT.exe` and archive name `VRCT.zip`; its displayed product name is **VRCT-0**. Installer and application icons use the new VRCT-0 mark.

Choose your preferred UI language during installation or in **Settings → Appearance → UI Language**. Theme and language settings are described in the [user guide](/docs/vrct-0.md).

## Develop and build

Requires Windows x64, Node.js/npm, Rust’s MSVC toolchain, Visual Studio C++ Build Tools, the Windows SDK, CMake, and libclang for ASIO. See [build instructions](/docs/readme_build.md) and [Windows audio requirements](/docs/windows_audio.md).

```powershell
npm ci
npm run dev
```

```powershell
npm run build
npm run release
npm run native:verify
```

Frontend-only build and appearance/localization checks:

```powershell
npm run vite-build
npm run test:appearance
```

The native backend and model resources are required for speech, translation, OSC, and VR features. See [native pipeline](/docs/native_pipeline.md) and [native tools](/docs/native_tools.md).

## Credits and license

VRCT-0 is maintained and rebranded by **LogicCuteGuy**, based on [VRCT by m’s software](https://github.com/misyaguziya/VRCT). Original developers, contributors, and translators remain credited in the application’s About page and localized documentation. Upstream donation/store links refer to the original project.

Source is distributed under the [MIT license](/LICENSE). See [NOTICE.md](/NOTICE.md) for third-party terms and the separately licensed chat-bubble detector, which is excluded from fork application and tools packages.
