# Phase 4 基准矩阵留档 —— hse-benches 全模块 criterion 基准

> 日期：2026-08-29（Phase 4 性能矩阵轮）
> 范围：Rust 支线 `HyperSoundEngineRust/benches/`（crate `hse-benches`）。本文件只记录基准
> 数字与方法，不修改任何 hse-core / hse-service / TS 源码；新增基准与数字只进
> `benches/` 与本文件。
> 复现命令：`cd HyperSoundEngineRust && cargo bench -p hse-benches`
> （单链基准：`cargo bench -p hse-benches --bench bench_chain_full`）

---

## 一、范围与方法

### 1.1 覆盖清单（22 个 bench 目标文件：19 个功能基准 + 3 个既有 parity 基准）

| 组 | bench 文件 | 覆盖对象 |
|---|---|---|
| `parity_biquad` / `parity_limiter` / `parity_reverb_simple` | 既有文件（未改） | biquad / limiter（truePeak 开关对照）/ reverb-simple |
| `bench_eq_chain` | 新增 | eq-chain：10 段非零增益（±6dB 交错，向量 case1 的 (f,q) 集合）+ Q 补偿 |
| `bench_compressor` | 新增 | compressor（向量 case1 参数）+ sidechain 对照组 |
| `bench_bass_enhancer` | 新增 | bass-enhancer（向量 case1，even 谐波）+ lowBoostDb=+6 对照组 |
| `bench_deesser` | 新增 | deesser（向量 case2 开启形态，splitBand on） |
| `bench_dynamic_eq` | 新增 | dynamic-eq（向量 case2 开启形态，5 带，内部分析块 128） |
| `bench_mod_effects` | 新增 | mod-effects 五级级联全开（delay→chorus→flanger→phaser→tremolo，引擎接线顺序） |
| `bench_loudness_comp` | 新增 | loudness-comp（mode=auto / volumePercent=100，活跃目标曲线） |
| `bench_mid_side` | 新增 | mid-side（width 2.5，向量 case1） |
| `bench_modulation_matrix` | 新增 | modulation-matrix（lfo→masterGain 路由，向量 case1） |
| `bench_fdn_reverb` | 新增 | fdn-reverb（8 线，wet 0.3 / dry 0.7，与 reverb-simple 同快照口径对照） |
| `bench_convolver` | 新增 | convolver：delta IR + expNoise IR×2 长度（6000 / 1024），块长矩阵 + IR 场景组 |
| `bench_fft` | 新增 | fft：尺寸 1024/2048/4096/8192（块长 = N，L=Re / R=Im 双平面） |
| `bench_share_codec` | 新增 | share_codec：decode_share_code（固化 v2 HSE2 串，854 字符） |
| `bench_wav` | 新增 | wav：encode/decode × PCM16 / Float32（32768 帧立体声） |
| `bench_chain_full` | 新增 | **完整 1–21 级离线吞吐（§三指标）**：hse-service `ServiceEngineChain` 驱动 60s 音频 |
| `bench_hse_stretch` | 新增 | hse-stretch：rate 0.1/1/2/8 与 semitones -36/0/+12/+36 参数域，2048 帧块窗映射 |
| `bench_lufs_meter` | 新增 | lufs-meter：44.1/48/96kHz 与 128/512 块长计量路径，含四项读数读取 |
| `bench_engine_param_domain` | 新增 | `EngineChainStage` 默认、全旁路边界、全开上边界三种参数域，连续 128 帧分块 |
| `bench_service_path` | 新增 | 服务纯内存热路径：deinterleave / 三会话 mix / DSP / interleave / 双 rtrb 搬运及串联块路径，块长 128/256/512；不启动线程或真实音频 |

新增四组仅建立 criterion 可执行基准与 CI 编译门禁；本轮按严格切片要求只执行
对应目标的 `cargo bench ... --no-run --locked`，未运行固定时长性能测量，因此不在下文历史数字表中补造数据。

### 1.2 计时方法（对齐既有 parity 基准的口径）

- **场景口径**：48kHz / 立体声 / 主块长 128 帧（对齐 TS `scripts/benchmark.mjs` 与规划书
  §三实时目标口径）。
