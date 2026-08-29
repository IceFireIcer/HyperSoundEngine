//! bench_share_codec —— share_codec::decode_share_code（v2 分享串解码）的 criterion 基准。
//!
//! 被解码串为固定 v2 (HSE2) 分享串：由 TS 支线 `src/engine/ShareCodec.ts` 的
//! `encodeShareCode` 对「createDefaultParams(48000) + customized=true +
//! sceneId='jazz' + eq.proBands[0]={40Hz,3.5dB,Q0.8}」编码生成，一次性固化——
//! 编码过程确定性（FNV-1a 校验和 + Crockford Base32），同参数必得同串。
//! 854 字符载荷 = 全链默认参数差分子树，覆盖 v2 还原（骨架 rehydrate）+
//! 白名单清洗的完整路径。
//!
//! 注：解码每次产出新参数 JSON（分配语义），属离线工具路径而非音频回调——
//! 本基准量化其量级，不适用实时铁律。

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use hse_core::share_codec::decode_share_code;

/// 固化的一次性 TS 编码产物（生成方式见模块注释； cargo test 校验可解码）。
const V2_CODE: &str = "HSE2-68X6C-CHM60-T3GDV-579XJ-4SBH4-8X7P8-KGE9Q-M4RBE-CHSJ4-EJVFC-H6CWK-5E5TP-AVK3F-4H3MD-1G5GH-6ERB9-DRH3M-CSE6M-P24W9-278R2-WE3X5-HXJ4S-KJCNR-QASBE-CDWJ4-EHP6C-P24SV-1D5Q2-4EHG5-GH728-HT64Q-32Z9C-FCH6C-WK5E5-TPAVK-3F4H3-MC9J6-MP24S-V1D5Q-24EHG-5GH72-8HT64-Q32Z9-CFCH6-CWK5E-5TPAV-K3F4H-3MCHN-60P24-SV1D5-Q24EH-G5GH7-28HT6-4Q32Z-9CFCH-6CWK5-E5TPA-VK3F4-H3MD9-G60P2-4SV1D-5Q24E-HG5GH-728HT-64Q32-Z9CFC-H6CWK-5E5TP-AVK3F-4H3MC-9G60R-2R8K7-C5MPW-8HT60-P24W9-278RJ-WCBX5-HXJ4S-KJCNR-QASBE-CDWJ4-EHJ60-R30B1-2CXGP-JVH27-8R2R8-KH48X-32BHH-FMP7P-8K6E9-JQ2XB-5DSHQ-J8HT6-GR30C-1C49K-P2TBE-48X30-B12E4-H3MC9-E65YJ-RYS2C-SS6AW-BNCNQ-66Y92-78W30-C1G5G-H6ERB-9DRH3-MC1C4-9RJ4E-HH5RR-QTB3V-49K74-SBHEN-JPWRV-S48X3-2DHG6-0R2R8-K7C5M-PW8HT-60P24-W9278-RJWCB-XBNYJ-R8KKC-DJPWS-A9CGH-3M8KA-C5X7M-8HC49-HQAWV-MDXPP-JYK5C-GH3MX-3JENJ-JR8KK-C5PQ0-V35A9-GQ8S9-278T3-GC1G6-1YG";

fn bench_share_codec(c: &mut Criterion) {
    let mut group = c.benchmark_group("bench_share_codec");
    group.sample_size(40);
    group.warm_up_time(std::time::Duration::from_secs(1));
    group.measurement_time(std::time::Duration::from_secs(3));
    group.throughput(Throughput::Bytes(V2_CODE.len() as u64));
    group.bench_function("decode_v2_854_chars", |b| {
        b.iter(|| {
            let params = decode_share_code(black_box(V2_CODE)).expect("固化 v2 串必须可解码");
            black_box(params)
        });
    });
    group.finish();
}

criterion_group!(benches, bench_share_codec);
criterion_main!(benches);
// 注：harness=false 的 bench 目标不运行 #[test]（无测试主入口），固化串的
// 防腐蚀锚点测试放在 hse-benches/src/lib.rs（cargo test 可达）。
