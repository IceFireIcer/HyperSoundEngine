//! fdn_reverb —— FDN 网络混响阶段（反馈延迟网络，算法创新模块）。
//!
//! 行为事实标准：仓库根 `src/dsp/FdnReverb.ts`；规格：`specs/dsp/fdn-reverb.md`。
//! 本文件是该 TS 类的逐行移植，关键对应关系：
//!
//! - 结构（规格 §一）：左右各持一个独立 [`FdnNetwork`]（两套不同素数表 → 立体声
//!   去相关）；每样本各线读出 → 一阶低通阻尼（Freeverb damping 语义）→ Householder
//!   正交混合 → 反馈增益 g（≤0.98 保证环路 g²·(低通 ≤1) < 1，无条件稳定）；输入按
//!   1/√N 注入（能量归一）、输出取 1/N 等权平均；preDelay 为输入侧独立环形延迟线
//!   （左右各持独立游标；干路不经 preDelay；len=0 时读取短路为恒等，但游标仍推进）。
//! - 参数语义（规格 §三）：type 基准表（§3.2）±0.25 微调（effRoom/effDamp 混合式）、
//!   wet/dry/width/preDelayMs 双向钳制、延迟长度 = max(1, round(素数基底 × delayScale ×
//!   fs/44100))、lines 仅 2/4/8/16（`Math.trunc` 后其余值报错，规格 GWT-FDN-12）；
//!   lines 线数结构变化时清空全部状态（规格 §4.2）。
//!
//! # 数值精度铁律的落点
//!
//! - 一切以 TS Number（f64）参与运算的中间量——effRoom/effDamp、inject/outGain、
//!   wet1/wet2/dry、阻尼低通输出 `f`、Householder 行和 `u`、湿路求和 `sum`/`y`——
//!   全部用 f64 复刻，运算顺序与 TS 逐行一致；
//! - f32 落点与 TS Float32Array 写入点一一对应：每线延迟缓冲 `buf`、阻尼 store 状态、
//!   过程暂存 `filt`（写回即量化；写回循环读回的是量化值）、preDelay 延迟线与最终
//!   输出样本。特别注意 `sum` 累加的是**量化前**的 f64 阻尼输出（TS `sum += f` 位于
//!   `filt[j] = f` 之后但使用未量化的 `f`），而反馈项 `filt[j] − u` 读的是量化后的
//!   `filt[j]`——两个量化口径并存是对拍逐位一致的关键；
//! - `Math.round`/`Math.ceil` 的操作数在本模块全部非负（先经钳制），与 Rust
//!   `f64::round`/`f64::ceil` 结果一致（正数域"半值向上"="半值远离零"）；
//!   `Math.min`/`Math.max` 的 NaN 传播语义以 [`js_min`]/[`js_max`] 显式复刻
//!   （理由同 biquad.rs）。
//!
//! # 实时安全
//!
//! 延迟缓冲在构造时按最大配置一次预分配（16 线 × `maxDelay = ceil(883·1.5·fs/44100)+2`、
//! preDelay 各 `ceil(fs)+1`）；`set_params` 只改系数与逻辑长度，不重新分配；
//! `process` 稳态零分配、零锁、零系统调用。

use crate::Stage;

/// 最大延迟线数（TS `MAX_LINES`，构造期预分配上限）。
const MAX_LINES: usize = 16;
/// 所有素数表中的最大基底（TS `MAX_DELAY_BASE`）。
const MAX_DELAY_BASE: f64 = 883.0;
/// type 表最大 delayScale（stage，TS `MAX_DELAY_SCALE`）。
const MAX_DELAY_SCALE: f64 = 1.5;
/// 反馈增益安全上限（TS `MAX_FEEDBACK`，g² < 1 → 无条件稳定）。
const MAX_FEEDBACK: f64 = 0.98;
/// preDelay 上限 ms（TS `MAX_PREDELAY_MS`）。
const MAX_PREDELAY_MS: f64 = 1000.0;

// 互质（素数）延迟基底 @44.1kHz（规格 §3.3；左右各一套不同素数 → 去相关）。
const DELAYS_L_2: [f64; 2] = [499.0, 547.0];
const DELAYS_R_2: [f64; 2] = [521.0, 563.0];
const DELAYS_L_4: [f64; 4] = [599.0, 641.0, 677.0, 709.0];
const DELAYS_R_4: [f64; 4] = [607.0, 653.0, 683.0, 727.0];
const DELAYS_L_8: [f64; 8] = [701.0, 719.0, 733.0, 757.0, 773.0, 797.0, 811.0, 823.0];
const DELAYS_R_8: [f64; 8] = [709.0, 727.0, 739.0, 761.0, 787.0, 809.0, 821.0, 829.0];
const DELAYS_L_16: [f64; 16] = [
    701.0, 719.0, 733.0, 757.0, 773.0, 797.0, 811.0, 823.0, 827.0, 839.0, 853.0, 857.0, 859.0,
    863.0, 877.0, 881.0,
];
const DELAYS_R_16: [f64; 16] = [
    709.0, 727.0, 739.0, 761.0, 787.0, 809.0, 821.0, 829.0, 839.0, 853.0, 857.0, 859.0, 863.0,
    877.0, 881.0, 883.0,
];

