# params —— 引擎参数模型兼容契约

> **适用范围**：HyperSoundEngine v1 的 `HyperSoundEngineParams`；TS 行为事实源为 `src/types.ts` 的 `createDefaultParams`。本规格冻结跨支线可交换的参数字段与 48 kHz 默认快照。

## 一、契约边界

1. 参数是完整快照，不是补丁；`setParams` 必须整体替换当前快照。
2. 字段名、对象层级、数组顺序、标量类型与枚举字符串属于兼容契约。新增、删除、改名或改变默认值必须先更新共享规格并同步两支线。
3. `sampleRate` 由调用方传入；本批冻结基准使用 `48000`。
4. `reverb.convolution.ir` 是运行时数据，默认值为 `null`；分享串不携带 IR 数组，只保留 `irName`。
5. `spatial` 在默认快照中存在且 `mode="off"`。缺省兼容属于读取旧快照的迁移能力，不改变新快照必须完整携带该字段的要求。
6. 可选兼容字段在 `createDefaultParams` 产物中仍须显式出现：`deesser.sidechainEnabled=false`、`compressor.sidechainEnabled=false`、`bassEnhancer.lowBoostDb=0`。

## 二、顶层字段顺序

48 kHz 默认快照固定按以下顺序序列化：

```text
sampleRate, eq, deesser, compressor, nightMode, bassEnhancer, reverb,
surround3d, loudnessCompensation, loudnessNormalization, limiter, ieq,
dynamicEq, pitch, modulation, modEffects, hearing, spatial, stereoWidth,
sceneId, customized
```

JSON 对象的语义比较不依赖键序；该顺序用于确定性导出与分享串编码。数组有顺序语义：EQ 段、Dynamic EQ 段、调制路由和空间对象列表不得重排。

## 三、行为条款

### GWT-PARAMS-01：默认快照完整性
- **给定（Given）**：采样率 48000。
- **当（When）**：调用 `createDefaultParams(48000)`。
- **则（Then）**：结果与 `vectors/default-params.48000.json` 的 `params` 结构化全等，不得缺字段或包含非有限数。

### GWT-PARAMS-02：采样率参数化
- **给定（Given）**：任意有限的宿主采样率。
- **当（When）**：创建默认快照。
- **则（Then）**：仅顶层 `sampleRate` 取调用值，其余默认字段保持同一契约形状和值。

### GWT-PARAMS-03：新快照相互隔离
- **给定（Given）**：以相同采样率连续创建两个默认快照。
- **当（When）**：修改其中一个快照的嵌套对象或数组。
- **则（Then）**：另一个快照不受影响；默认工厂不得泄漏共享可变引用。

### GWT-PARAMS-04：分享串默认骨架
- **给定（Given）**：48 kHz 默认快照。
- **当（When）**：使用当前 `ShareCodec` 编码并解码。
- **则（Then）**：编码结果与 `vectors/share-codes.48000.json` 的 `default` case 完全一致，解码结果与原快照结构化全等。

## 四、冻结夹具

- `vectors/default-params.48000.json`：完整默认参数快照。
- `vectors/share-codes.48000.json`：默认参数与 12 个内置场景的 v2 分享串 golden。

唯一生成入口为 `node scripts/export-engine-contracts.mjs`。目标存在时逐字节一致才允许通过；不一致必须拒绝覆盖。该导出器不得写入 `specs/dsp/vectors/`，既有 72 组 DSP 音频向量及其 144 个文件保持不变。
