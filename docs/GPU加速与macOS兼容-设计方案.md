# GPU 加速(N 卡)与 macOS 兼容 — 设计与开发方案存档

> 本文是一次设计讨论的存档,目的:换到**有 N 卡的机器**上打开本文即可照着开工。
> 写作日期:2026-06-23。作者上下文:开发机为 Intel Iris Xe 核显、无独显,纯 CPU 推理。
> **关键前提(别忘):本应用是要分发给很多人的产品,用户群异构——有人有 N 卡,有人没有。**
> 因此 GPU 自适应不是"过度设计",而是刚好合适;但要分阶段、守住"现有 CPU 功能/性能/体积零变化"的红线。

---

## 0. 一句话目标

> 轻量引擎(SenseVoice)默认 CPU;重量引擎(Qwen3)在有 N 卡时上显存。
> 启动时**自动探测 + 微基准实测**决定每个引擎跑 CPU 还是 GPU;没卡的用户明确提示"重量版不可用"。
> 兼容 macOS(CoreML 分支,逻辑同构)。**默认构建保持现状纯 CPU 静态,GPU 全部走 opt-in cargo feature。**

---

## 1. 背景与已确认的事实(决策依据,别重新纠结)

### 1.1 当前架构现状(代码事实)
- ASR 用 **sherpa-onnx 1.13.3**,**静态链接、进程内**,无运行期 DLL、无 sidecar、无 HTTP。
  - 预编译静态库在 `src-tauri/target/sherpa-onnx-prebuilt/sherpa-onnx-v1.13.2-win-x64-static-MT-Release-lib`(CPU-only)。
  - `src-tauri/Cargo.toml` 里依赖就一行:`sherpa-onnx = "1.13.3"`。
  - **VAD 也用 sherpa 自带的 Silero**,故意不引第二份 onnxruntime——Cargo.toml 注释已写明"避免与 sherpa 静态库符号冲突"。⚠️ 这条是后面 CUDA 方案最大的坑,见 §4.4。
- `src-tauri/src/asr.rs` 关键点:
  - `pub fn load(app, engine, language) -> Result<Arc<dyn Asr>>`:按 engine 字符串分发。`"qwen3Asr"` → Qwen3,其它 → SenseVoice。
  - `SenseVoice::load()` 里 `config.model_config.num_threads = 4;`
  - `Qwen3Asr::load()` 里 `config.model_config.num_threads = 6;`
  - **两处都没设 `config.model_config.provider`,默认就是 `"cpu"`。** 这就是要动的地方。
  - `fingerprint_for(engine, language)` / `Asr::fingerprint()`:身份指纹,与期望不符时上层重建识别器。provider 要纳入指纹。
- `src-tauri/src/capture.rs`:**macOS 采集目前是 `bail!` 占位,Mac 上整个 app 跑不起来。** 见 §6。
- 配置:`src-tauri/src/config.rs` 的 `RuntimeConfig`(serde camelCase),前端 `src/types.ts` 一一对应。

### 1.2 sherpa-onnx 的 provider 机制
- `OfflineRecognizerConfig.model_config.provider` 取值:`"cpu"` / `"cuda"` / `"directml"` / `"coreml"`。
- **但 provider 只是"请求",真正能用要看二进制里有没有编进对应 execution provider(EP)。**
  当前静态 CPU 库里**只有 CPU EP**;设 `"cuda"` 会被 onnxruntime **静默回退 CPU(不报错)**。
  → 所以"有卡走卡"必须**构建层 + 代码层同时改**,缺一不可。

### 1.3 实测数据:GPU 不一定更快(关键!别想当然)
来源:onnxruntime 官方 issue #10303(ResNet18,**batch=1**,跟实时 ASR 单路同性质):

| 环境 | onnxruntime CUDA | onnxruntime CPU | 结论 |
|---|---|---|---|
| 弱 GPU(Quadro M2000M) | 47.89 ms | 18.48 ms | **GPU 比 CPU 慢 2.6×** |
| 强 GPU(V100/T4/A100) | 远快于 CPU | — | GPU 明显赢 |
| DirectML / RDNA2 核显 | 仅比 CPU 快 3×,"GPU 没被正确利用,很慢" | — | 核显提速差 |

ORT 维护者定性原因:**单 batch / 小模型时,H2D 拷贝 + kernel 启动的固定开销占主导**,弱卡 cover 不住就比 CPU 慢。

旁证(SenseVoice 官方仓库):
- SenseVoice 非自回归,**10 秒音频仅需 70ms**,CPU 本就极快(本机实测 RTF≈0.02)。
- 它宣传的 GPU 大加速("V100 上 TensorRT 526×")**明确是批量并发服务**,不是单路场景。