/// 按线数取素数基底表（左/右）；线数合法性由 [`normalize_lines`] 先行保证。
fn delays_l(n: usize) -> &'static [f64] {
    match n {
        2 => &DELAYS_L_2,
        4 => &DELAYS_L_4,
        16 => &DELAYS_L_16,
        _ => &DELAYS_L_8,
    }
}

fn delays_r(n: usize) -> &'static [f64] {
    match n {
        2 => &DELAYS_R_2,
        4 => &DELAYS_R_4,
        16 => &DELAYS_R_16,
        _ => &DELAYS_R_8,
    }
}

/// type → 房间参数基准条目（对齐 TS `FdnTypeTable`；FDN 自有调音，与 reverb-simple 表不同）。
#[derive(Debug, Clone, Copy)]
struct TypeTable {
    room_size: f64,
    damping: f64,
    delay_scale: f64,
}

// type → 房间参数表（与 TS TYPE_TABLE 逐项一致，规格 §3.2）。
const TABLE_HALL: TypeTable = TypeTable { room_size: 0.7, damping: 0.4, delay_scale: 1.3 };
const TABLE_ROOM: TypeTable = TypeTable { room_size: 0.4, damping: 0.6, delay_scale: 0.6 };
const TABLE_PLATE: TypeTable = TypeTable { room_size: 0.6, damping: 0.2, delay_scale: 0.7 };
const TABLE_SPRING: TypeTable = TypeTable { room_size: 0.3, damping: 0.8, delay_scale: 0.35 };
const TABLE_STAGE: TypeTable = TypeTable { room_size: 0.55, damping: 0.5, delay_scale: 1.5 };

/// 查表；未知枚举运行时回退 hall（GWT-FDN-12 防御路径，向量不得依赖）。
fn type_table(reverb_type: &str) -> TypeTable {
    match reverb_type {
        "room" => TABLE_ROOM,
        "plate" => TABLE_PLATE,
        "spring" => TABLE_SPRING,
        "stage" => TABLE_STAGE,
        _ => TABLE_HALL,
    }
}

/// 对齐 TS `FdnReverbParams` 的参数快照（字段名蛇形转换）。
#[derive(Debug, Clone)]
pub struct FdnReverbParams {
    pub room_size: f64,
    pub damping: f64,
    pub wet: f64,
    pub dry: f64,
    pub pre_delay_ms: f64,
    pub width: f64,
    /// TS `ReverbType` 五种之一：hall / room / plate / spring / stage。
    /// 未知值运行时回退 hall（与 TS `TYPE_TABLE[p.type] ?? TYPE_TABLE.hall` 一致）。
    pub reverb_type: String,
    /// TS 可选字段；`None` = 缺省 8（TS `p.lines === undefined`）。
    pub lines: Option<f64>,
}

/// 复刻 JS Math.min(a, b) 的 NaN 传播语义（理由见 biquad.rs 的同名函数）。
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a < b {
        a
    } else {
        b
    }
}

/// 复刻 JS Math.max(a, b) 的 NaN 传播语义（理由同上）。
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a > b {
        a
    } else {
        b
    }
}

/// TS `clamp01`：v < 0 → 0；v > 1 → 1；否则原值（NaN 三目链均不命中 → 原样返回）。
fn clamp01(v: f64) -> f64 {
    if v < 0.0 {
        0.0
    } else if v > 1.0 {
        1.0
    } else {
        v
    }
}

/// 线数校验：仅允许 2/4/8/16（素数表齐备），缺省 8（TS `normalizeLines`）。
///
/// TS 对 `undefined` 取 8、其余值先 `Math.trunc`；非法值抛
/// `Error('FdnReverb: lines 必须为 2/4/8/16, 收到 <v>')`。
fn normalize_lines(v: Option<f64>) -> Result<usize, String> {
    let n = match v {
        None => 8.0_f64,
        Some(v) => v.trunc(),
    };
    if n != 2.0 && n != 4.0 && n != 8.0 && n != 16.0 {
        return Err(format!("FdnReverb: lines 必须为 2/4/8/16, 收到 {v:?}"));
    }
    Ok(n as usize)
}

/// FDN 单声道网络（左右各持一个实例，仅素数表不同；对齐 TS `FdnNetwork`）。
///
/// 状态方程（每样本，对每条线 j，规格 §4.1）：
/// ```text
/// out_j  = buf_j[pos_j]
/// filt_j = out_j × damp2 + store_j × damp1  （写回即量化 f32）
/// u = (2/N) × Σ_i filt_i（以量化前的 f64 值求和）
/// buf_j[pos_j] = (1/√N) × x + g × (filt_j − u)（写回即量化 f32）
/// y = (1/N) × Σ_j out_j
/// ```
struct FdnNetwork {
    fs: f64,
    /// 延迟线缓冲：MAX_LINES 条、构造时按 max_delay 预分配（TS `Float32Array[]`）。
    buf: Vec<Vec<f32>>,
    len: [usize; MAX_LINES],
    pos: [usize; MAX_LINES],
    /// 阻尼低通状态（TS `Float32Array`——每样本写回即量化 f32）。
    store: [f32; MAX_LINES],
    /// 读出暂存（写回即量化；由于源本身是 f32 读出，量化无损，保留以对齐结构）。
    out: [f32; MAX_LINES],
    /// 阻尼输出暂存（写回即量化——反馈项读的是量化值，而 sum 用量化前的 f64）。
    filt: [f32; MAX_LINES],
    n: usize,
    g: f64,
    damp1: f64,
    damp2: f64,
    inject: f64,
    out_gain: f64,
}

