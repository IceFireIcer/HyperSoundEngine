# engine-chain —— HyperSoundEngine 1–21 级整链规格

> **适用范围**：HyperSoundEngine v1；模块 id 为 `engine-chain`。本规格冻结 TS `HyperSoundEngine` 主链第 1–21 级的组装行为，供 Rust `EngineChainStage` 对拍。当前 5 组链级向量已纳入统一门禁，使总计达到 72/72 PASS。
>
> **空间边界**：第 22 级不在本规格内；参数必须满足 `spatial.mode="off"`，Rust 对非 off 值直接拒绝。

## 一、范围与链序

驱动器必须构造真实 `HyperSoundEngine(sampleRate, 2)`，并按以下固定顺序处理：

1. loudness-normalization
2. surround3d
3. mid-side
4. pre-eq
5. deesser
6. compressor
7. night-mode
8. delay
9. chorus
10. flanger
11. phaser
12. tremolo
13. reverb
14. bass-enhancer
15. loudness-compensation
16. ieq-post
17. analysis
18. dynamic-eq
19. lufs
20. mod-master-gain
21. limiter

第 22 级 `spatial` 不属于本规格，每条向量必须令 `spatial.mode="off"`。`HseStretch` 通过 `getStretch()` 暴露且不内联主链，同样不属于本规格。`analysis` 与 `lufs` 在整链中保持状态并读取信号，但不修改音频；本规格只冻结音频 stream，不把分析结果或 LUFS 读数混入同一向量，也不改变 meter/readings 绑定契约。

## 二、参数与驱动模型

向量 `params` 固定形态为：

```json
{
  "overrides": {}
}
```

驱动器先调用 `createDefaultParams(sampleRate)`，再把 `overrides` 深合并到默认快照：plain object 递归合并，数组、TypedArray 与标量整体替换。合并后必须确认 `spatial.mode="off"`，调用一次 `engine.setParams(fullParams)`；随后把输入按向量 `blockSize` 顺序分块，末块允许缩短，每块调用：

```ts
engine.process([inputLBlock, inputRBlock], [outputLBlock, outputRBlock])
```

同一引擎实例跨块保持全部状态。不得额外调用 `reset()`、`getAnalysis()`、`getStats()` 或 `getStretch()`。

`engine-chain` 是普通 `stream` 模块，沿用总纲四段 `.f32` 布局与相对容差公式。为兼容现有 TS/Rust 扫描器，冻结文件放在 `specs/dsp/vectors/engine-chain.<case>.{json,f32}`；目录位置不改变其引擎层规格归属。

## 三、行为条款

### GWT-CHAIN-01：全旁路逐位直通
- **给定（Given）**：全部可选 1–21 音频级关闭，M/S 为 `stereoWidth=1` 且 voice balance 不生效，空间模式关闭。
- **当（When）**：以冻结块长处理左右不同的确定性输入。
- **则（Then）**：输出与输入逐位一致；analysis/LUFS 取样不得改写音频。

### GWT-CHAIN-02：固定 128 帧多级组装
- **给定（Given）**：归一化、环绕、M/S、EQ、动态器、NightMode、五种调制效果、算法混响、低频增强、响度补偿、IEQ、DynamicEq、调制主增益与 Limiter 同时开启。
- **当（When）**：始终以 128 帧块处理确定性节目输入。
- **则（Then）**：输出符合冻结向量，固定链序、级间数据流或任一级门控差异均会被对拍检出。

### GWT-CHAIN-03：NightMode、IEQ 与 DynamicEq 跨块
- **给定（Given）**：三者同时开启，驱动块长既不整除 IEQ 的 2048 样本分析窗，也不整除 DynamicEq 的控制块。
- **当（When）**：处理有声后转静音的输入并保留同一实例状态。
- **则（Then）**：分析更新、动态包络与短末块共同产生的输出符合冻结向量。

### GWT-CHAIN-04：实时 LUFS 归一化启动边界
- **给定（Given）**：实时 LUFS 归一化开启，采样率 48000，400ms 首个测量窗为 19200 样本。
- **当（When）**：以 128 帧块连续越过该边界。
- **则（Then）**：首个完整读数产生前归一化增益保持 0dB；读数由第 19 级在边界块尾记录，下一块起第 1 级按该读数驱动 3 秒平滑增益。向量只比较音频输出，不承载 LUFS 标量读数。

### GWT-CHAIN-05：调制矩阵同时驱动双目标
- **给定（Given）**：同一矩阵包含指向 `masterGain` 与 `stereoWidth` 的两条活动路由。
- **当（When）**：每块先以未处理输入推进 LFO 与包络，再执行整链。
- **则（Then）**：块级 stereo width 在第 3 级 M/S 生效，块级 master gain 在第 20 级生效；两者组合输出符合冻结向量。

### GWT-CHAIN-06：默认工厂短尾只推进有效帧
- **给定（Given）**：通过默认 `createEngine` 创建两个同参数引擎，其中一个先按较大容量调用 `prepare`，另一个不预分配；两者接收相同的确定性变长块序列，序列包含短于预分配容量的块。
- **当（When）**：逐块调用公开 `process`，且处理器包含可观察跨块相位的 Tremolo。
- **则（Then）**：两引擎每块输出逐位一致；短尾不得按内部容量补零处理，也不得推进容量尾部对应的 DSP 状态。case 形状由 `vectors/frame-count.v1.json` 与 `../schema/frame-count.schema.json` 机械校验；旧 72 组音频向量继续使用 `legacyPaddedTail` 重放，不修改冻结期望值。

## 四、冻结向量

- `all-bypass-bitexact`：全旁路逐位锚点。
- `multistage-128`：1–21 多级同时开启，固定 128 帧块。
- `night-ieq-dynamic-crossblock`：NightMode、IEQ、DynamicEq 的跨块状态。
- `lufs-normalization-400ms`：实时归一化跨 400ms 首测边界。
- `mod-dual-target`：调制矩阵同时驱动 master gain 与 stereo width。

全部期望输出仅由 `scripts/export-vectors.mjs` 驱动 TS 事实实现生成；既有文件逐字节不一致时导出器必须拒绝覆盖。Phase 4 的 40 组广域参数扫描另由 [`param-scan.md`](param-scan.md) 与 `scripts/phase4-param-scan.mjs` 管理，不计入旧 72 组逐样本向量。
