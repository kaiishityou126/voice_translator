# 语音翻译 · Voice Translator

实时语音翻译桌面应用：捕获**电脑系统音频**（或麦克风）→ 本地语音识别 → 云端翻译 → 双栏字幕实时显示。中 / 英 / 日 三语互译。

> **Windows 端到端可用**：系统音频→本地识别→翻译→双栏字幕 + 悬浮窗。已含 RNNoise 降噪、Silero VAD、5 种翻译引擎（含免 key）、模型按需下载、应用图标、安装包。macOS（ScreenCaptureKit 系统音频 + 麦克风）与 Linux（PulseAudio/PipeWire monitor + 麦克风）采集代码已补全，待真机验证；代码签名需自备证书——见下方路线图。

## 架构

```
系统音频(WASAPI 环回) ──► 16k 单声道(WASAPI autoconvert)
                              │
                              ▼
                      能量 VAD 切段
                              │
                              ▼
            whisper.cpp (whisper-server sidecar, 本地)
                              │ 识别文本
                              ▼
              云 LLM (OpenAI 兼容 /chat/completions)
                              │ 译文
                              ▼
                  Tauri event ──► React 双栏字幕
```

- **框架**：Tauri 2.x（Rust 后端 + React/TS 前端，WebView2）
- **采集**：Windows 用 `wasapi` 默认 Render 设备 loopback（48kHz，autoconvert）；静音缓冲(SILENT)清零防污染。平台无关处理(降噪→抽取→切段)抽成 `Processor` 共享；macOS 为 ScreenCaptureKit/cpal 脚手架（见 [capture.rs](src-tauri/src/capture.rs)，待在 Mac 上完成）
- **降噪**：可开关。RNNoise（[nnnoiseless](src-tauri/src/denoise.rs)）在 48kHz 降噪 → 自写 /3 FIR 抽取到 16kHz 喂 whisper
- **VAD**：能量门限切段（[segmenter.rs](src-tauri/src/segmenter.rs)）
- **ASR**：whisper.cpp `whisper-server` 常驻 sidecar，模型 `ggml-small-q5_1`（多语种，离线，音频不出本机）；**后端自动选择** CUDA(GPU) > OpenBLAS > CPU；源语言可固定(中/英/日)或自动检测；`--suppress-nst` 抑制幻觉
- **流水线**：识别(本地)与翻译(远程)**两级并行**——原文识别出即刻显示，译文随后回填同一条字幕（[pipeline.rs](src-tauri/src/pipeline.rs)）
- **翻译**：多引擎可选（[translate.rs](src-tauri/src/translate.rs)）——**OpenAI 兼容**(需 key，可指向 OpenAI/DeepSeek/通义/Kimi) / **Ollama**(本地，无 key，需装 Ollama 并拉模型) / **Google 免费**(非官方 gtx 端点，无 key) / **DeepL**(免费 key，:fx 自动走 free 端点) / **纯字幕**(不翻译，无 key，只出原文)
- **隐私**：识别全程本地，仅识别出的**文本**发往翻译接口

## 运行（开发）

前置：Node 18+、Rust(stable-msvc)、WebView2（Win11 自带）。

首次需准备 whisper sidecar **二进制**（已 gitignore，~15MB）。**模型不用手动下**——首次「开始」时按所选档位自动下载到 app 数据目录（`%APPDATA%\com.administrator.voicetranslator\models`），带进度条。

```powershell
$base = "src-tauri\sidecar"
New-Item -ItemType Directory -Force "$base\whisper-blas" | Out-Null
# whisper.cpp OpenBLAS 构建（含 whisper-server.exe + ggml/openblas DLL），选择器优先用它
Invoke-WebRequest "https://github.com/ggml-org/whisper.cpp/releases/download/v1.8.6/whisper-blas-bin-x64.zip" -OutFile "$env:TEMP\w.zip"
Expand-Archive "$env:TEMP\w.zip" "$base\whisper-blas" -Force
```

然后：

```powershell
npm install
npm run tauri dev
```

首次「开始」：若该档位模型未下载会先下载（进度条），再拉起 whisper-server。在设置里选翻译引擎：云 API 填 key；想免 key 就选 **Google 免费** 或 **纯字幕**。

whisper **二进制**作为 Tauri resource 打包分发；**模型**按需下载到 app 数据目录，不进安装包（装包仅 ~15MB）。

