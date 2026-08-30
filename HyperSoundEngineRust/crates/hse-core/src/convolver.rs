//! convolver —— 非均匀分区卷积混响 + IR 去周期化（模块 9）。
//!
//! 行为事实标准：仓库根 `src/dsp/Convolver.ts`；规格：`specs/dsp/convolver.md`。
//! 频域内核 = [`crate::fft::Fft`]（基-4 复 FFT，正变换不缩放 / 逆变换 ÷N）。
//!
//! # 移植纪律（specs/dsp/convolver.md §四——逐条对齐 TS）
//!
//! - **分区规划**（§4.1）：Ls clamp [32, 8192]；k = 2^ceil(log2(want/Ls))；
//!   Ps ≥ k−1 约束；IR 短于短区段时 Pl=0 退化均匀分区；accLen = (Ps+Pl·k+2)·Ls；
//! - **IR 配方**（§4.2）：delta / expNoise 由确定性配方重建（LCG 推进序与
//!   表达式结合序逐字固化——f64 乘法不可交换结合），见 [`build_ir_recipe`]；
//! - **去周期化**（§4.3）：10ms 移动 RMS 包络 → 首个最大值（严格大于才更新）
//!   → −60dB **后缀**判定（防稀疏 IR 误衰减）→ τ≈50ms 指数衰减；不改变 IR 长度；
//! - **流式处理**（§4.4）：输入按 Ls 分组、湿块按块粒度生产、湿路逐样本放行；
//!   outAccum **跨块累加**（块处理前不得清零，左移保留各分区历史贡献）；
//!   pending 为固定容量滑动窗口队列，逐样本交替生产/消费，处理期不扩容；
//!   湿路放行条件四联：pendingLen>0 ∧ wetIdx≥0 ∧
//!   wetIdx<completedBlocks·Ls ∧ totalWetOut===wetIdx；
//! - **f32 落点**：IR、分区频谱、复乘结果（prodShort/prodLong）、outAccum 的
//!   overlap-add 累加、pending、preDelay 延迟线全部是 TS `Float32Array`——
//!   每次写入即量化 f32（`f32 += f32` 语义 = f64 求和后 f32 舍入写回）；
//!   dryGain/wetGain/复乘/蝶形累加等 TS Number 中间量全部 f64；
//! - **延迟语义**（§4.5）：`get_latency_samples()` 恒 = Ls，与 IR 长度/分区规划/
//!   mix/preDelay 无关；湿路总延迟 = Ls + preDelaySamples（干路不延迟）。
//!
//! # 与 TS 源码的逐行对应关系（Convolver.ts 行号）
//!
//! - 构造（L126–L160）→ `ConvolverStage::new`（钳制顺序逐行同序）；
//! - loadIR（L166–L277）→ `load_ir`（校验/规划/频谱预计算/缓冲分配）；
//! - setMix / setPreDelayMs（L280–L288）→ `set_mix` / `set_pre_delay_ms`；
//! - processStereo（L386–L464）→ `process_stereo`（喂入 + 放行两段循环同序）；
//! - processWetBlock（L523–L605）→ `process_wet_block`（长块累积 → 短 FFT →
//!   取块 → 左移，逐行同序）；
//! - pushDelay（L608–L618）→ `push_delay`（preDelay=0 直通且不推游标）；
//! - dePeriodizeIR（L624–L669）→ `de_periodize_ir`；
//! - reset（L471–L500）→ `reset`（清流式状态，保留 IR 与分区规划）。
//!
//! `process`（TS 单声道一次性卷积，单元测试域）不移植——本 crate 只需要
//! 流式 `processStereo` 形态；单测以朴素线性卷积作等价性对照（§4.4 数学等价）。

use crate::fft::{next_pow2, Fft};
use crate::Stage;

/// 复刻 JS `Math.min(a, b)` 的 NaN 传播语义（理由同 biquad.rs 的同名助手）。
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a < b {
        a
    } else {
        b
    }
}

/// 复刻 JS `Math.max(a, b)` 的 NaN 传播语义（理由同 js_min）。
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a > b {
        a
    } else {
        b
    }
}

/// 对齐 TS `ConvolverOptions` 的构造选项（specs/dsp/convolver.md §二）。
///
/// 向量域固定提供完整字段（§三：不依赖类字段初值）；[`Default`] 给出与 TS
/// 类字段一致的缺省（512 / 4096 / 100ms / true），供非向量调用方使用。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConvolverOptions {
    /// 最短分区长 Ls（默认 512）；clamp [32, 8192]；= 湿路延迟。
    pub partition_size: f64,
    /// 长分区长（默认 4096）；生效为 Ls 的 2 的幂整数倍（k = Ll/Ls）。
    pub long_partition_size: f64,
    /// IR 前部短分区时长 ms（默认 100）；clamp [0, 5000]。
    pub short_region_ms: f64,
    /// 是否对 IR 去周期化（默认 true）。
    pub de_periodize: bool,
}

impl Default for ConvolverOptions {
    fn default() -> Self {
        Self {
            partition_size: 512.0,
            long_partition_size: 4096.0,
            short_region_ms: 100.0,
            de_periodize: true,
        }
    }
}

/// 向量 `params.ir` 的确定性 IR 配方（specs/dsp/convolver.md §4.2）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IrRecipe {
    /// 单点冲激：length = delay + 1，ir[delay] = 1，其余全 0（逐位锚点用）。
    Delta { delay: f64 },
    /// 固定种子 LCG 指数衰减噪声（真实混响尾用）。
    ExpNoise {
        length: f64,
        seed: u32,
        decay: f64,
        amp: f64,
    },
}

/// IR 配方 → 确定性冲激响应（逐字复刻 TS `buildIrRecipe`，test/spec-vectors.test.ts
/// L151–L176 与 scripts/export-vectors.mjs L184–L206 同源）。
///
/// 全程 f64 求值、存入 f32 时一次量化；LCG 推进序（先推进再取值）与表达式
/// 结合序 `((u·2 − 1)·amp) · exp((−decay·i)/(length−1))` 逐字固化——任何重排
/// 都会改变 f32 量化结果（specs/dsp/convolver.md §4.2）。
pub fn build_ir_recipe(recipe: &IrRecipe) -> Result<Vec<f32>, String> {
    match *recipe {
        IrRecipe::Delta { delay } => {
            let delay = delay.round();
            if !(delay >= 0.0) {
                return Err("delta IR 配方 delay 非法".to_string());
            }
            let mut ir = vec![0.0_f32; delay as usize + 1];
            ir[delay as usize] = 1.0;
            Ok(ir)
        }
        IrRecipe::ExpNoise {
            length,
            seed,
            decay,
            amp,
        } => {
            let length = length.round();
            if !(length >= 2.0) || !(decay > 0.0) {
                return Err("expNoise IR 配方 length/decay 非法".to_string());
            }
            let n = length as usize;
            let mut ir = vec![0.0_f32; n];
            let mut s = seed;
            for (i, slot) in ir.iter_mut().enumerate() {
                // TS：s = (Math.imul(s, 1664525) + 1013904223) >>> 0 —— u32 环绕即 >>>0。
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let u = f64::from(s) / 4294967296.0;
                // 结合序逐字固化：((u * 2 - 1) * amp) * Math.exp((-decay * i) / (length - 1))。
                *slot =
                    (((u * 2.0 - 1.0) * amp) * ((-decay * i as f64) / (length - 1.0)).exp()) as f32;
            }
            Ok(ir)
        }
    }
}

