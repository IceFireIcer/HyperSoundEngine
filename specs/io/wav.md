# 规格：wav —— 标准（含双支线共同变体）RIFF/WAVE 文件编解码

> **规格属性**：本文件是双支线共享规格（I/O 层，非流式 DSP 模块）。行为事实标准 =
> `src/io/wav.ts`；Rust 支线实现 = `HyperSoundEngineRust/crates/hse-core/src/wav.rs`。
> 与 `specs/dsp/` 流式模块"相对容差 1e-6、跨实现不要求逐位一致"不同，本模块的
> 编码输出是**跨实现逐字节一致**契约（见 §四），解码错误消息逐字一致（见 §七）。
> 本模块为纯文件编解码：不接触音频设备、不进入实时音频回调、允许堆分配。

---

## 一、模块概述

- **定位**：引擎核心内置的音频文件读写入口。编码把非交叠 f32 声道写成 WAV 字节流；
  解码把 WAV 字节流还原为非交叠 f32 声道（可直接装配为引擎的多声道输入总线）。
- **格式范围**：16-bit 有符号 PCM（消费级）与 32-bit IEEE-754 float（专业级）；
  多通道（≥1 声道）交叠存储；不压缩、不改采样率、不做响度归一。
- **防注入语义**：解码对一切畸形输入显式报错（不静默跳过、不部分返回），与分享串
  解码的白名单拒收语义同源；错误消息字符串本身是契约的一部分。
- **确定性**：同输入、同参数必得同输出字节/同解码结果；无随机、无时钟、无控制台输出。

## 二、接口签名（事实标准摘录）

TS（`src/io/wav.ts`）：

```ts
export interface WavDecodeResult { sampleRate: number; channels: Float32Array[]; bitDepth: 16 | 32 }
export interface WavEncodeOptions { bitDepth?: 16 | 32 }   // 缺省 16
export function encodeWav(channels: Float32Array[], sampleRate: number, opts?: WavEncodeOptions): ArrayBuffer
export function decodeWav(buffer: ArrayBuffer | Uint8Array): WavDecodeResult   // 畸形输入抛 Error
```

Rust（`hse-core::wav`）——语义一一对应，错误通道以 `Result` 表达：

```rust
pub enum WavBitDepth { Pcm16, Float32 }
pub struct WavEncodeOptions { pub bit_depth: WavBitDepth }        // Default = Pcm16
pub struct WavData { pub sample_rate: u32, pub channels: Vec<Vec<f32>>, pub bit_depth: u16 }
pub fn encode_wav(channels: &[&[f32]], sample_rate: u32, opts: &WavEncodeOptions) -> Result<Vec<u8>, String>
pub fn decode_wav(bytes: &[u8]) -> Result<WavData, String>
```

Rust 侧的收窄（均为不可达路径的静态化，不改变可观察行为）：

- TS `opts?.bitDepth ?? 16` 的非法位深错误（`bitDepth must be 16 or 32`）由
  `WavBitDepth` 枚举在类型层排除；
- TS `sampleRate` 的非有限/负数分支由 `u32` 入参排除；`sample_rate == 0` 仍按
  TS 语义报 `invalid sampleRate`。

## 三、格式支持矩阵

| 维度 | 支持值 | 说明 |
|---|---|---|
| 位深 | 16 / 32 | 16 = 有符号 PCM（formatTag=1，编码默认）；32 = IEEE-754 float（formatTag=3） |
| 声道数 | ≥ 1 | 编码端 0 声道报错；解码端 0 声道报错；上限受 u16 字段宽度约束（编码端按模 2¹⁶ 截断写入，属病态域，向量不得依赖） |
| 帧数 | ≥ 0 | 0 帧合法（仅 44 字节头）；各声道必须等长 |
| 采样率 | 编码端 > 0 | 头字段原样写入；解码端不做范围校验（原样透传） |
| chunk | fmt + data 必需 | 未知 chunk（LIST/INFO/JUNK 等）跳过；data 之后不继续扫描 |
| 扩展 | fmt size ≥ 16 | size < 16 报错；size > 16 的扩展字段忽略 |

## 四、RIFF/WAVE 布局与字节序契约

