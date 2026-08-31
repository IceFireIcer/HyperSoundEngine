# Phase 4 SIMD / 自动向量化评估报告

> 日期：2026-08-29（Phase 4 性能矩阵轮）
> 性质：**分析 + 测量**留档，不含任何实现变更——hse-core / hse-service / TS 源码 / `benches/`
> 零改动；本文件只新增结论与两组只读测量（TS 基准重跑、`target-cpu=native` 对照）。
> 基准数据源：[`phase4-bench-matrix.md`](phase4-bench-matrix.md)（同轮姊妹篇，全模块
> criterion 矩阵与 §三指标，本文只引用不重复）。
> 复现命令见文末 §八。

---

## 一、结论摘要（TL;DR）

1. **Phase 4 的 ≥3× 吞吐目标已在无任何 SIMD 的现状下达标**：Rust 全链默认快照
   0.546% realtime（60s 音频 327.7ms，bench-matrix §三），对 TS 基线（本机实测
   5.33%，历史留档 5.59%）为 **9.7~10.2×**，余量 3.2~3.4×。SIMD 不是当前里程碑
   的必要项。
2. **自动向量化这条路已被实测证伪**：`target-cpu=native`（本机 7945HX 支持
   AVX2/AVX-512）重跑 FFT 与 convolver 基准仅得 **+6~10%**——rustc 1.95 的
   auto-vectorizer 在蝶形/复数乘/overlap-add 热循环上**没有触发**（见 §六）。要
   拿到真实向量化收益必须手写 SIMD（intrinsics / `wide`）。
3. **逐模块看，绝大多数热点是 SIMD 抗性结构**：eq-chain / biquad / dynamic-eq /
   reverb-simple 全通级 / mod-effects 全部是**逐样本 IIR 反馈递推**（环携带依赖，
   延迟界限），SIMD 无从下手；deesser / compressor 是**逐样本超越函数界限**
   （log10+powf），向量化需换非位精确的向量数学库。唯一存在实质向量化余量的是
   **convolver**（断层第一热点，385 ns/帧）：复数乘、overlap-add、FFT 蝶形都是
   **逐元素独立**循环，可用"保序 SIMD"（每元素运算顺序不变）做到位精确不变，
   预估 **1.8~2.0×**（§五）。
4. **对拍冻结约束是硬边界**（§四）：容差公式 `|got−want| ≤ 1e-6 × max(|want|, 1e-9)`
   对期望值为 0 的样本等效**逐位判定**（带宽 1e-15，小于任何非零 f32）；对非零
   期望仅留 **8~10 个 f32 ulp** 余量。当前对拍 **63/63 PASS 且 maxAbsDiff = 0.0**
   （本会话实测，向量集较任务简报时的 55 组已增长）——一切"改精度、改结合序、
   FMA 收缩、f32 FFT、块级 IIR"的方案都会烧掉该余量并大概率击穿门禁，全部否决；
   **只允许"逐元素保序"的 SIMD**。

---

## 二、TS 基线（本机实测，2026-08-29）

`npm run benchmark` / `npm run benchmark:scenes`（仓库根，48kHz / 128 帧 / 5s 音频）：

| 场景 | 耗时 | ×realtime |
|---|---|---|
| 默认全链（benchmark.mjs） | 266.71 ms | **5.33%** |
| 默认全链（scenes 同项） | 264.14 ms | 5.28% |
| 卷积混响（2s IR） | 718.48 ms | **14.37%** |
| FDN 混响（algorithmic） | 275.26 ms | 5.51% |
| DynamicEq | 274.19 ms | 5.48% |

- 换算逐帧成本：默认链 ≈ **1.10 µs/帧**（266.71ms ÷ 240,000 帧）。
- 与留档基线的一致性：bench-matrix §三引用 TS 基线 5.59%（279.61ms，来自
  `docs/audit/phase-status.md`），本机本次实测 5.33%——差异 ~5%，属笔记本
  未锁频的 run-to-run 波动（bench-matrix §1.3 注记 ±2%，TS 侧波动略大），
  趋势结论不受影响。
- 卷积场景按 IR 长度近似线性外推（分区卷积成本 ∝ 分区数）：TS 若跑 4s IR
  ≈ 14.37% + （14.37−5.28）≈ **23~24%**，作为 §五/§六跨支线对照的输入。

## 三、Rust 现状（引用 bench-matrix，不重复）

