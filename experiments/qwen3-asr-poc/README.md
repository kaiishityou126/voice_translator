# Qwen3-ASR-0.6B-int8 离线 POC

目的:在**不碰主工程**的前提下,离线验证 Qwen3-ASR-0.6B-int8 两件事:

1. **准确率** —— 专有名词/同音词是否识别对(对照 SenseVoice 的错误,如 高市早苗 / 特例公債 / 給付付き税額控除);
2. **纯 CPU 逐段延迟(RTF)** —— 这台开发机无 N 卡,RTF 决定能否实时上字幕。

## 一次性:下载模型(约 940MB,不进 git)

```powershell
cd experiments\qwen3-asr-poc
# 模型(tar.bz2 约 940MB,解压后约 940MB)
curl.exe -L -o model.tar.bz2 https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25.tar.bz2
tar -xf model.tar.bz2
del model.tar.bz2
```

解压后得到 `sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25/`,内含:

| 文件 | 大小 | 说明 |
|------|------|------|
| `conv_frontend.onnx` | 42M | 卷积前端 |
| `encoder.int8.onnx` | 174M | 编码器 |
| `decoder.int8.onnx` | 721M | LLM 解码器(自回归,延迟主要在这) |
| `tokenizer/` | — | merges.txt / vocab.json / tokenizer_config.json |
| `test_wavs/` | — | 官方多语种测试音频(含 `ja1.wav` 日语) |

## 编译

```powershell
cargo build --release
```

首次会自动下载 sherpa-onnx 1.13.3 预编译静态库(win-x64-static-MT)。

## 运行

```powershell
# 先拿官方日语样例 + 绕口令跑通
.\target\release\qwen3-asr-poc.exe `
  .\sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25 `
  .\sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25\test_wavs\ja1.wav `
  .\sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25\test_wavs\raokouling.wav `
  --threads 4
```

### 用热词(直接打专有名词)

```powershell
.\target\release\qwen3-asr-poc.exe `
  .\sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25 `
  <你的日语国会辩论.wav> `
  --threads 4 `
  --hotwords "高市早苗,特例公債,給付付き税額控除,飲食料品"
```

> **重点:Qwen3-ASR 支持热词**(SenseVoice CTC 不支持),可把固定专有名词直接喂进去纠偏。

## 看什么

- 每条打印 `音频 Xs | 解码 Ys | RTF Z`。**RTF<1 即比实时快**;配合主工程三线程并行,单段 6s 解码大致 <1.5s 不积压即可接受。
- 官方在 Mac/CPU(num_threads=2)实测 RTF ≈ 0.08–0.17(约 6–13× 实时)。本机 Iris Xe 会慢些,以实测为准。
- `text` 与 ground truth(`test_wavs/transcript.txt`)对比看准确率。

## 准入门槛(决定是否进 Phase 2 落地)

准确率明显优于 SenseVoice **且** 本机 CPU 单段延迟可接受 → 才改 `src-tauri/src/asr.rs` 落地。
