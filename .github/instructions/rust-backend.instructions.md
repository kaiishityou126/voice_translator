---
description: "Rust 后端(src-tauri)开发约定:三线程流水线、anyhow 错误传播、有界 mpsc 通道、serde camelCase、Tauri 命令与事件。"
applyTo: "src-tauri/src/**/*.rs"
---

# Rust 后端约定(src-tauri)

## 错误处理

- 所有可失败函数返回 `anyhow::Result<T>`,用 `?` 传播,用 `bail!("中文原因")` 主动报错。
- 面向用户的错误信息用**中文**;特殊约定:模型缺失返回 `Err("MODEL_MISSING:{tier}")`,前端据此触发下载。
- 不要吞掉错误;ASR/翻译失败时 `emit_status("error", ...)` 后 `continue`,不中断流水线。

## 三线程流水线([pipeline.rs](src-tauri/src/pipeline.rs))

- 捕获 → ASR → 翻译三线程,用**有界** mpsc 通道串联(`seg_tx` bounded=6,`txt_tx` bounded=32)。
- 通道满时用 `try_send` 丢弃并发出提示(如 `[翻译积压,已跳过]`),**绝不**改成无界通道堆积。
- 段落按 `id` 递增编号;先发 `{id, original, pending:true}`,翻译完按同一 id 回填 `{id, translated, pending:false}`。

## 配置与序列化

- `RuntimeConfig`([config.rs](src-tauri/src/config.rs))用 `#[serde(rename_all = "camelCase")]`,字段须与 [types.ts](src/types.ts) 的 `Settings` 一一对应。
- **改动后端字段必须同步改 [types.ts](src/types.ts)**,否则前后端 IPC 静默错位。

## Tauri 命令与事件

- 命令定义在 [lib.rs](src-tauri/src/lib.rs);共享状态用 `Mutex<>` 保护 `AppState` 中的 pipeline/sidecar。
- 后端只用 `emit` 发 `subtitle` / `status` 两类事件;不要新增前端轮询接口。
- 主窗口关闭事件 = 清理 sidecar 进程 + 停止流水线,改动窗口生命周期时勿破坏此清理路径。

## 平台与 sidecar

- Windows 音频采集走 WASAPI([capture.rs](src-tauri/src/capture.rs));macOS 路径用 `bail!` 占位,不要误删。
- whisper sidecar 后端按 CUDA > OpenBLAS > CPU 自动选择([asr.rs](src-tauri/src/asr.rs)),HTTP `/inference` @ 端口 8178。

## 校验

改完用 `cd src-tauri; cargo check` 快速验证编译,不必启动整个应用。代码注释用中文。