impl FdnNetwork {
    fn new(fs: f64, max_delay: usize) -> Self {
        Self {
            fs,
            buf: vec![vec![0.0_f32; max_delay]; MAX_LINES],
            len: [0; MAX_LINES],
            pos: [0; MAX_LINES],
            store: [0.0; MAX_LINES],
            out: [0.0; MAX_LINES],
            filt: [0.0; MAX_LINES],
            n: 0,
            g: 0.0,
            damp1: 0.0,
            damp2: 1.0,
            inject: 0.0,
            out_gain: 0.0,
        }
    }

    /// 配置线数、延迟长度、反馈/阻尼/注入/输出增益（只改系数，不重新分配；
    /// 对齐 TS `FdnNetwork.configure`，逐行同序）。
    fn configure(&mut self, n: usize, base_delays: &[f64], delay_scale: f64, g: f64, damp1: f64, damp2: f64) {
        self.n = n;
        self.g = g;
        self.damp1 = damp1;
        self.damp2 = damp2;
        // 注入 1/√N（能量归一）；输出 1/N（等权平均）。
        self.inject = 1.0 / (n as f64).sqrt();
        self.out_gain = 1.0 / n as f64;
        let scale = (delay_scale * self.fs) / 44100.0;
        for j in 0..n {
            // TS：Math.max(1, Math.round(baseDelays[j] * scale))——操作数非负。
            self.len[j] = js_max(1.0, (base_delays[j] * scale).round()) as usize;
        }
    }

    /// 单样本处理：输入 x，返回该网络的湿输出（就地更新状态；对齐 TS `process`）。
    fn process(&mut self, x: f64) -> f64 {
        let n = self.n;
        // 1) 读出各线输出 + 反馈回路一阶低通（sum 累加量化前的 f64 值）。
        let mut sum = 0.0_f64;
        for j in 0..n {
            let o = f64::from(self.buf[j][self.pos[j]]);
            self.out[j] = o as f32;
            let f = o * self.damp2 + f64::from(self.store[j]) * self.damp1;
            self.filt[j] = f as f32;
            self.store[j] = f as f32;
            sum += f;
        }
        // 2) Householder 正交混合 + 反馈增益：(H·filt)_j = filt_j − (2/N)·Σfilt。
        let u = (2.0 / n as f64) * sum;
        for j in 0..n {
            let p = self.pos[j];
            self.buf[j][p] = (self.inject * x + self.g * (f64::from(self.filt[j]) - u)) as f32;
            let np = p + 1;
            self.pos[j] = if np >= self.len[j] { 0 } else { np };
        }
        // 3) 输出：各线等权平均。
        let mut y = 0.0_f64;
        for j in 0..n {
            y += f64::from(self.out[j]);
        }
        y * self.out_gain
    }

    /// 清空全部 MAX_LINES 条缓冲、游标与 store（对齐 TS `reset`；out/filt 为
    /// 逐样本覆写的暂存，TS reset 不清，此处同样不清）。
    fn reset(&mut self) {
        for j in 0..MAX_LINES {
            self.buf[j].fill(0.0);
            self.pos[j] = 0;
            self.store[j] = 0.0;
        }
    }
}

/// FDN 算法混响阶段（立体声、就地处理；对齐 TS `FdnReverb` 私有域）。
pub struct FdnReverbStage {
    fs: f64,
    left: FdnNetwork,
    right: FdnNetwork,

    // preDelay 输入侧独立延迟线（左右各持独立缓冲与独立位置指针——共用游标会使
    // 有效延迟减半，TS 注释明确此点）。
    pre_delay_l: Vec<f32>,
    pre_delay_r: Vec<f32>,
    pre_delay_pos_l: usize,
    pre_delay_pos_r: usize,
    pre_delay_len: usize,

    // 混音参数（wet/dry + width 交叉，与 reverb-simple 相同公式）。
    wet1: f64,
    wet2: f64,
    dry: f64,
    line_count: usize,
}

impl FdnReverbStage {
    /// 以显式参数快照构造（对齐 TS「构造即分配最大缓冲 + `setParams(p)`」组合语义）。
    ///
    /// fs ≤ 0 或非有限时报错（对齐 TS `Error('invalid sample rate')`）；
    /// lines 非法（2/4/8/16 之外）时报错（GWT-FDN-12）。
    pub fn from_params(sample_rate: f64, params: FdnReverbParams) -> Result<Self, String> {
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            return Err("invalid sample rate".into());
        }
        // 预分配最大延迟：最长素数基底 × 最大 delayScale × fs/44100（TS 构造器
        // `Math.ceil((MAX_DELAY_BASE * MAX_DELAY_SCALE * fs) / 44100) + 2`）。
        let max_delay =
            ((((MAX_DELAY_BASE * MAX_DELAY_SCALE) * sample_rate) / 44100.0).ceil() as usize) + 2;
        // preDelay 上限 1000ms：物理长度 ceil(fs)+1（TS `new Float32Array(Math.ceil(fs) + 1)`）。
        let pre_delay_cap = (sample_rate.ceil() as usize) + 1;

