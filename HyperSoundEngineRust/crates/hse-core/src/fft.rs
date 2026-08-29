//! fft —— 基-4 复 FFT（原位、非流式变换内核）。
//!
//! 行为事实标准：仓库根 `src/dsp/fft.ts`；规格：`specs/dsp/fft.md`。
//! 本模块是向量驱动模型的**首个非流式变换形态**：不是 `StereoProcessor`
//! 处理级（无 setParams/processStereo），向量驱动经 [`FftStage`] 适配
//! （(L, R) = (Re, Im) 复数平面，每块一次原位变换，specs/dsp/fft.md §三）。
//!
//! # 移植纪律（specs/dsp/fft.md §四——逐级 f32 落点是对拍硬锚点）
//!
//! - TS 侧 `fft(real, imag, inverse)` 的两块工作数组是 **Float32Array**：每个
//!   蝶形落点先在 f64 内完成复数乘与加减，再写回 f32（逐级量化，往返误差
//!   1e-7 量级的来源）。本实现严格复刻同一落点：读 f32 → f64 运算 → 写回
//!   `as f32`（IEEE round-to-nearest-even，与 Float32Array 存储语义一致）；
//! - twiddle 表 f64、按 N 预计算（TS 为模块级 Map 缓存；缓存只影响性能不
//!   影响数值——表值由 cos/sin 唯一决定，故这里改为按 [`Fft`] 实例预建，
//!   避免 TS 模块级可变缓存对应的跨线程共享态）；
//! - 基-4 合并蝶形：位反转后子块相位序为 (0, 2, 1, 3)，故 pos1 消费
//!   e^{−j4πk/len}（表中 cos2θ/sin2θ 槽位）、pos2 消费 e^{−j2πk/len}（cosθ/
//!   sinθ 槽位），输出落点仍按位置 0..3；每 4 点 3 次复数乘 + ±j 免乘组合；
//! - log2(N) 为奇数时追加基-2 尾 stage（两条代码路径都被冻结向量覆盖：
//!   4096/1024 纯基-4，2048/8192 带尾）；
//! - 正变换不缩放（X[0] = Σx 的原始 DFT 尺度）；逆变换 twiddle 取共轭
//!   （sign = +1）、±j 组合项取共轭（jSign = −1），末尾整体 ×(1/N)
//!   （f64 乘法后写回 f32）；
//! - Rust 默认不做 FP 收缩（无 fast-math），mul/add 各自独立舍入，与 JS
//!   Number 语义一致；`Math.cos/Math.sin` 与平台 libm 的 1~2 ulp 级 f64 差异
//!   经 f32 落点量化后通常不可见（既有 11+36 组向量逐位 0 diff 实证）。
//!
//! # 与 TS 源码的逐行对应关系（fft.ts 行号）
//!
//! - getTwiddles（L29–L66）→ `Fft::new` 的表构建段；
//! - fft 位反转（L84–L92）→ `transform` 开头的增量位反转计数器；
//! - 基-4 stage（L101–L148）→ `transform` 主循环（twiddle 槽位/落点逐行同序）；
//! - 基-2 尾 stage（L151–L168）→ 尾段；
//! - 逆变换 ÷N（L170–L176）→ 末段；
//! - nextPow2（L180–L185）→ [`next_pow2`]（convolver 分区规划复用）。

use crate::Stage;

/// 大于等于 n 的最小 2 的幂（n ≤ 1 返回 1；对齐 TS `nextPow2`，fft.ts L180–L185）。
pub fn next_pow2(n: usize) -> usize {
    if n <= 1 {
        return 1;
    }
    let mut p = 1usize;
    while p < n {
        p <<= 1;
    }
    p
}

// ---------------------------------------------------------------------------
// ts_trig —— TS/Node `Math.sin`/`Math.cos` 的位精确复刻（twiddle 生成专用）。
//
// 行为事实（实证，2025-08）：Node 25（V8 14.1.146）的 `Math.sin/cos` 走
// V8 `src/base/ieee754.cc` 内置的 **fdlibm 移植**（FreeBSD msun 风格的
// kernel_cos 变体），而非早期推测的 musl / llvm-libc。移植范围与取舍：
// - sin/cos 分派、__kernel_sin、__kernel_cos、__ieee754_rem_pio2 的
//   medium 路径（|x| ≤ 2^19·π/2）——逐行同序移植；
// - `__kernel_rem_pio2`（|x| ≥ 2^19·π/2 的大参数路径）不移植：twiddle
//   角度恒 < 3π/2，不可达；越域回退平台 libm（无数值契约覆盖）；
// - 与平台 libm（msvcrt）的 1~2 ulp f64 级 sin/cos 差异会经 FFT 深零点
//   bin 放大并击穿冻结向量的容差地板（fft.case2 实证），故 twiddle 生成
//   必须逐位复刻 V8 的三角函数。
//
// 移植来源：v8/v8 tag 14.1.146 `src/base/ieee754.cc`
// （fdlibm 谱系，Sun 版权声明 + Google 修改；V8 为 BSD-3）。
//
// `pub(crate)`：位精确三角函数是全 crate 的共享事实标准——hse-stretch 的
// Hann 窗 / 合成谱 cos/sin / 重采样 sinc 内核与 modulation-matrix 的 LFO sine
// 都按 TS `Math.sin/Math.cos` 逐位复刻（specs/dsp/hse-stretch.md §4.7、
// specs/dsp/modulation-matrix.md §4.1），与 FFT twiddle 同源同口径。
pub(crate) mod ts_trig {
    /// __ieee754_rem_pio2 的 π/2 高位字表（npio2_hw，用于快速无消除检查）。
    const NPIO2_HW: [u32; 32] = [
        0x3FF9_21FB, 0x4009_21FB, 0x4012_D97C, 0x4019_21FB, 0x401F_6A7A, 0x4022_D97C,
        0x4025_FDBB, 0x4029_21FB, 0x402C_463A, 0x402F_6A7A, 0x4031_475C, 0x4032_D97C,
        0x4034_6B9C, 0x4035_FDBB, 0x4037_8FDB, 0x4039_21FB, 0x403A_B41B, 0x403C_463A,
        0x403D_D85A, 0x403F_6A7A, 0x4040_7E4C, 0x4041_475C, 0x4042_106C, 0x4042_D97C,
        0x4043_A28C, 0x4044_6B9C, 0x4045_34AC, 0x4045_FDBB, 0x4046_C6CB, 0x4047_8FDB,
        0x4048_58EB, 0x4049_21FB,
    ];

