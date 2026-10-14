# 海星同传 SeaStar — 技术开发文档

> **本文件是本仓库唯一权威的开发/AI 代理指南,面向 Claude Code、GitHub Copilot 及其它编码代理通用。**
> - Claude Code 读 [CLAUDE.md](CLAUDE.md) —— 已改为指向本文件。
> - GitHub Copilot 读 [.github/copilot-instructions.md](.github/copilot-instructions.md) —— 已改为指向本文件。
> - 背景与设计文档:[README.md](README.md)、[语音翻译回灌会议-设计方案.md](docs/语音翻译回灌会议-设计方案.md)、[GPU加速与macOS兼容-设计方案.md](docs/GPU加速与macOS兼容-设计方案.md)。
>
> 改动系统时,**先更新本文件**再改分发的两处入口(CLAUDE.md / copilot-instructions.md),避免文档漂移。

## 这是什么

实时语音翻译桌面应用:捕获系统音频(WASAPI 环回)或麦克风 → 进程内 sherpa-onnx 识别 → 云端/本地翻译 → 双语字幕 + 可选悬浮窗 + 可选 TTS 语音回灌。**隐私**:音频不出本机,只有识别后的文本发往翻译 API(配合本地 Ollama 可全程零外发)。

技术栈:**Tauri 2**(Rust 后端)+ **React 19 + TypeScript**(Vite 构建)。Windows 端到端可用;macOS 采集与回灌代码完整、**待真机验证**。

## 构建与开发命令

```powershell
npm install                  # 安装前端依赖
npm run tauri dev            # 开发:前端 HMR + Rust 热重建(完整运行应用)
npm run tauri build          # 生产打包 → src-tauri/target/release/bundle/(NSIS .exe + MSI)

# 快速校验(不启动应用,改完优先跑这两个)
npm run build                # 前端 TS 类型检查(tsc)+ Vite 打包
cd src-tauri; cargo check    # 仅 Rust 编译检查
```

CI([.github/workflows/ci.yml](.github/workflows/ci.yml))在 Win/macOS/Linux 三平台跑 `tsc --noEmit` + `cargo check --all-targets` + `tauri build`。无单元测试套件,验证靠 `cargo check` / `npm run build` + 手动运行。

> PowerShell 下 `cargo check` / `npm run tauri dev` 常因把 stderr 当错误而返回退出码 1,只要出现 `Finished ... profile` 即为编译成功,非真错误。

## 架构:双向并发流水线

核心在 [pipeline.rs](src-tauri/src/pipeline.rs)。`pipeline::start(app, recognizer, tts, gate, direction, cfg)` 启动**一条**流水线(内部若干线程经**有界 mpsc** 串联);[lib.rs](src-tauri/src/lib.rs) 的 `start_translation` 据配置最多并发跑**两条方向相反**的流水线:

```
入站 "in"  (听对方→字幕):  loopback 采集 → 降噪 → 重采标40ｘ16k → VAD分段(=喂料) + voice_active活动信号
   │[seg_tx,bounded=6]  → ASR逐段识别 → 攒句缓冲(停顿/句长定稿成整句) → 原文(pending) │[txt_tx,bounded=32] → 翻译worker×2 → 回填译文 → 写存档
出站 "out" (你说话→译出语回灌): microphone 采集 → …同上… → 翻译 → │[tts_tx,bounded=8] → Kokoro合成 → 串行播放到 CABLE Input
```

