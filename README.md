# 语音翻译 · Voice Translator

实时语音翻译桌面应用：捕获**电脑系统音频**（或麦克风）→ 本地语音识别 → 翻译 → 双栏字幕实时显示。中 / 英 / 日 三语互译。

> **Windows 端到端可用**：系统音频→本地识别→翻译→双栏字幕 + 悬浮窗。已含 RNNoise 降噪、Silero VAD、多种翻译引擎（含免 key）、模型按需下载、会话重点提炼、应用图标、安装包。macOS（ScreenCaptureKit 系统音频 + 麦克风）与 Linux（PulseAudio/PipeWire monitor + 麦克风）采集代码已补全，待真机验证；代码签名需自备证书——见下方路线图。

> **隐私**：音频全程不出本机，只有识别出的**文本**发往翻译接口。

## 架构

```
系统音频(WASAPI 环回, 48k) ──► 降噪(RNNoise, 可选) ──► /3 重采样 48k→16k
                                                          │
                                                          ▼
                                          VAD 切段(Silero / 能量门限)
                                                          │ [seg channel, 有界]
                                                          ▼
                              sherpa-onnx 进程内识别(SenseVoice / Qwen3-ASR, 纯 CPU)
                                                          │ 原文先 emit [txt channel, 有界]
                                                          ▼
                              翻译(OpenAI 兼容 / Ollama / Google 免费 / 纯字幕)
                                                          │ 译文回填同一 id
                                                          ▼
                                          Tauri event ──► React 双栏字幕 + 悬浮窗
```

- **框架**：Tauri 2.x（Rust 后端 + React 19 / TS 前端，WebView2）
- **采集**：Windows 用 `wasapi` 默认 Render 设备 loopback（48kHz，autoconvert）；静音缓冲(SILENT)清零防污染。平台无关处理（降噪→抽取→切段）抽成 `Processor` 共享；macOS / Linux 采集代码已补全但待真机验证（见 [capture.rs](src-tauri/src/capture.rs)）
- **降噪**：可开关。RNNoise（[nnnoiseless](src-tauri/src/denoise.rs)）在 48kHz 降噪 → 自写 /3 FIR 抽取到 16kHz 喂识别引擎
- **VAD**：默认 **Silero VAD**（sherpa-onnx 内置，ML 切分更准）；可关闭回退到零依赖的能量门限（[segmenter.rs](src-tauri/src/segmenter.rs)）。分段长度按识别引擎自适应（SenseVoice 偏短低延迟、Qwen3 偏长保上下文）
- **ASR**：**sherpa-onnx 静态链接、进程内离线识别**（无 sidecar、无 HTTP，音频不出本机），纯 CPU 推理。两种引擎可选（[asr.rs](src-tauri/src/asr.rs)）：
  - **SenseVoice**（默认，快）：CTC 模型，延迟低、模型小（int8，约 240MB），支持 zh/en/ja/ko/yue；日常字幕够用，专名准确度一般
  - **Qwen3-ASR**（精准）：LLM 解码，延迟略高、模型大（约 940MB）；专名/同音词更准
- **流水线**：采集→识别→翻译**三线程并行**，有界 mpsc 通道串联——原文识别出即刻显示，译文随后回填同一条字幕（[pipeline.rs](src-tauri/src/pipeline.rs)）
- **翻译**：多引擎可选（[translate.rs](src-tauri/src/translate.rs)）——**OpenAI 兼容**(需 key，可指向 OpenAI/DeepSeek/通义/Kimi) / **Ollama**(本地，无 key，需装 Ollama 并拉模型) / **Google 免费**(非官方 gtx 端点，无 key) / **纯字幕**(不翻译，无 key，只出原文)
- **会话重点**：一键把本次会话译文用 LLM 提炼成结构化重点（[translate.rs](src-tauri/src/translate.rs) `summarize`），支持手动编辑与重新提炼

## 运行（开发）

前置：Node 18+、Rust(stable-msvc)、WebView2（Win11 自带）。

**识别模型不用手动下**——首次「开始」时按所选引擎自动下载到 app 数据目录（`%APPDATA%\com.administrator.voicetranslator\models`），带进度条。仓库内已含 VAD 所需的 `src-tauri/sidecar/models/silero_vad.onnx`（约 2.2MB，随安装包打包）。

```powershell
npm install
npm run tauri dev
```

首次「开始」：若所选引擎的模型未下载会先下载（进度条，SenseVoice ≈240MB / Qwen3 ≈940MB），再加载识别器。在设置里选翻译引擎：云 API 填 key；想免 key 就选 **Google 免费** 或 **纯字幕**。

## 关键设计点