/// 分区规划与预计算频谱（`load_ir` 时一次性构建，处理期只读）。
struct ConvolverPlan {
    /// 短分区数 Ps（覆盖 IR 前部）。
    ps: usize,
    /// 长分区数 Pl（0 = 退化均匀分区）。
    pl: usize,
    /// 长分区起点（IR 样本索引 = Ps·Ls）。
    long_start: usize,
    /// 短 FFT 尺寸 Ns = nextPow2(2·Ls)。
    ns: usize,
    /// 长 FFT 尺寸 Nl = nextPow2(2·Ll)。
    nl: usize,
    /// 短分区预计算频谱（f32 存储，长度 Ps·Ns）。
    short_spec_real: Vec<f32>,
    short_spec_imag: Vec<f32>,
    /// 长分区预计算频谱（f32 存储，长度 Pl·Nl）。
    long_spec_real: Vec<f32>,
    long_spec_imag: Vec<f32>,
    short_fft: Fft,
    long_fft: Fft,
}

/// 共享工作缓冲（`load_ir` 时定容分配，process 复用）。
#[derive(Default)]
struct WorkBufs {
    short_work_real: Vec<f32>,
    short_work_imag: Vec<f32>,
    prod_short_real: Vec<f32>,
    prod_short_imag: Vec<f32>,
    long_work_real: Vec<f32>,
    long_work_imag: Vec<f32>,
    prod_long_real: Vec<f32>,
    prod_long_imag: Vec<f32>,
}

/// 每声道独立流式状态（TS：inputBlock/longIn/outAccum/pendingWet/wetDelay）。
struct ChannelState {
    input_block: Vec<f32>,
    long_in: Vec<f32>,
    out_accum: Vec<f32>,
    pending: Vec<f32>,
    wet_delay: Vec<f32>,
}

impl ChannelState {
    fn new(max_delay: usize) -> Self {
        Self {
            input_block: Vec::new(),
            long_in: Vec::new(),
            out_accum: Vec::new(),
            pending: Vec::new(),
            wet_delay: vec![0.0_f32; max_delay],
        }
    }
}

/// 非均匀分区卷积混响阶段（对齐 TS `Convolver` 类）。
///
/// 全部缓冲在 `load_ir`（非实时路径）定容分配；`process_stereo` 稳态零分配。
pub struct ConvolverStage {
    fs: f64,
    /// 最短分区长 Ls（= 湿路延迟，构造时锁定）。
    partition_size: usize,
    /// 长分区长 Ll = Ls·k（构造时锁定）。
    long_partition_size: usize,
    /// 长分区倍数 k = Ll/Ls（2 的幂，构造时锁定）。
    k: usize,
    /// 短区段样本数 S = round(clamp(shortRegionMs)/1000·fs)（构造时锁定）。
    short_region_samples: f64,
    de_periodize: bool,

    mix: f64,
    pre_delay_samples: usize,

    /// 分区规划 + 预计算频谱（`load_ir` 后 Some）。
    plan: Option<ConvolverPlan>,
    work: WorkBufs,
    ch_l: ChannelState,
    ch_r: ChannelState,

    // ---- 共享记账（左右声道各持独立缓冲但共用同一套计数，TS 同构）----
    input_pos: usize,
    pending_len: usize,
    pending_pos: usize,
    /// 已送入的输入样本总数（仅统计用，TS totalIn）。
    total_in: usize,
    /// 已放行的湿路样本总数。
    total_wet_out: usize,
    /// 已完成的输入块数（块完成时 +1）。
    completed_blocks: usize,
    /// 已输出的样本总数（跨调用累计，逐样本放行的位置依据）。
    total_out: usize,
    wet_delay_pos: usize,

    ir_loaded: bool,
    /// IR 去周期化后的长度 M（TS irLength；诊断/测试用途）。
    ir_length: usize,
    /// IR 名称标签（无数值行为；TS irName / getIrName）。
    ir_name: Option<String>,
}

impl ConvolverStage {
    /// 构造（钳制规则逐行对齐 TS 构造器，Convolver.ts L126–L160）。
    ///
    /// fs ≤ 0 或非有限时报 `Error('invalid sample rate')`。
    pub fn new(sample_rate: f64, opts: ConvolverOptions) -> Result<Self, String> {
        if sample_rate <= 0.0 || !sample_rate.is_finite() {
            return Err("invalid sample rate".to_string());
        }
        let fs = sample_rate;
        // TS L132–L135：round → 非有限/<1 回退默认 → clamp [32, 8192]。
        // Math.round 与 f64::round 在本模块全部调用点的可达结果一致（负半值
        // 经 `< 1` 守卫回退默认，-0 与 0 在后续算术中等价）。
        let mut l = opts.partition_size.round();
        if !l.is_finite() || l < 1.0 {
            l = 512.0;
        }
        let partition_size = js_min(8192.0, js_max(32.0, l)) as usize;
        // TS L138–L149：k = 2^ceil(log2(want/Ls))（向上取 Ls 的 2 的幂整数倍）。
        let mut want_ll = opts.long_partition_size.round();
        if !want_ll.is_finite() || want_ll < 1.0 {
            want_ll = 4096.0;
        }
        let k = if want_ll > partition_size as f64 {
            let ratio = want_ll / partition_size as f64;
            let mut pow = 1.0_f64;
            // 上界防御：非法超大输入下终止循环（TS 中 pow 变 inf，属无效域）。
            while pow < ratio && pow < 1.0e18 {
                pow *= 2.0;
            }
            pow.max(1.0) as usize
        } else {
            1
        };
        let long_partition_size = partition_size * k;
        // TS L151–L153：S = round((min(5000, sms) / 1000) · fs)。
        let mut sms = opts.short_region_ms.round();
        if !sms.is_finite() || sms < 0.0 {
            sms = 100.0;
        }
        let short_region_samples = ((js_min(5000.0, sms) / 1000.0) * fs).round();

        Ok(Self {
            fs,
            partition_size,
            long_partition_size,
            k,
            short_region_samples,
            de_periodize: opts.de_periodize,
            mix: 1.0, // TS 类字段初值
            pre_delay_samples: 0,
            plan: None,
            work: WorkBufs::default(),
            ch_l: ChannelState::new(fs as usize),
            ch_r: ChannelState::new(fs as usize),
            input_pos: 0,
            pending_len: 0,
            pending_pos: 0,
            total_in: 0,
            total_wet_out: 0,
            completed_blocks: 0,
            total_out: 0,
            wet_delay_pos: 0,
            ir_loaded: false,
            ir_length: 0,
            ir_name: None,
        })
    }

    /// 最短分区长 Ls（= `get_latency_samples()` 的返回值）。
    pub fn partition_size(&self) -> usize {
        self.partition_size
    }