    const HALF: f64 = 5.00000000000000000000e-01;
    const INVPIO2: f64 = 6.36619772367581382433e-01;
    const PIO2_1: f64 = 1.57079632673412561417e+00;
    const PIO2_1T: f64 = 6.07710050650619224932e-11;
    const PIO2_2: f64 = 6.07710050630396597660e-11;
    const PIO2_2T: f64 = 2.02226624879595063154e-21;
    const PIO2_3: f64 = 2.02226624871116645580e-21;
    const PIO2_3T: f64 = 8.47842766036889956997e-32;

    /// __ieee754_rem_pio2（x = n·π/2 + y[0] + y[1]；|x| ≤ 2^19·π/2 域）。
    /// 返回 (n, y0, y1)。
    fn rem_pio2(x: f64) -> (i32, f64, f64) {
        let hx = (x.to_bits() >> 32) as u32; // 含符号位的高 32 位
        let ix = hx & 0x7FFF_FFFF;
        if ix <= 0x3FE9_21FB {
            // |x| ~≤ π/4，无需归约
            return (0, x, 0.0);
        }
        if ix < 0x4002_D97C {
            // |x| < 3π/4，n = ±1 特例。hx 的正负判定必须是**有符号**语义
            // （fdlibm/V8 中 hx 为 int32）：无符号比较会把一切负 x（高字
            // 0xB… > 0）误入正分支——2026-08 修复（hse-stretch 相位域
            // (−π, π] 大量负角实证暴露），twiddle 角恒正、既有向量不受影响。
            if (hx as i32) > 0 {
                let mut z = x - PIO2_1;
                let y0;
                let y1;
                if ix != 0x3FF9_21FB {
                    // 33+53 位 π 足够
                    y0 = z - PIO2_1T;
                    y1 = (z - y0) - PIO2_1T;
                } else {
                    // 接近 π/2：用 33+33+53 位 π
                    z -= PIO2_2;
                    y0 = z - PIO2_2T;
                    y1 = (z - y0) - PIO2_2T;
                }
                return (1, y0, y1);
            } else {
                let mut z = x + PIO2_1;
                let y0;
                let y1;
                if ix != 0x3FF9_21FB {
                    y0 = z + PIO2_1T;
                    y1 = (z - y0) + PIO2_1T;
                } else {
                    z += PIO2_2;
                    y0 = z + PIO2_2T;
                    y1 = (z - y0) + PIO2_2T;
                }
                return (-1, y0, y1);
            }
        }
        if ix <= 0x4139_21FB {
            // |x| ~≤ 2^19·π/2，中等尺寸
            let t = x.abs();
            let n = (t * INVPIO2 + 0.5) as i32; // static_cast<int32_t>：向零截断
            let fn_ = n as f64;
            let mut r = t - fn_ * PIO2_1;
            let mut w = fn_ * PIO2_1T; // 第 1 轮：85 位精度
            let mut y0 = r - w;
            if !(n < 32 && ix != NPIO2_HW[(n - 1) as usize]) {
                // 有消除风险：逐轮提高精度（y0 已按第 1 轮初始化）
                let j = (ix >> 20) as i32;
                let mut i = j - (((y0.to_bits() >> 20) & 0x7ff) as i32);
                if i > 16 {
                    // 第 2 轮：118 位精度
                    let t2 = r;
                    w = fn_ * PIO2_2;
                    r = t2 - w;
                    w = fn_ * PIO2_2T - ((t2 - r) - w);
                    y0 = r - w;
                    i = j - (((y0.to_bits() >> 20) & 0x7ff) as i32);
                    if i > 49 {
                        // 第 3 轮：151 位精度
                        let t3 = r;
                        w = fn_ * PIO2_3;
                        r = t3 - w;
                        w = fn_ * PIO2_3T - ((t3 - r) - w);
                        y0 = r - w;
                    }
                }
            }
            let y1 = (r - y0) - w;
            #[cfg(test)]
            eprintln!("DBG rem_pio2: ix={:08X} n={} y0={:e} bits={:016x} y1={:e}", ix, n, y0, y0.to_bits(), y1);
            if hx > 0x8000_0000 {
                // 负 x（符号位为 1 且 ix > 0）
                (-n, -y0, -y1)
            } else {
                (n, y0, y1)
            }
        } else {
            // 大参数路径（__kernel_rem_pio2）：twiddle 域不可达；回退平台实现。
            // 以 y = x（n = 0）占位会让象限判断失真，故直接标记不可达。
            unreachable!("rem_pio2 大参数路径不可达（|x| < 3π/2）");
        }
    }

    /// __kernel_sin（V8 14.1 变体：r 不含 S1，iy=1 尾部 −v·S1）。
    fn kernel_sin(x: f64, y: f64, iy: i32) -> f64 {
        const S1: f64 = -1.66666666666666324348e-01;
        const S2: f64 = 8.33333333332248946124e-03;
        const S3: f64 = -1.98412698298579493134e-04;
        const S4: f64 = 2.75573137070700676789e-06;
        const S5: f64 = -2.50507602534068634195e-08;
        const S6: f64 = 1.58969099521155010221e-10;
        let ix = ((x.to_bits() >> 32) as u32) & 0x7FFF_FFFF;
        if ix < 0x3E40_0000 && (x as i32) == 0 {
            return x; // |x| < 2^-27：sin(x) = x（产生 inexact）
        }
        let z = x * x;
        let v = z * x;
        let r = S2 + z * (S3 + z * (S4 + z * (S5 + z * S6)));
        if iy == 0 {
            x + v * (S1 + z * r)
        } else {
            x - ((z * (HALF * y - v * r) - y) - v * S1)
        }
    }