- **计时结构**：母带缓冲整个基准只填充一次；每次 criterion 迭代先把母带复制进工作缓冲、
  复位阶段状态，再连续推若干块。模块基准每迭代总帧数恒为 **32768 帧（≈0.68 s）**；
  全链基准每迭代 **2,880,000 帧（= 60 s，§三指标口径）**。
- **构造在计时区外**：全部 `prepare(max_block)` / `load_ir` / FFT twiddle 表 / 链装配都在
  计时区外完成（对齐实时铁律：分配只发生在音频回调之外）。
- **确定性输入**（无随机/时钟）：三音正弦叠加×慢变包络（峰值逼近满幅，越过限幅阈值）
  与固定种子 LCG 伪噪声（宽谱，喂卷积/FFT 平面）；全链另附恒定 0.1 输入（与 TS 脚本
  逐字同口径）。
- **块长矩阵**：128 / 256 / 512 / 1024（每迭代总帧数恒定、只改分块大小——组内差异即
  "每调用固定开销 × 分块粒度"的净效应）。全部为 32768 的约数；冻结向量域的非 2 幂
  块长（333/384/441…）保留给对拍 harness，基准统一用 2 幂便于矩阵对比。
- **throughput 语义**：`Throughput::Elements(帧数)` → criterion 报告的 elem/s 即 **帧(样本)/s**，
  ×realtime = elem/s ÷ 48000；ns/帧 = 1e9 ÷ elem/s。I/O 组（share/wav）按字节计。

### 1.3 criterion 设置与运行环境

| 组 | sample_size | warm_up | measurement |
|---|---|---|---|
| 既有 parity 三文件 | 40 | 3 s（默认） | 5 s（默认） |
| 新增模块/FFT/I/O 组 | 40 | 1 s | 3 s |
| `bench_chain_full`（60s/迭代） | 10 | 2 s | 10 s |

- 工具链：rustc/cargo 1.95.0（release profile，`cargo bench` 默认）。
- 机器（`Get-CimInstance` 实测）：**AMD Ryzen 9 7945HX**（Zen4，16 核 32 线程，笔记本
  平台）、物理内存可用 15.2 GB、Windows 11 专业版。
- 环境注记：笔记本 CPU 的频速调节与后台负载未做锁定控制，criterion 斜率估计的
  run-to-run 波动约 ±2%（本次全链两组独立运行差 1.5%~1.9%）；下表数字为单次全量
  运行的中位值，趋势结论不受该波动影响。

---

## 二、模块 × 块长矩阵（48kHz / 立体声 / 32768 帧/迭代）

吞吐 = 每秒处理的**帧**数（criterion elem/s 中位值）；括号内为换算的 ns/帧。

| 模块（参数口径） | block 128 | block 256 | block 512 | block 1024 |
|---|---|---|---|---|
| mid-side（width 2.5） | 1.718 G/s (0.58ns) | 1.753 G/s | 1.760 G/s | 1.770 G/s |
| modulation-matrix（lfo→masterGain） | 407.3 M/s (2.46ns) | 410.5 M/s | 411.7 M/s | 414.4 M/s |
| biquad（单节 peaking，parity） | 222.2 M/s (4.50ns) | 217.8 M/s | 214.3 M/s | —（parity 矩阵到 512） |
| bass-enhancer（even 谐波） | 208.6 M/s (4.79ns) | 212.1 M/s | 213.4 M/s | 215.2 M/s |
| loudness-comp（auto/100%） | 82.88 M/s (12.1ns) | 83.91 M/s | 83.98 M/s | 84.31 M/s |
| compressor（-6dB/4:1） | 58.66 M/s (17.0ns) | 58.60 M/s | 58.80 M/s | 58.68 M/s |
| dynamic-eq（5 带全开，内块 128） | 49.41 M/s (20.2ns) | 49.34 M/s | 49.67 M/s | 49.63 M/s |
| limiter（truePeak **开**，parity） | 40.58 M/s (24.6ns) | — | — | — |
| limiter（truePeak 关，parity） | 75.36 M/s (13.3ns) | — | — | — |
| deesser（splitBand on） | 35.61 M/s (28.1ns) | 35.82 M/s | 35.88 M/s | 35.90 M/s |
| fdn-reverb（8 线，wet .3） | 28.35 M/s (35.3ns) | 28.69 M/s | 28.79 M/s | 28.89 M/s |
| reverb-simple（hall，wet .3，parity） | 27.91 M/s (35.8ns) | — | — | — |
| eq-chain（10 段 ±6dB + Q 补偿） | 22.68 M/s (44.1ns) | 22.19 M/s | 21.81 M/s | 21.74 M/s |
| mod-effects（五级全开） | 18.86 M/s (53.0ns) | 18.96 M/s | 18.99 M/s | 18.99 M/s |
| convolver（delta IR，机制底噪） | — | — | 17.63 M/s (56.7ns) | — |
| convolver（expNoise L=1024, p256） | — | — | 7.354 M/s (136ns) | — |
| convolver（expNoise L=6000, p512） | 2.596 M/s (385ns) | 2.635 M/s | 2.601 M/s | 2.641 M/s |