**推论(写进设计的核心):**
- 轻量版(SenseVoice):强独显上 GPU 也只是"无感",弱卡/核显上**实测会更慢**。→ 默认 CPU,除非微基准证明 GPU 明显更快。
- 重量版(Qwen3,自回归、计算量大):**强独显才真正划算**,这是 GPU 该上的地方。

### 1.4 重量版模型供给问题(跟有没有卡无关)
- **sherpa-onnx 只有 Qwen3-ASR-0.6B 的预转换 ONNX**(`sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25`,约 940MB,GitHub releases)。
- **Qwen3-ASR-1.7B 上游确实存在**(QwenLM 开源,Apache-2.0,HF/ModelScope),但**只有 PyTorch 权重**,官方运行时是 transformers/vLLM + CUDA,**没有 ONNX 导出**,sherpa-onnx **无法直接加载**。
- 结论:即使用户有 4090,**sherpa 当前能加载的重量版仍是 0.6B**。要上 1.7B 必须二选一:
  1. 自己把 1.7B 转成 sherpa ONNX(官方没给,有不确定性);
  2. 挂 Python + vLLM sidecar(抛弃进程内架构,大改,与隐私/进程内设计冲突)。
- **建议:第一版重量版就用 0.6B(走 sherpa),1.7B 单列为后续课题,别和 GPU 方案捆绑。**

### 1.5 本机 A/B 历史结论
- SenseVoice(int8)RTF≈0.02;Qwen3-0.6B RTF≈0.39(纯 CPU,约慢 19×),且识别质量未超过 SenseVoice。
- 所以**默认引擎保持 SenseVoice**;Qwen3 作为"有卡用户的高精度可选项"。

---

## 2. 总体设计:五层

| 层 | 职责 | 涉及文件 |
|---|---|---|
| **L0 构建** | 把 CUDA EP 编进二进制 | `src-tauri/Cargo.toml`、CI、`src-tauri/tauri.conf.json` |
| **L1 探测** | 启动枚举 GPU(厂商/显存/独显) | 新增 `src-tauri/src/gpu.rs` |
| **L2 决策** | 微基准 + 指纹缓存,定每引擎 provider | `gpu.rs` + `config.rs` |
| **L3 应用** | provider 注入识别器 + 重量版无卡拦截 | `asr.rs`、`lib.rs` |
| **L4 前端** | 设备下拉 + 实际设备反馈 + 重量版禁用提示 | `types.ts`、`SettingsPanel.tsx` |

---

## 3. 分阶段落地(**到了 N 卡机器,按这个顺序做**)

### 第 1 步 — 纯 CPU 骨架(零风险,不依赖 N 卡,先做)
**目的:对所有用户都有价值,且完全不碰现有 CPU 构建。**
1. `config.rs` / `types.ts`:`RuntimeConfig` 加 `provider: String`(默认 `"auto"`),camelCase 对齐,`DEFAULT_SETTINGS` 同步。
2. 新增 `src-tauri/src/gpu.rs`:实现 L1 硬件探测(见 §5),返回 `GpuInfo`。无 GPU feature 时永远返回"无可用 GPU"。
3. `asr.rs`:`load()` 增加 `provider` 参数,在两个 `load()` 里 `config.model_config.provider = provider.into();`。provider 纳入 `fingerprint`。
4. `lib.rs`:启动/起流水线时调用探测;重量版(qwen3Asr)在"无可用 GPU"时,在 load 前拦截并 emit 错误事件"重量版需 N 卡,本机无法运行"。
5. `SettingsPanel.tsx`:设备下拉(`自动/强制CPU/强制GPU`,平台门控);无 GPU 时禁用重量版引擎并显示原因。
6. 状态反馈:pipeline 启动后 emit status「ASR 运行于 CPU / CUDA(显卡名)」——**因为静默回退,必须让用户看见实际设备**。

> 第 1 步不开任何 GPU feature → 探测永远"无 N 卡" → **行为与今天 100% 一致**。可独立提交、独立验证。

### 第 2 步 — CUDA 构建(到了 N 卡机器先做这个 spike!)
**⚠️ 头号风险在 §4.4。先单独验证它,通过了再继续。**
1. 做 §4.4 的符号冲突 spike。
2. 通过后:Cargo.toml 加 `gpu-cuda` feature(§4.1)+ DLL 打包(§4.3)+ 双包/CI。

