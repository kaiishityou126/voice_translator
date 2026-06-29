# Voice Translator — AI 代理指南

实时语音翻译桌面应用:捕获系统音频(WASAPI 环回)或麦克风 → 本地 sherpa-onnx SenseVoice 识别 → 云端/本地翻译 → 双语字幕 + 可选悬浮窗。**隐私**:音频不出本机,只有识别后的文本发往翻译 API。

技术栈:**Tauri 2**(Rust 后端)+ **React 19 + TypeScript**(前端,Vite 构建)。当前 Windows 端到端可用,macOS 仅有占位代码。

详细安装与使用见 [README.md](README.md)。

## 构建与开发命令

```powershell
npm install                  # 安装前端依赖
npm run tauri dev            # 开发:前端 HMR + Rust 热重建
npm run tauri build          # 生产打包 → src-tauri/target/release/bundle/

# 快速校验(不启动应用)
npm run build                # 仅前端 TypeScript 类型检查 + 打包
cd src-tauri; cargo check    # 仅 Rust 编译检查
```

**识别引擎**:sherpa-onnx 静态链接进程内运行(无 sidecar、无 HTTP)。SenseVoice int8 模型(`sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2025-09-09`,约 237MB,支持 zh/en/ja/ko/yue)不打包,首次使用按需下载到 `%APPDATA%\com.administrator.voicetranslator\models\`。另需 `src-tauri/sidecar/models/silero_vad.onnx` 用于 VAD 分段。

## 架构:三线程流水线

核心在 [src-tauri/src/pipeline.rs](src-tauri/src/pipeline.rs),三个线程通过有界 mpsc 通道串联,实现低延迟并行:

```
捕获线程 (WASAPI 48k) → 降噪(可选 RNNoise) → 重采样 48k→16k → VAD 分段
   │ [seg_tx, bounded=6]
ASR 线程 (sherpa-onnx SenseVoice 进程内) → 识别 → 发出 {id, original, pending=true}
   │ [txt_tx, bounded=32]
翻译线程 (LLM/Google/DeepL) → 翻译 → 回填发出 {id, original, translated, pending=false}
```

**关键设计**:段落按 `id` 递增编号;前端用 id 先显示原文(pending),翻译完成后按同一 id 回填译文。段落 N+1 的 ASR 与段落 N 的翻译并行执行 → 实时响应。

## 后端模块地图(`src-tauri/src/`)

| 文件 | 职责 |
|------|------|
| [lib.rs](src-tauri/src/lib.rs) | Tauri 命令:`start_translation` / `stop_translation` / `toggle_overlay` / `download_model` / `load_settings` / `save_settings`;主窗口关闭 = 清理识别器 + 流水线 |
| [pipeline.rs](src-tauri/src/pipeline.rs) | 三线程编排器,发出 `subtitle` / `status` 事件 |
| [asr.rs](src-tauri/src/asr.rs) | sherpa-onnx SenseVoice 识别器加载/调用(进程内、num_threads=4 CPU)、模型按需下载、幻觉与套话熜断 |
| [capture.rs](src-tauri/src/capture.rs) | Windows WASAPI 环回/麦克风采集;macOS 为占位(`bail!`) |
| [denoise.rs](src-tauri/src/denoise.rs) | RNNoise 降噪(480 样本/帧 @48k)+ Decimator3 重采样(48k→16k) |
| [segmenter.rs](src-tauri/src/segmenter.rs) | 分段：默认原生 `SileroCore`（sherpa Silero VAD）/ 可关闭回退到 `EnergyVad`（`Vad` trait、零依赖）；带 preroll、最短 300ms；最长按引擎自适应（`SegLimits::for_engine`：SenseVoice 7s / Qwen3 10s） |
| [translate.rs](src-tauri/src/translate.rs) | 多引擎分发:OpenAI 兼容(openai/ollama)、Google 免费(非官方)、none 纯字幕;含会话重点提炼 |
| [config.rs](src-tauri/src/config.rs) | `RuntimeConfig` 结构体(serde camelCase) |
| [main.rs](src-tauri/src/main.rs) | 入口,调用 `lib::run()` |

## 前端组件(`src/`)

| 文件 | 职责 |
|------|------|
| [App.tsx](src/App.tsx) | 主窗口控制器:顶栏、字幕列表、设置面板;监听所有 Rust 事件 |
| [components/Overlay.tsx](src/components/Overlay.tsx) | 悬浮字幕窗(独立 Vite 入口 [overlay-main.tsx](src/overlay-main.tsx)),`data-tauri-drag-region` 拖动 |
| [components/SettingsPanel.tsx](src/components/SettingsPanel.tsx) | 设置表单:语言、模型档位、降噪/VAD 开关、引擎选择 + 条件字段 |
| [components/SubtitleView.tsx](src/components/SubtitleView.tsx) | 字幕滚动列表,自动滚到底部 |
| [types.ts](src/types.ts) | 共享类型、`DEFAULT_SETTINGS`、`loadSettings()` / `saveSettings()` |

## 关键约定

- **camelCase 对齐**:Rust 结构体用 `#[serde(rename_all = "camelCase")]`,与 TypeScript 字段一一对应;改后端字段必须同步改 [types.ts](src/types.ts)。
- **有界通道 + 溢出处理**:mpsc 通道满时丢弃并发提示(如 `[翻译积压,已跳过]`),绝不无界堆积。
- **事件驱动**:Rust 用 `emit` 发出 `subtitle` / `status` 事件,前端用 `listen` 监听;不要在前端轮询。
- **设置持久化**:前端 onChange → `saveSettings` → Rust 写 `%APPDATA%\...\settings.json`;前端 `userTouched` ref 防止异步 `loadSettings()` 覆盖用户输入。
- **面向用户的错误信息用中文**,但保留原始 error 供调试,并尽量给恢复提示。
- **Rust** 用 `anyhow::Result<T>` 传播错误,`Mutex<>` 保护 `AppState` 中的 pipeline/识别器。
- **React** 用函数式组件 + hooks,无 Redux/状态库;字幕只保留最近 200 条。
- 代码注释用中文(贴近现有风格)。

## 工具链要求

- Node.js 18+、Rust stable(**MSVC** 工具链)、Windows 11 + WebView2(Win11 内置)
- ASR 当前为纯 CPU 推理(SenseVoice int8,`num_threads=4`),无 GPU 依赖
- 权限在 [capabilities/default.json](src-tauri/capabilities/default.json),窗口配置在 [tauri.conf.json](src-tauri/tauri.conf.json)

## 已知陷阱

- Google 引擎为非官方接口,可能被限流。

> 提交规范:Conventional Commits(`type(scope): subject`),推 `main` 分支需二次确认,详见用户全局 CLAUDE.md。
