<div align="center">
<picture>
    <source srcset="/docs/img/vrct_logo_white.png" media="(prefers-color-scheme: dark)">
    <img src="/docs/img/vrct_logo_black.png" alt="VRCT-0 — VRChat Chatbox Translator &amp; Transcription" width="50%">
</picture>

# VRCT-0

用于 VRChat 对话的翻译与语音转写。

[English](/docs/readmes/README.en.md) · [日本語](/docs/readmes/README.ja.md) · [한국어](/docs/readmes/README.ko.md) · [繁體中文](/docs/readmes/README.zh-Hant.md) · [简体中文](/docs/readmes/README.zh-Hans.md) · [ไทย](/docs/readmes/README.th.md)

[下载](https://github.com/LogicCuteGuy/VRCT-0/releases) · [文档](/docs/README.md)
</div>

## 此 fork 的变更

VRCT-0 是基于 [VRCT](https://github.com/misyaguziya/VRCT) 的 fork，保留翻译与语音转写流程，并调整以下部分：

- **原生 Rust 后端：** 取代 Python sidecar 和随应用打包的 Python runtime，使用原生设置、翻译服务、模型管理、OSC 和叠加层。
- **Windows 音频控制：** 麦克风和扬声器各自的 Host/Device 选择、WASAPI 输入与播放回环、ASIO 采集和驱动控制面板。
- **原生开发工具：** 采集诊断、数据集收集与准备、标注、检测器训练与导出、语音转写评估。
- **品牌：** VRCT-0 名称、新应用图标，以及明暗背景的标志版本。
- **外观：** 可保存的深色、浅色、系统主题，以及界面本地化扩展。
- **构建与分发：** 使用 Rust xtask 准备资源并验证软件包，更新来自此 fork 的 Releases；分发包不包含受限制的上游聊天气泡检测模型。

详细信息见 [原生后端](/docs/native_pipeline.md)、[Windows 音频](/docs/windows_audio.md) 和 [原生工具](/docs/native_tools.md)。

## 安装与设置

从此 fork 的 Releases 下载并解压整个软件包，再启动 `VRCT.exe`。安装时或在 **设置 → 外观 → 界面语言** 中选择所需语言，在同一页面选择主题。

便携包名为 `VRCT-0.zip`，可执行文件仍使用 `VRCT.exe`，应用显示名称为 VRCT-0。开发环境及命令见 [构建说明](/docs/readme_build.md)。

## 致谢与许可

此 fork 的品牌、界面本地化和外观变更由 LogicCuteGuy 提供，保留原 VRCT 开发者、贡献者和翻译者的署名。源代码使用 [MIT 许可证](/LICENSE)，第三方组件和模型的条款见 [NOTICE](/NOTICE.md)。