### 第 3 步 — 微基准自适应
接 §5 的微基准 + 指纹缓存,把 `"auto"` 从"探到卡就用"升级为"实测更快才用"。这样弱卡/核显用户自动回退 CPU,不被坑。

---

## 4. L0 — CUDA 构建详解

### 4.1 cargo feature 门控(默认关,守红线)
```toml
# src-tauri/Cargo.toml
[features]
default = []
gpu-cuda = ["sherpa-onnx/cuda"]   # ⚠️ feature 名待核对 sherpa-onnx 1.13.3 实际定义
```
- `cargo build` → 现状 CPU 静态,零变化。
- `cargo build --features gpu-cuda` → 带 CUDA EP。

### 4.2 两套产出
- **CPU 版**:无 CUDA DLL,装机包维持几十 MB。
- **GPU 版**:附 `onnxruntime_providers_cuda.dll` / `onnxruntime_providers_shared.dll` 等,装机包涨到几百 MB,要求目标机有 NVIDIA 驱动(cuDNN 随包带或要求装 CUDA Toolkit)。

### 4.3 打包(tauri)
- `src-tauri/tauri.conf.json`:GPU 版把 CUDA 相关 DLL 加进 `bundle.resources`;CPU 版不带。
- 两份 bundle 配置或 CI 矩阵分别出包。

### 4.4 ⚠️⚠️ 头号风险 spike:CUDA EP 会不会和 sherpa 静态 onnxruntime 冲突
**这是整个 GPU 方案能否成立的关键。到 N 卡机器第一件事就做这个,别先写业务代码。**

背景:当前 VAD + ASR 共用 sherpa 自带的**静态** onnxruntime;CUDA EP 走的是 onnxruntime-gpu。两份 onnxruntime 可能**符号冲突**。

要回答两个问题:
1. `sherpa-onnx` 1.13.3 开 `cuda`(或等价)feature 后,能否正常编译链接、且 Silero VAD 仍工作?
2. **开 cuda feature 会不会把 CPU 那条路也从"静态 onnxruntime"变成"动态 onnxruntime"?**(若会,CPU 回退路径也得带 DLL,失去"无运行期 DLL"的干净 → 影响 §7 的双包/单包决策。)

spike 步骤建议:
```powershell
# 在 N 卡机器上,先查 sherpa-onnx crate 到底有哪些 feature
# 1) 看 crate 文档/源码的 [features]:cargo 文档或 https://docs.rs/sherpa-onnx/1.13.3
# 2) 临时开 feature 试编译(单独建分支,别动主线)
cd src-tauri
cargo build --features gpu-cuda 2>&1 | Tee-Object build-gpu.log
# 关注:链接期符号重复(duplicate symbol / multiply defined)、onnxruntime 版本冲突
# 3) 编过后跑起来,确认 VAD 仍能分段、识别正常、且日志里 provider 真的是 CUDA(没静默回退)
```
spike 结论决定后续:能共存 → 继续;冲突 → 要么换 sherpa 的 GPU 预编译包、要么把 VAD 与 ASR 拆成两份 onnxruntime(更大改)。

---

## 5. L1/L2 — 探测与微基准

### 5.1 硬件探测(`gpu.rs`)
- Windows:`windows` crate 的 DXGI `EnumAdapters1` → 取 `VendorId`(NVIDIA = 0x10DE)、`Description`、`DedicatedVideoMemory`;有专用显存=独显。可选 NVML 取算力/型号。
- 产出:`struct GpuInfo { vendor: u32, name: String, dedicated_vram: u64, is_discrete: bool }`。
- macOS:`cfg(target_os = "macos")` → 无 NVIDIA;Apple Silicon(`cfg(target_arch="aarch64")`)标记 CoreML 可用。
- 没开 GPU feature 时:探测可保留,但 provider 一律 `cpu`。

### 5.2 ⚠️ "强独显"没有现成 API —— 用微基准实测,别用型号猜
- 反例:实测里 Quadro M2000M **是独显但比 CPU 慢**。所以"检测到独显就上卡"会误判老/弱卡。
- **正解:启动(或某引擎首次加载)时跑一次微基准,GPU vs CPU 各推一小段,谁快用谁,结果按指纹缓存。**
  - 自动覆盖所有情况:强独显→用 GPU;弱独显/核显→回退 CPU;不用维护 GPU 强弱清单,新卡也不过期。

