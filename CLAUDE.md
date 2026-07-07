# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

> **本仓库的权威开发文档统一为 [AGENTS.md](AGENTS.md)。请以它为准。**
> CLAUDE.md、[.github/copilot-instructions.md](.github/copilot-instructions.md) 都只是各工具的入口,内容一律回指 AGENTS.md,避免文档分叉漂移。

**开工前请先读 [AGENTS.md](AGENTS.md)**,它涵盖:项目概述与隐私模型、构建/校验命令、双向并发流水线架构、前后端模块地图、关键约定(camelCase 三处对齐、事件驱动、设置持久化、错误处理)、Tauri 命令/事件/信号码、模型与外部依赖、工具链、sherpa 能力储备、已知陷阱。

背景与设计文档:[README.md](README.md)、[语音翻译回灌会议-设计方案.md](docs/语音翻译回灌会议-设计方案.md)、[GPU加速与macOS兼容-设计方案.md](docs/GPU加速与macOS兼容-设计方案.md)。

改动系统约定/架构时,**先改 [AGENTS.md](AGENTS.md)**,不要在本文件里另写一份会漂移的副本。
