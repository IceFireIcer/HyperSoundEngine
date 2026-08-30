# 规格：wav —— legacy / standard RIFF/WAVE 文件编解码

> **规格属性**：本文件是双支线共享规格（I/O 层，非流式 DSP 模块）。TS 实现位于
> `src/io/wav.ts`，Rust 实现位于 `HyperSoundEngineRust/crates/hse-core/src/wav.rs`。
> 编码输出是跨实现逐字节一致契约；本模块允许堆分配，不进入实时音频回调。

## 一、模块概述

- 支持 16-bit 有符号 PCM（formatTag=1）与 32-bit IEEE-754 float（formatTag=3）。
- 声道采用 WAV 交错存储，API 输入输出采用非交错 f32 声道数组。
- 不压缩、不改采样率、不做响度归一。
- 编码提供 `legacy` 与 `standard` 两种容器模式；解码自动识别两者。
- 同输入、同参数必须得到相同字节或相同解码结果。

## 二、公开接口

TS：

```ts
export type WavContainerFormat = 'legacy' | 'standard'
export interface WavEncodeOptions {
  bitDepth?: 16 | 32
  format?: WavContainerFormat
}
export function encodeWav(channels: Float32Array[], sampleRate: number, opts?: WavEncodeOptions): ArrayBuffer
export function decodeWav(buffer: ArrayBuffer | Uint8Array): WavDecodeResult
```

`bitDepth` 缺省为 16，`format` 缺省为 `legacy`。

Rust：

```rust
pub enum WavContainerFormat { Legacy, Standard }
pub fn encode_wav(...) -> Result<Vec<u8>, String> // legacy 兼容入口
pub fn encode_wav_with_format(..., format: WavContainerFormat) -> Result<Vec<u8>, String>
pub fn decode_wav(bytes: &[u8]) -> Result<WavData, String>
```

原 `encode_wav` 签名与行为保持不变，避免破坏已有结构体字面量和调用方。

## 三、两种容器模式

| 字段 | `legacy` | `standard` |
|---|---|---|
| Chunk ID (`RIFF`/`WAVE`/`fmt `/`data`) | ASCII 原字节 | ASCII 原字节 |
| 所有头部数值字段 | 大端 | 小端（RIFF/WAVE 标准） |
| PCM16 / float32 样本 | 小端 | 小端 |
| 无 `format` 的编码默认值 | 是 | 否，必须显式选择 |
| data 声明大于实际文件 | 兼容历史行为，按实际完整帧截取 | 拒绝 |
| PCM16 `0x8000` 解码 | `-32768 / 32767`（历史行为） | `-1.0` |

legacy 是 1.0.0 及更早版本冻结的 `RIFF + 大端数值头 + 小端样本` 私有变体，不是标准 RIFF。保留它仅为字节契约兼容。standard 的所有 RIFF 数值字段均按小端读写，可与外部 WAV 工具互操作。

## 四、编码语义

### 4.1 公共语义

- 至少一个声道；各声道帧数必须相等；零帧合法。
- 位深 16：PCM16；位深 32：IEEE float32。
- PCM16 量化在两种模式下相同：样本钳制到 `[-1, 1]`，乘 32767，再按 JavaScript `Math.round` 的半值向正无穷规则舍入；NaN 写 0，±Infinity 写 ±32767。
- float32 按 f32 位模式原样小端写出。
- 多声道按帧交错。

### 4.2 standard 入口约束

- `sampleRate` 必须是 `1..=u32::MAX` 的整数。
- 声道数必须在 `1..=u16::MAX`。
- `BlockAlign` 与 `ByteRate` 必须分别可由 u16 与 u32 表示，否则报 `encodeWav: blockAlign exceeds WAV limit` / `encodeWav: byteRate exceeds WAV limit`。
- data size 与 RIFF `ChunkSize` 必须可由 u32 表示，否则报 `encodeWav: data size exceeds RIFF limit`；本模块不生成 RF64。
- 头部 `ChunkSize`、`ByteRate`、`BlockAlign` 与 `Subchunk2Size` 必须由实际输入精确计算并按小端写入。

legacy 保留原有 JavaScript 数值转换与头字段环绕语义；旧编码 golden 不得修改。

## 五、解码识别与扫描

1. 文件至少 44 字节，偏移 0/8 必须为 ASCII `RIFF`/`WAVE`。
2. 优先比较偏移 4 的两种端序与 `文件长度 - 8`。仅小端匹配时识别为 standard；仅大端匹配时识别为 legacy。
3. 对总长歧义或截断文件，分别按两种端序扫描 chunk，能定位到格式/位深组合合理的 `fmt ` 解释作为辅助判别；两种解释都成立时优先 legacy。standard 身份一旦成立，总长不匹配必须报错，不能退回 legacy。
4. chunk ID 始终按 ASCII 原字节识别；chunk size 与 fmt 数值字段按识别出的模式读取。
5. 未知 chunk 按 `8 + size + (size % 2)` 跳过；data 前可有多个 fmt，以最后一个为准。
6. data 出现后停止扫描。