    /// 载入单声道 IR（逐行对齐 TS `loadIR`，Convolver.ts L166–L277）。
    ///
    /// 空 / 含 NaN/Inf / 全零 IR 抛错；`de_periodize = true` 时先做去周期化
    /// （不改写调用方数组）。分区频谱在本文f32 存储、预计算（非实时路径）。
    pub fn load_ir(&mut self, ir: &[f32], ir_name: Option<&str>) -> Result<(), String> {
        if ir.is_empty() {
            return Err("invalid impulse response: empty".to_string());
        }
        let mut any_non_zero = false;
        for &v in ir {
            if !v.is_finite() {
                return Err("invalid impulse response: contains NaN/Infinity".to_string());
            }
            if v != 0.0 {
                any_non_zero = true;
            }
        }
        if !any_non_zero {
            return Err("invalid impulse response: all zero".to_string());
        }

        let ls = self.partition_size;
        let k = self.k;
        let ll = self.long_partition_size;
        let src: Vec<f32> = if self.de_periodize {
            self.de_periodize_ir(ir)
        } else {
            ir.to_vec()
        };
        let m = src.len();

        // ---- 非均匀分区规划（TS L189–L207）----
        let mut ps = js_max(1.0, (self.short_region_samples / ls as f64).ceil()) as usize;
        // 保证长分区贡献写入偏移非负：Ps >= k-1（k ≥ 1，k-1 无下溢）。
        let km1 = k - 1;
        if ps < km1 {
            ps = km1.max(1);
        }
        let long_start = ps * ls;
        let pl = if long_start < m {
            js_max(1.0, ((m - long_start) as f64 / ll as f64).ceil()) as usize
        } else {
            0
        };
        // IR 短于短区段规划 → 收敛 Ps（退化均匀分区）。
        if pl == 0 {
            ps = js_max(1.0, (m as f64 / ls as f64).ceil()) as usize;
        }
        let long_start_final = ps * ls;

        let ns = next_pow2(2 * ls);
        let nl = next_pow2(2 * ll);
        let p_total = ps + pl * k; // 按短块粒度折算的总分区数
        let acc_len = (p_total + 2) * ls;

        // ---- 预计算短分区频谱（f32 存储）----
        let short_fft = Fft::new(ns).expect("Ns 必为 2 的幂");
        let long_fft = Fft::new(nl).expect("Nl 必为 2 的幂");
        let mut short_spec_real = vec![0.0_f32; ps * ns];
        let mut short_spec_imag = vec![0.0_f32; ps * ns];
        let mut long_spec_real = vec![0.0_f32; pl * nl];
        let mut long_spec_imag = vec![0.0_f32; pl * nl];
        let work_len = ns.max(nl);
        let mut work_r = vec![0.0_f32; work_len];
        let mut work_i = vec![0.0_f32; work_len];
        for p in 0..ps {
            work_r.fill(0.0);
            work_i.fill(0.0);
            let base = p * ls;
            let count = ls.min(m - base);
            work_r[..count].copy_from_slice(&src[base..base + count]);
            short_fft
                .transform(&mut work_r[..ns], &mut work_i[..ns], false)
                .expect("长度已按 Ns 分配");
            short_spec_real[p * ns..(p + 1) * ns].copy_from_slice(&work_r[..ns]);
            short_spec_imag[p * ns..(p + 1) * ns].copy_from_slice(&work_i[..ns]);
        }
        for p in 0..pl {
            work_r.fill(0.0);
            work_i.fill(0.0);
            let base = long_start_final + p * ll;
            let count = ll.min(m - base);
            work_r[..count].copy_from_slice(&src[base..base + count]);
            long_fft
                .transform(&mut work_r[..nl], &mut work_i[..nl], false)
                .expect("长度已按 Nl 分配");
            long_spec_real[p * nl..(p + 1) * nl].copy_from_slice(&work_r[..nl]);
            long_spec_imag[p * nl..(p + 1) * nl].copy_from_slice(&work_i[..nl]);
        }

        // ---- （重新）分配流式缓冲与工作缓冲 ----
        self.work = WorkBufs {
            short_work_real: vec![0.0; ns],
            short_work_imag: vec![0.0; ns],
            prod_short_real: vec![0.0; ns],
            prod_short_imag: vec![0.0; ns],
            long_work_real: vec![0.0; nl],
            long_work_imag: vec![0.0; nl],
            prod_long_real: vec![0.0; nl],
            prod_long_imag: vec![0.0; nl],
        };
        for ch in [&mut self.ch_l, &mut self.ch_r] {
            ch.input_block = vec![0.0; ls];
            ch.long_in = vec![0.0; ll];
            ch.out_accum = vec![0.0; acc_len];
            ch.pending = vec![0.0; acc_len];
        }

        self.input_pos = 0;
        self.pending_len = 0;
        self.pending_pos = 0;
        self.total_in = 0;
        self.total_wet_out = 0;
        self.completed_blocks = 0;
        self.total_out = 0;
        self.ir_loaded = true;
        self.ir_length = m;
        self.ir_name = ir_name.map(str::to_string);
        self.plan = Some(ConvolverPlan {
            ps,
            pl,
            long_start: long_start_final,
            ns,
            nl,
            short_spec_real,
            short_spec_imag,
            long_spec_real,
            long_spec_imag,
            short_fft,
            long_fft,
        });
        Ok(())
    }

    /// 设置干湿混合 0..1（双向钳制；1 = 纯湿）。
    pub fn set_mix(&mut self, mix: f64) {
        self.mix = js_min(1.0, js_max(0.0, mix));
    }

    /// 设置湿路预延迟 ms（clamp [0, 1000]；样本 = round(ms·fs/1000)）。
    pub fn set_pre_delay_ms(&mut self, ms: f64) {
        let clamped = js_min(1000.0, js_max(0.0, ms));
        // NaN 域（无效参数域）按 0 处理：NaN as usize = 0，不参与向量。
        self.pre_delay_samples = ((clamped * self.fs) / 1000.0).round() as usize;
    }

    /// 湿路引入的延迟（样本数）恒 = partitionSize（§4.5，与 IR 长度/分区规划/
    /// mix/preDelay 无关）。
    pub fn get_latency_samples(&self) -> usize {
        self.partition_size
    }

    /// 当前 IR 名称标签（未载入返回 None）。
    pub fn get_ir_name(&self) -> Option<&str> {
        self.ir_name.as_deref()
    }

    /// IR 长度 M（去周期化不改变长度；`load_ir` 后有效）。
    pub fn ir_length(&self) -> usize {
        self.ir_length
    }

