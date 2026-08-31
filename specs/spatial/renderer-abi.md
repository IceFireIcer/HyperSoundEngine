# 规格：renderer-abi —— Rust 双耳渲染器与 C ABI

> **规格属性**：共享的机械验证契约。数值对拍只覆盖 `nearest`、时域卷积、单声源与 room off；
> Rust 事实实现为 `hrtf-core::BinauralRenderer::process_planar`，C ABI 映射为
> `hse-wasm::spatial_abi::spatial_render_objects`。TS 侧只生成和验证共享夹具，不新增生产渲染器。

## 一、固定边界

- 输入为 object-major planar mono；第 `i` 个对象从 `input[i * inputStride]` 开始。
- 每对象另有一个稳定 `u32` slot，独立于对象在当前块中的排列；本单源夹具固定为 `objectSlots: [0]`。
- 每对象参数顺序固定为 `[azimuthDeg, elevationDeg, distance, gain]`。
- 输出为互不重叠的 planar stereo；全部长度和 stride 均以 `f32` 元素计，不以字节计。
- 本夹具固定单声源、48 kHz、nearest 插值、直接时域卷积；输出按相对容差判定。partitioned 为等价渲染模式：按 `RenderProfile` 使用 64/128 样本固定分区，输出相对 time 精确延迟一个分区，并在 Rust 测试中以绝对误差 `2e-5` 对照。
- grid 展平顺序为 elevation-major：`entry = elevationIndex * azimuthCount + azimuthIndex`。
- nearest 方位先规范到 `[-180,180)` 并按圆周距离选点；仰角先钳到 grid 范围。等距时保留较小索引。

## 二、距离与空气吸收

- 距离模型与 `hrtf-core::model` 一致：reference 内 unity，超过 maximum 后钳制，最终增益钳到 `[0,1]`。
- 空气吸收为一阶低通：`fc = min(4000 / (1 + distance), sampleRate / 2)`，
  `coefficient = 1 - exp(-2πfc/sampleRate)`，状态按对象跨块保持。
- 冻结夹具分别覆盖 inverse、linear、exponential、maximum clamp 与不同距离的空气系数。

## 三、GWT 条款

### GWT-SPATIAL-RENDER-01：nearest grid 选择
- **给定**：共享夹具的合成 grid，以及精确点、跨 ±180°、越界仰角和等距查询。
- **当**：选择 nearest HRIR。
- **则**：索引及左右 HRIR 与 `renderer-abi.v1.json` 完全一致。

### GWT-SPATIAL-RENDER-02：距离与空气衰减
- **给定**：夹具中的距离模型参数和距离 case。
- **当**：计算距离增益与空气吸收系数。
- **则**：结果在夹具相对容差内一致。

### GWT-SPATIAL-RENDER-03：单源时域渲染
- **给定**：nearest HRIR、单声源输入和 room off。
- **当**：按声明块长依次调用 planar renderer，末块允许短块。
- **则**：左右输出逐样本与冻结期望一致，且 delta case 保留左右不对称。

### GWT-SPATIAL-RENDER-04：reset 与 room off
- **给定**：已经处理过完整输入的 renderer。
- **当**：调用 `reset` 后按原分块重放；或配置 room 但 amount 固定为 0。
- **则**：reset 重放与初始输出一致；room zero 与 room off 走相同冻结输出。

### GWT-SPATIAL-RENDER-05：分区卷积等价与延迟
- **给定**：nearest HRIR、room off，以及 `LowLatency` 或 `Compatibility` profile。
- **当**：选择 partitioned 模式并以任意短块连续渲染。
- **则**：输出相对 time 模式延迟一个固定分区（分别为 64/128 样本），延迟后逐样本绝对误差不超过 `2e-5`；reset 与模式切换清空全部卷积状态。`LowLatency` 在 44.1/48/96 kHz 均严格小于 5 ms。

## 四、不做伪对拍的范围

- `spherical` 插值与非零 room 当前没有 TS 同算法实现，因此不生成跨语言数值期望。
- 两项仅由 Rust `renderer_features.rs` 的行为测试和 `realtime_alloc.rs` 的 prepare 后零分配测试覆盖。
- C ABI 的句柄生命周期、错误码、指针/重叠检查由 `hse-wasm::spatial_abi` 单元测试覆盖；共享夹具只冻结其成功路径的数据布局与 renderer 数值语义。

## 五、资产与门禁

- Schema：`specs/schema/spatial-renderer-abi.schema.json`。
- 数据：`specs/spatial/vectors/renderer-abi.v1.json`。
- 导出：`node scripts/export-spatial-vectors.mjs`，既有文件不一致时拒绝覆盖。
- TS：`npx vitest run test/spatial-spec-vectors.test.ts`。
- Rust：`cargo run --manifest-path HyperSoundEngineRust/Cargo.toml -q -p hse-parity`，任一 spatial fixture 失败均返回非零退出码。