## 关键设计点

- **loopback 实现**：打开默认 **Render** 设备后，以 `Direction::Capture` 初始化 → WASAPI 置 `AUDCLNT_STREAMFLAGS_LOOPBACK`。这是 wasapi 0.23 的环回方式。
- **whisper 作 sidecar 而非进程内编译**：避免引入 CMake / libclang（whisper-rs 的编译依赖），端到端更快落地；模型常驻 server 内存，免去每段重载延迟。
- **两级并行流水线**：采集→切段→`seg` channel→识别线程→(原文先 emit)→`txt` channel→翻译线程→回填。段 N+1 的本地识别与段 N 的远程翻译并行，远程翻译不再阻塞识别；每段用自增 id，前端按 id 回填，顺序仍有保证。

## 后端与加速

[asr.rs](src-tauri/src/asr.rs) 的 `pick_backend` 启动时按优先级探测可用构建，存在即用：

| 优先级 | 目录 | 条件 |
|---|---|---|
| 1. CUDA(GPU) | `sidecar/whisper-cuda/Release/` | 检测到 NVIDIA 驱动(`nvcuda.dll`) |
| 2. OpenBLAS | `sidecar/whisper-blas/Release/` | 存在即用（CPU 上比纯 CPU 略快） |
| 3. 纯 CPU | `sidecar/whisper/Release/` | 兜底 |

**启用 GPU（NVIDIA 用户，识别快 ~5–10×）**：下载对应 CUDA 版本的 cublas 构建解压到 `sidecar/whisper-cuda/` 即可，重启后自动识别。

```powershell
# 按驱动支持的 CUDA 版本二选一（12.x 驱动用 12.4，老驱动用 11.8）
Invoke-WebRequest "https://github.com/ggml-org/whisper.cpp/releases/download/v1.8.6/whisper-cublas-12.4.0-bin-x64.zip" -OutFile "$env:TEMP\cu.zip"
Expand-Archive "$env:TEMP\cu.zip" "src-tauri\sidecar\whisper-cuda" -Force
```

**模型档位**：设置页可切 `base(~57MB) / small(~181MB) / medium(~539MB)`（换档自动重启 server）。任一档位**首次使用时自动下载**到 app 数据目录（`download_model` 命令 + `model-progress` 进度事件，[asr.rs](src-tauri/src/asr.rs)），无需手动下。

## 打包 / 发布

```powershell
npm run tauri build   # 产物在 src-tauri/target/release/bundle/（NSIS .exe + MSI）
```

- 模型不打包，安装包仅含 whisper 二进制 + 前端（~15MB）；用户首次运行按需下载模型。
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
  - 准确：RNNoise 降噪（48k 采集→降噪→16k，可开关）；**Silero VAD** 语音切分（`voice_activity_detector` crate，模型内嵌，可开关，失败回退能量门限）；源语言可固定（中/英/日，短片段比 auto 准）；静音缓冲清零；`--suppress-nst` 抑制幻觉；模型档位 base/small/medium 可切换（换档自动重启 server）
  - 速度：识别/翻译两级并行（原文先出、译文回填）；后端自动选择 CUDA(GPU) > OpenBLAS > CPU；段长 8s / 静音阈值 500ms 收紧延迟
  - 鲁棒：播放停止时兜底收尾半句（不卡半句）；采集→识别→翻译全程有界队列，过载时丢最旧音频 / 标记翻译跳过，保证实时不退化
  - 翻译引擎多选：OpenAI 兼容 / 本地 Ollama(无 key) / Google 免费(无 key) / DeepL / 纯字幕(无 key)，免 key 也能用
  - 体验：悬浮字幕窗（独立透明置顶窗口，实时显示最新原文+译文，可拖动，主窗口一键开关）；历史导出为 txt（带时间戳的原文+译文）
  - 打包：应用图标（「译」字）；模型**按需下载**到 app 数据目录（带进度），安装包仅 ~15MB；`tauri build` 出 Windows 安装包（NSIS/MSI）
- **P2（待做）** DeepFilterNet3 深度降噪（更强，替 RNNoise）；SRT 带时轴字幕导出；多说话人分离
- **P3** macOS 采集（已搭好 cfg 隔离脚手架 + 共享 Processor，需在 Mac 上接 ScreenCaptureKit/cpal + 屏幕录制权限引导）；代码签名（需自备证书）+ macOS 打包