    /// 流式立体声就地处理（逐行对齐 TS `processStereo`，Convolver.ts L386–L464）。
    ///
    /// 未载入 IR 时报 `Error('no impulse response loaded')`。
    pub fn process_stereo(&mut self, l: &mut [f32], r: &mut [f32]) -> Result<(), String> {
        if !self.ir_loaded {
            return Err("no impulse response loaded".to_string());
        }
        let b = l.len().min(r.len());
        let ls = self.partition_size;
        let ll = self.long_partition_size;
        let k = self.k;
        let dry_gain = 1.0 - self.mix;
        let wet_gain = self.mix;

        // 每帧先喂入；块满时产出一个湿块，然后立即按序放行当前帧。这样 pending
        // 峰值不随调用块长增长，load_ir 分配的固定容量足够覆盖所有合法状态。
        for i in 0..b {
            self.ch_l.input_block[self.input_pos] = l[i];
            self.ch_r.input_block[self.input_pos] = r[i];
            self.input_pos += 1;
            if self.input_pos >= ls {
                let cap = self.ch_l.pending.len();
                if self.pending_pos + self.pending_len + ls > cap {
                    let remain = self.pending_len;
                    if remain > 0 && self.pending_pos > 0 {
                        self.ch_l
                            .pending
                            .copy_within(self.pending_pos..self.pending_pos + remain, 0);
                        self.ch_r
                            .pending
                            .copy_within(self.pending_pos..self.pending_pos + remain, 0);
                    }
                    self.pending_pos = 0;
                }
                assert!(
                    self.pending_len + ls <= cap,
                    "convolver pending 固定容量不足"
                );
                let write_at = self.pending_pos + self.pending_len;
                let block_idx = self.completed_blocks;
                let plan = self.plan.as_ref().expect("IR 已载入");
                Self::process_wet_block(
                    plan,
                    ls,
                    ll,
                    k,
                    &mut self.work,
                    &mut self.ch_l,
                    write_at,
                    block_idx,
                );
                Self::process_wet_block(
                    plan,
                    ls,
                    ll,
                    k,
                    &mut self.work,
                    &mut self.ch_r,
                    write_at,
                    block_idx,
                );
                self.pending_len += ls;
                self.completed_blocks += 1;
                self.input_pos = 0;
            }
            self.total_in += 1;

            let mut wet_l = 0.0_f32;
            let mut wet_r = 0.0_f32;
            let wet_idx = self.total_out as i64 - ls as i64;
            if self.pending_len > 0
                && wet_idx >= 0
                && (wet_idx as u64) < (self.completed_blocks * ls) as u64
                && self.total_wet_out as i64 == wet_idx
            {
                wet_l = self.ch_l.pending[self.pending_pos];
                wet_r = self.ch_r.pending[self.pending_pos];
                self.pending_pos += 1;
                self.pending_len -= 1;
                self.total_wet_out += 1;
                if self.pending_len == 0 {
                    self.pending_pos = 0;
                }
            }
            self.total_out += 1;
            wet_l = Self::push_delay(
                &mut self.ch_l.wet_delay,
                &mut self.wet_delay_pos,
                self.pre_delay_samples,
                wet_l,
            );
            wet_r = Self::push_delay(
                &mut self.ch_r.wet_delay,
                &mut self.wet_delay_pos,
                self.pre_delay_samples,
                wet_r,
            );
            l[i] = (dry_gain * f64::from(l[i]) + wet_gain * f64::from(wet_l)) as f32;
            r[i] = (dry_gain * f64::from(r[i]) + wet_gain * f64::from(wet_r)) as f32;
        }
        Ok(())
    }

    /// 处理一个完整短输入块（逐行对齐 TS `processWetBlock`，Convolver.ts L523–L605）。
    ///
    /// 1) 长输入块累积（仅 Pl > 0）：本块复制进 longIn[blockIdx mod k]；每第 k 个
    ///    短块做长 FFT → 与 Pl 个长分区复乘、IFFT（÷Nl）→ overlap-add 进 outAccum
    ///    （前半起点 = (longStart + p·Ll) − (k−1)·Ls，后半 = 前半 + Ll）；
    /// 2) 短 FFT（Ns）→ 与 Ps 个短分区复乘、IFFT（÷Ns）→ overlap-add（前半
    ///    p·Ls，后半 (p+1)·Ls）——outAccum 跨块累加，块处理前**不能**清零；
    /// 3) 取 outAccum[0..Ls) 写入 pending[writeAt..]，左移 outAccum、尾部清零。
    fn process_wet_block(
        plan: &ConvolverPlan,
        ls: usize,
        ll: usize,
        k: usize,
        work: &mut WorkBufs,
        ch: &mut ChannelState,
        write_at: usize,
        block_idx: usize,
    ) {
        // 长输入块累积（仅在存在长分区时；TS L539–L573）。
        if plan.pl > 0 {
            let long_pos = (block_idx % k) * ls;
            ch.long_in[long_pos..long_pos + ls].copy_from_slice(&ch.input_block[..ls]);
            // 长块满（第 k 个短块完成）：长 FFT + 长分区。
            if block_idx % k == k - 1 {
                work.long_work_real.fill(0.0);
                work.long_work_imag.fill(0.0);
                work.long_work_real[..ll].copy_from_slice(&ch.long_in);
                plan.long_fft
                    .transform(&mut work.long_work_real, &mut work.long_work_imag, false)
                    .expect("长度已按 Nl 分配");
                for p in 0..plan.pl {
                    let spec_base = p * plan.nl;
                    for kk in 0..plan.nl {
                        let r1 = f64::from(work.long_work_real[kk]);
                        let i1 = f64::from(work.long_work_imag[kk]);
                        let r2 = f64::from(plan.long_spec_real[spec_base + kk]);
                        let i2 = f64::from(plan.long_spec_imag[spec_base + kk]);
                        work.prod_long_real[kk] = (r1 * r2 - i1 * i2) as f32;
                        work.prod_long_imag[kk] = (r1 * i2 + i1 * r2) as f32;
                    }
                    plan.long_fft
                        .transform(&mut work.prod_long_real, &mut work.prod_long_imag, true)
                        .expect("长度已按 Nl 分配");
                    // 长分区 p 贡献写入 outAccum：前半 = (Ps + p·k − k + 1)·Ls，
                    // 后半 = 前半 + Ll（Ps ≥ k−1 保证偏移非负，§4.1）。
                    let base1 = (plan.long_start + p * ll) - (k - 1) * ls;
                    let base2 = base1 + ll;
                    for j in 0..ll {
                        ch.out_accum[base1 + j] = (f64::from(ch.out_accum[base1 + j])
                            + f64::from(work.prod_long_real[j]))
                            as f32;
                        ch.out_accum[base2 + j] = (f64::from(ch.out_accum[base2 + j])
                            + f64::from(work.prod_long_real[ll + j]))
                            as f32;
                    }
                }
            }
        }

        // 短 FFT + 短分区（TS L576–L597）。
        work.short_work_real.fill(0.0);
        work.short_work_imag.fill(0.0);
        work.short_work_real[..ls].copy_from_slice(&ch.input_block);
        plan.short_fft
            .transform(&mut work.short_work_real, &mut work.short_work_imag, false)
            .expect("长度已按 Ns 分配");
        for p in 0..plan.ps {
            let spec_base = p * plan.ns;
            for kk in 0..plan.ns {
                let r1 = f64::from(work.short_work_real[kk]);
                let i1 = f64::from(work.short_work_imag[kk]);
                let r2 = f64::from(plan.short_spec_real[spec_base + kk]);
                let i2 = f64::from(plan.short_spec_imag[spec_base + kk]);
                work.prod_short_real[kk] = (r1 * r2 - i1 * i2) as f32;
                work.prod_short_imag[kk] = (r1 * i2 + i1 * r2) as f32;
            }
            plan.short_fft
                .transform(&mut work.prod_short_real, &mut work.prod_short_imag, true)
                .expect("长度已按 Ns 分配");
            let base1 = p * ls;
            let base2 = base1 + ls;
            for j in 0..ls {
                ch.out_accum[base1 + j] = (f64::from(ch.out_accum[base1 + j])
                    + f64::from(work.prod_short_real[j]))
                    as f32;
                ch.out_accum[base2 + j] = (f64::from(ch.out_accum[base2 + j])
                    + f64::from(work.prod_short_real[ls + j]))
                    as f32;
            }
        }

        ch.pending[write_at..write_at + ls].copy_from_slice(&ch.out_accum[..ls]);

        // 左移：块 1.. → 0..，尾部清零（保留各分区历史贡献；TS L601–L604）。
        let len = ch.out_accum.len();
        ch.out_accum.copy_within(ls..len, 0);
        ch.out_accum[len - ls..len].fill(0.0);
    }

