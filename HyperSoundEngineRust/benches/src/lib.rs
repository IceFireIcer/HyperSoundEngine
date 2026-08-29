//! hse-benches —— Phase 1 criterion 基准雏形的共享工具（纯库壳）。
//!
//! 场景口径对齐 TS 支线 `scripts/benchmark.mjs`：48kHz / 立体声 / 主块长
//! 128 帧。与 TS 脚本用常量 0.1 激励不同，这里用一段确定性合成信号
//! （三音正弦叠加 × 慢变包络，峰值逼近满幅），让限幅器增益路径与混响反馈
//! 网络在基准里有真实工作量；同时遵守仓库确定性铁律：不引入随机与时钟。
//!
//! 计时结构：母带缓冲只填充一次；每次 criterion 迭代先把母带复制进工作
//! 缓冲并复位阶段状态，再连续推若干块（默认总帧数 32768 帧 ≈ 0.68 s 音频）。
//! 多块摊薄单次调用的计时噪声；块长矩阵组保持每次迭代总帧数恒定、只改分块
//! 大小，以隔离"每调用固定开销 × 分块大小"的影响。
//!
//! 注意：Phase 1 试点模块在真实算法落地前是直通占位——此时基准数字为
//! 占位直通基线，仅供验证基准链路能出数。

use hse_core::Stage;

/// 基准采样率：对齐 TS benchmark 口径（48kHz）。
pub const SAMPLE_RATE_HZ: f64 = 48_000.0;

/// 主基准块长：对齐 TS benchmark 与 §三实时目标口径（128 帧）。
pub const MAIN_BLOCK_FRAMES: usize = 128;

/// 每次 criterion 迭代处理的总帧数（= 128×256 ≈ 0.68 s @48kHz）。
/// 被 128/256/512/1024 整除，供块长矩阵组复用。
pub const FRAMES_PER_ITER: usize = 32_768;

/// Phase 4 块长矩阵（规划书 §五：128/256/512，另加 1024 覆盖大分块）。
/// 全部为 [`FRAMES_PER_ITER`] 的约数，保证每迭代总帧数恒定。
pub const BLOCK_MATRIX: [usize; 4] = [128, 256, 512, 1024];

/// 一对 planar（非交错）立体声缓冲。
pub struct StereoBuffer {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl StereoBuffer {
    /// 分配全零缓冲（工作缓冲用：内容每次迭代由母带覆盖）。
    pub fn zeroed(frames: usize) -> Self {
        Self { left: vec![0.0; frames], right: vec![0.0; frames] }
    }

    /// 分配并填充确定性合成信号（母带缓冲用：整个基准期间只填充这一次）。
    pub fn synthesized(frames: usize) -> Self {
        let mut buf = Self::zeroed(frames);
        fill_synthesized(&mut buf.left, &mut buf.right);
        buf
    }

    /// 分配并填充固定种子 LCG 伪噪声对（母带缓冲用，确定性）。
    pub fn lcg_noise(frames: usize, seed_l: u32, seed_r: u32, amp: f64) -> Self {
        let mut buf = Self::zeroed(frames);
        fill_lcg_noise(&mut buf.left, &mut buf.right, seed_l, seed_r, amp);
        buf
    }

    /// 分配并填充常数值（对齐 TS `scripts/benchmark.mjs` 的 0.1 恒定激励，
    /// 供全链基准与 TS 基线同口径对比）。
    pub fn constant(frames: usize, value: f32) -> Self {
        Self { left: vec![value; frames], right: vec![value; frames] }
    }
}

/// 用确定性合成信号就地填充立体声缓冲。
///
/// 构成：220/997/3157 Hz（左）、233/1209/2793 Hz（右）三音正弦叠加，
/// 乘 0.5Hz 慢变包络与全局驱动增益 1.25——峰值逼近满幅但不削波，
/// 保证越过限幅器 -1dBFS 阈值、给真峰值过采样与混响反馈网络真实工况。
/// 同长度输入必得同一位型输出（纯 f64 算术 + 截断落点 f32，无随机源）。
pub fn fill_synthesized(left: &mut [f32], right: &mut [f32]) {
    assert_eq!(left.len(), right.len(), "左右声道必须等长");
    const TAU: f64 = core::f64::consts::TAU;
    const DRIVE: f64 = 1.25;
    for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
        let t = (i as f64) / SAMPLE_RATE_HZ;
        let env = 0.55 + 0.45 * (TAU * 0.5 * t).sin();
        let l_sig = 0.34 * (TAU * 220.0 * t).sin()
            + 0.21 * (TAU * 997.0 * t).sin()
            + 0.09 * (TAU * 3157.0 * t).sin();
        let r_sig = 0.30 * (TAU * 233.0 * t).sin()
            + 0.22 * (TAU * 1209.0 * t).sin()
            + 0.08 * (TAU * 2793.0 * t + 1.3).sin();
        *l = (DRIVE * env * l_sig) as f32;
        *r = (DRIVE * env * r_sig) as f32;
    }
}