数据全部来自 [`phase4-bench-matrix.md`](phase4-bench-matrix.md)（rustc 1.95.0
release 默认 profile，AMD Ryzen 9 7945HX / 48kHz / 立体声 / 主块 128）：

- **已测三项离线 DSP 性能均达标**：全链默认快照 0.546% realtime（60s 音频 327.7ms，113.8 ns/帧）
  = TS 基线的 10.2×（按 5.59%）/ 9.7×（按本机今日 5.33%）；最重场景（全开 +
  IR≈4s 卷积）10.7% 单核 ≤ 25% 目标；全模块开启 1.152% = 4.85× TS。
- **热点排名**（单模块 ns/帧 @block128，重→轻）：convolver exp6000 **385**（断层
  第一，≈全链其余部分之和的 1.6 倍）＞ convolver exp1024 136 ＞ mod-effects 53.0
  ＞ eq-chain 44.1 ＞ reverb-simple 35.8 ≈ fdn 35.3 ＞ deesser 28.1 ＞ limiter
  24.6 ＞ dynamic-eq 20.2 ＞ compressor 17.0 ＞ 其余 ≤12。
- **对拍门禁（本会话实测复核）**：`cargo run -p hse-parity` → **63/63 PASS，
  全程 maxAbsDiff = 0.000e0**——即 16 个规格模块的共享冻结向量当前**逐位一致**
  （正式门禁为 1e-6 相对容差，实测处于逐位零偏差的更强状态）。

---

## 四、对拍冻结约束分析：哪些路被封死，哪些还开着

### 4.1 容差公式实际有多紧

`specs/README.md` §3.5 与 `hse-parity/src/tolerance.rs` 的统一判定：

```
|got − want| ≤ 1e-6 × max(|want|, 1e-9)     （floor = 1e-9）
```

| 期望值量级 | 容差带宽（绝对） | f32 ulp（同量级） | 余量 |
|---|---|---|---|
| `want = 0` | 1e-15 | 最小非零 f32 = 1.4e-45 | **必须逐位 ±0** |
| `want ≈ 1e-3` | 1e-9 | 1.16e-10 | ≈ 8.6 ulp |
| `want ≈ 1` | 1e-6 | 1.19e-7 | ≈ 8.4 ulp |
| `want ≈ 1e4` | 1e-2 | 9.77e-4 | ≈ 10.2 ulp |

结论：对**非零**期望样本约 8~10 个 f32 ulp 的余量；对**期望为零**的样本（冻结
向量里大量存在：convolver δ IR 的逐位 +0 头段、静音段、深零 bin）是**等效逐位**
约束。而 FFT 每级 f32 落点的量化噪声本身就 ~0.5 ulp/级（8192 点 13 级，RMS
≈2 ulp、最坏线性 ≈6.5 ulp）——**预算已经被现有算法用掉大半**。

### 4.2 冻结面（f32 落点 / f64 中间量不可动）

- **全部 16 个有向量的规格模块当前实质冻结**：biquad、limiter、reverb-simple、
  compressor、bass-enhancer、mid-side、eq-chain、fdn-reverb、deesser、
  loudness-comp、dynamic-eq、mod-effects、modulation-matrix、**fft**（规格
  `specs/dsp/fft.md` §四明确"逐级 f32 落点是对拍硬锚点"，fft.rs 模块文档同）、
  **convolver**（convolver.md §四逐条列出 f32 落点：IR / 分区频谱 / prodShort /
  prodLong / outAccum / pending / preDelay 线）、hse-stretch。
- **f64 中间量语义不可降级为 f32**（"数值精度铁律"，各模块规格 §四）；**FMA
  收缩不可引入**：rustc 默认不收缩（fft.rs L25 注释明示这是与 JS Number 语义
  一致的前提），任何 `mul_add` / fast-math 改写都会改变舍入点。
- **IIR 结构重排不可做**：块级 IIR（state-space 块处理）会消灭"逐样本 f32 段间
  量化"这一规格行为（eq-chain 规格明确段间信号经 f32 量化落点）——属规格变更，
  按 `docs/VERSIONING.md` = MAJOR，不在 Phase 4 评估域内。
- 已冻结向量的期望值永不修改（AGENTS.md 铁律）：行为变更 = 新增向量或 MAJOR。

### 4.3 还开着的路：逐元素保序 SIMD