    /// 环形延迟线：写入 x，返回 preDelaySamples 前的样本（0 = 直通，不推游标）。
    ///
    /// TS 事实（超出规格文本的实证）：`wetDelayPos` 为左右声道**共享游标**，
    /// 每输出帧推进两次（每次 pushDelay 各 +1，Convolver.ts L459–L461），故
    /// **每声道有效 preDelay 延迟 = preDelaySamples / 2**（node 直跑 TS 实证：
    /// 25ms@48k 的冲激经 δ IR 出现在 Ls + 600 处；与 reverb_simple 的"游标每帧
    /// 推进两次"先例同构）。冻结向量全部 preDelayMs = 0（直通分支既不读写也
    /// 不推游标），该象限不进入对拍。
    fn push_delay(line: &mut [f32], pos: &mut usize, pre_delay: usize, x: f32) -> f32 {
        if pre_delay == 0 {
            return x;
        }
        let size = line.len();
        let mut read_pos = *pos as isize - pre_delay as isize;
        if read_pos < 0 {
            read_pos += size as isize;
        }
        let out = line[read_pos as usize];
        line[*pos] = x;
        *pos += 1;
        if *pos >= size {
            *pos = 0;
        }
        out
    }

    /// IR 去周期化（逐行对齐 TS `dePeriodizeIR`，Convolver.ts L624–L669）。
    ///
    /// 返回新数组（不改写调用方传入的 IR）；不改变 IR 长度 M。
    fn de_periodize_ir(&self, ir: &[f32]) -> Vec<f32> {
        let m = ir.len();
        let mut out = ir.to_vec();
        let w = js_max(4.0, (0.01 * self.fs).round()); // 10ms 包络窗
        let half = (w as usize) >> 1;

        // 移动平均 RMS 包络；首个最大值（严格大于才更新，TS L634–L645）。
        let env_at = |n: usize| -> f64 {
            let lo = n.saturating_sub(half);
            let hi = (n + half + 1).min(m);
            let cnt = (hi - lo) as f64;
            let mut sum = 0.0_f64;
            for j in lo..hi {
                sum += f64::from(ir[j]) * f64::from(ir[j]);
            }
            (sum / cnt).sqrt()
        };
        let mut peak_idx = 0_usize;
        let mut peak_val = -1.0_f64;
        for n in 0..m {
            let env = env_at(n);
            if env > peak_val {
                peak_val = env;
                peak_idx = n;
            }
        }
        if peak_val <= 1e-12 {
            return out; // 极静 IR（load_ir 已挡全零，防御性分支）
        }

        // −60dB 后缀判定：从峰值扫到末尾，最后一个 env > threshold 的 n
        // （而非"首次低于"——防稀疏 IR 误衰减，TS L651–L659）。
        let threshold = peak_val * 1e-3;
        let mut last_above = peak_idx;
        for n in peak_idx..m {
            if env_at(n) > threshold {
                last_above = n;
            }
        }
        let n0 = last_above + 1;
        if n0 >= m {
            return out; // 尾部未掉到 −60dB 以下，无需处理
        }

        // 从 n0 起乘 exp 衰减（τ ≈ 50ms；f64 乘法后写回 f32）。
        let tau = 0.05 * self.fs;
        for n in n0..m {
            out[n] = (f64::from(out[n]) * (-((n - n0) as f64) / tau).exp()) as f32;
        }
        out
    }
}

impl Stage for ConvolverStage {
    /// 全部缓冲在 `load_ir`（非实时路径）定容分配，与块长无关，无需再分配。
    fn prepare(&mut self, _max_block_size: usize) {}