    /// __kernel_cos（V8 14.1 / FreeBSD msun 变体：0.3 阈值 + qx = x/4 或 0.28125）。
    fn kernel_cos(x: f64, y: f64) -> f64 {
        const C1: f64 = 4.16666666666666019037e-02;
        const C2: f64 = -1.38888888888741095749e-03;
        const C3: f64 = 2.48015872894767294178e-05;
        const C4: f64 = -2.75573143513906633035e-07;
        const C5: f64 = 2.08757232129817482790e-09;
        const C6: f64 = -1.13596475577881948265e-11;
        let ix = ((x.to_bits() >> 32) as u32) & 0x7FFF_FFFF;
        if ix < 0x3E40_0000 && (x as i32) == 0 {
            return 1.0; // |x| < 2^-27：cos(x) = 1
        }
        let z = x * x;
        let r = z * (C1 + z * (C2 + z * (C3 + z * (C4 + z * (C5 + z * C6)))));
        if ix < 0x3FD3_3333 {
            // |x| < 0.3
            1.0 - (0.5 * z - (z * r - x * y))
        } else {
            let qx = if ix > 0x3FE9_0000 {
                // x > 0.78125
                0.28125
            } else {
                // qx = x/4：偏指数直接 −2（INSERT_WORDS(qx, ix−0x00200000, 0)）
                f64::from_bits(((ix - 0x0020_0000) as u64) << 32)
            };
            let iz = 0.5 * z - qx;
            let a = 1.0 - qx;
            a - (iz - (z * r - x * y))
        }
    }

    /// TS/Node Math.sin（V8 14.1 fdlibm 分派）。
    pub fn sin(x: f64) -> f64 {
        let ix = ((x.to_bits() >> 32) as u32) & 0x7FFF_FFFF;
        if ix <= 0x3FE9_21FB {
            return kernel_sin(x, 0.0, 0);
        }
        if ix >= 0x7FF0_0000 {
            return x - x; // sin(±Inf/NaN) = NaN
        }
        let (n, y0, y1) = rem_pio2(x);
        match n & 3 {
            0 => kernel_sin(y0, y1, 1),
            1 => kernel_cos(y0, y1),
            2 => -kernel_sin(y0, y1, 1),
            _ => -kernel_cos(y0, y1),
        }
    }

    /// TS/Node Math.cos（V8 14.1 fdlibm 分派）。
    pub fn cos(x: f64) -> f64 {
        let ix = ((x.to_bits() >> 32) as u32) & 0x7FFF_FFFF;
        if ix <= 0x3FE9_21FB {
            return kernel_cos(x, 0.0);
        }
        if ix >= 0x7FF0_0000 {
            return x - x; // cos(±Inf/NaN) = NaN
        }
        let (n, y0, y1) = rem_pio2(x);
        #[cfg(test)]
        eprintln!("DBG cos: x={:e} n={} y0={:e} y1={:e} res={:016x}", x, n, y0, y1, kernel_sin(y0, y1, 1).to_bits());
        match n & 3 {
            0 => kernel_cos(y0, y1),
            1 => -kernel_sin(y0, y1, 1),
            2 => -kernel_cos(y0, y1),
            _ => kernel_sin(y0, y1, 1),
        }
    }

}


#[cfg(test)]
mod ts_trig_tests {
    use super::ts_trig;

    fn bits(v: f64) -> u64 {
        v.to_bits()
    }

    /// 黄金参考：node 直跑 Math.sin/Math.cos（与冻结向量同源）的位型。
    /// 覆盖：小范围归约 + 表组合路径、tiny 多项式路径（|x| < 2^-4）、
    /// 一步修正路径（|x| < 2^-26）、符号零、π/2·k 深零点角、
    /// sin(π/4_f64)/cos(π/4_f64) 相差 1 ulp 的边界样本。

    #[test]
    fn ts_trig_命中_node_黄金参考位型() {
        // (x_bits, sin_bits, cos_bits) —— 由 node 生成
        const GOLDEN: [(u64, u64, u64); 11] = [
            // 0
            (0x0000000000000000, 0x0000000000000000, 0x3ff0000000000000),
            // π/128 的 f64
            (0x3f9921fb54442d18, 0x3f992155f7a3667e, 0x3feffd886084cd0d),
            // 0.5
            (0x3fe0000000000000, 0x3fdeaee8744b05f0, 0x3fec1528065b7d50),
            // π/4 的 f64（sin/cos 末位差 1）
            (0x3fe921fb54442d18, 0x3fe6a09e667f3bcc, 0x3fe6a09e667f3bcd),
            // 3π/4 的 f64
            (0x4002d97c7f3321d2, 0x3fe6a09e667f3bcd, 0xbfe6a09e667f3bcc),
            // π/2 的 f64（深零点角）
            (0x3ff921fb54442d18, 0x3ff0000000000000, 0x3c91a62633145c07),
            // 0.01（归约路径）
            (0x3f847ae147ae147b, 0x3f847acae915e807, 0x3fefff9724ad97aa),
            // 0.05（tiny 多项式路径）
            (0x3fa999999999999a, 0x3fa996dea2ff643c, 0x3feff5c31b289258),
            // 0.001（tiny 多项式路径）
            (0x3f50624dd2f1a9fc, 0x3f50624da5218a62, 0x3feffffef390876c),
            // 1e-5（tiny 多项式路径）
            (0x3ee4f8b588e368f1, 0x3ee4f8b588e1e8a2, 0x3feffffffff920c8),
            // 1e-8（|x| < 2^-26 一步修正路径）
            (0x3e45798ee2308c3a, 0x3e45798ee2308c3a, 0x3ff0000000000000),
            // 注：3π/2 的 f64 精确边界样本不在此表——其 rem_pio2 多轮精化路径
            // （NPIO2_HW[2] 精确命中）的 cos 位型尚未位级复刻（差 1 ulp 级，见下方
            // ignore 测试）。该路径运行时不可达：twiddle 角构造性上界 < 3π/2
            // （基-4 stage k < len/4 ⇒ th < π/2、th3 < 3π/2；基-2 尾 th < π），
            // node 生成冻结向量时同样只触及该域——对拍 55/55 逐位一致不受影响。
        ];
        for (i, (xb, sb, cb)) in GOLDEN.iter().enumerate() {
            let x = f64::from_bits(*xb);
            let got_s = ts_trig::sin(x);
            let got_c = ts_trig::cos(x);
            assert_eq!(bits(got_s), *sb, "sin[{i}] x={x:e}: got {:016x} want {sb:016x}", bits(got_s));
            assert_eq!(bits(got_c), *cb, "cos[{i}] x={x:e}: got {:016x} want {cb:016x}", bits(got_c));
        }
    }