对照参考（块长 128 单独跑的吞吐）：bass-enhancer lowBoost +6dB ≈ 无 lowBoost（214.1
对 208.6 M/s，差值在噪声内——低频架支路成本可忽略）；compressor sidechain 开 55.78
对关 58.66 M/s（单声道派生 +5%）。

**换算×realtime（block 128，吞吐 ÷ 48000）**：mid-side 35781×、modMatrix 8485×、biquad
4628×、bass 4346×、loudness 1727×、limiter(关) 1570×、compressor 1222×、dynamicEq 1029×、
limiter(开) 845×、deesser 742×、fdn 591×、reverb-simple 581×、eq-chain 472×、mod-effects
393×、convolver-delta 367×、convolver-exp1024 153×、convolver-exp6000 54×。全部模块
单开都能在 ≤2% 单核下实时运行（卷积长 IR 除外：1.85%）。

### 2.1 FFT 尺寸矩阵（块长 = N，每迭代 32768 帧 × Re/Im 双平面）

| N | 每迭代耗时 | 吞吐（帧/s） | 单次 N 点变换耗时 | ns/点 |
|---|---|---|---|---|
| 1024 | 210.18 µs | 155.9 M/s | 3.28 µs（64 次/迭代） | 3.20 |
| 2048 | 239.89 µs | 136.6 M/s | 7.50 µs（32 次） | 3.66 |
| 4096 | 247.21 µs | 132.6 M/s | 15.45 µs（16 次） | 3.77 |
| 8192 | 274.80 µs | 119.2 M/s | 34.35 µs（8 次） | 4.19 |

O(N log N) 增长平缓（1024→8192 每点成本 +31%），twiddle 表预建后单次变换无分配。

### 2.2 I/O 工具路径（离线语义，分配合法，不适用音频回调实时铁律）

| 场景 | 耗时（中位） | 吞吐 |
|---|---|---|
| share_codec decode v2（854 字符 HSE2 串） | 117.1 µs | 6.96 MiB/s |
| wav encode PCM16（32768 帧立体声，131156 B） | 151.1 µs | 827 MiB/s |
| wav decode PCM16 | 70.5 µs | 1.73 GiB/s |
| wav encode Float32（262196 B） | 33.3 µs | 7.33 GiB/s |
| wav decode Float32 | 103.6 µs | 2.36 GiB/s |

---

## 三、§三指标状态：全链离线吞吐（bench_chain_full，60s @48kHz / 块 128）

被测对象现为引擎服务实际装配的 `hse-service::dsp_chain::ServiceEngineChain`（Rust `EngineChainStage` 第 1–21 级，spatial 固定 off），每迭代复制母带 → reset → 22,500 块连续
`process_planar`。

| 场景 | 处理 60s 音频耗时 | 吞吐（帧/s） | ×realtime | 实时 CPU（单核） |
|---|---|---|---|---|
| TS 默认链等价快照 × 恒定 0.1 输入（**与 TS 基线同口径**） | **327.7 ms** | 8.788 M/s | **183.1×** | **0.546%** |
| TS 默认链等价快照 × 满幅合成信号 | 345.3 ms | 8.340 M/s | 173.7× | 0.576% |
| 全模块开启 × reverb-simple | 691.6 ms | 4.165 M/s | 86.8× | 1.152% |
| 全模块开启 × fdn | 688.0 ms | 4.186 M/s | 87.2× | 1.147% |
| 全模块开启 × convolver（expNoise 6000） | 1.6695 s | 1.725 M/s | 35.9× | 2.78% |
| **最重场景：全开 + IR≈4s（192000 样本）卷积** | 6.423 s | 448.4 k/s | 9.34× | **10.7%** |