SIMD 并不必然改变数值：**当每个 SIMD lane 执行的逐元素运算顺序与标量实现完全
一致时（无收缩、无重结合），结果逐位等于标量**。三类结构满足：

1. **逐元素映射**（convolver 复数乘：`prod[k] = (r1·r2 − i1·i2, r1·i2 + i1·r2)`，
   每元素 4 乘 2 加，元素间无依赖）；
2. **逐元素累加**（overlap-add：`outAccum[base+j] += prod[j]`，每元素恰好一次
   f64 加 + f32 舍入写回，j 之间独立）；
3. **蝶形组内独立**（FFT 同 stage 内不同 k 的蝶形互不依赖，lane 各自保序）。

这三类恰好构成 convolver 的全部大块成本（§五）。另注意：**双通道可作免费
lane**——`process_wet_block` 对 L/R 逐声道调用同一形状的 FFT，L/R 合并打包
（等效复平面 / 双平面并行）在不改变每声道运算序的前提下翻倍 lane 利用率。

### 4.4 build-flag 的一个澄清

`-C target-cpu=native`（或 `+avx2`）**不会**引入 FMA 自动收缩——Rust 从不自动
收缩乘加，故 AVX2 codegen 本身是对拍安全的（本次 native 对照 63 向量路径未重跑
逐位校验，但原理上每元素 IEEE 语义不变；若未来落地 native 构建须重跑对拍门禁
确认）。真正的数值风险只来自手写 `mul_add` / 显式 fast-math。

---

## 五、逐模块 SIMD 评估表（热点优先）

依据 bench-matrix §四热点排名逐个核对源码热循环
（`HyperSoundEngineRust/crates/hse-core/src/`）：

| # | 模块 | ns/帧@128 | 主导成本（源码证据） | 数据依赖结构 | SIMD 潜力 | 预估增益 | 冻结风险 |
|---|---|---|---|---|---|---|---|
| 1 | **convolver**（exp6000） | **385** | 复数乘（逐元素）+ overlap-add（逐元素）+ 基-4 FFT 蝶形（组内独立）+ 缓冲搬运 | **三类全是可保序 SIMD 的独立循环** | **高（唯一）** | **1.8~2.0×**（AVX2 4×f64 lane；详见下文分解） | 低——保序 lane 即逐位一致；δ IR 零段不受影响 |
| 2 | mod-effects（五级全开） | 53.0 | delay/chorus/flanger 分数延迟读改写（含反馈环）+ phaser 逐样本 `tan`/`sin` + 逐样本相位模 | 逐样本反馈递推 + 逐样本超越函数 | 低 | ≤1.2×（仅相位/混合可批，占比小） | 反馈环保序要求 → 无 lane 可并 |
| 3 | **eq-chain**（10 段） | 44.1 | 10×TDF2 逐样本递推；**左右声道共享状态**（规格 §4.4，L 整块跑完再 R） | 双重串行：样本间递推 + 段间级联 + 声道间共享 | **极低（SIMD 抗性）** | ≈1.0× | 块级 IIR 改写会消灭规格 f32 段间落点 = MAJOR，**否决** |
| 4 | reverb-simple / fdn | 35.8 / 35.3 | 4 并联梳状（每声道）+ 4 串联全通（reverb）；8 线反馈 + Householder 矩阵（fdn） | 梳状并联**彼此独立**（可 lane）；全通/反馈矩阵逐样本串行 | 中低 | 1.3~1.6×（仅梳状段，4/8 lane） | 低（lane 保序）；但单模块权重小 |
| 5 | deesser | 28.1 | 9×TDF2 串行 + **逐样本 `log10` + `powf`**（deesser.rs L272/L275） | 递推 + 逐样本超越函数 | 低（数学库界限） | ≈1.0~1.1× | 向量 pow/log 非位精确 → 烧 ulp 预算，**否决** |
| 6 | limiter（truePeak 开） | 24.6 | 4× 过采样 sinc 插值（3 相位×8 tap 点积）+ 分支密集单调队列（~47% 成本在过采样） | 8-tap 点积可 lane 但**加法链顺序即行为**；队列纯标量分支 | 低 | ≤1.15× | 点积 SIMD 重结合 → 消耗 ulp 预算，**否决** |
| 7 | dynamic-eq | 20.2 | 8 格串行交叉树（每样本 8+ 次 tick）+ 块级 powf（5 次/块，可忽略） | 同 eq-chain 的递推串行 | 极低 | ≈1.0× | 同 eq-chain |
| 8 | compressor | 17.0 | 逐样本 `log10` + `powf`（compressor.rs L276/L294）+ 包络一阶递推 | 递推 + 超越函数 | 低 | ≈1.0× | 同 deesser |
| 9 | fft（独立基准 6.4~8.4 ns/点） | —— | 基-4 蝶形（f32 读入→f64 运算→f32 写回）| 同 stage 组内独立、跨 stage 串行 | 中高 | 2~3×（蝶形 lane 化）+ L/R 双平面免费 lane | 落点保序即可逐位 |
| 10 | biquad / bass / modMatrix / midSide | 4.5~0.6 | TDF2 递推 / 简单映射 | 递推 或 已是廉价映射 | 无 | ≈1.0×（已近递推延迟地板，biquad ≈2.25 ns/样本 ≈ 9-10 周期） | —— |