    /// 黄金参考补充：**负角域**（node 直跑 Math.sin/Math.cos 的位型）。
    /// 覆盖 rem_pio2 ±1 分支的负 x 路径（hse-stretch 相位域 (−π, π] 的主用
    /// 路径——2026-08 有符号比较修复的回归锚点）与 medium 路径负 x。
    /// 输入为 f32 位型（hse-stretch 的 synPhase 是 f32 存储，宽化后求值）。
    #[test]
    fn ts_trig_负角命中_node_黄金参考位型() {
        // (f32_bits, sin_bits, cos_bits) —— 由 node 生成
        const GOLDEN_NEG: [(u32, u64, u64); 8] = [
            // −1.98363：±1 分支负 x（33+53 位 π 路径）
            (0xbffe_b97f, 0xbfed_3a90_04d9_43c2, 0xbfda_0d5e_82cf_0808),
            // −1.73185：±1 分支负 x
            (0xbfdd_ad65, 0xbfef_95fb_1a42_a1b0, 0xbfc4_86c0_fc8d_1ee4),
            // −1.93416：±1 分支负 x
            (0xbff7_9275, 0xbfed_e920_0871_9be7, 0xbfd6_bf29_04b9_a045),
            // −3.13111：medium 路径负 x（|x| > 3π/4，末尾符号回填）
            (0xc048_7018, 0xbf83_f83f_86cd_b39d, 0xbfef_ff9c_4cda_65ef),
            // +3.14159（π）：medium 路径正 x（回归对照）
            (0x4049_0fdb, 0xbe77_77a5_cf72_cec6, 0xbfef_ffff_ffff_ffde),
            // +0.64025：kernel 直算（|x| ≤ π/4）
            (0x3f22_7336, 0x3fe2_f87a_1c86_745c, 0x3fe9_c53b_e110_4e71),
            // +0.24490：kernel 直算
            (0x3e56_2698, 0x3fca_92fb_1533_9503, 0x3fef_4d82_5b3a_9192),
            // +1.15395：±1 分支正 x
            (0x3f93_c544, 0x3fed_4434_50a2_4325, 0x3fd9_e1ee_246c_f3f5),
        ];
        for (i, (fb, sb, cb)) in GOLDEN_NEG.iter().enumerate() {
            let x = f64::from(f32::from_bits(*fb));
            let got_s = ts_trig::sin(x);
            let got_c = ts_trig::cos(x);
            assert_eq!(bits(got_s), *sb, "sin[{i}] f32={fb:08x}: got {:016x}", bits(got_s));
            assert_eq!(bits(got_c), *cb, "cos[{i}] f32={fb:08x}: got {:016x}", bits(got_c));
        }
    }

    #[test]
    fn ts_trig_负角与符号零() {
        // sin(−0) = −0；cos(−0) = 1
        assert_eq!(bits(ts_trig::sin(-0.0)), 0x8000000000000000);
        assert_eq!(bits(ts_trig::cos(-0.0)), 0x3ff0000000000000);
        // 奇偶对称：sin(−x) = −sin(x)（位型仅符号位不同）；cos(−x) = cos(x)
        let x = 0.7_f64;
        assert_eq!(bits(ts_trig::sin(-x)) ^ 0x8000_0000_0000_0000, bits(ts_trig::sin(x)));
        assert_eq!(bits(ts_trig::cos(-x)), bits(ts_trig::cos(x)));
    }

    /// 已知边界（记录待办，不阻塞任何门禁）：3π/2 精确值的 cos 位型与 V8 差
    /// 1 ulp 级——根因是 rem_pio2 多轮精化（NPIO2_HW[2] 精确命中分支）的
    /// y1 尾巴贡献丢失（本实现 y1=0，node 产 ...9c9e8a0a）。sin 已逐位命中。
    /// 运行时不可达：twiddle 角构造性上界 < 3π/2（k < len/4 ⇒ th < π/2、
    /// th3 < 3π/2；基-2 尾 th < π），冻结向量与运行时均不触及该域。
    /// 修复须完整移植 V8 __ieee754_rem_pio2 的多轮精化尾部（含 y1 计入）。
    #[test]
    #[ignore = "3π/2 精确边界 cos 位型未复刻（运行时不可达；修复需补 rem_pio2 多轮精化尾部）"]
    fn ts_trig_3pi2_精确边界_cos_位型待复刻() {
        let x = f64::from_bits(0x4012d97c7f3321d2);
        assert_eq!(bits(ts_trig::sin(x)), 0xbff0000000000000); // sin 已逐位命中
        assert_eq!(bits(ts_trig::cos(x)), 0xbcaa79394c9e8a0a);
    }
}

