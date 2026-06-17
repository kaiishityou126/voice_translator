---
mode: agent
description: "引导式工作流:为 Voice Translator 添加一个新的翻译引擎,贯通后端分发、配置、前端类型与设置表单四处改动。"
---

# 添加新翻译引擎

为 Voice Translator 接入一个新的翻译引擎(例如 ${input:engineName:引擎标识,如 baidu/azure}),需在下面 **4 处**保持一致地改动。请逐项完成并在结束时自检对齐。

## 1. 后端分发 — [src-tauri/src/translate.rs](src-tauri/src/translate.rs)

- 在 `translate()` 的 `match cfg.translation_engine.as_str()` 中加一个分支,指向新的 `translate_<engine>()` 函数。
- 新函数签名参照现有 `translate_google` / `translate_deepl`:接收 `&reqwest::blocking::Client`、`&RuntimeConfig`、`&str`,返回 `anyhow::Result<String>`。
- 错误用 `bail!("中文原因")`;空响应要报错;需要 key 的引擎先校验 key 非空。
- 如需语言代码映射,仿照 `google_lang` / `deepl_lang` 加一个映射函数。

## 2. 配置字段 — [src-tauri/src/config.rs](src-tauri/src/config.rs)

- 若新引擎需要额外字段(base url / api key / model),加到 `RuntimeConfig`,保持 `#[serde(rename_all = "camelCase")]`。

## 3. 前端类型 — [src/types.ts](src/types.ts)

- 在 `TranslationEngine` 联合类型加入新值。
- 在 `TRANSLATION_ENGINE_LABEL` 加中文显示名。
- 若加了配置字段,同步加到 `Settings` 与 `DEFAULT_SETTINGS`(字段名 camelCase,与 `RuntimeConfig` 对齐)。

## 4. 设置表单 — [src/components/SettingsPanel.tsx](src/components/SettingsPanel.tsx)

- 在引擎下拉中加入新选项。
- 若新引擎有专属字段(如 API Key),加对应的条件渲染输入框,仅在选中该引擎时显示。

## 完成后自检

- 后端 `cfg.translation_engine` 字符串值与前端 `TranslationEngine` 取值**完全一致**。
- 所有新增字段名在 `RuntimeConfig`(camelCase serde)与 `Settings` 之间一一对应。
- 运行 `cd src-tauri; cargo check` 与 `npm run build` 两侧都通过。
- 不破坏 `"none"`(纯字幕)分支——它在 pipeline 层跳过,不进 `translate()`。

> 约定细节见 [AGENTS.md](AGENTS.md)、[.github/instructions/rust-backend.instructions.md](.github/instructions/rust-backend.instructions.md) 与 [.github/instructions/frontend.instructions.md](.github/instructions/frontend.instructions.md)。