**关键设计**:
- **攒句定稿(翻译/落盘/贴标签的单元)**:断句下沉到 VAD——`segSilenceMs`(默认 750ms)是唯一停顿阈值(=句界)。VAD 给每个切出的段打 **`CutReason`标签**：**Pause**(静音达 750ms 自然切，=句界、立即定稿) / **Max**(顶到 `segMaxSec` 上限强切，=半句、继续攒)。ASR 线程按标签定稿，连续独白无停顿时靠 `sentenceMaxS` 硬兜底。定稿后经 diarize-first(pyannote 段内按人切，`split_turns` 贪心去重叠)可拆多行。另含 preroll 段边界重复碎渣过滤。参见 [docs/实时精确断句-设计方案.md](docs/实时精确断句-设计方案.md)。
- **说话人归号**:定稿时对**整句音频**提一次 CAM++ 声纹 → `SpeakerRegistry`(见 [diarize.rs](src-tauri/src/diarize.rs))全局库就近归并,编号跨段稳定。**短句(<1s)声纹不可信 → 标未知(不归号)**,既不新建假号也不硬挂上一个人。段级声纹换人判定**已移除**(段声纹波动 ±20%,据此切句是噪声误切);换人分句改由停顿定稿承担。**已知短板**:段内**无停顿接连换人**(问答访谈第二人开头)整句归主导人,需句内 pyannote 分离才能治(储备未启用)。
- **id 回填**:全局单调递增 `NEXT_SUBTITLE_ID`(跨重启不重置)。先发 `{id, original, pending:true}` 让前端立即显示原文,翻译完按**同一 id** 回填 `{id, translated, pending:false}`。句 N+1 的识别与句 N 的翻译并行 → 实时。
- **代际守卫**:`PIPELINE_GEN` 自增代号丢弃旧流水线的迟到事件,避免"停止需点两次"竞态。
- **有界通道 + 溢出丢弃**:通道满用 `try_send` 丢弃并发提示(如 `[翻译积压,已跳过]`),**绝不**改无界堆积。
- **翻译并发**:`NUM_TRANSLATORS=2` 个 worker 抢锁消费同一 `txt_rx`,HTTP 在锁外真并发;喂 LLM 的上下文取最近 `CONTEXT_USE=2` 条**成功**译文(失败译文不进上下文,防传染)。
- **热切目标语言**:`set_target_lang` 写 `live_target` 共享槽,翻译 worker 每段读一次,不重启流水线。
- **麦克风门控**:**近场音量门**(`MIC_GATE_RMS=0.02`)在**麦克风源**(回灌出站 + 麦克风翻译,即 `source=="microphone"`)按段 RMS 粗筛远场旁人/杂音——环回系统声响度天然波动大,不设门。**说话人声纹门**(见 [voiceprint.rs](src-tauri/src/voiceprint.rs))**仅回灌出站(out)**再叠一道,非登记者的段直接丢。
- **回灌防回环**:开启回灌(`ttsEnabled && translationEngine != "none"`)时,入站强制 `source="loopback"`、出站强制 `source="microphone"`;TTS **只灌 CABLE Input**,绝不进默认输出,否则环回会把自己的 TTS 又转一遍。会议软件麦克风须选 CABLE Output。详见[设计方案](docs/语音翻译回灌会议-设计方案.md)。

## 后端模块地图(`src-tauri/src/`)

| 文件 | 职责 |
|------|------|
| [lib.rs](src-tauri/src/lib.rs) | 所有 Tauri 命令 + `AppState`(双流水线 + 双识别器,`Mutex` 保护)+ 启动清理过期存档 + 主窗关闭=干净退出 |
| [pipeline.rs](src-tauri/src/pipeline.rs) | 单条流水线编排器(采集/识别/翻译/可选TTS线程);**攒句定稿**(VAD段=喂料→累积整句,voice_active 停顿定稿+句长上限)+ 整句声纹归号;发 `subtitle`/`status` 事件 |
| [asr.rs](src-tauri/src/asr.rs) | `Asr` trait(`transcribe`/`fingerprint`/`reset_session`)+ 两实现:SenseVoice(CTC,快)/ Qwen3(LLM解码,准);进程内静态链接 sherpa-onnx,**无 sidecar/HTTP**;模型按需下载;幻觉/套话/模板泄漏熔断 |
| [tts.rs](src-tauri/src/tts.rs) | Kokoro v1.0 多语进程内合成(复用同一份 sherpa,不引第二份 onnxruntime);模型 ~310MB 按需下载 |
| [playback.rs](src-tauri/src/playback.rs) | WASAPI render 到输出设备(24k→48k 升采样、单声道→双声道);枚举设备 + 自动找 CABLE Input |
| [capture.rs](src-tauri/src/capture.rs) | Windows WASAPI 环回/麦克风采集;平台无关处理抽成 `Processor`;macOS(ScreenCaptureKit)代码已补全**待真机验证** |
| [denoise.rs](src-tauri/src/denoise.rs) | RNNoise 降噪(480样本/帧@48k)+ /3 FIR 重采样 48k→16k |
| [segmenter.rs](src-tauri/src/segmenter.rs) | `Vad` trait:默认 `SileroCore`(sherpa Silero VAD)/ 回退 `EnergyVad`(零依赖);`SegLimits::for_engine` 按引擎自适应段长(SenseVoice 短 / Qwen3 长);带 preroll、最短 300ms。切出的段是**攒句喂料**,`has_open()` 经 `voice_active` 供 ASR 线程判静音 |
| [translate.rs](src-tauri/src/translate.rs) | 多引擎:openai 兼容 / ollama / google(免费非官方)/ none;`summarize` 会话重点提炼(超预算自动 map-reduce 分块);whatlang + 假名/谚文脚本检测兜底判"译文是否还是原语种"并清上下文重译 |
| [voiceprint.rs](src-tauri/src/voiceprint.rs) | 说话人声纹：CAM++ 嵌入（复用 sherpa）登记"我的声音" + 出站门控余弦相似度挡旁人；登记落 `voiceprint.json`；声纹提取器亦被分离的全局说话人库复用 |
| [diarize.rs](src-tauri/src/diarize.rs) | 说话人归号(仅入站)：**当前主用** `SpeakerRegistry` 会话级**全局说话人库**——定稿整句提 CAM++ 声纹,余弦就近归并+质心精炼(EMA),编号跨段稳定;`match_only` 供短句只挂不建。**diarize-first(已启用)**:`Diarizer`(pyannote-segmentation-3.0 段内分段)+ `split_turns`(贪心去重叠)在入站开启「说话人分离」且块够长时按人切音频、逐轮重识别、多行出;`attribute`/`speaker_at`(逐 token 归属+迟滞平滑)仍为储备。pyannote 模型随包打包,缺失则降级 |
| [transcript.rs](src-tauri/src/transcript.rs) | 会话译文落盘(`<stem>.jsonl`)+ 摘要(`<stem>.summary.<lang>.md`)+ 历史列举 + 过期清理 |
| [config.rs](src-tauri/src/config.rs) | `RuntimeConfig`(serde camelCase),含语言名/TTS espeak 语种码映射 |
| [main.rs](src-tauri/src/main.rs) | 入口,调用 `lib::run()` |