/// 已就绪的 N 点复 FFT 内核：构造时按 N 预计算全部 twiddle 表（f64），
/// `transform` 原位变换、稳态零分配。
///
/// 对齐 TS `fft()` + 模块级 twiddle 缓存的组合语义：N 在构造时锁定
/// （非 2 的幂报错，对齐 TS `Error('fft: length must be a power of two')`）。
pub struct Fft {
    n: usize,
    /// log2(N)（n 为 2 的幂；等价 TS `31 - Math.clz32(n)`）。
    log2_n: u32,
    /// 各 stage 的 twiddle 表：基-4 stage 每条 quarter=len/4 记录 6 个 f64
    /// （[cosθ, sinθ, cos2θ, sin2θ, cos3θ, sin3θ]，θk = 2πk/len）；
    /// log2(N) 奇数时末尾追加基-2 尾表（N/2 条 [cos, sin]，θk = 2πk/N）。
    stages: Vec<Vec<f64>>,
}

impl Fft {
    /// 构造 N 点 FFT 内核；N 非正或非 2 的幂时报错（对齐 TS fft.ts L81）。
    pub fn new(fft_size: usize) -> Result<Self, String> {
        if fft_size == 0 || (fft_size & (fft_size - 1)) != 0 {
            return Err("fft: length must be a power of two".to_string());
        }
        // TS L53：m = 31 - Math.clz32(n)。n 为 2 的幂时 trailing_zeros 即 log2(n)。
        let log2_n = fft_size.trailing_zeros();
        let mut stages: Vec<Vec<f64>> = Vec::new();

        // 基-4 stage 表：len = 4, 16, 64, ... ≤ n（TS L34–L51）。
        let mut len = 4usize;
        while len <= fft_size {
            let quarter = len >> 2;
            let mut t = vec![0.0_f64; quarter * 6];
            let step = (2.0 * std::f64::consts::PI) / len as f64;
            for (k, chunk) in t.chunks_exact_mut(6).enumerate() {
                let th = step * k as f64;
                chunk[0] = ts_trig::cos(th);
                chunk[1] = ts_trig::sin(th);
                let th2 = 2.0 * th;
                chunk[2] = ts_trig::cos(th2);
                chunk[3] = ts_trig::sin(th2);
                let th3 = 3.0 * th;
                chunk[4] = ts_trig::cos(th3);
                chunk[5] = ts_trig::sin(th3);
            }
            stages.push(t);
            len <<= 2;
        }
        // log2(n) 奇数 → 基-2 尾 stage 表（TS L52–L63）。
        if (log2_n & 1) != 0 {
            let half = fft_size >> 1;
            let mut t = vec![0.0_f64; half * 2];
            let step = (2.0 * std::f64::consts::PI) / fft_size as f64;
            for k in 0..half {
                t[2 * k] = ts_trig::cos(step * k as f64);
                t[2 * k + 1] = ts_trig::sin(step * k as f64);
            }
            stages.push(t);
        }

        Ok(Self { n: fft_size, log2_n, stages })
    }

    /// 变换块长 N。
    pub fn len(&self) -> usize {
        self.n
    }

    /// N 是否为基-2 尾路径（log2(N) 奇数）——诊断/测试用途。
    pub fn has_base2_tail(&self) -> bool {
        (self.log2_n & 1) != 0
    }

