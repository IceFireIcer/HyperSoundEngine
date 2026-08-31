# Rust 空间音频薄 ABI

本文定义 `hse-wasm` 导出的 C 风格空间音频 ABI。ABI 面向 wasm32 线性内存，也可由 native Rust/C 测试调用。可分发声明位于 `HyperSoundEngineRust/crates/hse-wasm/include/hypersoundengine_spatial.h`，`spatial_abi_version()` 与头文件的 `HSE_SPATIAL_ABI_VERSION` 当前均为 `1`。八个规划契约函数的符号名保持精确：

```c
uint32_t spatial_load_hrtf(const uint8_t *data, size_t data_len,
                           uint32_t sample_rate, uint32_t max_objects,
                           uint32_t max_frames);
int32_t spatial_get_hrir(uint32_t handle, float azimuth_deg, float elevation_deg,
                         float *out_l, size_t out_l_len,
                         float *out_r, size_t out_r_len);
int32_t spatial_render_objects(uint32_t handle,
                               const float *input, size_t input_len, size_t input_stride,
                               const uint32_t *object_slots, size_t object_slots_len,
                               const float *object_params, size_t object_params_len,
                               uint32_t object_count,
                               float *out_l, size_t out_l_len,
                               float *out_r, size_t out_r_len,
                               uint32_t frame_count);
int32_t spatial_set_room(uint32_t handle, float width, float height, float depth,
                         float reflectivity, uint32_t early_orders,
                         float rt60, float amount);
int32_t spatial_set_room_preset(uint32_t handle, uint32_t preset, float amount);
int32_t spatial_set_hrtf_interp_mode(uint32_t handle, uint32_t mode);
int32_t spatial_set_convolution_mode(uint32_t handle, uint32_t mode);
int32_t spatial_set_distance_model(uint32_t handle, uint32_t model,
                                   float reference_distance,
                                   float maximum_distance,
                                   float rolloff_factor);
```

辅助符号：

```c
uint32_t spatial_abi_version(void);
int32_t spatial_destroy(uint32_t handle);
int32_t spatial_reset_slot(uint32_t handle, uint32_t slot);
size_t spatial_hrir_length(uint32_t handle);
int32_t spatial_last_error_code(uint32_t handle);
size_t spatial_last_error_copy(uint32_t handle, uint8_t *out, size_t out_len);
```

## Handle 与所有权

- `spatial_load_hrtf` 在控制线程解析 caller-owned SOFA 字节，复制所需数据，预分配 renderer，并返回非零 handle。输入字节只在调用期间借用。SOFA 解析保持 `sofar` 的 `default-features=false`，不启用其 DSP renderer；源与目标采样率限 44.1/48/96k，采样率不同时在控制路径以确定性 129-tap Kaiser-windowed sinc 转换 HRIR。输出长度按 `ceil(source_len * target_rate / source_rate)` 计算，`Data.Delay` 先在源采样率验证为整数采样，再按物理时间四舍五入到目标采样点。
- handle 编码槽位和 generation；销毁后的旧 handle 会被拒绝。单线程最多同时持有 64 个 handle。
- handle 归调用方所有，必须以 `spatial_destroy` 释放。销毁后相关状态失效。
- ABI 注册表使用 thread-local 状态，不跨 worker/thread 共享。创建、控制、render、销毁应在同一个 AudioWorklet/worker 线程调用，因此 render 不需要锁。
- `spatial_load_hrtf` 失败返回 `0`；此时以 handle `0` 查询全局 last-error。其他函数返回负状态时以实际 handle 查询实例 last-error。

## 指针、长度与布局

所有 `*_len` 都是对应指针元素数，不是字节数；`object_slots_len` 计 `uint32_t` 元素，其余 render 缓冲长度计 `float` 元素。`float` 是 IEEE-754 `f32`，wasm32 下 `size_t`/指针是 32 位。所有 `float*` 和 `uint32_t*` 必须按 4 字节对齐。

调用方必须保证非空指针指向当前 wasm 实例线性内存中的有效连续范围。ABI 会拒绝 null、未对齐、地址/长度算术溢出、容量不足及已知重叠区间；C/Rust 裸指针本身无法证明任意非空地址确实已映射，因此传入悬垂或越界非空地址仍违反调用契约。

`spatial_render_objects` 的输入布局：