## 前端模块地图(`src/`)

| 文件 | 职责 |
|------|------|
| [App.tsx](src/App.tsx) | 主窗控制器:顶栏、字幕、设置、历史、摘要;`listen` 所有 Rust 事件,按 id 回填字幕 |
| [components/RelayBar.tsx](src/components/RelayBar.tsx) | 回灌·双向同传的顶部控制条(方向/设备/状态) |
| [components/SettingsPanel.tsx](src/components/SettingsPanel.tsx) | 语言/引擎/降噪/VAD/回灌设备/说话人门控等设置表单(条件字段) |
| [components/SubtitleView.tsx](src/components/SubtitleView.tsx) | 字幕虚拟滚动列表(@tanstack/react-virtual),按 direction 区分入/出站 |
| [components/Overlay.tsx](src/components/Overlay.tsx) | 悬浮字幕窗,独立 Vite 入口 [overlay-main.tsx](src/overlay-main.tsx),`data-tauri-drag-region` 拖动 |
| [components/SummaryPanel.tsx](src/components/SummaryPanel.tsx) | 会话重点提炼(流式 `summary` 事件,可编辑/重提炼) |
| [components/HistoryPanel.tsx](src/components/HistoryPanel.tsx) | 历史会话回看(只读) |
| [types.ts](src/types.ts) | 共享类型、`Settings`、`DEFAULT_SETTINGS`、`loadSettings()` / `saveSettings()` |

## 关键约定(改代码前必读)

- **camelCase 三处对齐**:改后端 `RuntimeConfig` 字段 → **必须**同步改 [types.ts](src/types.ts) 的 `Settings` + `DEFAULT_SETTINGS`,否则 IPC 静默错位。
- **事件驱动，不轮询**：后端只 `emit`，前端 `listen<T>(...)`。字幕用 `findIndex` 命中 id 则替换、否则追加；整场留内存（不截断），DOM 由 [SubtitleView.tsx](src/components/SubtitleView.tsx) 虚拟滚动兼底。
- **设置持久化**:onChange → `saveSettings`(fire-and-forget,Rust 同步写 `%APPDATA%\com.seastar.voicetranslator\settings.json`);用 `userTouched` ref 防异步 `loadSettings()` 覆盖用户输入,改加载逻辑时保留此保护。
- **错误处理**:Rust 全程 `anyhow::Result<T>` + `?`,`bail!("中文原因")` 主动报错;ASR/翻译失败 `emit_status("error", ...)` 后 `continue`,不中断流水线。面向用户文案中文,保留原始 error 供调试并给恢复提示。
- **状态保护**:`Mutex<>` 保护 `AppState` 中的双 pipeline / 双识别器。
- **React**:函数式组件 + hooks,无 Redux/状态库。
- 代码注释用**中文**(贴近现有风格)。**不擅自加**向后兼容 shim / feature flag / 过度错误兜底,除非明确要求。