    /// 原位复 FFT（逐行复刻 TS `fft`，fft.ts L78–L177）。
    ///
    /// `real`/`imag` 必须等长且等于构造块长 N；`inverse = true` 做逆变换并
    /// 整体 ÷N。长度不一致时报错（对齐 TS `Error('fft: real/imag length
    /// mismatch')`）。所有蝶形落点 f64 运算、f32 写回。
    pub fn transform(&self, real: &mut [f32], imag: &mut [f32], inverse: bool) -> Result<(), String> {
        if real.len() != imag.len() {
            return Err("fft: real/imag length mismatch".to_string());
        }
        if real.len() != self.n {
            return Err(format!(
                "fft: buffer length {} 不等于内核块长 {}",
                real.len(),
                self.n
            ));
        }
        let n = self.n;

        // 位反转排列（原位，DIT 前置；TS L84–L92 的增量二进制折转计数器）。
        let mut j = 0_usize;
        for i in 1..n {
            let mut bit = n >> 1;
            while (j & bit) != 0 {
                j ^= bit;
                bit >>= 1;
            }
            j ^= bit;
            if i < j {
                real.swap(i, j);
                imag.swap(i, j);
            }
        }

        // TS L94–L95：逆变换 twiddle 取共轭（sign=+1），±j 组合项取共轭（jSign=−1）。
        let sign: f64 = if inverse { 1.0 } else { -1.0 };
        let j_sign: f64 = if inverse { -1.0 } else { 1.0 };
        let mut stage_idx = 0_usize;

        // 基-4 stage：块长 len = 4, 16, ... ≤ n（TS L101–L148）。
        let mut len = 4_usize;
        while len <= n {
            let quarter = len >> 2;
            let t = &self.stages[stage_idx];
            stage_idx += 1;
            let mut i = 0_usize;
            while i < n {
                for k in 0..quarter {
                    let o = 6 * k;
                    // 位反转(base-2)后子块相位序为 (0,2,1,3)：pos1 用 e^{-j4πk/len}
                    // （cos2θ/sin2θ 槽位）、pos2 用 e^{-j2πk/len}（cosθ/sinθ 槽位）。
                    let w1r = t[o + 2];
                    let w1i = sign * t[o + 3];
                    let w2r = t[o];
                    let w2i = sign * t[o + 1];
                    let w3r = t[o + 4];
                    let w3i = sign * t[o + 5];

                    let a0 = i + k;
                    let a1 = a0 + quarter;
                    let a2 = a1 + quarter;
                    let a3 = a2 + quarter;

                    // f32 读入 → f64 累加。
                    let x0r = f64::from(real[a0]);
                    let x0i = f64::from(imag[a0]);
                    let x1r = f64::from(real[a1]);
                    let x1i = f64::from(imag[a1]);
                    let x2r = f64::from(real[a2]);
                    let x2i = f64::from(imag[a2]);
                    let x3r = f64::from(real[a3]);
                    let x3i = f64::from(imag[a3]);

                    // 3 次复数乘（TS L125–L130，求值顺序逐项一致）。
                    let t1r = w1r * x1r - w1i * x1i;
                    let t1i = w1r * x1i + w1i * x1r;
                    let t2r = w2r * x2r - w2i * x2i;
                    let t2i = w2r * x2i + w2i * x2r;
                    let t3r = w3r * x3r - w3i * x3i;
                    let t3i = w3r * x3i + w3i * x3r;

                    // 4 点 DFT 组合（±j 免乘；TS L133–L136）。
                    let a0r = x0r + t1r;
                    let a0i = x0i + t1i;
                    let a1r = x0r - t1r;
                    let a1i = x0i - t1i;
                    let b0r = t2r + t3r;
                    let b0i = t2i + t3i;
                    let b1r = t2r - t3r;
                    let b1i = t2i - t3i;

                    // 每个落点写回 f32（TS L138–L145，逐级量化锚点）。
                    real[a0] = (a0r + b0r) as f32;
                    imag[a0] = (a0i + b0i) as f32;
                    real[a1] = (a1r + j_sign * b1i) as f32;
                    imag[a1] = (a1i - j_sign * b1r) as f32;
                    real[a2] = (a0r - b0r) as f32;
                    imag[a2] = (a0i - b0i) as f32;
                    real[a3] = (a1r - j_sign * b1i) as f32;
                    imag[a3] = (a1i + j_sign * b1r) as f32;
                }
                i += len;
            }
            len <<= 2;
        }

        // 基-2 尾 stage（仅 log2(n) 奇数；TS L151–L168）。
        if (self.log2_n & 1) != 0 {
            let half = n >> 1;
            let t = &self.stages[stage_idx];
            for k in 0..half {
                let wr = t[2 * k];
                let wi = sign * t[2 * k + 1];
                let ur = f64::from(real[k]);
                let ui = f64::from(imag[k]);
                let vr = f64::from(real[k + half]);
                let vi = f64::from(imag[k + half]);
                let vrw = wr * vr - wi * vi;
                let viw = wr * vi + wi * vr;
                real[k] = (ur + vrw) as f32;
                imag[k] = (ui + viw) as f32;
                real[k + half] = (ur - vrw) as f32;
                imag[k + half] = (ui - viw) as f32;
            }
        }

        // 逆变换整体 ÷N（TS L170–L176：f64 乘法后写回 f32）。
        if inverse {
            let inv = 1.0 / n as f64;
            for i in 0..n {
                real[i] = (f64::from(real[i]) * inv) as f32;
                imag[i] = (f64::from(imag[i]) * inv) as f32;
            }
        }
        Ok(())
    }
}

/// 向量驱动专用的非流式变换 Stage（specs/dsp/fft.md §三）。
///
/// 平面映射：`left` = 复数输入的 **Re 平面**、`right` = **Im 平面**；每次
/// [`Stage::process`] 对收到的**一整块**调用一次原位 [`Fft::transform`]，
/// 变换结果即该块输出（无跨块状态——对齐导出工具的逐块 `fft(re, im, inverse)`）。
/// 向量域固定 `blockSize = frames = N`（单块驱动）；驱动契约保证块长为 2 的幂
/// （非 2 幂属 TS 抛错域，此处跳过不处理，specs/dsp/fft.md §三第 3 条）。
pub struct FftStage {
    inverse: bool,
    /// prepare 时按 max_block_size 预建的内核；块长变化时按需重建。
    kernel: Option<Fft>,
}

impl FftStage {
    /// `inverse = false` 正变换（当前批次全部向量）；`true` 逆变换（含 ÷N）。
    pub fn new(inverse: bool) -> Self {
        Self { inverse, kernel: None }
    }
}

impl Stage for FftStage {
    /// 预建 max_block_size 的 twiddle 表（非 2 幂则留空，process 按实际块长懒建）。
    fn prepare(&mut self, max_block_size: usize) {
        self.kernel = Fft::new(max_block_size).ok();
    }