```text
input: object-major planar mono
  object 0: input[0 .. frame_count]
  object 1: input[input_stride .. input_stride + frame_count]
  ...
  要求 input_stride >= frame_count
  要求 input_len >= object_count * input_stride

object_slots: 每对象一个 uint32_t
  object_slots[object_index] 是该对象的稳定状态槽位
  要求 object_slots_len >= object_count
  每块内不得重复，且每个值必须小于 load 时的 max_objects

object_params: 每对象连续四个 f32
  [azimuth_deg, elevation_deg, distance, gain]
  要求 object_params_len >= object_count * 4

output:
  out_l[0 .. frame_count]
  out_r[0 .. frame_count]
```

左右输出之间不得重叠，输入、槽位或参数的实际使用区间也不得与任一输出重叠。所有角度、距离和增益必须有限，距离不得小于 0。对象数和帧数不得超过 load 时的 `max_objects` / `max_frames`。

renderer 的卷积 history、空气吸收滤波和写指针按 `object_slots` 中的稳定槽位绑定，而不是按当前对象数组顺序绑定。因此对象删除、重排或暂时缺席不会把历史串到其他对象；暂时缺席的槽位会保留状态，直到对象再次出现、调用 `spatial_reset_slot`，或销毁 handle。`spatial_reset_slot` 只清指定槽位的对象状态，不清 room 状态；槽位越界返回 `SPATIAL_INVALID_ARGUMENT`。

`spatial_hrir_length` 返回每耳所需的 `f32` 元素数，调用方据此准备缓冲。`spatial_get_hrir` 把当前插值模式选出的 HRIR 写入左右输出，成功返回同一长度；容量不足返回 `SPATIAL_BUFFER_TOO_SMALL`，不会部分写入。

## 状态与枚举

| 值 | 常量 | 含义 |
|---:|---|---|
| `0` | `SPATIAL_OK` | 成功 |
| `-1` | `SPATIAL_INVALID_HANDLE` | handle 为零、过期或不存在 |
| `-2` | `SPATIAL_INVALID_ARGUMENT` | 指针、长度、枚举或数值参数非法 |
| `-3` | `SPATIAL_BUFFER_TOO_SMALL` | caller-owned 缓冲不足 |
| `-4` | `SPATIAL_PARSE_ERROR` | SOFA 解析/校验失败 |
| `-5` | `SPATIAL_CAPACITY_EXCEEDED` | 预分配容量或 handle 表容量超限 |
| `-6` | `SPATIAL_UNSUPPORTED` | 已知但尚未实现的能力 |
| `-7` | `SPATIAL_INTERNAL_ERROR` | 核心控制路径内部失败 |

枚举值：

| 参数 | `0` | `1` | `2` | 其余 |
|---|---|---|---|---|
| room preset | studio | hall | stage | 另有 `3=church, 4=outdoor, 5=bathroom, 6=corridor` |
| HRTF interp | nearest | spherical L=3 | - | invalid argument |
| convolution | time | partitioned | - | invalid argument |
| distance model | inverse | linear | exponential | invalid argument |

`hrtf-core` renderer 支持直接时域与均匀分区卷积。`spatial_set_convolution_mode(handle, 0)` 选择 time，值 `1` 选择 partitioned；切换发生在控制路径，会按 `RenderProfile` 重建 64/128 样本分区的 HRIR 频谱和全部工作缓冲，并清空卷积状态。当前 ABI 以 `LowLatency` profile 加载，因此 partitioned 延迟固定为 64 样本。partitioned 仅与 nearest 插值组合；与 spherical 的冲突返回 `SPATIAL_INVALID_ARGUMENT`，不会静默回退。

## Last-error 与实时约束

`spatial_last_error_copy` 返回包含末尾 nul 的所需字节数。`out` 为 null、未对齐或容量不足时只返回所需长度，不写入；消息是 UTF-8。成功调用会清除该 handle 的旧错误。

`spatial_load_hrtf`、其 SOFA 解析/重采样、HRIR 查询、`spatial_reset_slot` 和所有 `set_*` 都属于控制路径，可以分配。SOFA 加载完成后会严格校验输出长度、规则轴与所有 HRIR 样本有限性。`spatial_render_objects` 只校验 POD 参数、借用 caller-owned slice，并调用 prepare 后的 `process_planar`：不得分配、释放、加锁、解析字符串或格式化错误。render 错误消息来自固定静态文本并写入实例内固定容量缓冲。
