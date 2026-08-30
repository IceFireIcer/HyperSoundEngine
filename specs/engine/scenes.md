# scenes —— 内置场景与分享串兼容契约

> **适用范围**：HyperSoundEngine v1 的 12 个内置 `ScenePreset` 及其 v2 分享串。TS 行为事实源为 `src/engine/ScenePresets.ts` 与 `src/engine/ShareCodec.ts`。

## 一、固定场景集合

场景数量和顺序固定如下：

```text
pop, enhance, jazz, dance, classical, livehouse,
studio, warm, dts, vocal-stage, night-bass, heavy-bass
```

每个场景必须满足：

- `id` 与上述对应项一致，且全局唯一；
- `name`、`description`、`builtin` 和完整 `params` 都属于冻结内容；
- `builtin=true`、`params.sampleRate=48000`、`params.sceneId=id`、`params.customized=false`；
- 场景从默认参数派生，未覆盖字段保持默认值；
- `params.reverb.convolution.ir=null`，`params.spatial.mode="off"`。

## 二、分享串格式

1. 当前编码版本为 `2`，规范编码以 `HSE2-` 开头。
2. 编码载荷只保存相对相同采样率默认快照的差异，但始终携带 `sampleRate`。
3. JSON 字段顺序、FNV-1a 校验、Crockford Base32 和五字符分组共同决定规范字符串；同一参数必须得到同一字符串。
4. 分享串不携带卷积 IR 数组；解码后 `ir` 恒为 `null`，`irName` 保留。
5. 解码器继续接受历史 v1 全量载荷；本批 golden 只冻结当前 v2 规范编码。

## 三、行为条款

### GWT-SCENE-01：场景顺序与身份
- **给定（Given）**：内置 `SCENE_IDS` 与 `SCENE_PRESETS`。
- **当（When）**：按导出顺序读取两者。
- **则（Then）**：两者均恰有 12 项，ID 顺序与本规格一致，且逐项一一对应。

### GWT-SCENE-02：完整场景快照
- **给定（Given）**：48 kHz 的 12 个内置场景。
- **当（When）**：导出场景元数据及参数。
- **则（Then）**：结果与 `vectors/scenes.48000.json` 结构化全等，包括名称、描述以及所有继承和覆盖后的参数字段。

### GWT-SCENE-03：默认值继承
- **给定（Given）**：任一内置场景与 48 kHz 默认参数。
- **当（When）**：比较场景未定制的参数域。
- **则（Then）**：未覆盖字段与默认快照一致；场景至少覆盖一个听感相关字段，且不得共享可变参数对象。

### GWT-SCENE-04：规范分享串
- **给定（Given）**：默认参数及按固定顺序排列的 12 个场景参数快照。
- **当（When）**：调用 `encodeShareCode`。
- **则（Then）**：13 个编码结果逐项等于 `vectors/share-codes.48000.json`，再次编码同一快照结果不变。

### GWT-SCENE-05：分享串往返
- **给定（Given）**：冻结的 13 个规范分享串。
- **当（When）**：调用 `decodeShareCode`。
- **则（Then）**：`default` 还原为默认快照；每个场景 case 还原为对应场景的完整参数快照；再次编码得到原字符串。

## 四、冻结夹具

- `vectors/scenes.48000.json`：固定 ID 顺序及 12 个完整 `ScenePreset`。
- `vectors/share-codes.48000.json`：`default`、12 个场景及编码边界 case 的规范 v2 编码。

唯一生成入口为 `node scripts/export-engine-contracts.mjs`。夹具落库后不得静默更新；行为变化必须按版本规则处理。