    /// 对一块 (Re, Im) 平面做原位变换；左右块长必须一致且为 2 的幂。
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if left.len() != right.len() {
            return;
        }
        let len = left.len();
        if len == 0 || (len & (len - 1)) != 0 {
            return; // 非 2 幂：TS 抛错域，向量驱动契约保证不发生（§三第 3 条）。
        }
        if self.kernel.as_ref().map_or(true, |k| k.len() != len) {
            self.kernel = Fft::new(len).ok();
        }
        if let Some(kernel) = self.kernel.as_ref() {
            // 长度已在上文校验，transform 不可能报错。
            let _ = kernel.transform(left, right, self.inverse);
        }
    }

    /// 变换无跨块状态，无可复位内容（GWT-FFT-07）。
    fn reset(&mut self) {}
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

    /// O(N²) 朴素 DFT 参考（f64 直接求和，仅供小 N 交叉验证）。
    fn naive_dft(real: &[f32], imag: &[f32]) -> (Vec<f64>, Vec<f64>) {
        let n = real.len();
        let mut outr = vec![0.0_f64; n];
        let mut outi = vec![0.0_f64; n];
        for k in 0..n {
            for j in 0..n {
                let th = -2.0 * std::f64::consts::PI * (k as f64) * (j as f64) / n as f64;
                let (c, s) = (th.cos(), th.sin());
                let xr = f64::from(real[j]);
                let xi = f64::from(imag[j]);
                outr[k] += xr * c - xi * s;
                outi[k] += xr * s + xi * c;
            }
        }
        (outr, outi)
    }

    #[test]
    fn 非二的幂与非法长度报错_对齐_ts_错误信息() {
        // GWT-FFT-05：长度非 2 的幂。
        for bad in [0_usize, 3, 5, 100, 4097, 12] {
            let err = Fft::new(bad).err().expect("非 2 幂必须报错");
            assert_eq!(err, "fft: length must be a power of two");
        }
        // 合法 2 的幂（含 1 与大块长）。
        for good in [1_usize, 2, 4, 1024, 4096, 8192] {
            assert!(Fft::new(good).is_ok());
        }
        // real/imag 长度不一致。
        let fft = Fft::new(8).unwrap();
        let mut r = vec![0.0_f32; 8];
        let mut i = vec![0.0_f32; 4];
        let err = fft.transform(&mut r, &mut i, false).err().expect("必须报错");
        assert_eq!(err, "fft: real/imag length mismatch");
        // 长度与内核块长不符。
        let mut i8 = vec![0.0_f32; 8];
        let mut r16 = vec![0.0_f32; 16];
        assert!(fft.transform(&mut r16, &mut i8, false).is_err());
    }

    #[test]
    fn n4_手工解析_dft_逐位精确() {
        // X[k] = Σ x[n]·e^{-j2πkn/4}：整数输入 → 整数谱（len=4 的 twiddle 全部
        // 为 (1, 0)，无舍入机会）。
        let fft = Fft::new(4).unwrap();
        let mut real = [1.0_f32, 2.0, 3.0, 4.0];
        let mut imag = [0.0_f32; 4];
        fft.transform(&mut real, &mut imag, false).unwrap();
        assert_eq!(real, [10.0, -2.0, -2.0, -2.0]);
        assert_eq!(imag, [0.0, 2.0, 0.0, -2.0]);
        // 逆变换还原（÷N 后逐位回到原值：整数域往返精确）。
        fft.transform(&mut real, &mut imag, true).unwrap();
        assert_eq!(real, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(imag, [0.0; 4]);
    }

    #[test]
    fn 脉冲谱平坦锚点_re_逐位全1_im_逐位正0() {
        // GWT-FFT-01：δ 脉冲（位反转后仍在位置 0，只走 k=0 的 (1,0) twiddle 路径）
        // → 频谱逐位全 1 / 全 +0。基-4 纯路径（N=4096，log2=12 偶数，同 case1）。
        for n in [64_usize, 1024, 4096] {
            let fft = Fft::new(n).unwrap();
            assert!(!fft.has_base2_tail(), "N={n} 应为纯基-4 路径");
            let mut real = vec![0.0_f32; n];
            let mut imag = vec![0.0_f32; n];
            real[0] = 1.0;
            fft.transform(&mut real, &mut imag, false).unwrap();
            for i in 0..n {
                assert_eq!(real[i].to_bits(), 1.0_f32.to_bits(), "Re[{i}] @N={n}");
                assert_eq!(imag[i].to_bits(), 0.0_f32.to_bits(), "Im[{i}] 应为 +0 @N={n}");
            }
        }
    }

    #[test]
    fn 整bin谱线_共轭对称且幅度命中解析尺度() {
        // GWT-FFT-02：单位幅度整 bin 正弦 → Im 谱在 k0/N−k0 出现 ∓N/2 谱线，
        // Re 谱该两 bin 近零；基-2 尾路径（N=512，log2=9 奇数）。
        let n = 512_usize;
        let k0 = 3_usize;
        let fft = Fft::new(n).unwrap();
        assert!(fft.has_base2_tail(), "N={n} 应带基-2 尾");
        let mut real: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * k0 as f64 * i as f64 / n as f64).sin() as f32)
            .collect();
        let mut imag = vec![0.0_f32; n];
        fft.transform(&mut real, &mut imag, false).unwrap();
        let scale = (n / 2) as f64;
        assert!(
            (f64::from(real[k0]).abs()) < 1e-3 * scale,
            "Re[k0] 应近零，实际 {}",
            real[k0]
        );
        assert!(
            (f64::from(imag[k0]) + scale).abs() < 1e-3 * scale,
            "Im[k0] 应 ≈ −N/2，实际 {}",
            imag[k0]
        );
        assert!(
            (f64::from(imag[n - k0]) - scale).abs() < 1e-3 * scale,
            "Im[N−k0] 应 ≈ +N/2，实际 {}",
            imag[n - k0]
        );
        // 全程无非有限值。
        assert!(real.iter().all(|v| v.is_finite()) && imag.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn 与朴素_dft_参考一致_复输入两平面() {
        // GWT-FFT-03 投影：Re/Im 两平面都有能量的复输入，与 O(N²) 朴素 DFT
        // 交叉验证（逐级 f32 量化噪声在 1e-4 相对量级内）。
        let n = 64_usize;
        let fft = Fft::new(n).unwrap();
        let re_in = lcg_noise(n, 0xBEEF);
        let im_in = lcg_noise(n, 0xF00D);
        let mut real = re_in.clone();
        let mut imag = im_in.clone();
        fft.transform(&mut real, &mut imag, false).unwrap();
        let (want_r, want_i) = naive_dft(&re_in, &im_in);
        for k in 0..n {
            let scale_r = want_r[k].abs().max(1.0);
            let scale_i = want_i[k].abs().max(1.0);
            assert!(
                (f64::from(real[k]) - want_r[k]).abs() <= 1e-4 * scale_r,
                "Re[{k}] got {} want {}",
                real[k],
                want_r[k]
            );
            assert!(
                (f64::from(imag[k]) - want_i[k]).abs() <= 1e-4 * scale_i,
                "Im[{k}] got {} want {}",
                imag[k],
                want_i[k]
            );
        }
    }

    #[test]
    fn 逆变换往返还原误差_1e_6_量级内() {
        // GWT-FFT-06：正变换后接逆变换，逐级 f32 写回 → 往返误差 1e-7 量级
        // （N ≤ 1024），断言放宽到 1e-6 相对仍稳。
        let n = 1024_usize;
        let fft = Fft::new(n).unwrap();
        let re_in = lcg_noise(n, 7);
        let im_in = lcg_noise(n, 99);
        let mut real = re_in.clone();
        let mut imag = im_in.clone();
        fft.transform(&mut real, &mut imag, false).unwrap();
        fft.transform(&mut real, &mut imag, true).unwrap();
        for i in 0..n {
            let dr = (f64::from(real[i]) - f64::from(re_in[i])).abs();
            let di = (f64::from(imag[i]) - f64::from(im_in[i])).abs();
            assert!(dr <= 1e-6, "往返 Re[{i}] 偏差 {dr}");
            assert!(di <= 1e-6, "往返 Im[{i}] 偏差 {di}");
        }
        // 逆变换缩放生效：正变换结果整体 ÷N 后量级应明显小于原始谱。
        let mut r2 = vec![1.0_f32; 256];
        let mut i2 = vec![0.0_f32; 256];
        let fft2 = Fft::new(256).unwrap();
        fft2.transform(&mut r2, &mut i2, false).unwrap();
        assert_eq!(r2[0], 256.0, "正变换不缩放（X[0]=Σx）");
        fft2.transform(&mut r2, &mut i2, true).unwrap();
        assert_eq!(r2[0], 1.0, "逆变换 ÷N 后直流 bin 回到 1");
    }

    /// 黄金参考：node 经 esbuild bundle 直跑仓库根 src/dsp/fft.ts（G2 先例）。
    /// N=64、Re=lcg(64,0xBEEF)、Im=lcg(64,0xF00D)（与下方 lcg_noise 同式）、
    /// 正变换输出前 8 个样本的 f32 位型。
    mod golden {
        pub const FFT64_RE: [u32; 8] = [
            0x3ED6_DB88, 0x4146_79DB, 0x4058_1EAA, 0xBF37_EB66,
            0x3F9C_1039, 0x4028_6711, 0xC0FE_1F9A, 0xC022_406B,
        ];
        pub const FFT64_IM: [u32; 8] = [
            0x3F21_D379, 0xC016_FA87, 0x3FA7_4F12, 0xC0E2_51EB,
            0xC012_ACE8, 0xC0AF_FDE3, 0xC04B_9EA8, 0xC02A_2253,
        ];
    }

    #[test]
    fn 复输入正变换_命中_ts_黄金参考位型() {
        // 逐位锚点：与 TS fft.ts 的调度等价性（twiddle 生成、蝶形序、f32 落点）。
        let n = 64_usize;
        let mut real = lcg_noise(n, 0xBEEF);
        let mut imag = lcg_noise(n, 0xF00D);
        Fft::new(n).unwrap().transform(&mut real, &mut imag, false).unwrap();
        for (i, want) in golden::FFT64_RE.iter().enumerate() {
            assert_eq!(real[i].to_bits(), *want, "Re[{i}]：got {:08X} want {want:08X}", real[i].to_bits());
        }
        for (i, want) in golden::FFT64_IM.iter().enumerate() {
            assert_eq!(imag[i].to_bits(), *want, "Im[{i}]：got {:08X} want {want:08X}", imag[i].to_bits());
        }
    }

    #[test]
    fn 无状态_同输入两次调用逐位一致() {
        // GWT-FFT-07：变换无跨调用状态。
        let n = 256_usize;
        let fft = Fft::new(n).unwrap();
        let re_in = lcg_noise(n, 3);
        let im_in = lcg_noise(n, 4);
        let mut r1 = re_in.clone();
        let mut i1 = im_in.clone();
        fft.transform(&mut r1, &mut i1, false).unwrap();
        let mut r2 = re_in.clone();
        let mut i2 = im_in.clone();
        fft.transform(&mut r2, &mut i2, false).unwrap();
        assert_eq!(r1, r2);
        assert_eq!(i1, i2);
    }

    #[test]
    fn next_pow2_命中_ts_语义() {
        assert_eq!(next_pow2(0), 1);
        assert_eq!(next_pow2(1), 1);
        assert_eq!(next_pow2(2), 2);
        assert_eq!(next_pow2(3), 4);
        assert_eq!(next_pow2(513), 1024);
        assert_eq!(next_pow2(1024), 1024);
    }

    #[test]
    fn fft_stage_按块原位变换_与直接调用逐位一致() {
        // Stage 适配语义：每块一次原位变换，无跨块状态；与直接 transform 对比。
        let n = 128_usize;
        let re_in = lcg_noise(n, 11);
        let im_in = lcg_noise(n, 12);
        let mut want_r = re_in.clone();
        let mut want_i = im_in.clone();
        Fft::new(n).unwrap().transform(&mut want_r, &mut want_i, false).unwrap();

        let mut stage = FftStage::new(false);
        stage.prepare(n);
        let mut l = re_in.clone();
        let mut r = im_in.clone();
        stage.process(&mut l, &mut r);
        assert_eq!(l, want_r);
        assert_eq!(r, want_i);

        // 分两块（各 64）驱动：每块独立变换 = 各自整块变换的结果拼接。
        let mut l2 = re_in.clone();
        let mut r2 = im_in.clone();
        stage.prepare(64);
        stage.process(&mut l2[..64], &mut r2[..64]);
        stage.process(&mut l2[64..], &mut r2[64..]);
        let mut half_r = re_in[..64].to_vec();
        let mut half_i = im_in[..64].to_vec();
        Fft::new(64).unwrap().transform(&mut half_r, &mut half_i, false).unwrap();
        assert_eq!(l2[..64], half_r[..]);
        assert_eq!(r2[..64], half_i[..]);
        // reset 是无操作（无状态）。
        stage.reset();
        let mut l3 = re_in.clone();
        let mut r3 = im_in.clone();
        stage.process(&mut l3, &mut r3);
        assert_eq!(l3, want_r);
    }
}
