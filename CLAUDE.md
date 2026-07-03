# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

实时语音翻译桌面应用:捕获系统音频(WASAPI 环回)或麦克风 → 进程内 sherpa-onnx 识别 → 云端/本地翻译 → 双语字幕 + 可选悬浮窗 + 可选 TTS 语音回灌。**隐私**:音频不出本机,只有识别后的文本发往翻译 API。技术栈 **Tauri 2**(Rust 后端)+ **React 19 + TypeScript**(Vite)。Windows 端到端可用,macOS/Linux 采集代码已补全待真机验证。

> 本仓库另有更细的背景文档,深挖前先读:[AGENTS.md](AGENTS.md)、[README.md](README.md)、[语音翻译回灌会议-设计方案.md](语音翻译回灌会议-设计方案.md)、[GPU加速与macOS兼容-设计方案.md](GPU加速与macOS兼容-设计方案.md)。
> ⚠️ [.github/instructions/rust-backend.instructions.md](.github/instructions/rust-backend.instructions.md) 部分内容**已过时**(提到的 "whisper sidecar / HTTP :8178" 已不存在,现为进程内 sherpa-onnx)——以本文件和实际代码为准。

## 常用命令

```powershell
npm install                  # 安装前端依赖
npm run tauri dev            # 开发:前端 HMR + Rust 热重建(完整运行应用)
npm run tauri build          # 生产打包 → src-tauri/target/release/bundle/(NSIS .exe + MSI)

# 快速校验(不启动应用,改完优先跑这两个)
npm run build                # 前端 TS 类型检查(tsc)+ Vite 打包
cd src-tauri; cargo check    # 仅 Rust 编译检查
```

CI([.github/workflows/ci.yml](.github/workflows/ci.yml))在 Win/macOS/Linux 三平台跑 `tsc --noEmit` + `cargo check --all-targets` + `tauri build`。无单元测试套件,验证靠 `cargo check`/`npm run build` + 手动运行。

## 架构:双向并发流水线

核心在 [pipeline.rs](src-tauri/src/pipeline.rs)。`pipeline::start(app, recognizer, tts, direction, cfg)` 启动**一条**流水线,内部是若干线程经**有界 mpsc** 串联;[lib.rs](src-tauri/src/lib.rs) 的 `start_translation` 据配置最多并发跑**两条方向相反的流水线**:

```
入站 "in"  (听对方→中文字幕): loopback 采集 → 降噪 → 重采样48k→16k → VAD分段
   │[seg_tx,bounded=6]  → ASR识别 → 原文(pending) │[txt_tx,bounded=32] → 翻译worker×2 → 回填译文 → 写存档
出站 "out" (你说话→译出语回灌): microphone 采集 → …同上… → 翻译 → │[tts_tx,bounded=8] → Kokoro合成 → 串行播放到 CABLE Input
```

**关键设计**:
- **id 回填**:全局单调递增 `NEXT_SUBTITLE_ID`(跨重启不重置)。先发 `{id, original, pending:true}` 让前端立即显示原文,翻译完按**同一 id** 回填 `{id, translated, pending:false}`。段 N+1 的识别与段 N 的翻译并行 → 实时。
- **有界通道 + 溢出丢弃**:通道满用 `try_send` 丢弃并发提示(如 `[翻译积压,已跳过]`),**绝不**改无界堆积。
- **回灌(双向)防回环**:开启回灌(`tts_enabled && translation_engine != "none"`)时,入站强制 `source="loopback"`、出站强制 `source="microphone"`;TTS 只灌 CABLE Input,绝不进默认输出,否则入站环回会把你的 TTS 又转一遍。会议软件麦克风须选 CABLE Output。详见[设计方案 §6B](语音翻译回灌会议-设计方案.md)。
- **翻译并发**:`NUM_TRANSLATORS=2` 个 worker 抢锁消费同一 `txt_rx`,HTTP 在锁外执行真并发;喂 LLM 的上下文取最近 `CONTEXT_USE=2` 条完成译文(失败译文不进上下文)。
- **热切目标语言**:`set_target_lang` 写 `live_target` 共享槽,翻译 worker 每段读一次,不重启流水线。

## 后端模块地图(`src-tauri/src/`)

| 文件 | 职责 |
|------|------|
| [lib.rs](src-tauri/src/lib.rs) | 所有 Tauri 命令 + `AppState`(双流水线 + 双识别器,`Mutex` 保护)+ 启动清理过期存档 + 主窗关闭=干净退出 |
| [pipeline.rs](src-tauri/src/pipeline.rs) | 单条流水线编排器(采集/识别/翻译/可选TTS线程),发 `subtitle`/`status` 事件 |
| [asr.rs](src-tauri/src/asr.rs) | `Asr` trait(`transcribe`/`fingerprint`/`reset_session`)+ 两实现:SenseVoice(CTC,快)/ Qwen3(LLM解码,准);进程内静态链接 sherpa-onnx,**无 sidecar/HTTP**;模型按需下载;幻觉/套话/模板泄漏熔断 |
| [tts.rs](src-tauri/src/tts.rs) | Kokoro v1.0 多语进程内合成(复用同一份 sherpa,不引第二份 onnxruntime);模型 ~310MB 按需下载 |
| [playback.rs](src-tauri/src/playback.rs) | WASAPI render 到输出设备(24k→48k 升采样、单声道→双声道);枚举设备 + 自动找 CABLE Input |
| [capture.rs](src-tauri/src/capture.rs) | Windows WASAPI 环回/麦克风采集;平台无关处理抽成 `Processor`;macOS(ScreenCaptureKit)/Linux(cpal)代码已补全待验证 |
| [denoise.rs](src-tauri/src/denoise.rs) | RNNoise 降噪(480样本/帧@48k)+ /3 FIR 重采样 48k→16k |
| [segmenter.rs](src-tauri/src/segmenter.rs) | `Vad` trait:默认 `SileroCore`(sherpa Silero VAD)/ 回退 `EnergyVad`(零依赖);`SegLimits::for_engine` 按引擎自适应段长(SenseVoice 短/Qwen3 长) |
| [translate.rs](src-tauri/src/translate.rs) | 多引擎:openai 兼容 / ollama / google(免费非官方)/ none;含 `summarize` 会话重点提炼(超预算自动 map-reduce 分块);whatlang 兜底判译文是否还是原语种 |
| [transcript.rs](src-tauri/src/transcript.rs) | 会话译文落盘(`<stem>.jsonl`)+ 摘要(`<stem>.summary.<lang>.md`)+ 历史列举 + 过期清理 |
| [config.rs](src-tauri/src/config.rs) | `RuntimeConfig`(serde camelCase),含语言名/TTS espeak 语种码映射 |