### 4.1 头部布局（44 字节，字段全部大端）

| 偏移 | 长度 | 字段 | 值 |
|---|---|---|---|
| 0 | 4 | ChunkID | `'RIFF'`（ASCII，大端 u32 = 0x52494646） |
| 4 | 4 | ChunkSize | `bufferSize − 8`（ToUint32 环绕） |
| 8 | 4 | Format | `'WAVE'` |
| 12 | 4 | Subchunk1ID | `'fmt '` |
| 16 | 4 | Subchunk1Size | 16 |
| 20 | 2 | AudioFormat | 1（PCM）/ 3（IEEE float） |
| 22 | 2 | NumChannels | 声道数 |
| 24 | 4 | SampleRate | 采样率 Hz |
| 28 | 4 | ByteRate | `sampleRate × blockAlign`（ToUint32 环绕） |
| 32 | 2 | BlockAlign | `channels × bytesPerSample`（ToUint16 环绕） |
| 34 | 2 | BitsPerSample | 16 / 32 |
| 36 | 4 | Subchunk2ID | `'data'` |
| 40 | 4 | Subchunk2Size | `frames × blockAlign`（ToUint32 环绕） |
| 44 | N | 交叠样本数据 | 小端 |

> **双支线共同变体（契约）**：TS 实现的头部数值字段按**大端**读写
> （`DataView` 的 `littleEndian=false`），与 RIFF 标准的小端头字段不同；
> **样本数据为小端**（PCM16 有符号补码 / float32 位模式）。这是两侧实现
> 共同锁定的历史行为：本模块解码端只承诺识别**本规格编码端**产出的文件形态。

### 4.2 逐字节一致保证（编码）

- **给定**：两侧以同一批 f32 位模式（bit pattern）作为声道输入、同采样率、同位深。
- **当**：分别调用 TS `encodeWav` 与 Rust `encode_wav`。
- **则**：输出字节序列逐字节相等——头字段值与排列一致、交叠顺序一致、
  PCM16 量化字节一致、float32 位模式一致（含 denormal、−0、Infinity）。

### 4.3 数值语义（编码）

- PCM16：样本先钳制到 [-1, 1]（NaN 不受钳制影响），再乘 **32767**（非 32768）
  后按 **Math.round 半值向 +∞** 舍入：`0.5 → 16384`、`−0.5×32767 → −16383`、
  `0.25×32767 → 8192`；`±1 → ±32767`（−1 不得写 −32768）；NaN 经 JS
  `ToInt32` 语义落 0（字节 `00 00`）；±Infinity 钳制到 ±1。
- float32：按 f32 位模式原样小端写出（TS `setFloat32(…, true)`）。
- 解码 PCM16：i16 先升 f64、除以 **32767**、再收窄 f32（TS 先 f64 除法后写入
  Float32Array；f32 直除可能在末位产生不同位模式，禁止）。
- 解码 float32：f32 位模式原样读入。

## 五、chunk 扫描语义（解码）

- 从偏移 12 起扫描：读 chunk ID（4 字节）与 size（4 字节，均大端）；
  `ID == 'fmt '` 读取字段，`ID == 'data'` 记录数据区起点并**停止扫描**；
  其余 chunk 跳过，推进量 = `8 + size + (size % 2)`（2 字节对齐，奇数长度补位）。
- 扫描循环条件 `off + 8 ≤ 文件长度`：尾部残缺的 chunk 头静默结束扫描
  （随后按缺 chunk 报错）。
- fmt 字段读取位置：AudioFormat@+0、NumChannels@+2、SampleRate@+4、
  BitsPerSample@+14（各 2/2/4/2 字节，大端）。文件长度不足时视为越界读取
  （见 §七"DataView 越界"行）。
- `dataLen`（声明值）可大于文件实际剩余字节：按 `available = min(dataLen, 文件长 − 数据区起点)`
  取整帧数解码（截断容忍，不报错）；`dataLen` 小于实际剩余时按声明值截取。
- 多个 fmt chunk：以**最后一个**（data 之前）为准。

## 六、GWT 行为条款