        let mut stage = Self {
            fs: sample_rate,
            left: FdnNetwork::new(sample_rate, max_delay),
            right: FdnNetwork::new(sample_rate, max_delay),
            pre_delay_l: vec![0.0; pre_delay_cap],
            pre_delay_r: vec![0.0; pre_delay_cap],
            pre_delay_pos_l: 0,
            pre_delay_pos_r: 0,
            pre_delay_len: 0,
            wet1: 0.0,
            wet2: 0.0,
            dry: 0.0,
            line_count: 8,
        };
        stage.set_params(params)?;
        Ok(stage)
    }

    /// 覆盖参数快照（对齐 TS `setParams`，逐行同序；缓冲与游标保留，唯一例外是
    /// lines 线数结构变化时清空全部状态，规格 §4.2）。
    pub fn set_params(&mut self, p: FdnReverbParams) -> Result<(), String> {
        let t = type_table(&p.reverb_type);
        let n = normalize_lines(p.lines)?;

        // type 提供基准，用户参数在基准附近 ±0.25 微调（中性 0.5 即类型本身）。
        // TS：min(0.98, max(0, base + (clamp01(u) - 0.5) * 0.5))。
        let eff_room = js_min(MAX_FEEDBACK, js_max(0.0, t.room_size + (clamp01(p.room_size) - 0.5) * 0.5));
        // TS：min(0.99, max(0.01, base + (clamp01(u) - 0.5) * 0.5))。
        let eff_damp = js_min(0.99, js_max(0.01, t.damping + (clamp01(p.damping) - 0.5) * 0.5));

        // wet/dry + width 交叉混合（与 reverb-simple 相同公式）。
        let wet = js_min(4.0, js_max(0.0, p.wet));
        let width = js_min(2.0, js_max(0.0, p.width));
        self.wet1 = wet * (width / 2.0 + 0.5);
        self.wet2 = wet * ((1.0 - width) / 2.0);
        self.dry = js_min(4.0, js_max(0.0, p.dry));

        // preDelay：TS `Math.round((pdMs * this.fs) / 1000)`（操作数非负）。
        let pd_ms = js_min(MAX_PREDELAY_MS, js_max(0.0, p.pre_delay_ms));
        self.pre_delay_len = ((pd_ms * self.fs) / 1000.0).round() as usize;

        // 延迟长度：素数基底 × type.delayScale × fs/44100（互质属性取整后近似保持）。
        let (base_l, base_r) = (delays_l(n), delays_r(n));
        self.left.configure(n, base_l, t.delay_scale, eff_room, eff_damp, 1.0 - eff_damp);
        self.right.configure(n, base_r, t.delay_scale, eff_room, eff_damp, 1.0 - eff_damp);

        // 线数结构变化：清空状态，避免残留数据跨结构泄漏（TS 同款判断）。
        if n != self.line_count {
            self.line_count = n;
            self.reset();
        }
        Ok(())
    }
}

impl Stage for FdnReverbStage {
    /// 缓冲已在构造时按最大配置分配，与块长无关，无需再分配。
    fn prepare(&mut self, _max_block_size: usize) {}