### 5.3 微基准 + 指纹缓存
- 指纹:`gpu_fp = hash(vendor + name + vram + engine + sherpa_ver)`。
- 缓存(`settings.json` 新增,不破坏现有字段):
  ```jsonc
  "providerCache": {
    "<gpu_fp>:senseVoice": { "provider": "cpu", "gpuMs": 41, "cpuMs": 18, "decidedAt": "..." },
    "<gpu_fp>:qwen3Asr":   { "provider": "cuda", "gpuMs": 120, "cpuMs": 470, "decidedAt": "..." }
  }
  ```
- 决策流程(每引擎首次或指纹变化时):
  1. 无可用 GPU EP → `cpu`,不测。
  2. 否则跑微基准:固定一段 ~3s 16k 样本,GPU/CPU 各 `transcribe` 数次取中位数(warmup 首次丢弃)。
  3. **轻量引擎**:GPU 中位数 < CPU × 0.9 才切 GPU,否则 CPU。
  4. **重量引擎**:阈值放宽(GPU 不慢于 CPU × 1.2 就用,精度/吞吐优先),或"有可用 GPU 直接上"。
  5. 写缓存;同指纹同引擎下次直接读,**不重测**(避免每次启动卡 1~2s)。
- 用户可在设置里覆盖(auto / 强制 cpu / 强制 gpu)。

---

## 6. macOS 兼容(必须先泼的冷水)
1. **更底层拦路虎**:`src-tauri/src/capture.rs` 的 macOS 采集是 `bail!` 占位,**整个 app 在 Mac 上跑不起来**。要 Mac 真可用,得先做 ScreenCaptureKit / CoreAudio 环回采集(已在 Cargo.toml 引了 `screencapturekit`/`core-media-rs` 0.3,但逻辑未实现)——这是独立大工程,**不在本方案范围内**。
2. **链接物不同**:Mac 不链接 Windows 那个 `.lib`,要换 sherpa-onnx 的 macOS 构建(dylib/xcframework),且要带 CoreML 的版本。"多套构建"在跨平台下天然就要做,正好和 GPU 多构建需求合流。
3. **GPU 收益**:Apple Silicon CPU 本身很快,SenseVoice 跑 CPU 绰绰有余;CoreML 主要对 Qwen3 这类重模型有意义。Intel Mac 基本只有 CPU 路径。
4. **provider 取值要平台门控**:`cuda`/`directml` 仅 Windows,`coreml` 仅 Mac。前端下拉按平台动态给选项,别让 Mac 用户看到 CUDA。
5. **本方案对 Mac 的承诺**:第 1 步能保证**代码编译通过、provider 逻辑分支正确**;Mac 真正可用要等采集实现。

---

## 7. 分发:CPU 包 vs GPU 包

| 方案 | 怎么发 | 优点 | 缺点 |
|---|---|---|---|
| **A. 两个包(推荐)** | CPU 小包 + GPU 大包 | CPU 版完全不动现状、体积小;出问题不波及 CPU 主线 | 两份产物、两条 CI;用户要选对 |
| B. 一个大包(都带 CUDA) | 单包始终含 CUDA DLL,运行时探测 | 用户不选 | 所有人白下几百 MB;要验证无卡机能正常启动回退 |
| C. 小包 + 按需下载 GPU 组件 | 发 CPU 大小的包,探到 N 卡才下 CUDA DLL | 默认小、单一安装包 | 工程最复杂;依赖"开 cuda feature 不污染 CPU 静态路径"(= §4.4 spike 结果) |

- **现阶段选 A**,守住"CPU 版零变化"红线。
- 等 §4.4 spike 确认"cuda feature 不拖累 CPU 静态路径"后,若想要"一个包自动适配"体验,再升级到 C。**没验证前别赌 B/C。**

---

## 8. 待核实 / 风险清单(开工前逐项确认)
- [ ] ⚠️ **sherpa-onnx 1.13.3 的 GPU feature 确切名 + 与现有静态 onnxruntime 是否冲突**(§4.4)——决定可行性,**第一优先**。
- [ ] ⚠️ 开 cuda feature 是否把 CPU 路径也变成动态 onnxruntime(影响双包/单包决策)。
- [ ] CUDA/cuDNN runtime 分发策略(随包带 vs 要求用户装)。
- [ ] 装机包体积(CPU 版保持小,GPU 版单独发)。
- [ ] 重量版到底用 0.6B(走 sherpa,推荐)还是 1.7B(自转 ONNX / vLLM sidecar,后续课题)。
- [ ] DirectML(核显路径):实测收益差,默认不做,L1/L3 留接口即可。
- [ ] macOS:CoreML 构建 + 采集占位未实现 → Mac 真可用是后话。

---