### GWT-WAV-01：float32 编码解码往返逐位一致
- **给定**：任取 f32 样本集（含 0、±0.5、±0.25、±1、denormal、−0、±Infinity）。
- **当**：以 bitDepth=32 编码后立即解码。
- **则**：解码声道数/帧数/采样率与输入一致，样本按 f32 位模式逐位相等。

### GWT-WAV-02：PCM16 往返量化误差有界
- **给定**：256 帧正弦 ×0.8 的单声道（f32）。
- **当**：以默认位深（16）编码后解码。
- **则**：逐样本误差 < 1.7e-5（量化步长 ~3.05e-5，半步 ≈1.5e-5）。

### GWT-WAV-03：PCM16 钳制与 32767 标度
- **给定**：含 ±2.0、±1.5 的超量程样本。
- **当**：以 PCM16 编码。
- **则**：编码字节为 ±32767（非 ±32768）；解码值钳制在 [-1, 1]。

### GWT-WAV-04：Math.round 半值向 +∞
- **给定**：量化中间值恰为半值的样本（如 s×32767 = ±16383.5）。
- **当**：以 PCM16 编码。
- **则**：正半值向上取整、负半值向零方向取整（`Math.round(−16383.5) = −16383`），
  编码字节与 TS 逐字节一致（golden 用例 `enc_1ch_pcm16_edge`）。

### GWT-WAV-05：NaN 样本量化为 0
- **给定**：声道样本含 NaN。
- **当**：以 PCM16 编码。
- **则**：该样本写出字节 `00 00`（ToInt32(NaN)=0），同行其他样本不受影响。

### GWT-WAV-06：多声道交叠写与非交叠读
- **给定**：6 声道（5.1）× 128 帧，声道 c 填 (c+1)×0.1。
- **当**：float32 编码后解码。
- **则**：6 个声道各 128 帧、逐位还原；data 区第 k 帧为 6 个声道的第 k 样本依序交叠。

### GWT-WAV-07：未知 chunk 跳过与奇数长度对齐
- **给定**：fmt 与 data 之间插入 7 字节 LIST 与 3 字节 JUNK（均奇数长度）。
- **当**：解码。
- **则**：两个未知 chunk 被跳过且按 2 字节对齐推进，data 正确定位、样本完整解码。

### GWT-WAV-08：data 声明长度与实际字节不符的截断容忍
- **给定**：合法文件，把 data size 字段改写为大于实际剩余字节的值。
- **当**：解码。
- **则**：不报错；帧数 = min(声明值, 实际剩余) ÷ blockAlign 向下取整；
  反向（声明值小于实际剩余）按声明值截取，尾部字节忽略。

### GWT-WAV-09：编码参数校验（错误消息为契约）
- **给定**：空声道表 / 各声道不等长 / 采样率 0。
- **当**：调用编码。
- **则**：报错，消息逐字为 `encodeWav: at least one channel required` /
  `encodeWav: all channels must have equal length` / `encodeWav: invalid sampleRate`。

### GWT-WAV-10：零帧音频仅产出头部
- **给定**：1 或多声道、每声道 0 样本。
- **当**：编码（任一位深）。
- **则**：输出恰为 44 字节头（data size = 0），可被本规格解码端解出 0 帧结果。

### GWT-WAV-11：畸形输入一律报错（由单元/golden 测试覆盖，不入向量）
- **给定**：§七表列的全部畸形输入。
- **当**：解码。
- **则**：返回错误且消息与 §七表逐字一致；任何情况下不静默返回部分结果。

### GWT-WAV-12：fmt 中 formatTag=0 等同缺失 fmt
- **给定**：存在 fmt chunk 但 AudioFormat 字段为 0。
- **当**：解码。
- **则**：报 `decodeWav: missing fmt chunk`（formatTag=0 与"未见 fmt"不可区分，
  node 实测行为）。

## 七、畸形输入拒绝表（消息逐字为契约）