## 六、standard 严格校验

standard 在公共格式/位深校验后依次执行：

1. `sampleRate > 0`，否则 `decodeWav: invalid sampleRate`。
2. `BlockAlign == channels × bytesPerSample`，否则 `decodeWav: blockAlign does not match format`。
3. `ByteRate == sampleRate × BlockAlign`，否则 `decodeWav: byteRate does not match format`。
4. `dataOff + dataLen == 文件长度`，否则 `decodeWav: data chunk size does not match file length`。
5. `dataLen` 可整除计算出的 blockAlign，否则 `decodeWav: data length not aligned to block size`。

RIFF 声明总长不等于实际文件长度时优先报：

`decodeWav: RIFF size does not match file length`

legacy 不启用上述新增严格字段一致性检查，继续兼容历史截断与额外尾字节行为。

## 七、共同错误契约

以下历史错误字符串及顺序保持不变：

- `decodeWav: file too short (<44 bytes)`
- `decodeWav: bad RIFF magic`
- `decodeWav: bad WAVE magic`
- `decodeWav: missing fmt chunk`
- `decodeWav: missing data chunk`
- `decodeWav: fmt chunk too small`
- `decodeWav: channel count must be >= 1`
- `decodeWav: unsupported bit depth <N>`
- `decodeWav: PCM format requires 16-bit`
- `decodeWav: float format requires 32-bit`
- `decodeWav: unsupported format tag <N>`
- `decodeWav: data length not aligned to block size`
- `Offset is outside the bounds of the DataView`

standard 新增错误见 §六；TS 抛 `Error`，Rust 返回 `Err(String)`，字符串逐字一致。

## 八、验收契约

### GWT-WAV-01：legacy 默认行为冻结

- **给定**：1.0.0 的 13 个编码与 24 个解码 golden。
- **当**：调用无 format 的 TS `encodeWav` / Rust `encode_wav` 及自动解码。
- **则**：编码字节、解码位模式与错误消息全部不变。

### GWT-WAV-02：standard 跨支线逐字节一致

- **给定**：`specs/io/vectors/wav-standard.json` 的 PCM16 与 float32 输入。
- **当**：两支线显式选择 standard 编码。
- **则**：输出与手工冻结的标准 RIFF 字节逐字节一致。

### GWT-WAV-03：双模式自动解码

- **给定**：legacy 与 standard 合法文件。
- **当**：调用同一个解码入口。
- **则**：无需调用方提供模式即可还原采样率、位深、声道与样本。

### GWT-WAV-04：standard 严格拒绝畸形头

- **给定**：RIFF/data 长度截断、错误 blockAlign、错误 byteRate 或零采样率。
- **当**：解码 standard 文件。
- **则**：按 §六返回确定性错误，不返回部分音频。

### GWT-WAV-05：PCM16 最小码

- **给定**：standard PCM16 样本字节 `00 80`（-32768）。
- **当**：解码。
- **则**：结果恰为 f32 `-1.0`；legacy 仍保留历史 `/32767` 行为。

### GWT-WAV-06：未知 chunk 与多声道

- **给定**：fmt 与 data 之间含奇数长度未知 chunk，或 1/2/6/8 声道输入。
- **当**：按任一模式编解码。
- **则**：chunk 按偶数字节对齐跳过，声道顺序与帧数保持。

## 九、冻结资产与门禁

- legacy：Rust `wav.rs` 内嵌 37 个 1.0.0 golden，期望值永不修改。
- standard：`specs/io/vectors/wav-standard.json`，由 TS 与 Rust 测试共同读取；编码期望是独立手工核算的标准 RIFF hex，不由待测编码器生成。
- TS 门禁：`npx vitest run test/wav.test.ts test/public-api.test.ts`。
- Rust 门禁：`cargo test -p hse-core wav --locked -j 1`。
- WAV 为离线 I/O 工具，不进入 DSP 72 组音频向量计数，也不适用实时零分配要求。

## 十、关联文件

- `src/io/wav.ts`
- `test/wav.test.ts`
- `HyperSoundEngineRust/crates/hse-core/src/wav.rs`
- `HyperSoundEngineRust/benches/benches/bench_wav.rs`
- `adapters/waveforge/attachEngine.ts`
- `specs/io/vectors/wav-standard.json`