## 9. 文件级改动地图(速查)
| 文件 | 改什么 |
|---|---|
| `src-tauri/Cargo.toml` | 加 `[features] gpu-cuda`;(spike 时)透传 sherpa cuda feature |
| `src-tauri/src/gpu.rs`(新增) | L1 探测 + L2 微基准 + 指纹缓存 |
| `src-tauri/src/asr.rs` | `load()` 加 `provider` 参数;两处 `config.model_config.provider = ...`;provider 纳入 `fingerprint` |
| `src-tauri/src/lib.rs` | 启动调用探测;重量版无卡拦截 + emit 错误事件 |
| `src-tauri/src/config.rs` | `RuntimeConfig` 加 `provider`(+ 可选 `providerCache`) |
| `src-tauri/tauri.conf.json` | GPU 版 bundle 带 CUDA DLL |
| `src/types.ts` | `RuntimeConfig` 加 `provider`;`DEFAULT_SETTINGS` 同步;camelCase |
| `src/components/SettingsPanel.tsx` | 设备下拉(平台门控)+ 无卡禁用重量版 + 实际设备反馈 |

---

## 10. 决策记录(为什么这么定)
- **GPU 自适应值得做**:产品面向异构用户群(有卡/无卡),不是个人工具。一套 app 自动适配两拨人。
- **守红线**:默认构建保持 CPU 静态,GPU 走 opt-in feature,CPU 用户零影响。
- **轻量版默认 CPU**:实测 GPU 对小模型单路无收益,弱卡更慢;SenseVoice CPU 已 50× 实时。
- **重量版优先 GPU + 无卡拦截**:Qwen3 自回归重,强独显才划算;没卡的用户明确提示不可用。
- **用微基准而非型号清单判"强卡"**:Quadro M2000M 反例证明"独显≠强";实测最稳、自动兼容核显/弱卡/强卡/Mac。
- **分发选两个包**:守红线、隔离风险;单包留待 spike 验证后再说。
- **重量版先用 0.6B**:1.7B 无 sherpa ONNX,单列后续课题,不绑死 GPU 方案。

---

## 11. 到家第一步 TL;DR
1. 切到一个新分支。
2. **先做 §4.4 的 CUDA spike**(`cargo build --features gpu-cuda` 看链接冲突 + provider 是否真 CUDA + VAD 是否仍工作)。这一步决定一切。
3. spike 通过 → 按 §3 第 1 步(纯 CPU 骨架,零风险)→ 第 2 步(CUDA 双包)→ 第 3 步(微基准)。
4. spike 失败(符号冲突)→ 先解决 onnxruntime 共存(换 sherpa GPU 预编译包 / 拆 VAD 与 ASR),再继续。

---

## 12. 决策记录:单包 = 纯 CPU,GPU 暂缓

> 2026-06:经联网核实 sherpa-onnx 官方发布与构建文档后,决定**只出一个纯 CPU 安装包**,GPU 加速无限期搁置。此前为「有 N 卡走 CUDA」加的代码骨架(`gpu.rs`、`provider` 贯穿、`gpu-cuda` feature)已**全部回退**,代码回到纯 CPU 干净状态。

**为什么不做 GPU(核实结论):**
- **DirectML 路线不可行**:sherpa-onnx 的 CMake **没有 DirectML 开关**,官方也无 DML 预编译库。虽然 onnxruntime 本身支持 DML,但 sherpa 这层接不进去,单包靠 DirectML 全设备通吃的设想落空。
- **Windows GPU 只有 CUDA 源码构建一条路**:官方 Windows 预编译库全是 CPU-only(shared/static、MT/MD);GPU 仅能「从源码构建(With NVIDIA GPU)」,且只支持 CUDA 11.8 / 12.x。
- **重且收益有限**:CUDA 单包要打进 cuDNN/cuBLAS 等 ≈1GB 运行库,还有版本锁死与无 N 卡机器无法构建/测试的问题。而默认的 SenseVoice int8 是小 CTC 模型,int8 量化在 GPU 上反而是雷区,iGPU 更没收益;只有重量级 Qwen3 + 独立强 N 卡才划算。

**若将来要做 GPU(远期可选):** 需在有 N 卡的机器上,拿到/自编 CUDA 版 sherpa-onnx 原生库,构建时设 `SHERPA_ONNX_LIB_DIR` 覆盖默认 CPU 下载,改为动态链接并把 CUDA/cuDNN DLL 打进安装包;`provider` 字段本身是 pass-through,真正能否走 CUDA 取决于链接进去的原生库是否含 CUDA EP。届时再重新评估按需下载 GPU 运行库的方案。