- **loopback 实现**：打开默认 **Render** 设备后，以 `Direction::Capture` 初始化 → WASAPI 置 `AUDCLNT_STREAMFLAGS_LOOPBACK`。这是 wasapi 0.23 的环回方式。
- **sherpa-onnx 进程内、静态链接**：识别器常驻进程内存，无 sidecar 进程 / HTTP，免去每段重载与跨进程开销；模型静态链接库随程序分发，运行期无需额外 DLL。
- **三线程并行流水线**：采集→切段→`seg` channel→识别线程→(原文先 emit)→`txt` channel→翻译线程→回填。段 N+1 的识别与段 N 的翻译并行，翻译不阻塞识别；每段用自增 id，前端按 id 回填，顺序仍有保证。通道有界，过载时丢最旧音频 / 标记翻译跳过，保证实时不退化。
- **分段长度按引擎自适应**：SenseVoice 成本平、长片段反而掉点 → 用短段降延迟；Qwen3 需要上下文降低幻觉 → 用长段（[segmenter.rs](src-tauri/src/segmenter.rs) `SegLimits::for_engine`）。

## 模型与下载

识别模型**首次使用时自动下载**到 app 数据目录（`download_model` 命令 + `model-progress` 进度事件，[asr.rs](src-tauri/src/asr.rs)），无需手动下：

| 引擎 | 模型 | 体积 | 说明 |
|---|---|---|---|
| SenseVoice（默认） | `sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09` (int8) | ≈237MB | CTC，快，支持 zh/en/ja/ko/yue |
| Qwen3-ASR | Qwen3-ASR-0.6B (int8) | ≈940MB | LLM 解码，专名更准，延迟略高 |

VAD 模型 `silero_vad.onnx`（约 2.2MB）已随仓库与安装包打包，无需下载。

## 打包 / 发布

```powershell
npm run tauri build   # 产物在 src-tauri/target/release/bundle/（NSIS .exe + MSI）
```

- 识别模型不打包，安装包仅含程序 + VAD 模型 + 前端；用户首次运行按需下载识别模型。
- **代码签名**：需自备代码签名证书。Windows 在 `tauri.conf.json` 的 `bundle.windows.certificateThumbprint`（+ `signCommand` 或 `timestampUrl`）配置；无证书则产物为**未签名**安装包（用户安装时 SmartScreen 会警告）。

## 安装时的安全提示（未签名说明）

本项目是**免费开源软件，未购买代码签名证书**，因此各平台首次安装会弹安全提示。这与软件本身是否安全无关——操作系统对任何未签名应用都会这样提示。绕过方法如下：

**Windows（SmartScreen）**

双击安装包若出现「Windows 已保护你的电脑 / Windows protected your PC」：

1. 点蓝字 **更多信息 / More info**
2. 再点 **仍要运行 / Run anyway**

**macOS（Gatekeeper）**

DMG 未签名 / 未公证，安装后首次打开可能提示「无法打开，因为无法验证开发者」。任选其一：

- **右键点 App → 打开 → 在弹窗里再点「打开」**（推荐，最简单）；或
- 终端去掉隔离属性后再打开：

  ```bash
  xattr -dr com.apple.quarantine "/Applications/语音翻译.app"
  ```

**Linux**

`.AppImage` 下载后需加可执行权限：`chmod +x 语音翻译*.AppImage`，再双击或命令行运行；`.deb` 用 `sudo dpkg -i 语音翻译*.deb` 安装。

> 想彻底消除 Windows 警告，开源项目可申请 [SignPath Foundation](https://signpath.org/) 的**免费**代码签名；macOS 公证需 Apple 开发者账号（$99/年）。详见路线图。

## 路线图

- **P1（已完成）** Windows 端到端：系统音频 → 识别 → 翻译 → 字幕
- **准确/速度优化（已完成）**
  - 准确：RNNoise 降噪（48k 采集→降噪→16k，可开关）；**Silero VAD** 语音切分（sherpa-onnx 内置，可开关，失败回退能量门限）；源语言可固定（中/英/日/韩/粤，短片段比 auto 准）；静音缓冲清零；幻觉与套话熔断；两种识别引擎（SenseVoice 快 / Qwen3 精准）可切换
  - 速度：采集/识别/翻译三线程并行（原文先出、译文回填）；纯 CPU 推理（SenseVoice int8，`num_threads=4`）；分段长度按引擎自适应收紧延迟
  - 鲁棒：播放停止时兜底收尾半句（不卡半句）；采集→识别→翻译全程有界队列，过载时丢最旧音频 / 标记翻译跳过，保证实时不退化
  - 翻译引擎多选：OpenAI 兼容 / 本地 Ollama(无 key) / Google 免费(无 key) / 纯字幕(无 key)，免 key 也能用
  - 体验：悬浮字幕窗（独立透明置顶窗口，实时显示最新原文+译文，可拖动，主窗口一键开关）；会话重点一键提炼（LLM，可编辑/重提炼）；历史导出为 txt（带时间戳的原文+译文）
  - 打包：应用图标（「译」字）；识别模型**按需下载**到 app 数据目录（带进度），安装包仅含程序 + VAD 模型；`tauri build` 出 Windows 安装包（NSIS/MSI）
- **P2（待做）** DeepFilterNet3 深度降噪（更强，替 RNNoise）；SRT 带时轴字幕导出；多说话人分离
- **P3** macOS / Linux 采集真机验证（已搭好 cfg 隔离脚手架 + 共享 Processor，需接 ScreenCaptureKit / PulseAudio + 权限引导）；代码签名（需自备证书）+ macOS 打包