/// 固定种子 LCG 伪噪声对（确定性、无随机源；镜像 hse-service dsp_chain 测试与
/// convolver expNoise 配方的推进序：先 `u = u·1664525 + 1013904223` 再取值）。
///
/// 与 [`fill_synthesized`] 的"宽带正弦叠加"互补：宽谱噪声让卷积/FDN 的分区
/// 累加与动态均衡的分析器处于持续非平凡工况。
pub fn fill_lcg_noise(left: &mut [f32], right: &mut [f32], seed_l: u32, seed_r: u32, amp: f64) {
    assert_eq!(left.len(), right.len(), "左右声道必须等长");
    let mut ul = seed_l;
    let mut ur = seed_r;
    for (l, r) in left.iter_mut().zip(right.iter_mut()) {
        ul = ul.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        ur = ur.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *l = ((f64::from(ul) / 4294967296.0 * 2.0 - 1.0) * amp) as f32;
        *r = ((f64::from(ur) / 4294967296.0 * 2.0 - 1.0) * amp) as f32;
    }
}

/// 把母带数据复制进工作缓冲、复位阶段状态，然后连续推
/// `blocks_per_iter` 个 `block_frames` 帧的块（前置条件：阶段已在计时区外
/// 按 `prepare(block_frames)` 完成全部预分配）。
///
/// 返回贯穿全过程的校验和（每块处理后的首样本累加）：调用方用
/// `black_box` 吞掉该值即可阻断编译器把整条处理循环当死代码消除。
pub fn push_blocks(
    stage: &mut dyn Stage,
    master: &StereoBuffer,
    work: &mut StereoBuffer,
    block_frames: usize,
    blocks_per_iter: usize,
) -> f32 {
    debug_assert_eq!(master.left.len(), work.left.len(), "母带/工作缓冲必须等长");
    debug_assert_eq!(blocks_per_iter * block_frames, work.left.len(), "总帧数必须恰好铺满工作缓冲");
    work.left.copy_from_slice(&master.left);
    work.right.copy_from_slice(&master.right);
    stage.reset();
    let mut checksum = 0.0_f32;
    for i in 0..blocks_per_iter {
        let off = i * block_frames;
        let end = off + block_frames;
        stage.process(&mut work.left[off..end], &mut work.right[off..end]);
        checksum += work.left[off];
    }
    checksum
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 固化 v2 分享串（bench_share_codec 基准输入）的防腐蚀锚点：TS 编码器
    /// 产物必须被 Rust 解码器接受，且解码出 sceneId=jazz 的参数对象
    /// （生成快照未被改动；harness=false 的 bench 目标不运行 #[test]，故放本 lib）。
    #[test]
    fn 固化v2分享串可解码且场景标记一致() {
        let code = "HSE2-68X6C-CHM60-T3GDV-579XJ-4SBH4-8X7P8-KGE9Q-M4RBE-CHSJ4-EJVFC-H6CWK-5E5TP-AVK3F-4H3MD-1G5GH-6ERB9-DRH3M-CSE6M-P24W9-278R2-WE3X5-HXJ4S-KJCNR-QASBE-CDWJ4-EHP6C-P24SV-1D5Q2-4EHG5-GH728-HT64Q-32Z9C-FCH6C-WK5E5-TPAVK-3F4H3-MC9J6-MP24S-V1D5Q-24EHG-5GH72-8HT64-Q32Z9-CFCH6-CWK5E-5TPAV-K3F4H-3MCHN-60P24-SV1D5-Q24EH-G5GH7-28HT6-4Q32Z-9CFCH-6CWK5-E5TPA-VK3F4-H3MD9-G60P2-4SV1D-5Q24E-HG5GH-728HT-64Q32-Z9CFC-H6CWK-5E5TP-AVK3F-4H3MC-9G60R-2R8K7-C5MPW-8HT60-P24W9-278RJ-WCBX5-HXJ4S-KJCNR-QASBE-CDWJ4-EHJ60-R30B1-2CXGP-JVH27-8R2R8-KH48X-32BHH-FMP7P-8K6E9-JQ2XB-5DSHQ-J8HT6-GR30C-1C49K-P2TBE-48X30-B12E4-H3MC9-E65YJ-RYS2C-SS6AW-BNCNQ-66Y92-78W30-C1G5G-H6ERB-9DRH3-MC1C4-9RJ4E-HH5RR-QTB3V-49K74-SBHEN-JPWRV-S48X3-2DHG6-0R2R8-K7C5M-PW8HT-60P24-W9278-RJWCB-XBNYJ-R8KKC-DJPWS-A9CGH-3M8KA-C5X7M-8HC49-HQAWV-MDXPP-JYK5C-GH3MX-3JENJ-JR8KK-C5PQ0-V35A9-GQ8S9-278T3-GC1G6-1YG";
        let params = hse_core::share_codec::decode_share_code(code).expect("固化 v2 串应可解码");
        assert_eq!(params.get("sceneId").and_then(|v| v.as_str()), Some("jazz"));
        assert_eq!(params.get("customized").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(params.get("sampleRate").and_then(|v| v.as_f64()), Some(48000.0));
    }

    #[test]
    fn 合成信号确定且逐位可复现() {
        let a = StereoBuffer::synthesized(3 * MAIN_BLOCK_FRAMES);
        let b = StereoBuffer::synthesized(3 * MAIN_BLOCK_FRAMES);
        assert_eq!(a.left, b.left, "同长度合成信号必须逐位一致（确定性铁律）");
        assert_eq!(a.right, b.right);
    }

    #[test]
    fn 合成信号有界且非平凡() {
        // 总帧数规模下包络已扫过多个周期，检查峰值上下界。
        let buf = StereoBuffer::synthesized(FRAMES_PER_ITER);
        let mut peak = 0.0_f32;
        for s in buf.left.iter().chain(buf.right.iter()) {
            assert!(s.is_finite(), "出现非有限样本");
            peak = peak.max(s.abs());
        }
        assert!(peak <= 1.0, "峰值越界满幅: {peak}");
        assert!(peak > 0.5, "信号过于平淡，不足以激励限幅器阈值: {peak}");
    }

    #[test]
    fn 推块循环按块长正确切分且校验和自洽() {
        struct Doubler {
            calls: Vec<usize>,
        }
        impl Stage for Doubler {
            fn prepare(&mut self, _max_block_size: usize) {}
            fn process(&mut self, left: &mut [f32], _right: &mut [f32]) {
                self.calls.push(left.len());
                for s in left.iter_mut() {
                    *s *= 2.0; // 全部是二的幂运算，浮点结果精确
                }
            }
            fn reset(&mut self) {
                self.calls.clear();
            }
        }

        let frames = 3072; // 可被 128/256/512 整除的小规模
        let master = StereoBuffer::synthesized(frames);
        for block in [128_usize, 256, 512] {
            let mut stage = Doubler { calls: Vec::new() };
            stage.prepare(block);
            let mut work = StereoBuffer::zeroed(frames);
            let sum = push_blocks(&mut stage, &master, &mut work, block, frames / block);

            assert_eq!(stage.calls.len(), frames / block, "块长 {block} 的 process 调用次数");
            assert!(stage.calls.iter().all(|&n| n == block), "块长 {block} 出现错误切片");

            // push_blocks 先复位（清空调用记录）再处理：记录仍在说明复位发生在处理前。
            // 校验和应等于每块首样本被 ×2 后的顺序累加（加法顺序一致 → 逐位相等）。
            let expect: f32 = (0..frames / block)
                .map(|i| 2.0 * master.left[i * block])
                .sum();
            assert_eq!(sum, expect, "块长 {block} 校验和不自洽");
            assert!(
                work.left.iter().any(|&s| s != 0.0),
                "工作缓冲未被就地改写"
            );
        }
    }
}