### convolver 成本分解与增益推导（expNoise 6000 / Ls=512 口径）

分区规划：Ps=10（短区段 100ms=4800 样本）、Pl=1、k=8；Ns=1024、Nl=8192。
每 512 帧湿块成本（对照 bench-matrix §2.1 单次变换实测价）：

| 分量 | 每 512 帧工作量 | 估算耗时（用 §2.1 实测 FFT 单价） | 占比 |
|---|---|---|---|
| FFT（11×1024 点 + 摊销 0.25×8192 点） | 36.1 + 8.6 µs | ≈ 44.7 µs | ≈ 23% |
| 复数乘（10×1024 + 摊销 1024 ≈ 11.3k 次） | 逐元素独立 | ≈ 50~65 µs | ≈ 25~33% |
| overlap-add 累加（≈22.5k 次 f32←f64 加） | 逐元素独立 | ≈ 30~45 µs | ≈ 15~23% |
| 缓冲 fill/copy/copy_within（outAccum 移位等） | memcpy 类 | ≈ 10~20 µs | ≈ 5~10% |
| 逐样本胶水（喂入/放行/干湿混合） | 512 帧 | ≈ 2~5 µs | ≈ 2% |

（合计 ≈197 µs/块，与实测 385 ns/帧 × 512 = 197 µs 自洽。）

**AVX2（4×f64 lane）保序 SIMD 下的可压缩量**：复数乘 ~4×（省 ~40µs）+ 累加
~4-6×（省 ~30µs）+ FFT 蝶形 ~2×（省 ~22µs）≈ 90~100µs → **385 → ≈195~210
ns/帧，1.8~2.0×**。若叠加 L/R 双通道合并 lane 与 `wide`/intrinsics 的转换开销
摊薄，上限略高；再往上（>2×）需要动精度或算法（f32 FFT / Winograd / 更大分区）
——全部触碰 §4.2 冻结面，不在无 MAJOR 的前提下可行。

---

## 六、免费杠杆实测：`target-cpu=native` 对照（本会话新增测量）

**动机**：仓库无 `.cargo/config.toml`，无 `RUSTFLAGS` 约定——当前全部基准与
发布构建使用 x86-64 **baseline（仅 SSE2）** codegen。native（AVX2/AVX-512 +
更优标量编码）是否已"免费"拿到？用隔离 `CARGO_TARGET_DIR` 重跑两个 bench
（不改任何源码/不触碰共享 target 与 criterion 基线）：

| 基准（每迭代 32768 帧） | bench-matrix 基线（SSE2） | native 实测 | 增益 |
|---|---|---|---|
| fft 1024（单次 210.18 µs） | 210.18 µs/次 | 195.7~199.6 µs/次 | **1.06~1.07×** |
| fft 2048（239.89） | 239.89 | 219.0~220.5 | **1.09~1.10×** |
| fft 4096（247.21） | 247.21 | 228.9~234.2 | **1.06~1.08×** |
| fft 8192（274.80） | 274.80 | 256.5~257.9 | **1.07×** |
| convolver exp6000 block512 | 12.60 ms/迭代 | 11.50~11.59 | **1.09×** |
| convolver exp1024 p256 | 4.46 | 4.11~4.13 | **1.08×** |
| convolver delta | 1.86 | 1.73~1.74 | **1.07×** |

**解读**： uniformly +6~10%，与"标量编码改善"（AVX 编码、寄存器压力缓解）一致
而非向量化——**rustc auto-vectorizer 在这些热循环上没有触发**（蝶形的跨平面
跨步访问 + f32↔f64 转换 + 复数乘循环的边界检查使向量化判据不满足）。因此：