## 前端模块地图(`src/`)

| 文件 | 职责 |
|------|------|
| [App.tsx](src/App.tsx) | 主窗控制器:顶栏、字幕、设置、历史、摘要;`listen` 所有 Rust 事件,按 id 回填字幕 |
| [components/SettingsPanel.tsx](src/components/SettingsPanel.tsx) | 语言/引擎/降噪/VAD/回灌设备等设置表单(条件字段) |
| [components/SubtitleView.tsx](src/components/SubtitleView.tsx) | 字幕虚拟滚动列表(@tanstack/react-virtual),按 direction 区分入/出站 |
| [components/Overlay.tsx](src/components/Overlay.tsx) | 悬浮字幕窗,独立 Vite 入口 [overlay-main.tsx](src/overlay-main.tsx),`data-tauri-drag-region` 拖动 |
| [components/SummaryPanel.tsx](src/components/SummaryPanel.tsx) | 会话重点提炼(流式 `summary` 事件,可编辑/重提炼) |
| [components/HistoryPanel.tsx](src/components/HistoryPanel.tsx) | 历史会话回看(只读) |
| [types.ts](src/types.ts) | 共享类型、`DEFAULT_SETTINGS`、`loadSettings()`/`saveSettings()` |

## 关键约定

- **camelCase 三处对齐**:改后端 `RuntimeConfig` 字段 → **必须**同步改 [types.ts](src/types.ts) 的 `Settings` + `DEFAULT_SETTINGS`,否则 IPC 静默错位。
- **事件驱动,不轮询**:后端只 `emit`,前端 `listen<T>(...)`。字幕用 `findIndex` 命中 id 则替换、否则追加;前端只保留最近 200 条。
- **设置持久化**:onChange → `saveSettings`(fire-and-forget,Rust 同步写 `%APPDATA%\...\settings.json`);用 `userTouched` ref 防异步 `loadSettings()` 覆盖用户输入,改加载逻辑时保留此保护。
- **错误处理**:Rust 全程 `anyhow::Result<T>` + `?`,`bail!("中文原因")` 主动报错;ASR/翻译失败 `emit_status("error",...)` 后 `continue`,不中断流水线。面向用户文案中文,保留原始 error 供调试。
- **React**:函数式组件 + hooks,无 Redux/状态库。
- 代码注释用**中文**(贴近现有风格)。

## Tauri 命令、事件与约定错误码

命令(全在 [lib.rs](src-tauri/src/lib.rs) 的 `invoke_handler`):`start_translation` / `stop_translation` / `set_target_lang` / `toggle_overlay` / `model_exists` / `download_model` / `tts_model_exists` / `download_tts_model` / `list_output_devices` / `cable_exists` / `load_settings` / `save_settings` / `summarize_session` / `open_summary_dir` / `load_summary` / `save_summary` / `retranslate` / `list_sessions` / `load_session`。

事件:`subtitle`、`status`、`model-progress`、`tts-progress`、`summary`。

`start_translation` 用 `Err(String)` 作**信号码**让前端触发对应引导流程,**不要当普通错误**直接弹给用户:
- `MODEL_MISSING` → 触发 ASR 模型下载
- `TTS_MODEL_MISSING` → 触发 Kokoro 下载
- `CABLE_MISSING` → 提示装 VB-Cable
- `RELAY_LANG_UNSUPPORTED` → SenseVoice 只认 zh/en/ja/ko/yue,回灌出站源语言越界(改用 Qwen3 或换目标语言)

## 模型与外部依赖

- **识别模型按需下载**到 `%APPDATA%\com.seastar.voicetranslator\models\`(带进度),不打包:SenseVoice int8 ≈237MB / Qwen3-ASR ≈940MB / Kokoro TTS ≈310MB。下载用 `tar.bz2` + 系统 `tar` 解压(需 Win10+ 自带 tar)。
- **随仓库打包**:`src-tauri/sidecar/models/silero_vad.onnx`(≈2.2MB,VAD 用)。
- **回灌依赖 VB-Cable**(用户自装的虚拟声卡):app 把 TTS render 到 `CABLE Input`,会议软件麦克风选 `CABLE Output`。
- 权限在 [capabilities/default.json](src-tauri/capabilities/default.json),窗口配置在 [tauri.conf.json](src-tauri/tauri.conf.json)(主窗 `main` + 悬浮窗 `overlay`)。

## 工具链

- Node.js 18+、Rust stable(**MSVC** 工具链)、Windows 11 + WebView2(内置)。
- ASR/TTS 当前纯 CPU 推理(`num_threads=4`),无 GPU 依赖(GPU 化见设计文档,属未来工作)。