TS 默认链等价快照 = `PilotParams::default()`（与 TS `createDefaultParams` 逐键对齐）+
显式装配 eqChain（PRO 10 段 0dB + Q 补偿——0dB 下滤波器仍逐样本运算，成本同级）；
其余缺省形态与 TS 相同（deesser/comp/modEffects/bass/dynamicEq 关、modMatrix 无路由、
limiter 开 truePeak、reverb simple wet .3/dry .7）。

### 指标结论

| §三指标 | 目标 | 实测 | 结论 |
|---|---|---|---|
| 离线吞吐 | ≥ 3× TS 支线基线（5.59% realtime → 需 ≤1.863%，即 60s 音频 ≤1.118s） | **0.546% realtime（327.7ms / 60s）= TS 的 10.2×** | **达标（超 3.4× 余量）** |
| 实时链 CPU（默认全链） | ≤ 5% 单核 | **0.546%**（同机同口径；TS 基线 5.59%） | **达标** |
| 实时链 CPU（最重场景，IR≈4s 卷积 + 全链开） | ≤ 25% 单核 | **10.7%** | **达标** |
| 离线吞吐（全模块开启，reverb-simple 路参考） | —— | 1.152% = 4.85× TS 基线 | 仍 ≥3× |

**成本核算交叉验证**：全开链实测 240.1 ns/帧，各单模块基准 @128 之和为 246.9 ns/帧
（midSide 0.6 + biquad 4.5 + eq 44.1 + deesser 28.1 + comp 17.0 + modFx 53.0 +
reverbSimple 35.8 + bass 4.8 + loudness 12.1 + dynEq 20.2 + modMatrix 2.5 + limiter 24.6）
——链内数字略优于独立求和（缓存局部性更好），量级自洽。TS 默认链实测 113.8 ns/帧，
其中活跃级之和 105.1（eq 44.1 + reverbSimple 35.8 + limiter 24.6 + midSide 0.6）+
约 9 ns 为八个旁路级的守卫路径——旁路开销 ≈8%，可接受。

---

## 四、热点排名（单模块 ns/帧 @block 128，重→轻）

1. **convolver expNoise 6000（真实长尾 IR）：385 ns/帧** —— 断层第一，是全链其余部分之和的 1.6 倍；长 IR 卷积是唯一逼近"单模块即 2% CPU"的模块
2. convolver expNoise 1024（p256）：136 ns/帧
3. mod-effects（五级全开）：53.0
4. eq-chain（10 段非零增益）：44.1
5. reverb-simple：35.8 ≈ fdn-reverb：35.3
6. deesser：28.1
7. limiter（truePeak 开）：24.6（truePeak 关 13.3 → 4× 过采样占 limiter 成本的 ~47%）
8. dynamic-eq：20.2
9. compressor：17.0
10. loudness-comp：12.1
11. convolver delta IR（机制底噪）：56.7（分区调度固定开销很小，成本随 IR 长度线性增长）
12. fft：6.4~8.4 ns/帧（1024→8192）
13. bass-enhancer：4.8；biquad：4.5；modulation-matrix：2.5；mid-side：0.6

对默认链（TS 口径）的贡献排序：eq-chain ≈ reverb-simple > limiter > 其余旁路级守卫。

## 五、块长行为观察

- **大多数模块**：block 128→1024 吞吐提升 0.5%~3%（bass +3.2%、midSide +3.1%、fdn
  +1.9%、modMatrix +1.7%、convolver +1.7%、loudness +1.7%、deesser/dynEq/modFx ≈持平）
  ——符合"每调用固定开销随分块变粗摊薄"的预期，且幅度很小：**分块粒度在本引擎里
  不是性能杠杆**，128 帧实时块长没有可观的税。
- **双二阶级联家族呈反向趋势**：eq-chain 22.68→21.74 M/s（−4.1%）、parity_biquad
  222.2→214.3 M/s（−3.6%）——块越大吞吐越略降。两者同为逐样本级联状态更新，疑似
  缓存/预取局部性随块内循环体变长而劣化；幅度 ≤4%，机制未进一步取证（见 §七）。
- compressor 在四种块长下完全持平（±0.2%）——包络块处理内部本身按固定步长推进。

## 六、异常与备注

