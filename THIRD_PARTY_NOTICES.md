# THIRD-PARTY NOTICES（第三方许可声明）

本模块核心代码为**自主实现**（纯 TypeScript，零运行时依赖），但算法概念、公开公式与部分实现思路
来源于以下开源项目与标准。按各自许可条款，这里保留版权声明；移植/参考时已在对应源文件头部注释出处。

## 概念/公式来源（实现为自主代码）

| 来源 | 许可 | 用途 | 上游来源 |
|---|---|---|---|
| RBJ Audio EQ Cookbook（Robert Bristow-Johnson） | 公开文档 | biquad 系数公式 | https://webaudio.github.io/Audio-EQ-Cookbook/ |
| DSPFilters（Vinnie Falco） | MIT（源码头声明） | TDF2 状态机思路 | https://github.com/vinniefalco/DSPFilters |
| kissfft（Mark Borgerding） | BSD-3-Clause | FFT 蝶形分解思路 | https://github.com/mborgerding/kissfft |
| Freeverb（Jezar @ Dreampoint） | 公有领域 | 梳状+全通混响结构 | https://ccrma.stanford.edu/~jos/pasp/Freeverb.html |
| stk（Perry R. Cook & Gary P. Scavone） | MIT 等价宽松许可 | 算法混响参考 | https://github.com/thestk/stk |
| DaisySP（Electro-Smith） | MIT | 压限/混响接口思路 | https://github.com/electro-smith/DaisySP |
| signalsmith-basics（Signalsmith Audio） | MIT | 效果器紧凑实现思路 | https://github.com/Signalsmith-Audio/basics |
| signalsmith-stretch（Signalsmith Audio） | MIT | 变速变调（可选 WASM 依赖） | https://github.com/Signalsmith-Audio/signalsmith-stretch |
| meyda（Hugh Rawlinson 等） | MIT | 频谱特征定义的概念参考；未作为当前 npm 依赖 | https://github.com/meyda/meyda |
| speexdsp（Xiph.Org） | BSD-3-Clause | 多相重采样思路 | https://gitlab.xiph.org/xiph/speexdsp |
| ITU-R BS.1770-4 / EBU R128 | 标准（公开） | LUFS 测量公式与 K 加权系数 | https://www.itu.int/rec/R-REC-BS.1770 |
| ISO 226:2003 | 标准（公开） | 等响度曲线（本模块为简化近似） | https://www.iso.org/standard/34222.html |
| YIN（de Cheveigné & Kawahara, 2002） | 学术公开 | 音高检测算法 | https://doi.org/10.1121/1.1458024 |
| spleeter（Deezer） / demucs（Meta） | MIT | 声源分离（离线适配层，模型权重按各仓库声明） | https://github.com/deezer/spleeter / https://github.com/facebookresearch/demucs |
| crepe（Marl/CMU） | MIT | 音高检测（离线可选） | https://github.com/marl/crepe |

## npm 开发依赖

- `playwright-core`：Apache-2.0，Copyright Microsoft Corporation。仅用于 headless Chromium AudioWorklet E2E，不进入引擎运行时包。

## Rust 运行时与构建依赖

| 库 | 许可 | 用途 |
|---|---|---|
| `rustfft` | MIT OR Apache-2.0 | Rust HRTF 分区卷积 FFT |
| `sofar` | MIT OR Apache-2.0 | 纯 Rust SOFA / HDF5 解析控制路径；禁用默认 DSP/resample features |
| `wasapi` | MIT | Windows shared/exclusive 音频设备后端 |
| `rtrb` | MIT OR Apache-2.0 | 服务实时 SPSC 环形缓冲 |
| `wasm-bindgen` | MIT OR Apache-2.0 | Rust wasm32 边界与浏览器 glue |

`sofar` 基于 BSD-3-Clause 的 libmysofa 设计进行纯 Rust 重写；分发 Rust 二进制时应同时保留其 MIT/Apache-2.0 许可和项目内列出的上游归属。仓库当前不捆绑第三方 SOFA 数据文件；用户提供的数据集许可由调用方负责，未来随包加入数据集前必须单独记录来源、版本、校验和、署名与再分发条款。

## 可选 npm 依赖（MIT）

- `signalsmith-stretch`：MIT，Copyright (c) 2022 Geraint Luff / Signalsmith Audio Ltd.

## LGPL（仅宿主侧；引擎包零 LGPL 依赖）

> 2026-08-22：引擎包的可选依赖 soundtouchjs（含 `vendor/` 原包副本、`StretchLgplAdapter.ts` 与其测试）已整体移除；引擎包现无任何 LGPL 依赖。以下仅涉及宿主/融合侧。

| 库 | 许可 | 使用方式（不修改） | 合规要点 |
|---|---|---|---|
| @soundtouchjs/audio-worklet | LGPL-2.1 | WaveForge 宿主侧实时变速变调（`adapters/waveforge/attachEngine.ts` 依赖宿主自有包），以"未修改、链接调用"方式 | 库作为宿主独立依赖分发，不并入我方源码；随附 LICENSE；不修改其源码 |
| FFmpeg 滤波器（f_ebur128 等）/ libsoxr / sox | LGPL | 仅作公式与算法对照；如需引入须保持"未修改、独立链接"，并随附 LICENSE | 同上原则 |
| ebur128（Rust crate）等 | 视具体项目 | 引入前核对 SPDX | — |

> LGPL 合规红线（即便允许链接）：不修改 LGPL 源码、不静态合并其代码进我方文件、
> 分发时随附许可文本与源码获取途径、保留版权声明。

## 明确不引入（GPL/AGPL 类，仅概念对照）

- Rubber Band（GPL）、Essentia（AGPL）、Freeverb3 / zita-rev1 / zita-convolver（GPL）、Audacity（GPL）。
- pitch-time-example-code（无 LICENSE 文件，仅阅读）。

## 合规执行规则

1. 引入可选 npm 依赖时在分发物附本文件。
2. 移植/借鉴代码时保留源文件头版权注释（已在各源文件实现）。
3. 模型权重（spleeter/demucs/crepe）分发时保留各仓库 LICENSE。