- "开个编译选项就白拿 2×"的假设**不成立**；真实向量化必须手写（§五）。
- +6~10% 的 flag 收益相对其分发代价（native 构建绑定宿主机特性，需运行时
  特性检测或分级构建）**当前不值得**——记录在案，若未来做 SIMD 冲刺时一并
  处理构建分层。

---

## 七、SIMD 后续建议

### 7.1 结论：已测性能目标无需 SIMD；Phase 4 自动实现完成，真机出口仍未关闭

- ≥3× 目标以 **9.7~10.2×** 达成（余量 3.2~3.4×），实时 CPU 与最重场景双达标
  （bench-matrix §三）；SIMD/向量化对当前里程碑**全部可推迟**。
- 推迟不等于否决：convolver 是唯一"成本随 IR 长度无界增长"的模块（bench-matrix
  §六.4：δ 56.7 vs exp6000 385 ns/帧），SIMD 是它的第一顺位杠杆。

### 7.2 若未来做 SIMD 冲刺的优先级

1. **convolver 保序 SIMD**（复数乘 + overlap-add + FFT 蝶形 lane 化，AVX2 4×f64
   或 `wide::f64x4`；L/R 双通道合并打包）：预估 1.8~2.0×，把 385 → ~200 ns/帧、
   最重场景 10.7% → ~6%。**前置条件**：以 bench-matrix 为 before 基线复测 +
   `cargo run -q -p hse-parity` 综合门禁全绿（音频 72/72、空间 28/28、参数扫描结构摘要 40/40）。
2. **FFT 内核 lane 化**（同 stage 组内并行 + L/R 平面并行）：2~3×，惠及 convolver
   与 fft stage；与 1 有重叠，注意合并施工。
3. **reverb-simple 梳状并联 lane 化**（4/8 lane）：1.3~1.6×，单模块权重小，随手
   顺带。
4. **明确不做**：eq-chain / dynamic-eq / biquad 的任何向量化（递推串行，无 lane
   可并；块级 IIR = MAJOR）；deesser / compressor / mod-effects 的向量数学库
   （非位精确）；fast-math / FMA 收缩 / f32 FFT（烧穿 §4.1 预算）。

### 7.3 64 对象 / 4s IR 场景的前瞻（若 convolver 成为瓶颈）

- 单对象现状（bench-matrix §三实测）：全开 + 4s IR = **10.7% 单核**，其中
  convolver 占 ≈9.5%。
- 64 对象线性外推 ≈ **610% 单核**——《空间音频规划书》§八的"32-64 对象
  <25% 单核"按**单核**口径不可能靠 SIMD（≤2×）达成，需要架构组合拳：
  1. **对象级多线程**（服务当前"每会话一链单线程"；空间多对象需链内并发，属
     新 ADR 域）：本机 16C/32T 下 64×9.5% ≈ 6.1 核，可行；参考机（Ryzen 5 /
     i5，6 核）饱和——距离目标还需 2~3× 的单对象压缩；
  2. **SIMD 2×（§7.2 第 1 项）**：9.5% → ~5%，参考机需求压到 ≈3 核（64×4.75%）；
  3. **IR 共享 / 频谱复用**（同 IR 多对象共享分区频谱与输入 FFT 批处理）与
     **IR 长度/分区参数控制**（成本 ∝ IR 长度，bench-matrix §六.4）。
- 结论：convolver SIMD 是该场景的**必要非充分**项；当前（单对象/常规 IR）无需
  启动。

---

## 八、复现命令

```bash
# TS 基线（本文件 §二）
npm run benchmark && npm run benchmark:scenes

# 对拍门禁（本会话复核 63/63，maxAbsDiff=0）
cd HyperSoundEngineRust && cargo run -q -p hse-parity

# native flag 对照（§六；隔离 target，不触碰共享构建与 criterion 基线）
cd HyperSoundEngineRust && RUSTFLAGS="-C target-cpu=native" \
  CARGO_TARGET_DIR=.scratch/target-native \
  cargo bench -p hse-benches --bench bench_fft --bench bench_convolver

# SIMD 冲刺时的 before 基线（全矩阵）
cd HyperSoundEngineRust && cargo bench -p hse-benches
```

native 对照原始输出：`.scratch/native-fft-conv.txt`（gitignored，随会话留存）。
