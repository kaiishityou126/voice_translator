# 海星同传 · SeaStar Voice Translator

**实时语音翻译桌面应用 · Real-time speech-translation desktop app**

[English](#english) · [中文](#中文)

---

## English

Turn any sound playing on your PC — meetings, videos, live streams, podcasts — into live **bilingual subtitles**, and even speak your translation **back into a meeting**. Speech recognition runs **locally on your machine**; only the recognized **text** is sent to the translation service.

> **Windows: ready to use. macOS / Linux: capture code written, pending real-hardware testing.**

### Highlights

- 🎧 **Translate any system audio** — not tied to one app. Works with Zoom / Teams / Meet, videos, live streams, anything playing on your PC (system-audio loopback), or your microphone.
- 🔒 **Privacy first** — audio never leaves your machine; only recognized text goes to the translation API. Pair it with a local Ollama model for **zero data leaving your PC**.
- 🗣️ **Two-way voice relay** — speak your language and have it translated and *spoken* into the meeting so others hear their language (while you read theirs). ★ the standout feature.
- 📝 **Live bilingual subtitles** + a draggable, always-on-top **overlay** window.
- 🌐 **Multiple translation engines**, including no-key options — OpenAI-compatible (OpenAI / DeepSeek / Qwen / Kimi), local **Ollama** (no key), free **Google** (no key), or **subtitles-only** (no translation).
- 🧠 **One-click session summary** (LLM) + **history** review.
- 🎯 **Speaker voiceprint gate** — during relay, only let *your* voice through and drop nearby bystanders.

### Who it's for

Windows users who want cross-app, privacy-friendly, optionally-offline, real-time translation of foreign-language audio — and optionally two-way voice relay for cross-language meetings.

### System requirements (minimum)

- **OS:** Windows 10 / 11 (64-bit), with the WebView2 runtime (built into Windows 11). *macOS / Linux capture code is written but unverified on real hardware, so it currently suits Windows users only.*
- **CPU:** x86-64 (Intel / AMD). Recognition and speech synthesis run **in real time on the CPU** — **4+ cores recommended**; for **two-way relay** (listening + speaking + synthesis at once), **6+ cores recommended**. *(Windows on ARM is not supported.)*
- **RAM:** **8 GB+**; 16 GB recommended for the Qwen3 model or two-way relay.
- **Disk:** about **2 GB** free for the on-demand recognition / TTS models.
- **GPU:** none required — CPU only.
- **Network:** needed for cloud translation and the first-time model download; choose local Ollama for offline translation.

### Run from source

Prerequisites: Node 18+, Rust (stable-msvc), WebView2 (built into Windows 11).

```powershell
npm install
npm run tauri dev      # run in dev mode
npm run tauri build    # build installers (NSIS .exe / MSI)
```

### Quick start

1. **Install** the app. (Unsigned build — see [Install notes](#install-notes-unsigned) to get past the OS warning.)
2. **Open it and pick a translation engine** in Settings: a cloud API → paste your key; or a no-key option (Google / subtitles-only / local Ollama).
3. **Choose the source & target language.**
4. **Click Start.** On first run, the speech-recognition model downloads automatically (with a progress bar).
5. **Play foreign-language audio** (or speak into your mic) → read the live bilingual subtitles. Toggle the overlay for an always-on-top view.

### Two-way voice relay (meetings)

Speak your language → the other side hears it in theirs; they speak → you read subtitles. This needs a virtual audio cable so the app can "speak" into the meeting.

1. Install **VB-CABLE** (a free virtual audio device).
2. In the app, turn on **voice relay**, then set **your language / the other side's language / voice**.
3. In your meeting software, set the **microphone to `CABLE Output`**.
4. *(Optional)* **Enroll your voiceprint** and enable the speaker gate so people nearby don't get relayed.
5. **Start.** Your speech is translated and played into the meeting; what others say shows up as bilingual subtitles.

> Design rationale (anti-echo-loop, etc.) lives in [docs/语音翻译回灌会议-设计方案.md](docs/语音翻译回灌会议-设计方案.md).

### Install notes (unsigned)

This is free, open-source software without a paid code-signing certificate, so your OS will warn on first install. This is normal for any unsigned app — it says nothing about whether the software is safe.

- **Windows (SmartScreen):** click **More info** → **Run anyway**.

### Which translation engine?

Translation is done by the engine you pick; quality and speed differ here:

| Engine | Needs | Notes | Best for |
|---|---|---|---|
| **OpenAI-compatible** | API key | Point it at GPT / DeepSeek / Qwen / Zhipu / Kimi — **most accurate, best across languages** | Quality |
| **Local Ollama** | Ollama + a model | **Offline, nothing leaves your PC**; quality scales with model size | Privacy / offline |
| **Google (free)** | nothing | No key, wide coverage; unofficial endpoint **may rate-limit** | Just works |
| **Subtitles-only** | nothing | No translation, shows the recognized text | Transcription only |

**Free & real-time pick:** point the OpenAI-compatible engine at **Zhipu GLM-4-Flash** (free — register at [open.bigmodel.cn](https://open.bigmodel.cn/) for a key). It's fast — the translation usually shows up within about a second of you finishing a sentence, which is plenty for live subtitles. Step up to GPT / DeepSeek etc. when you want higher quality.

> **Avoid reasoning models** (names with `o1` / `o3` / `R1` / `QwQ` / `reasoner` — they "think" before replying and lag your subtitles). Just **pick a fast model by name**: `Flash` / `mini` / `turbo` / `chat` / `V3` (e.g. GLM-4-Flash, deepseek-chat, qwen-turbo). These don't think by default — no extra settings needed.

### Good to know

- **Recognition (which spoken languages it understands) depends on the engine:** **SenseVoice** (default, fast) handles Chinese / English / Japanese / Korean / Cantonese (5); **Qwen3-ASR** (more accurate) handles ~30 languages, plus many Chinese dialects.
- **Model sizes** (downloaded on first use): SenseVoice ≈237 MB (fast) / Qwen3-ASR ≈940 MB (more accurate) / Kokoro TTS ≈310 MB (relay). Everything runs on CPU.

### Feedback & contributing

Run into a problem or have an idea to make it better? Please open an [issue](../../issues). Want it on **macOS / Linux**? Speak up too — the capture code is written and just needs testing on real hardware. Knowing people actually use it is what keeps me motivated to maintain and improve this project.

For architecture, the module map, and dev conventions, see **[AGENTS.md](AGENTS.md)**.

---

## 中文

把电脑上正在播放的**任何声音**——会议、视频、直播、播客——变成实时**双语字幕**,甚至能把你的翻译"说"回**会议里**。语音识别**在本机完成**,只有识别出的**文本**才发往翻译服务。

> **Windows:开箱可用。macOS / Linux:采集代码已写好,待真机验证。**

### 特色亮点

- 🎧 **翻译任何系统声音** —— 不绑定某个软件。Zoom / Teams / 腾讯会议、视频、直播,凡是电脑在放的声音(系统音频环回)或你的麦克风,都能翻。
- 🔒 **隐私优先** —— 音频全程不出本机,只有识别出的文本发往翻译接口;配本地 Ollama 可做到**数据零外发**。
- 🗣️ **双向语音回灌** —— 你说母语,应用自动译成对方的语言,再用语音替你"讲"进会议,对方直接听翻译完的语言;对方说话,你这边看双语字幕。**就像随身带了个同传。** ★ 最独特的能力。
- 📝 **实时双语字幕** + 可拖动、置顶的**悬浮窗**。
- 🌐 **多种翻译引擎**,含免 key —— OpenAI 兼容(OpenAI / DeepSeek / 通义 / Kimi)、本地 **Ollama**(无 key)、**Google 免费**(无 key)、**纯字幕**(不翻译)。
- 🧠 **会话重点一键提炼**(LLM)+ **历史回看**。
- 🎯 **说话人声纹门控** —— 回灌时只放行你本人,挡掉旁边的人。

### 适合谁

Windows 用户,想要**跨应用、重隐私、可离线**地实时翻译外语音频;还可以双向语音回灌,用于跨语言开会。

### 系统配置(最低)

- **操作系统**:Windows 10 / 11(64 位),需 WebView2 运行时(Win11 自带)。*macOS / Linux 采集代码已写好但未真机验证,暂只适合 Windows 用户。*
- **处理器**:x86-64(Intel / AMD)。识别与语音合成为**纯 CPU 实时推理** —— 建议 **4 核以上**;开**双向回灌**(同时听 + 说 + 合成三路并发)建议 **6 核以上**。*(不支持 Windows on ARM。)*
- **内存**:**8GB 以上**;用 Qwen3 大模型或双向回灌建议 16GB。
- **硬盘**:约 **2GB** 可用空间(按需下载的识别 / TTS 模型)。
- **显卡**:无需独显,纯 CPU 运行。
- **网络**:用云 API 翻译需联网,首次需联网下模型;选本地 Ollama 可离线翻译。

### 从源码运行

前置:Node 18+、Rust(stable-msvc)、WebView2(Win11 自带)。

```powershell
npm install
npm run tauri dev      # 开发运行
npm run tauri build    # 打包安装包(NSIS .exe / MSI)
```

### 快速开始

1. **安装**(未签名版,过系统警告见 [安装说明](#安装说明未签名))。
2. **打开,在设置里选翻译引擎**:云 API 填 key;或选免 key 的 Google / 纯字幕 / 本地 Ollama。
3. **选源语言和目标语言。**
4. **点「开始」。** 首次会自动下载识别模型(带进度条)。
5. **播放外语音频**(或对着麦克风说)→ 看实时双语字幕。想置顶就开悬浮窗。

### 回灌·双向同传(开会用)

你说母语 → 对方听到他的语言;对方说话 → 你读字幕。这需要一个虚拟声卡,好让应用能"说"进会议。

1. 安装 **VB-CABLE**(免费虚拟声卡)。
2. 应用里打开**语音回灌**,设好**我的语言 / 对方语言 / 音色**。
3. 会议软件里把**麦克风设成 `CABLE Output`**。
4. *(可选)* **登记你的声纹**并开启说话人门控,挡掉旁边的人被一起回灌。
5. **开始。** 你说的话会被译成对方语言并"说"进会议;对方说的话显示为双语字幕。

> 设计原理(防音频回环等)见 [docs/语音翻译回灌会议-设计方案.md](docs/语音翻译回灌会议-设计方案.md)。

### 安装说明(未签名)

本项目是**免费开源、未购买代码签名证书**的软件,首次安装系统会弹安全提示——这对任何未签名应用都一样,与软件是否安全无关。

- **Windows(SmartScreen):** 点**更多信息** → **仍要运行**。
### 选哪个翻译引擎

翻译由你选的引擎完成,质量与速度差别主要在这里:

| 引擎 | 需要 | 特点 | 适合 |
|---|---|---|---|
| **OpenAI 兼容** | API key | 可指向 GPT / DeepSeek / 通义 / 智谱 / Kimi 等,**翻译最准、多语最稳** | 追求质量 |
| **本地 Ollama** | 装 Ollama + 拉模型 | **离线、零外发**;质量看模型大小,小模型对冷门语言偏弱 | 重隐私 / 离线 |
| **Google 免费** | 无 | 免 key、覆盖广;非官方接口**可能限流** | 随手就用 |
| **纯字幕** | 无 | 不翻译,只显示识别原文 | 只要转写 |

**省钱又要实时的推荐:** 用 OpenAI 兼容接口指向**智谱 GLM-4-Flash**（免费,在[智谱开放平台](https://open.bigmodel.cn/)注册拿 key）—— 速度快,一般说完 1 秒内就出译文,日常字幕完全够用;想要更高质量再换 GPT / DeepSeek 等更强的模型。

> **别选推理模型**（名字带 `o1` / `o3` / `R1` / `QwQ` / `reasoner` 的,出答案前要先“想”很久,会拖慢字幕）。直接**按模型名选个“快的”**:`Flash` / `mini` / `turbo` / `chat` / `V3`（如 GLM-4-Flash、deepseek-chat、qwen-turbo）—— 这些默认不思考,应用里填对模型名即可,无需其它设置。
### 使用小贴士

- **识别语言(听懂哪些话)看引擎:** **SenseVoice**(默认、快)可识别中 / 英 / 日 / 韩 / 粤 5 种;**Qwen3-ASR**(更准)可识别约 30 种语言,另含多种中文方言。
- **模型体积**(首次使用自动下载):SenseVoice ≈237MB(快)/ Qwen3-ASR ≈940MB(更准)/ Kokoro TTS ≈310MB(回灌用)。全部纯 CPU 运行。

### 反馈与参与

如果你在使用中遇到问题、或者有更好的想法,欢迎到 [Issues](../../issues) 提出来。想在 **macOS / Linux** 上用的朋友也欢迎冒个泡 —— 采集代码已经写好,只差真机调试。**有人在用,我就更有动力把这个项目持续维护、做得更好。**

想了解架构、模块地图与开发约定,见 **[AGENTS.md](AGENTS.md)**。
