# 版本策略（VERSIONING）

> 本文件是版本号更迭的唯一权威规则。日常开发由实施变更的会话/代理按规则自动执行，无需用户手动管理。

## 生成代号与 semver 的关系

- **生成代号 HyperSoundEngine vN ↔ semver MAJOR = N**。
- 当前线：**HyperSoundEngine v1**，对应 semver `1.x`（及其预发布阶段 `0.x`）。
- 历史上的 WaveForge v1/v2/v3 引擎谱系已废止；现引擎从 v1 重新计起，仓库内不得再出现旧谱系描述。

## 版本载体

| 载体 | 位置 | 规则 |
|------|------|------|
| npm 包版本 | `package.json` `version`（TS 支线） | 两支线版本号保持一致 |
| Rust 版本 | `HyperSoundEngineRust/` 各 `Cargo.toml` | 与 package.json 同步 |
| 分享串版本 | `src/engine/ShareCodec.ts` `SHARE_CODEC_VERSION` | 独立演进，见下 |
| 规格版本 | `specs/`（每个模块规格标注适用的引擎版本） | 随功能演进 |

## Bump 规则

| 级别 | 触发条件 |
|------|----------|
| **MAJOR（= 新生成代号 HyperSoundEngine v(N+1)）** | 破坏兼容契约三层之一：① `AudioEngine` 接口语义；② 参数模型 / 场景预设 / 分享串格式；③ 引擎服务进程控制协议 |
| **MINOR** | 新增功能、新增规格与测试向量（向后兼容） |
| **PATCH** | 行为不变的修复与优化（既有对拍向量结果不变） |

## 测试向量与规格纪律（双支线防漂移）

- 已冻结向量的期望值**永不修改**；行为需要变化时=新增向量（MINOR）或替换+MAJOR。
- 删除向量仅允许在 MAJOR 进行。
- 功能"完成"的定义：规格落定 + TS/Rust 两支线对拍双绿；版本号由**后完成的一方**对齐 bump。

## 分享串（ShareCodec）

- `SHARE_CODEC_VERSION` 当前为 `2`（v2 载荷 = 仅存与默认参数的差异项 + HSE2/Crockford Base32 分组传输）；v1 全量载荷旧串持续可解码（迁移语义）；encode 恒写当前值，decode 接受 1/2 两代。
- 升级版本号必须走 MAJOR 并提供旧串迁移逻辑；禁止静默兼容多版本。

## CHANGELOG 纪律

- 未发布变更累积在 `CHANGELOG.md` 顶部 `## [Unreleased]` 段。
- 发布（版本 bump）时把 Unreleased 固化为 `## [x.y.z] - YYYY-MM` 新段。
- 判断"功能是否已存在"先查 CHANGELOG。

## 标识符与命名规范（防止版本字样再次入侵代码）

- 代码标识符、localStorage 键、自定义事件名、CSS 动画名、worklet 文件 URL 一律**无版本前缀**或使用 `hse-` / `hse` 前缀（如 `hseHearingPlay`、`hypersound:hse-*`、`/hse-worklet.js`、`hse-panel-in`）。
- 行文中需要指明引擎版本时写作 **HyperSoundEngine v1**（可简称 HSE v1）。
- 禁止在上述标识位置再引入 `v1/v2/v3` 字样；例外：第三方名称（GPLv3、Freeverb3 等）与文档自身修订号。

## 公开标识符命名分层（2026-08-22 审计确立）

| 层级 | 规则 | 示例 |
|------|------|------|
| 引擎/宿主/UI/协议级类与类型 | **必须 `HyperSoundEngine*` 前缀** | `HyperSoundEngine`、`HyperSoundEngineHost`、`HyperSoundEngineParams`、`HyperSoundEngineMixingStudio` |
| 泛词类（通用音频生态高撞名风险的单词/短词名） | **加 `Hse` 前缀，文件名同步** | `HseAudioBus`、`HseStretch`、`HseHearingTest`、`HseSeparationQueue`、`HseAudioEffectsProcessor` |
| DSP 行业域名类 | 允许无前缀（经包命名空间导出无歧义） | `Biquad`、`Compressor`、`Convolver`、`FdnReverb`、`Limiter`、`EqChain`、`ModulationMatrix` |
| 接口接缝 | 沿用架构文档既定名 | `AudioEngine`、`StereoProcessor`、`ProcessingStage`、`EngineStats` |
| 动词起头的工厂/工具函数 | 不强制前缀 | `createEngine`、`createDefaultParams`、`encodeWav`、`fft`、`designBiquad` |
| 运行时字符串（worklet 注册名/事件/存储键/CSS/URL） | `hypersoundengine` 或 `hse`/`hse-` 前缀 | `WORKLET_PROCESSOR_NAME='hypersoundengine'`、`hseHearingPlay` |

新增公开导出时按本表选层；拿不准时：类名偏高疑似泛词就加 `Hse`。