1. **parity 基准的 criterion "change" 基线失真**：本次运行相对 criterion 旧存基线报告
   `+3022%`（biquad）~`+25645%`（reverb-simple）的 "Performance has regressed"。这是
   因为旧基线录制于 Phase 1 **直通占位桩**时代（process 为 no-op）；真实实现天然比
   no-op 慢一个量级以上，非真实回归。本次运行后 criterion 已存新基线（真实实现口径）。
2. **wav encode Float32（33.3µs）比 encode PCM16（151.1µs）快 4.5×**：f32 路径近似
   memcpy；PCM16 需逐样本 clamp+scale+i16 转换。行为正确，记录为特性而非缺陷。
3. **share_codec decode（117µs/次）**为分配型路径（产出全新参数 JSON），只允许出现在
   控制面/导入路径——与服务现状一致，不进音频回调。
4. **卷积 delta IR（56.7 ns/帧）与 exp6000（385 ns/帧）差 6.8×**：分区机制本身的开销
   很小，卷积成本几乎全部随 IR 长度（长分区数）线性增长——控制最重场景成本的手段
   就是控制 IR 长度。
5. 基准输入为满幅合成信号时限幅器处于真实增益衰减工况（TS 基线脚本用 0.1 恒定激励，
   限幅器不触发 GR）；本矩阵两种输入都跑了全链（327.7 vs 345.3 ms，满幅输入仅 +5%），
   结论对激励选择不敏感。

## 七、Rust 空间 renderer 对象矩阵（本机 release 实测）

口径：128 帧块，确定性合成规则网格，所有 HRIR 频谱/房间拓扑与工作缓冲均在计时区外完成；
实时路径执行 `BinauralRenderer.process`。以下数字用于验证 §八对象数与单核预算，不替代真实 SOFA
数据集和目标机复测。

| 场景 | 对象数 | ns/帧 | 单核 realtime |
|---|---:|---:|---:|
| 48k / 256 taps / time / room off | 32 | 5,236 | 25.13% |
| 48k / 256 taps / time / room off | 64 | 10,432 | 50.07% |
| 48k / 256 taps / partitioned / room off | 32 | 365 | 1.75% |
| 48k / 256 taps / partitioned / room off | 64 | 724 | 3.47% |
| 44.1k / 512 taps / partitioned / hall | 64 | 1,190 | 5.25% |
| 48k / 512 taps / partitioned / hall | 64 | 1,189 | 5.71% |
| 96k / 512 taps / partitioned / hall | 64 | 1,202 | 11.54% |

结论：**partitioned 模式在 64 对象、512 taps、非零房间与 96k 最重组合下仍低于 25% 单核目标**；
time 直接卷积在 32 对象已约 25%，64 对象约 50%，只能作为低对象数参考/兼容模式，不得作为多对象生产默认。
LowLatency 分区为 64 样本，在 44.1/48/96k 均低于 5ms renderer 延迟目标。

## 八、未测量 / 未覆盖（留档待后续）

- **SIMD 显式向量化**：当前数字为 rustc 1.95 auto-vectorize 的结果，未做 AVX2/AVX-512
  intrinsics 专项（TS 审计确认的 radix-4 FFT / 块级向量化 EqChain 对应项）。若后续做
  SIMD 冲刺，以本矩阵为 before 基线复测。
- **hse-stretch / 计量 / 参数域 / 服务路径新增组**：已补 criterion 目标并纳入 `cargo bench --workspace --no-run --locked` 编译门禁；服务路径基准为纯内存组件与串联块口径，不运行真实音频。本轮未执行测量，因此尚无可与历史矩阵并列的性能数字。
- **真实设备路径**：捕获线程已由 WASAPI event readiness 驱动，移除固定 10ms 空轮询；全链 CPU 数字仍为离线驱动 `process_planar` 的纯 DSP 成本，未含
  WASAPI 后端拷贝/线程唤醒的服务进程端到端开销（Phase 2 已做真机回放机制验证，
  系统级性能仍需单独测量）。
- **笔记本功耗态**：未锁频、未控制后台负载，数字 ±2% 波动；正式对外发布数字建议在
  锁频桌面机（规划书参考机 Ryzen 5 / i5 级）复测。
- **多实例并发**：单链单线程口径；N 会话并发扩展性未测（服务为每会话一链模型）。