## Tauri 命令、事件与约定信号码

命令(全在 [lib.rs](src-tauri/src/lib.rs) 的 `invoke_handler`):`start_translation` / `stop_translation` / `set_target_lang` / `toggle_overlay` / `model_exists` / `download_model` / `tts_model_exists` / `download_tts_model` / `voiceprint_exists` / `clear_voiceprint` / `enroll_voiceprint` / `list_output_devices` / `cable_exists` / `load_settings` / `save_settings` / `summarize_session` / `open_summary_dir` / `load_summary` / `save_summary` / `retranslate` / `list_sessions` / `load_session`。

事件:`subtitle`、`status`、`model-progress`、`tts-progress`、`summary`。

`start_translation` 用 `Err(String)` 作**信号码**让前端触发对应引导流程,**不要当普通错误**直接弹给用户:
- `MODEL_MISSING` → 触发 ASR 模型下载
- `TTS_MODEL_MISSING` → 触发 Kokoro 下载
- `CABLE_MISSING` → 提示装 VB-Cable
- `RELAY_LANG_UNSUPPORTED` → SenseVoice 只认 zh/en/ja/ko/yue,回灌出站源语言越界(改用 Qwen3 或换语言)

## 模型与外部依赖

- **按需下载**到 `%APPDATA%\com.seastar.voicetranslator\models\`(带进度,不打包):SenseVoice int8 ≈237MB / Qwen3-ASR ≈940MB / Kokoro TTS ≈310MB。下载用 `tar.bz2` + 系统 `tar` 解压(需 Win10+ 自带 tar)。
- **随仓库/安装包打包**:`src-tauri/sidecar/models/silero_vad.onnx`(≈2.2MB,VAD)、`campplus_sv_zh_16k.onnx`(≈27MB,声纹/分离共用)、`pyannote_segmentation_3.onnx`(≈5.7MB,说话人分离分段)。三者在 `.gitignore` 里单独白名单放行,列入 [tauri.conf.json](src-tauri/tauri.conf.json) 的 `bundle.resources`;**新增打包模型时这两处都要改**。pyannote 解析优先打包资源、回退 app 数据目录旧路径(兼容开发期手放)。
- **回灌依赖 VB-Cable**(用户自装虚拟声卡):app 把 TTS render 到 `CABLE Input`,会议软件麦克风选 `CABLE Output`。
- 权限在 [capabilities/default.json](src-tauri/capabilities/default.json),窗口配置在 [tauri.conf.json](src-tauri/tauri.conf.json)(主窗 `main` + 悬浮窗 `overlay`)。

## 工具链要求

- Node.js 18+、Rust stable(**MSVC** 工具链)、Windows 11 + WebView2(内置)。
- ASR/TTS 当前纯 CPU 推理(`num_threads=4`),无 GPU 依赖(GPU 化见设计文档,属未来工作)。

## sherpa-onnx 能力储备（供规划参考）

当前依赖 `sherpa-onnx = 1.13.3` crate。已启用：说话人分离 `OfflineSpeakerDiarization`（见 [diarize.rs](src-tauri/src/diarize.rs)，pyannote-segmentation-3.0 + 复用 CAM++）。**尚未启用**：语音增强 `OnlineSpeechDenoiser`（流式 GTCRN）/ `OfflineSpeechDenoiser`。**未绑定**：人声/伴奏源分离（C++ 有，Rust crate 与 sys 层均无符号，要用需自写 FFI 或升级版本）。

## 已知陷阱

- Google 引擎为非官方接口,可能被限流。
- 目标语言 = 源语言时(如 `targetLang=ja` 又听日语音频),译文=原文属**配置**问题非 bug——`translate.rs` 判定"原文本就是目标语"会故意放行不重译。
- macOS 采集+回灌代码完整但仅编译过、未真机验证(权限弹窗时机、设备选择、出声均需真机)。

> 提交规范:Conventional Commits(`type(scope): subject`),推 `main` 分支需二次确认,详见用户全局 CLAUDE.md。
