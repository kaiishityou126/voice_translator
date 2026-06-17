---
description: "前端(React + TypeScript)开发约定:函数式组件、事件监听、字幕 id 回填、设置持久化、camelCase 对齐。"
applyTo: "src/**/*.{ts,tsx}"
---

# 前端约定(React + TypeScript)

## 组件与状态

- 一律函数式组件 + hooks,**无 Redux/状态库**;字幕只保留最近 200 条。
- 类型集中在 [types.ts](src/types.ts);新增配置字段同时改 `Settings`、`DEFAULT_SETTINGS` 和对应 Rust 端 `RuntimeConfig`。

## 事件驱动

- 用 `listen<T>("subtitle" | "status", ...)` 监听 Rust 事件;**不要轮询**后端。
- 字幕**按 id 回填**:同一 `id` 先到 `pending:true` 的原文,再到 `pending:false` 的译文;用 `findIndex` 命中则替换、否则追加(见 [App.tsx](src/App.tsx))。

## 设置持久化

- 改动 onChange → `saveSettings()`(fire-and-forget,Rust 同步落盘);**不要**自己写 localStorage 为主存储。
- 用 `userTouched` ref 防止异步 `loadSettings()` 覆盖用户正在输入的值,改动加载逻辑时保留此保护。

## 约定

- 字段名用 **camelCase**,与 Rust `#[serde(rename_all="camelCase")]` 对齐。
- 面向用户文案用中文;保留原始 error 供 console 调试,并给恢复提示(如缺 API Key 的引导)。
- 悬浮窗是独立 Vite 入口([overlay-main.tsx](src/overlay-main.tsx) → [Overlay.tsx](src/components/Overlay.tsx)),拖动靠 `data-tauri-drag-region`。

## 校验

改完用 `npm run build` 做 TypeScript 类型检查 + 打包(不启动应用)。代码注释用中文。
