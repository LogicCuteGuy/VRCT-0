# VRCT-0

[ไทย](vrct-0.th.md) · [Documentation index](README.md)

VRCT-0 builds on VRCT by m's software. Its product name, window title, sidebar logo, startup logo, updater icon, favicon, and installer icons use the VRCT-0 identity. The About page also identifies LogicCuteGuy and retains upstream contributor credits.

## What changed in this fork

VRCT-0 is a fork of [VRCT](https://github.com/misyaguziya/VRCT). It keeps the translation and transcription workflow while changing the desktop implementation and appearance:

- **Native Rust backend:** replaces the Python sidecar and bundled Python runtime, with native settings, translation providers, model management, OSC, and overlays. See [native pipeline](native_pipeline.md).
- **Windows audio controls:** separate microphone and speaker Host/Device selectors, WASAPI input and playback-loopback sources, ASIO capture, and driver control panels. See [Windows audio](windows_audio.md).
- **Native development tools:** capture diagnostics, dataset collection/preparation, annotation, detector training/export, and speech evaluation. See [native tools](native_tools.md).
- **Branding:** VRCT-0 name, new application icons, and light/dark logo variants.
- **Appearance:** persistent Dark, Light, and System themes, plus expanded UI localization.
- **Build and distribution:** Rust `xtask` prepares resources and verifies packages; the updater uses this fork’s releases. The restricted upstream chat-bubble detector is excluded from distributed packages.

## Language

Open **Settings → Appearance → UI Language** and choose your preferred language. Changes apply immediately and are saved in `config.json`. The installer also provides a UI-language selector.

UI language is separate from speech, translation, and OCR language selection. Configure each according to the languages you use and the capabilities of the selected models or providers.

## Theme

Open **Settings → Appearance → Theme**:

| Choice | Behavior |
| --- | --- |
| Dark | Dark surfaces and light text |
| Light | Light surfaces and dark text |
| System | Follows the operating system’s light/dark preference, including live changes |

The default is System. Theme changes apply immediately across the main window, settings, popups, and notification surfaces. The preference is stored in WebView local storage under `vrct-0.theme`, separately from native `config.json`, and persists on this device. Clearing WebView storage resets it to System. If storage is unavailable, the choice works for the current session. An invalid saved value also falls back to System.

Theme changes affect the desktop UI. VR overlays, OBS output, model settings, and conversation contents keep their independent configuration.

## Compatibility and credits

The native executable (`VRCT.exe`), portable archive (`VRCT.zip`), crate names, application identifier, and protocol/settings keys keep their existing technical names for compatibility with the installer and releases. User-facing branding is VRCT-0. The AI-generated wordmark has the subtitle “VRChat Chatbox Translator & Transcription”, with black and white variants selected by the desktop theme. Markdown pages use the corresponding image for the reader’s color scheme. Attribution appears in credits sections rather than the logo or README introduction. Assets can be regenerated from `src-ui/views/assets/vrct-0-icon.svg` with:

```powershell
npm run tauri -- icon src-ui/views/assets/vrct-0-icon.svg --output src-tauri/icons
```

Use [LogicCuteGuy/0-VRCT](https://github.com/LogicCuteGuy/0-VRCT) for fork downloads and issues. Original developer and license attribution remains intact. Historical documents and external upstream stores retain their original identities.

## Checks

```powershell
npm run test:appearance
npm run vite-build
node utils/native_env.js cargo test --manifest-path src-tauri/Cargo.toml -p vrct-core --no-default-features --test settings --test settings_system
```

The appearance checks cover locale key/placeholder parity, theme restoration, live System changes, invalid preferences, unavailable storage, and theme subscriptions. Native settings tests cover Thai persistence and validation. A frontend build does not validate native audio or a compiled NSIS installer.