    /// 就地处理立体声块；状态跨块保持（规格 §4.1：全部状态为纯逐样本递推，
    /// GWT-FDN-08 分块与整块逐位一致）。
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        debug_assert_eq!(left.len(), right.len(), "左右声道块长必须一致（Stage 契约）");
        let frames = left.len().min(right.len());
        for i in 0..frames {
            let xl = f64::from(left[i]);
            let xr = f64::from(right[i]);

            // ---- preDelay（输入侧；左右各持独立游标，len=0 时读取短路为恒等，
            //      但游标每样本仍推进一次——对齐 TS delayPush/advancePos 拆分写法）----
            let dl: f64;
            if self.pre_delay_len == 0 {
                dl = xl;
            } else {
                let size = self.pre_delay_l.len();
                let mut rp = self.pre_delay_pos_l as isize - self.pre_delay_len as isize;
                if rp < 0 {
                    rp += size as isize;
                }
                dl = f64::from(self.pre_delay_l[rp as usize]);
                self.pre_delay_l[self.pre_delay_pos_l] = xl as f32;
            }
            self.pre_delay_pos_l += 1;
            if self.pre_delay_pos_l >= self.pre_delay_l.len() {
                self.pre_delay_pos_l = 0;
            }
            let dr: f64;
            if self.pre_delay_len == 0 {
                dr = xr;
            } else {
                let size = self.pre_delay_r.len();
                let mut rp = self.pre_delay_pos_r as isize - self.pre_delay_len as isize;
                if rp < 0 {
                    rp += size as isize;
                }
                dr = f64::from(self.pre_delay_r[rp as usize]);
                self.pre_delay_r[self.pre_delay_pos_r] = xr as f32;
            }
            self.pre_delay_pos_r += 1;
            if self.pre_delay_pos_r >= self.pre_delay_r.len() {
                self.pre_delay_pos_r = 0;
            }

            // ---- 左右独立 FDN 网络（不同素数表 → 去相关）----
            let wet_l = self.left.process(dl);
            let wet_r = self.right.process(dr);

            // ---- wet/dry + width 交叉混合（加法结合顺序与 TS 一致；交叉项使用对方
            //      声道湿声。width=0 时左右在 1 ulp f32 量级一致而非逐位，规格 §4.4.3）----
            left[i] = (xl * self.dry + wet_l * self.wet1 + wet_r * self.wet2) as f32;
            right[i] = (xr * self.dry + wet_r * self.wet1 + wet_l * self.wet2) as f32;
        }
    }

    /// 清空左右网络的缓冲/游标/store 与 preDelay 缓冲、游标（对齐 TS `reset`；
    /// 派生参数与延迟长度保留）。
    fn reset(&mut self) {
        self.left.reset();
        self.right.reset();
        self.pre_delay_l.fill(0.0);
        self.pre_delay_r.fill(0.0);
        self.pre_delay_pos_l = 0;
        self.pre_delay_pos_r = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 确定性伪噪声（LCG，无随机依赖），幅度 [-amp, amp)。
    fn lcg_noise(n: usize, seed: u32, amp: f64) -> Vec<f32> {
        let mut u = seed;
        (0..n)
            .map(|_| {
                u = u.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (((f64::from(u) / 4_294_967_296.0) * 2.0 - 1.0) * amp) as f32
            })
            .collect()
    }

    fn base_params(reverb_type: &str) -> FdnReverbParams {
        FdnReverbParams {
            room_size: 0.5,
            damping: 0.5,
            wet: 0.3,
            dry: 0.7,
            pre_delay_ms: 0.0,
            width: 1.0,
            reverb_type: reverb_type.to_string(),
            lines: Some(8.0),
        }
    }

    /// 与 Node 直跑 TS 源取得的黄金参考比对（写入点 f32 量化后的期望值）。
    fn assert_f32_close(got: f32, want: f64, label: &str) {
        let diff = (f64::from(got) - want).abs();
        assert!(
            diff <= 1e-6 * want.abs().max(1e-9),
            "{label}：got {got}，want {want}"
        );
    }

    #[test]
    fn fs48000_hall基准_延迟长度与派生系数命中ts黄金参考() {
        // 黄金参考：node 直跑 src/dsp/FdnReverb.ts（case2 参数形态）。
        let s = FdnReverbStage::from_params(48000.0, base_params("hall")).unwrap();
        assert_eq!(s.left.n, 8);
        assert_eq!(s.left.g, 0.7, "中性 0.5 → 生效值即 hall 基准 roomSize");
        assert_eq!(s.left.damp1, 0.4);
        assert_eq!(s.left.damp2, 0.6);
        assert_eq!(s.left.inject, 0.35355339059327373);
        assert_eq!(s.left.out_gain, 0.125);
        assert_eq!(s.left.len, [992, 1017, 1037, 1071, 1094, 1128, 1148, 1165, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(s.right.len, [1003, 1029, 1046, 1077, 1114, 1145, 1162, 1173, 0, 0, 0, 0, 0, 0, 0, 0]);
        // 预分配上限：ceil(883*1.5*48000/44100)+2 与 ceil(fs)+1。
        assert_eq!(s.left.buf[0].len(), 1444);
        assert_eq!(s.pre_delay_l.len(), 48_001);
        assert_eq!(s.pre_delay_len, 0);
        assert_eq!(s.wet1, 0.3 * (1.0 / 2.0 + 0.5));
        assert_eq!(s.wet2, 0.3 * ((1.0 - 1.0) / 2.0));
        assert_eq!(s.dry, 0.7);
    }

    #[test]
    fn fs44100_room_与fs48000_stage满配_黄金参考() {
        // case3 形态：room @44100，scale = 0.6 → 延迟 = 基底 × 0.6。
        let mut p = base_params("room");
        p.wet = 0.8;
        p.dry = 0.3;
        p.width = 0.0;
        let s = FdnReverbStage::from_params(44100.0, p).unwrap();
        assert_eq!(s.left.g, 0.4);
        assert_eq!(s.left.damp1, 0.6);
        assert_eq!(s.left.len, [421, 431, 440, 454, 464, 478, 487, 494, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(s.right.len, [425, 436, 443, 457, 472, 485, 493, 497, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(s.left.buf[0].len(), 1327);
        assert_eq!(s.pre_delay_l.len(), 44_101);
        // width=0 → wet1 = wet2 = wet/2。
        assert_eq!(s.wet1, 0.4);
        assert_eq!(s.wet2, 0.4);

        // case4 形态：stage + lines=16 + 多项越界钳制。
        let mut p = base_params("stage");
        p.room_size = 1.5;
        p.damping = -0.3;
        p.wet = 4.5;
        p.dry = -0.5;
        p.pre_delay_ms = 80.0;
        p.width = 2.5;
        p.lines = Some(16.0);
        let s = FdnReverbStage::from_params(48000.0, p).unwrap();
        // clamp01(1.5)=1 → 0.55+0.25=0.8；clamp01(-0.3)=0 → 0.5-0.25=0.25。
        assert_eq!(s.left.g, 0.8);
        assert_eq!(s.left.damp1, 0.25);
        assert_eq!(s.left.damp2, 0.75);
        assert_eq!(s.left.inject, 0.25);
        assert_eq!(s.left.out_gain, 0.0625);
        assert_eq!(
            s.left.len,
            [1144, 1174, 1197, 1236, 1262, 1301, 1324, 1344, 1350, 1370, 1393, 1399, 1402, 1409, 1432, 1438]
        );
        assert_eq!(
            s.right.len,
            [1158, 1187, 1207, 1242, 1285, 1321, 1340, 1353, 1370, 1393, 1399, 1402, 1409, 1432, 1438, 1442]
        );
        assert_eq!(s.pre_delay_len, 3840, "80ms @48k");
        // wet=4.5→4、width=2.5→2：wet1 = 4*(2/2+0.5)=6、wet2 = 4*((1-2)/2)=-2（负交叉）。
        assert_eq!(s.wet1, 6.0);
        assert_eq!(s.wet2, -2.0);
        assert_eq!(s.dry, 0.0);
    }

    #[test]
    fn wet0_dry1时输出与输入逐位一致_含preDelay越上界() {
        // GWT-FDN-01/06：湿路项精确为零、干路乘 1.0 → 逐位恒等；preDelayMs=1200
        // 越上界按 1000ms 生效且只作用于湿路。
        let mut p = base_params("hall");
        p.wet = 0.0;
        p.dry = 1.0;
        p.pre_delay_ms = 1200.0;
        let mut s = FdnReverbStage::from_params(48000.0, p).unwrap();
        s.prepare(64);
        assert_eq!(s.pre_delay_len, 48_000, "1200ms 越上界按 1000ms 生效");

        let in_l = lcg_noise(512, 0xC0FFEE, 0.7);
        let in_r: Vec<f32> = (0..512)
            .map(|i| ((2.0 * std::f64::consts::PI * 220.0 * i as f64 / 48000.0).sin() * 0.5
                + (2.0 * std::f64::consts::PI * 3300.0 * i as f64 / 48000.0 + std::f64::consts::FRAC_PI_4).sin() * 0.25)
                as f32)
            .collect();
        let mut offset = 0_usize;
        while offset < 512 {
            let end = (offset + 64).min(512);
            let mut l = in_l[offset..end].to_vec();
            let mut r = in_r[offset..end].to_vec();
            s.process(&mut l, &mut r);
            for (k, &x) in in_l[offset..end].iter().enumerate() {
                assert_eq!(l[k].to_bits(), x.to_bits(), "left 帧 {}", offset + k);
            }
            for (k, &x) in in_r[offset..end].iter().enumerate() {
                assert_eq!(r[k].to_bits(), x.to_bits(), "right 帧 {}", offset + k);
            }
            offset = end;
        }
    }

    #[test]
    fn 冲激响应_首个湿声样本出现在最短延迟线处_黄金值() {
        // case2 形态（hall @48k 纯湿声）：最短左线 = 992 → 前 992 帧逐位为零，
        // 第 992 帧黄金值 = f32(inject·1) × outGain 的写回落点。
        let mut p = base_params("hall");
        p.wet = 1.0;
        p.dry = 0.0;
        let mut s = FdnReverbStage::from_params(48000.0, p).unwrap();
        s.prepare(128);
        // 窗口覆盖多轮环路往返（最短线 992 帧/轮），尾部应显著低于首回波。
        let n = 19_200;
        let mut l = vec![0.0_f32; n];
        let mut r = vec![0.0_f32; n];
        l[0] = 1.0;
        let mut offset = 0;
        while offset < n {
            let end = (offset + 128).min(n);
            s.process(&mut l[offset..end], &mut r[offset..end]);
            offset = end;
        }
        for (i, &x) in l.iter().enumerate().take(992) {
            assert_eq!(x.to_bits(), 0_u32, "left[{i}] 应仍为零（冲激未从最短线回读）");
        }
        assert_f32_close(l[992], 0.044_194_173_067_808_15, "首个湿声样本");
        assert!(r.iter().all(|&x| x.to_bits() == 0_u32), "右声道零输入 + width=1 无交叉");
        // GWT-FDN-03：尾音有限、无自激发散，且远端尾部显著低于首回波（衰减）。
        assert!(l.iter().all(|&x| x.is_finite()));
        let first_echo = f64::from(l[992]).abs();
        let tail_peak = l[n - 2000..].iter().map(|&x| f64::from(x).abs()).fold(0.0_f64, f64::max);
        assert!(
            tail_peak < first_echo * 0.1,
            "远端尾音应显著低于首回波（衰减），实际 tail={tail_peak}，first={first_echo}"
        );
    }

    #[test]
    fn width0_同源输入左右差不超过1e6容差_非逐位断言() {
        // GWT-FDN-05：width=0 湿路对半交叉；左右湿声来自不同素数表网络，
        // 混合求和顺序不同 → 只主张 ≤1e-6 量级一致（规格 §4.4.3，不主张逐位）。
        let mut p = base_params("room");
        p.wet = 0.8;
        p.dry = 0.3;
        p.width = 0.0;
        let mut s = FdnReverbStage::from_params(44100.0, p).unwrap();
        s.prepare(441);
        let n = 2000;
        let input = lcg_noise(n, 61_003, 0.6);
        let mut l = input.clone();
        let mut r = input.clone();
        let mut offset = 0;
        while offset < n {
            let end = (offset + 441).min(n);
            s.process(&mut l[offset..end], &mut r[offset..end]);
            offset = end;
        }
        for i in 0..n {
            let diff = (f64::from(l[i]) - f64::from(r[i])).abs();
            let reference = f64::from(l[i]).abs().max(1e-9);
            assert!(diff <= 1e-6 * reference, "width=0 左右差超容差 @{i}: {diff}");
        }
    }

    #[test]
    fn 分块处理与整块处理逐位一致() {
        // GWT-FDN-08：blockSize=97 不整除 1000（末块短块），全部状态逐样本递推。
        let mut p = base_params("plate");
        p.wet = 0.9;
        p.dry = 0.4;
        p.pre_delay_ms = 7.5;
        let mut whole = FdnReverbStage::from_params(48000.0, p.clone()).unwrap();
        whole.prepare(1000);
        let in_l = lcg_noise(1000, 1234, 0.7);
        let in_r = lcg_noise(1000, 4321, 0.5);
        let mut wl = in_l.clone();
        let mut wr = in_r.clone();
        whole.process(&mut wl, &mut wr);

        let mut chunked = FdnReverbStage::from_params(48000.0, p).unwrap();
        chunked.prepare(97);
        let mut cl = in_l.clone();
        let mut cr = in_r.clone();
        let mut offset = 0;
        while offset < 1000 {
            let end = (offset + 97).min(1000);
            chunked.process(&mut cl[offset..end], &mut cr[offset..end]);
            offset = end;
        }
        for i in 0..1000 {
            assert_eq!(wl[i].to_bits(), cl[i].to_bits(), "left[{i}] 分块不一致");
            assert_eq!(wr[i].to_bits(), cr[i].to_bits(), "right[{i}] 分块不一致");
        }
    }

    #[test]
    fn reset后重放与全新实例逐位一致() {
        // GWT-FDN-09：reset 清空延迟缓冲/游标/store/preDelay 后重放 = 首次从零状态。
        let mut p = base_params("hall");
        p.pre_delay_ms = 13.7;
        p.width = 0.8;
        let mut a = FdnReverbStage::from_params(48000.0, p.clone()).unwrap();
        a.prepare(128);
        let in_l = lcg_noise(1024, 7, 0.7);
        let in_r = lcg_noise(1024, 99, 0.6);
        let mut l1 = in_l.clone();
        let mut r1 = in_r.clone();
        let mut offset = 0;
        while offset < 1024 {
            let end = (offset + 128).min(1024);
            a.process(&mut l1[offset..end], &mut r1[offset..end]);
            offset = end;
        }
        a.reset();
        let mut l2 = in_l.clone();
        let mut r2 = in_r.clone();
        let mut offset = 0;
        while offset < 1024 {
            let end = (offset + 128).min(1024);
            a.process(&mut l2[offset..end], &mut r2[offset..end]);
            offset = end;
        }
        let mut b = FdnReverbStage::from_params(48000.0, p).unwrap();
        b.prepare(128);
        let mut l3 = in_l.clone();
        let mut r3 = in_r.clone();
        let mut offset = 0;
        while offset < 1024 {
            let end = (offset + 128).min(1024);
            b.process(&mut l3[offset..end], &mut r3[offset..end]);
            offset = end;
        }
        for i in 0..1024 {
            assert_eq!(l1[i].to_bits(), l3[i].to_bits(), "首轮 vs 全新实例 left[{i}]");
            assert_eq!(l2[i].to_bits(), l3[i].to_bits(), "reset 重放 left[{i}]");
            assert_eq!(r1[i].to_bits(), r3[i].to_bits(), "首轮 vs 全新实例 right[{i}]");
            assert_eq!(r2[i].to_bits(), r3[i].to_bits(), "reset 重放 right[{i}]");
        }
    }

    #[test]
    fn lines结构变化清空状态_非法lines报错() {
        // GWT-FDN-10/12：lines 切换 → reset 清状态（与全新实例逐位一致）；非法值报错。
        let mut p = base_params("hall");
        let mut a = FdnReverbStage::from_params(48000.0, p.clone()).unwrap();
        a.prepare(128);
        let in_l = lcg_noise(512, 5, 0.7);
        let in_r = lcg_noise(512, 6, 0.7);
        let mut al = in_l.clone();
        let mut ar = in_r.clone();
        let mut offset = 0;
        while offset < 512 {
            let end = (offset + 128).min(512);
            a.process(&mut al[offset..end], &mut ar[offset..end]);
            offset = end;
        }
        // 切换 lines=16 → 内部 reset；随后冲激响应应与全新 16 线实例逐位一致。
        p.lines = Some(16.0);
        a.set_params(p.clone()).unwrap();
        let mut al2 = vec![0.0_f32; 256];
        let mut ar2 = vec![0.0_f32; 256];
        al2[0] = 1.0;
        let mut offset = 0;
        while offset < 256 {
            let end = (offset + 128).min(256);
            a.process(&mut al2[offset..end], &mut ar2[offset..end]);
            offset = end;
        }
        let mut b = FdnReverbStage::from_params(48000.0, p).unwrap();
        b.prepare(128);
        let mut bl = vec![0.0_f32; 256];
        let mut br = vec![0.0_f32; 256];
        bl[0] = 1.0;
        b.process(&mut bl, &mut br);
        assert_eq!(al2, bl, "lines 切换后状态必须被清空（与全新实例一致）");
        assert_eq!(ar2, br);

        // 非法 lines：0/3/5/小数截断后非法 → 报错。
        for bad in [0.0_f64, 3.0, 5.0, 7.9] {
            let mut q = base_params("hall");
            q.lines = Some(bad);
            assert!(FdnReverbStage::from_params(48000.0, q).is_err(), "lines={bad} 必须报错");
        }
        // 缺省（None）→ 8 线。
        let mut q = base_params("hall");
        q.lines = None;
        let s = FdnReverbStage::from_params(48000.0, q).unwrap();
        assert_eq!(s.left.n, 8);
        // 小数截断后合法：2.9 → 2。
        let mut q = base_params("hall");
        q.lines = Some(2.9);
        let s = FdnReverbStage::from_params(48000.0, q).unwrap();
        assert_eq!(s.left.n, 2);
    }

    #[test]
    fn 静音输入零输出() {
        // GWT-FDN-11：零状态出发的反馈回路恒保持零。
        let mut p = base_params("spring");
        p.wet = 1.0;
        p.dry = 0.0;
        let mut s = FdnReverbStage::from_params(48000.0, p).unwrap();
        s.prepare(256);
        let mut l = vec![0.0_f32; 9600];
        let mut r = vec![0.0_f32; 9600];
        let mut offset = 0;
        while offset < 9600 {
            let end = (offset + 256).min(9600);
            s.process(&mut l[offset..end], &mut r[offset..end]);
            offset = end;
        }
        assert!(l.iter().all(|&x| x.to_bits() == 0_u32));
        assert!(r.iter().all(|&x| x.to_bits() == 0_u32));
    }

    #[test]
    fn 满幅激励极值钳制下有界不发散() {
        // GWT-FDN-03/07：case4 同款极值形态，突发 + 静音衰减覆盖环路稳定性。
        let mut p = base_params("stage");
        p.room_size = 1.5;
        p.damping = -0.3;
        p.wet = 4.5;
        p.dry = -0.5;
        p.pre_delay_ms = 80.0;
        p.width = 2.5;
        p.lines = Some(16.0);
        let mut s = FdnReverbStage::from_params(48000.0, p).unwrap();
        s.prepare(700);
        // 窗口覆盖足够多轮环路往返（g=0.8 → 能量每轮 ×0.64，需 >11 轮才能衰减
        // 两个数量级；线长 ~1300 帧 → 取 0.8s 窗口、突发占前半）。
        let n = 38_400;
        let burst = lcg_noise(n, 61_004, 0.9);
        let mut l = burst.clone();
        let mut r = lcg_noise(n, 61_005, 0.9);
        for x in l.iter_mut().take(n).skip(n / 2) {
            *x = 0.0;
        }
        for x in r.iter_mut().take(n).skip(n / 2) {
            *x = 0.0;
        }
        let mut offset = 0;
        while offset < n {
            let end = (offset + 700).min(n);
            s.process(&mut l[offset..end], &mut r[offset..end]);
            offset = end;
        }
        let mut peak = 0.0_f64;
        for i in 0..n {
            assert!(l[i].is_finite() && r[i].is_finite(), "输出必须有限 @{i}");
            peak = peak.max(f64::from(l[i].abs()).max(f64::from(r[i].abs())));
        }
        assert!(peak < 100.0, "输出应有界（g²<1 无条件稳定），实际峰值 {peak}");
        // 衰减：远端尾部能量低于突发段两个数量级以上。
        let energy = |a: usize, b: usize| -> f64 {
            l[a..b].iter().map(|&x| f64::from(x) * f64::from(x)).sum::<f64>()
                + r[a..b].iter().map(|&x| f64::from(x) * f64::from(x)).sum::<f64>()
        };
        let excite = energy(0, n / 2);
        let far_tail = energy(n - 1000, n);
        assert!(
            far_tail < excite * 1e-2,
            "尾音应显著衰减: tail={far_tail} vs 激励 {excite}"
        );
    }

    #[test]
    fn 非法采样率报错_对齐ts错误信息() {
        let p = base_params("hall");
        for bad in [0.0_f64, -44100.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = FdnReverbStage::from_params(bad, p.clone()).err().expect("非法采样率必须报错");
            assert!(err.contains("invalid sample rate"), "错误信息应与 TS 一致：{err}");
        }
    }
}