| 输入形态 | 错误消息（逐字） |
|---|---|
| 文件 < 44 字节 | `decodeWav: file too short (<44 bytes)` |
| 偏移 0 非 `'RIFF'` | `decodeWav: bad RIFF magic` |
| 偏移 8 非 `'WAVE'` | `decodeWav: bad WAVE magic` |
| data 之前无 fmt（含 fmt 缺失、formatTag=0） | `decodeWav: missing fmt chunk` |
| 扫描结束未见 data | `decodeWav: missing data chunk` |
| fmt chunk size < 16 | `decodeWav: fmt chunk too small` |
| NumChannels = 0 | `decodeWav: channel count must be >= 1` |
| BitsPerSample ∉ {16, 32}（如 24） | `decodeWav: unsupported bit depth 24`（数值随实际值） |
| AudioFormat=1 且 BitsPerSample=32 | `decodeWav: PCM format requires 16-bit` |
| AudioFormat=3 且 BitsPerSample=16 | `decodeWav: float format requires 32-bit` |
| AudioFormat ∉ {1, 3}（如 2） | `decodeWav: unsupported format tag 2`（数值随实际值） |
| dataLen 不整除 blockAlign | `decodeWav: data length not aligned to block size` |
| fmt 字段读取越过文件末尾（文件 ≥ 44 字节） | `Offset is outside the bounds of the DataView`（对齐 node RangeError 原话） |

校验顺序即上表顺序（先文件长度、后魔数、后 chunk 存在性、再参数合法性、
最后对齐）；同一输入命中多条时以最先命中者为准。

## 八、黄金用例（golden 对拍）

- **生成方法**：以 esbuild 将 TS 事实标准 `src/io/wav.ts` 打包为 CJS，在 node 中
  执行 `encodeWav`/`decodeWav`，产出冻结 JSON：
  - `enc` 组：输入声道以 **f32 位模式 8 位十六进制**给出（杜绝十进制解析歧义）、
    采样率、位深（null = 不传 opts）；`want` 为整文件字节 hex（ok）或抛错消息（err）；
  - `dec` 组：输入为整文件字节 hex；`want` 为解码结果（采样率、位深、各声道
    f32 位模式 hex，ok）或抛错消息（err）。
- **消费方式**：golden 冻结在 `hse-core/src/wav.rs` 测试模块常量中，Rust 测试逐
  case 复算并断言（编码逐字节、解码逐位、错误消息逐字）。TS 侧行为由
  `test/wav.test.ts` 覆盖，不依赖 golden 文件。
- **冻结约束**：golden 期望值永不修改；行为变更 = 新增 case 或走主版本。

冻结 case 清单（13 编码 + 24 解码）：

- 编码 ok（10）：`enc_2ch_pcm16_sine`（2 声道 16 帧正弦/余弦）、`enc_2ch_f32_sine`、
  `enc_1ch_pcm16_default`（不传 opts）、`enc_1ch_pcm16_edge`（钳制/半值/NaN/±Infinity/
  ±1 共 15 样本）、`enc_odd_2ch_pcm16`（3 帧奇数）、`enc_odd_2ch_f32`、
  `enc_6ch_f32_51`（5.1）、`enc_zero_frames_1ch`、`enc_zero_frames_2ch_f32`、
  `enc_f32_special`（canonical NaN、±最小 denormal、−0、±MAX）；
- 编码 err（3）：空声道、声道不等长、采样率 0；
- 解码 ok（7）：`ok_data_declared_larger`（声明 8 帧实际 4）、
  `ok_data_declared_smaller`（声明 1 帧实际 8，尾部忽略）、`ok_list_chunk_skip`
  （LIST+JUNK 奇数对齐）、`ok_fmt_size_18`（fmt 扩展）、`ok_two_fmt_chunks`
  （后者覆盖前者）、`ok_roundtrip_from_encode_f32`、`ok_roundtrip_from_encode_pcm16`；
- 解码 err（17）：§七表全部 13 类（其中"过短"两组：20 字节与 43 字节；魔数组两组；
  fmt 越界读两组：bits 字段越界 / sampleRate 字段越界）另加 `err_tag_0`。

## 九、关联文件

- 总纲契约：[`specs/README.md`](../README.md)
- 行为事实标准（TS）：`src/io/wav.ts`；TS 测试：`test/wav.test.ts`
- Rust 实现：`HyperSoundEngineRust/crates/hse-core/src/wav.rs`（golden 常量内嵌）
- 兄弟规格：`specs/service/push-stream.md`（推流协议）、`specs/service/control-plane.md`（控制面契约）