    /// 就地处理一个立体声块；状态跨块保持。
    ///
    /// 未载入 IR 属调用契约违反（TS 抛 `Error`）——此处以 panic 镜像该抛错；
    /// 正常路径（先 `load_ir`）不会触发。
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if let Err(msg) = self.process_stereo(left, right) {
            panic!("ConvolverStage::process 契约违反：{msg}");
        }
    }

    /// 清零全部流式状态（对齐 TS `reset`，Convolver.ts L471–L500）：
    /// 分区频谱与分区规划保留（IR 不重载），mix/preDelay 参数保留。
    fn reset(&mut self) {
        self.input_pos = 0;
        self.pending_len = 0;
        self.pending_pos = 0;
        self.total_in = 0;
        self.total_wet_out = 0;
        self.completed_blocks = 0;
        self.total_out = 0;
        self.wet_delay_pos = 0;
        for ch in [&mut self.ch_l, &mut self.ch_r] {
            ch.out_accum.fill(0.0);
            ch.pending.fill(0.0);
            ch.wet_delay.fill(0.0);
            ch.input_block.fill(0.0);
            ch.long_in.fill(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 确定性伪噪声（LCG 同族，无随机依赖），[-1, 1) 幅度 f32。
    fn lcg_noise(n: usize, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (f64::from(s) / 4294967296.0 * 2.0 - 1.0) as f32
            })
            .collect()
    }

    /// 按给定块长驱动整段输入（分段 process_stereo），返回输出拷贝。
    fn drive(
        stage: &mut ConvolverStage,
        l: &[f32],
        r: &[f32],
        block: usize,
    ) -> (Vec<f32>, Vec<f32>) {
        let mut out_l = l.to_vec();
        let mut out_r = r.to_vec();
        let mut off = 0_usize;
        while off < l.len() {
            let end = (off + block).min(l.len());
            stage
                .process_stereo(&mut out_l[off..end], &mut out_r[off..end])
                .expect("合法驱动");
            off = end;
        }
        (out_l, out_r)
    }

    #[test]
    fn ir_配方_delta_逐位与_ts_同构() {
        let ir = build_ir_recipe(&IrRecipe::Delta { delay: 0.0 }).unwrap();
        assert_eq!(ir, vec![1.0]);
        let ir = build_ir_recipe(&IrRecipe::Delta { delay: 5.0 }).unwrap();
        assert_eq!(ir, vec![0.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
        assert!(build_ir_recipe(&IrRecipe::Delta { delay: -1.0 }).is_err());
        assert!(build_ir_recipe(&IrRecipe::Delta { delay: f64::NAN }).is_err());
    }

    /// 黄金参考：node 直跑 TS buildIrRecipe（test/spec-vectors.test.ts L151 同式）
    /// 的 expNoise(length=1024, seed=777, decay=5, amp=0.5) 前 6 个样本
    /// （f32 位型，JSON 最短往返）。
    const GOLDEN_EXPNOISE_SEED777_DECAY5_AMP05: [u32; 6] = [
        0x3D18_5B0D,
        0x3E82_1F96,
        0x3EAB_4D64,
        0xBD8E_1D4C,
        0xBE88_6320,
        0x3DBA_2B07,
    ];

    #[test]
    fn ir_配方_expnoise_命中_ts_黄金参考() {
        // GWT-CV-07：同 seed 同 length 同 decay 同 amp → f32 量化点逐位一致。
        let ir = build_ir_recipe(&IrRecipe::ExpNoise {
            length: 1024.0,
            seed: 777,
            decay: 5.0,
            amp: 0.5,
        })
        .unwrap();
        assert_eq!(ir.len(), 1024);
        for (i, want_bits) in GOLDEN_EXPNOISE_SEED777_DECAY5_AMP05.iter().enumerate() {
            assert_eq!(
                ir[i].to_bits(),
                *want_bits,
                "expNoise[{i}] 位型不一致：got {:08X} want {want_bits:08X}",
                ir[i].to_bits()
            );
        }
        // 确定性：同配方两次构建逐位一致。
        let ir2 = build_ir_recipe(&IrRecipe::ExpNoise {
            length: 1024.0,
            seed: 777,
            decay: 5.0,
            amp: 0.5,
        })
        .unwrap();
        assert_eq!(ir, ir2);
        // 非法域。
        assert!(build_ir_recipe(&IrRecipe::ExpNoise {
            length: 1.0,
            seed: 1,
            decay: 1.0,
            amp: 1.0
        })
        .is_err());
        assert!(build_ir_recipe(&IrRecipe::ExpNoise {
            length: 8.0,
            seed: 1,
            decay: 0.0,
            amp: 1.0
        })
        .is_err());
        assert!(build_ir_recipe(&IrRecipe::ExpNoise {
            length: f64::NAN,
            seed: 1,
            decay: 1.0,
            amp: 1.0
        })
        .is_err());
    }

    #[test]
    fn 构造钳制与非法采样率() {
        // partitionSize clamp [32, 8192]；round 语义。
        let s = ConvolverStage::new(
            48000.0,
            ConvolverOptions {
                partition_size: 16.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.partition_size(), 32);
        let s = ConvolverStage::new(
            48000.0,
            ConvolverOptions {
                partition_size: 100000.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.partition_size(), 8192);
        // longPartitionSize 向上取 Ls 的 2 的幂整数倍。
        let s = ConvolverStage::new(
            48000.0,
            ConvolverOptions {
                partition_size: 512.0,
                long_partition_size: 1000.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.long_partition_size, 1024); // k = 2
        let s = ConvolverStage::new(
            48000.0,
            ConvolverOptions {
                partition_size: 512.0,
                long_partition_size: 512.0,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(s.long_partition_size, 512); // want ≤ Ls → k=1 退化均匀
                                                // 非法 fs。
        for bad in [0.0_f64, -48000.0, f64::NAN, f64::INFINITY] {
            assert!(ConvolverStage::new(bad, ConvolverOptions::default()).is_err());
        }
    }

    #[test]
    fn mix与_predelay_钳制生效() {
        let mut s = ConvolverStage::new(48000.0, ConvolverOptions::default()).unwrap();
        s.set_mix(-1.0);
        assert_eq!(s.mix, 0.0);
        s.set_mix(2.0);
        assert_eq!(s.mix, 1.0);
        s.set_pre_delay_ms(-5.0);
        assert_eq!(s.pre_delay_samples, 0);
        s.set_pre_delay_ms(2000.0);
        assert_eq!(s.pre_delay_samples, 48000); // 1000ms @48k
        s.set_pre_delay_ms(25.0);
        assert_eq!(s.pre_delay_samples, 1200);
    }

    #[test]
    fn 载入抛错路径_空_nan_全零_未载入即处理() {
        // GWT-CV-08 投影。
        let mut s = ConvolverStage::new(48000.0, ConvolverOptions::default()).unwrap();
        assert_eq!(
            s.load_ir(&[], None).err().unwrap(),
            "invalid impulse response: empty"
        );
        assert_eq!(
            s.load_ir(&[0.1, f32::NAN], None).err().unwrap(),
            "invalid impulse response: contains NaN/Infinity"
        );
        assert_eq!(
            s.load_ir(&[0.1, f32::INFINITY], None).err().unwrap(),
            "invalid impulse response: contains NaN/Infinity"
        );
        assert_eq!(
            s.load_ir(&[0.0, 0.0, 0.0], None).err().unwrap(),
            "invalid impulse response: all zero"
        );
        let mut l = vec![0.0_f32; 8];
        let mut r = vec![0.0_f32; 8];
        assert_eq!(
            s.process_stereo(&mut l, &mut r).err().unwrap(),
            "no impulse response loaded"
        );
    }

    #[test]
    fn delta_ir_延迟直通_首_ls_逐位正零() {
        // GWT-CV-01：δ IR（delay=0）→ 首 Ls 个输出逐位 +0，其后湿路 = 输入延迟
        // Ls 的直通（FFT 往返舍入 1e-7 量级）；getLatencySamples = Ls。
        let ls = 64_usize;
        let mut s = ConvolverStage::new(
            48000.0,
            ConvolverOptions {
                partition_size: ls as f64,
                long_partition_size: ls as f64, // k=1 均匀
                short_region_ms: 100.0,
                de_periodize: true, // 对 δ 为精确无操作
            },
        )
        .unwrap();
        s.load_ir(
            &build_ir_recipe(&IrRecipe::Delta { delay: 0.0 }).unwrap(),
            Some("test"),
        )
        .unwrap();
        s.set_mix(1.0);
        s.set_pre_delay_ms(0.0);
        assert_eq!(s.get_latency_samples(), ls);

        let f = 640_usize;
        let x = lcg_noise(f, 0x1234);
        let (mut out_l, mut out_r) = (x.clone(), vec![0.0_f32; f]);
        // 分块恰好 = Ls（12 块整除语义）。
        let mut off = 0;
        while off < f {
            s.process_stereo(&mut out_l[off..off + ls], &mut out_r[off..off + ls])
                .unwrap();
            off += ls;
        }
        for i in 0..ls {
            assert_eq!(out_l[i].to_bits(), 0_u32, "首 Ls 应逐位 +0 @i={i}");
            assert_eq!(out_r[i].to_bits(), 0_u32, "右声道首 Ls 应逐位 +0 @i={i}");
        }
        for i in ls..f {
            let want = f64::from(x[i - ls]);
            assert!(
                (f64::from(out_l[i]) - want).abs() <= 1e-5 * want.abs().max(1.0),
                "延迟直通偏差 @i={i}: got {} want {want}",
                out_l[i]
            );
        }
    }

    #[test]
    fn 块长无关性_六种切分逐位一致() {
        // GWT-CV-05：湿块按 Ls 记账、湿路按样本放行，均与调用切分无关。
        let f = 4096_usize;
        let x_l = lcg_noise(f, 0xA5A5);
        let x_r = lcg_noise(f, 0x5A5A);
        let mk = || {
            let mut s = ConvolverStage::new(
                48000.0,
                ConvolverOptions {
                    partition_size: 256.0,
                    long_partition_size: 2048.0,
                    short_region_ms: 100.0,
                    de_periodize: false,
                },
            )
            .unwrap();
            s.load_ir(
                &build_ir_recipe(&IrRecipe::ExpNoise {
                    length: 1024.0,
                    seed: 777,
                    decay: 5.0,
                    amp: 0.5,
                })
                .unwrap(),
                None,
            )
            .unwrap();
            s.set_mix(0.8);
            s.set_pre_delay_ms(0.0);
            s
        };
        let (base_l, base_r) = drive(&mut mk(), &x_l, &x_r, 4096);
        for block in [128_usize, 333, 384, 512, 700, 1000] {
            let (got_l, got_r) = drive(&mut mk(), &x_l, &x_r, block);
            assert_eq!(got_l, base_l, "blockSize={block} 左声道不一致");
            assert_eq!(got_r, base_r, "blockSize={block} 右声道不一致");
        }
    }

    /// 朴素线性卷积参考：y[t] = Σ_j ir[j]·x[t−j]。
    fn naive_conv(x: &[f32], ir: &[f32]) -> Vec<f64> {
        let mut out = vec![0.0_f64; x.len() + ir.len() - 1];
        for (t, slot) in out.iter_mut().enumerate() {
            let jmax = ir.len().min(t + 1);
            for j in 0..jmax {
                if t >= j && t - j < x.len() {
                    *slot += f64::from(ir[j]) * f64::from(x[t - j]);
                }
            }
        }
        out
    }

    #[test]
    fn 均匀多短分区_流式输出命中朴素线性卷积() {
        // 数学等价性（§4.4）：湿路 = 完整线性卷积延迟 Ls 对齐后的流式窗口。
        let ls = 32_usize;
        let mut s = ConvolverStage::new(
            48000.0,
            ConvolverOptions {
                partition_size: ls as f64,
                long_partition_size: 2048.0, // IR 短于短区段 → Pl=0 均匀退化
                short_region_ms: 100.0,
                de_periodize: false,
            },
        )
        .unwrap();
        let ir = build_ir_recipe(&IrRecipe::ExpNoise {
            length: 128.0,
            seed: 777,
            decay: 5.0,
            amp: 0.5,
        })
        .unwrap();
        s.load_ir(&ir, None).unwrap();
        s.set_mix(1.0);
        assert_eq!(s.partition_size(), ls);

        let f = 1024_usize; // 恰好 32 块，输入全部进块
        let x = lcg_noise(f, 0x0DD0);
        let (mut out_l, out_r) = drive(&mut s, &x, &vec![0.0_f32; f], 97);
        let full = naive_conv(&x, &ir);
        for i in ls..f {
            let want = full[i - ls];
            let got = f64::from(out_l[i]);
            assert!(
                (got - want).abs() <= 2e-3 * want.abs().max(1.0),
                "均匀分区卷积偏差 @i={i}: got {got} want {want}"
            );
            assert!(got.is_finite());
        }
        // 右声道（全零输入）应全程近零。
        assert!(out_r.iter().all(|v| v.abs() < 1e-5));
    }

    #[test]
    fn 非均匀分区_长分区参与_命中朴素线性卷积() {
        // Pl ≥ 1：shortRegionMs=0 → Ps 收敛到 k−1；IR 尾部由长分区承载。
        let ls = 32_usize;
        let mut s = ConvolverStage::new(
            48000.0,
            ConvolverOptions {
                partition_size: ls as f64,
                long_partition_size: 64.0, // k=2
                short_region_ms: 0.0,
                de_periodize: false,
            },
        )
        .unwrap();
        let ir = build_ir_recipe(&IrRecipe::ExpNoise {
            length: 128.0,
            seed: 777,
            decay: 5.0,
            amp: 0.5,
        })
        .unwrap();
        s.load_ir(&ir, None).unwrap();
        s.set_mix(1.0);
        // 规划断言：Ps = max(1, k−1) = 1、longStart = 32、Pl = ceil(96/64) = 2。
        let plan = s.plan.as_ref().unwrap();
        assert_eq!((plan.ps, plan.pl, plan.long_start), (1, 2, 32));
        assert_eq!(plan.ns, 64);
        assert_eq!(plan.nl, 128);

        let f = 512_usize;
        let x = lcg_noise(f, 0x3141);
        let (out_l, _) = drive(&mut s, &x, &vec![0.0_f32; f], 97);
        let full = naive_conv(&x, &ir);
        for i in ls..f {
            let want = full[i - ls];
            let got = f64::from(out_l[i]);
            assert!(
                (got - want).abs() <= 2e-3 * want.abs().max(1.0),
                "非均匀分区卷积偏差 @i={i}: got {got} want {want}"
            );
        }
    }

    #[test]
    fn 去周期化_对_delta_无操作_对衰减尾真实触发() {
        // §4.3：δ IR（尾部包络未跌破 −60dB）→ dePeriodize on/off 逐位一致；
        // 指数衰减 IR（尾部跌破 −60dB）→ on/off 输出可区分且共享触发点前前缀。
        let f = 2048_usize;
        let x = lcg_noise(f, 0x0721);
        let delta = build_ir_recipe(&IrRecipe::Delta { delay: 0.0 }).unwrap();
        let run = |de: bool, ir: &[f32]| {
            let mut s = ConvolverStage::new(
                48000.0,
                ConvolverOptions {
                    partition_size: 256.0,
                    long_partition_size: 512.0,
                    short_region_ms: 100.0,
                    de_periodize: de,
                },
            )
            .unwrap();
            s.load_ir(ir, None).unwrap();
            s.set_mix(1.0);
            drive(&mut s, &x, &x, 333)
        };
        let (on_l, on_r) = run(true, &delta);
        let (off_l, off_r) = run(false, &delta);
        assert_eq!(on_l, off_l, "δ IR 下 dePeriodize 必须为精确无操作（左）");
        assert_eq!(on_r, off_r, "δ IR 下 dePeriodize 必须为精确无操作（右）");

        // 指数衰减 IR：decay 足够大使尾部跌破 −60dB。
        let noisy = build_ir_recipe(&IrRecipe::ExpNoise {
            length: 1024.0,
            seed: 777,
            decay: 12.0,
            amp: 0.5,
        })
        .unwrap();
        let (on_l, _) = run(true, &noisy);
        let (off_l, _) = run(false, &noisy);
        let first_diff = on_l.iter().zip(off_l.iter()).position(|(a, b)| a != b);
        assert!(first_diff.is_some(), "去周期化触发后输出必须可区分");
        assert!(
            first_diff.unwrap() > 0,
            "触发点之前应逐样本一致（存在共享前缀）"
        );
        // 触发段显著可区分：差异样本占比可观。
        let diff_count = on_l
            .iter()
            .zip(off_l.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(
            diff_count > f / 10,
            "差异样本 {diff_count} 应可观（衰减尾被压缩）"
        );
    }

    #[test]
    fn reset_后重放与全新实例逐位一致() {
        // GWT-CV-08 投影：reset 清流式状态、保留 IR 与分区规划。
        let f = 2048_usize;
        let x_l = lcg_noise(f, 0x7777);
        let x_r = lcg_noise(f, 0x8888);
        let mk = || {
            let mut s = ConvolverStage::new(
                48000.0,
                ConvolverOptions {
                    partition_size: 256.0,
                    long_partition_size: 2048.0,
                    short_region_ms: 100.0,
                    de_periodize: true,
                },
            )
            .unwrap();
            s.load_ir(
                &build_ir_recipe(&IrRecipe::ExpNoise {
                    length: 1024.0,
                    seed: 777,
                    decay: 5.0,
                    amp: 0.5,
                })
                .unwrap(),
                Some("golden"),
            )
            .unwrap();
            s.set_mix(0.7);
            s.set_pre_delay_ms(3.0);
            s
        };
        let mut a = mk();
        let (first_l, first_r) = drive(&mut a, &x_l, &x_r, 384);
        assert_eq!(a.get_ir_name(), Some("golden"));

        a.reset();
        let (replay_l, replay_r) = drive(&mut a, &x_l, &x_r, 384);
        assert_eq!(first_l, replay_l, "reset 重放必须逐位一致（左）");
        assert_eq!(first_r, replay_r, "reset 重放必须逐位一致（右）");

        let (fresh_l, fresh_r) = drive(&mut mk(), &x_l, &x_r, 384);
        assert_eq!(first_l, fresh_l, "reset 重放 = 全新实例（左）");
        assert_eq!(first_r, fresh_r, "reset 重放 = 全新实例（右）");
    }

    #[test]
    fn 延迟语义与_ir_长度解耦() {
        // GWT-CV-06：同一 partitionSize、不同长度 IR → getLatencySamples 恒等。
        for len in [1_usize, 64, 2048, 48000] {
            let mut s = ConvolverStage::new(48000.0, ConvolverOptions::default()).unwrap();
            let ir = match len {
                1 => build_ir_recipe(&IrRecipe::Delta { delay: 0.0 }).unwrap(),
                _ => build_ir_recipe(&IrRecipe::ExpNoise {
                    length: len as f64,
                    seed: 42,
                    decay: 3.0,
                    amp: 0.5,
                })
                .unwrap(),
            };
            s.load_ir(&ir, None).unwrap();
            assert_eq!(s.get_latency_samples(), 512, "IR 长度 {len}");
        }
    }

    #[test]
    fn pre_delay_湿路整体右移_干路不延迟() {
        // §4.5 + TS 实测：wetDelay 共享游标每输出帧推进两次（左右各一次，
        // TS pushDelay 每次 this.wetDelayPos++），故**每声道有效 preDelay 延迟
        // = preDelaySamples / 2**（与 reverb_simple 的"游标每帧推进两次"先例
        // 同构；node 直跑 TS 实证 25ms@48k 的冲激出现在 Ls + 600 = 664 处）。
        // 冻结向量全部 preDelayMs=0（直通分支不推游标），此象限不入向量。
        let ls = 64_usize;
        let mut s = ConvolverStage::new(
            48000.0,
            ConvolverOptions {
                partition_size: ls as f64,
                long_partition_size: ls as f64,
                short_region_ms: 100.0,
                de_periodize: true,
            },
        )
        .unwrap();
        s.load_ir(
            &build_ir_recipe(&IrRecipe::Delta { delay: 0.0 }).unwrap(),
            None,
        )
        .unwrap();
        s.set_mix(1.0);
        s.set_pre_delay_ms(25.0); // 1200 样本 → 每声道有效延迟 600

        let f = 2048_usize;
        let x = lcg_noise(f, 0x0F0F);
        let (mut out_l, _) = drive(&mut s, &x, &x, 128);
        let total_delay = ls + 600;
        for i in 0..total_delay {
            assert_eq!(out_l[i].to_bits(), 0_u32, "延迟段应逐位 +0 @i={i}");
        }
        for i in total_delay..f {
            let want = f64::from(x[i - total_delay]);
            assert!(
                (f64::from(out_l[i]) - want).abs() <= 1e-5 * want.abs().max(1.0),
                "preDelay 后直通偏差 @i={i}"
            );
        }
    }

    /// 黄金参考：node 经 esbuild bundle 直跑仓库根 src/dsp/Convolver.ts（G2 先例）。
    /// 配置：fs=48000、Ls=32、wantLl=64（k=2）、shortRegionMs=0（Ps=1/longStart=32/
    /// Pl=2）、IR=expNoise(128,777,5,0.5)、preDelay=0；L=lcg(512,0x3141)、R=全零、
    /// blockSize=97 非整除流式。
    mod golden {
        /// mix=1：湿路延迟段之后（下标 32..48）的输出 f32 位型。
        pub const CONV_WET_32_48: [u32; 16] = [
            0xBCE5_ECDA,
            0xBE1A_7F57,
            0xBCBC_8D04,
            0x3E6F_7C9E,
            0xBCBA_F6D1,
            0xBE05_23DD,
            0x3F06_EA1F,
            0xBE9C_DD43,
            0xBED3_1A2B,
            0x3F20_4D21,
            0x3E93_5181,
            0x3E2B_F357,
            0x3EDD_D6C4,
            0xBD89_8B03,
            0xBE8F_3B06,
            0x3D1E_57B1,
        ];
        /// mix=0.8：干湿混合（含逐样本放行首段）的输出 f32 位型（下标 0..8）。
        pub const CONV_MIX08_0_8: [u32; 8] = [
            0xBE1A_890A,
            0x3E3D_600E,
            0xBD9E_CF74,
            0xBD18_00CD,
            0x3E12_46E9,
            0x3B55_2366,
            0xBDAC_4E41,
            0xBD0E_FB60,
        ];
    }

    /// 复刻黄金参考场景的驱动（左 = lcg 噪声，右 = 全零，块长 97）。
    fn drive_golden(mix: f64) -> Vec<f32> {
        let mut s = ConvolverStage::new(
            48000.0,
            ConvolverOptions {
                partition_size: 32.0,
                long_partition_size: 64.0,
                short_region_ms: 0.0,
                de_periodize: false,
            },
        )
        .unwrap();
        s.load_ir(
            &build_ir_recipe(&IrRecipe::ExpNoise {
                length: 128.0,
                seed: 777,
                decay: 5.0,
                amp: 0.5,
            })
            .unwrap(),
            Some("golden"),
        )
        .unwrap();
        s.set_mix(mix);
        s.set_pre_delay_ms(0.0);
        let f = 512_usize;
        let l = lcg_noise(f, 0x3141);
        let r = vec![0.0_f32; f];
        let (out_l, _) = drive(&mut s, &l, &r, 97);
        out_l
    }

    #[test]
    fn 非均匀分区流式输出_命中_ts_黄金参考位型() {
        // 逐位锚点：分区规划 + IR 重建 + 滑动窗口放行 + 全部 f32 落点与 TS 同态。
        let wet = drive_golden(1.0);
        for (i, want) in golden::CONV_WET_32_48.iter().enumerate() {
            let idx = 32 + i;
            assert_eq!(
                wet[idx].to_bits(),
                *want,
                "wet[{idx}]：got {:08X} want {want:08X}",
                wet[idx].to_bits()
            );
        }
        let mixed = drive_golden(0.8);
        for (i, want) in golden::CONV_MIX08_0_8.iter().enumerate() {
            assert_eq!(
                mixed[i].to_bits(),
                *want,
                "mix0.8[{i}]：got {:08X} want {want:08X}",
                mixed[i].to_bits()
            );
        }
    }

    #[test]
    fn stage_契约_未载入即_panic_镜像_ts_抛错() {
        let mut s = ConvolverStage::new(48000.0, ConvolverOptions::default()).unwrap();
        s.prepare(64);
        let mut l = vec![0.0_f32; 64];
        let mut r = vec![0.0_f32; 64];
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            s.process(&mut l, &mut r);
        }));
        assert!(
            result.is_err(),
            "未载入 IR 的 Stage::process 应 panic（镜像 TS 抛错）"
        );
    }
}
