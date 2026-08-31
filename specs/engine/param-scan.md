# Phase 4 固定种子全链参数扫描

本规格在既有 72 组音频冻结向量之外，追加 40 个 `HyperSoundEngine` 第 1–21 级全链跨语言参数扫描 case；不改变旧向量及生产 DSP。第 22 级固定 `spatial.mode="off"`。

## 驱动契约

- 矩阵固定为 `44100/63`、`48000/128`、`48000/257`、`96000/512`（采样率/块长）。每组包含 8 个固定种子合法快照、1 个最小边界快照和 1 个最大边界快照。
- 每 case 的 `frames = blockSize * 5 + 17`，因此必有 17 帧短尾。输入由夹具 `inputSeed` 驱动 LCG：`state = state * 1664525 + 1013904223 (mod 2^32)`，每帧依次生成左、右声道，映射到 `[-0.95, 0.95)` 后量化为 f32。
- `params.overrides` 深合并默认参数，数组整体替换；构造全新实例、设置一次参数、按 `blockSize` 顺序处理，状态跨块保持，不额外 reset。
- 每个 case 的 `expected.left/right` 直接冻结 `finiteRatio`、`nonZeroRatio`、`peakOrder=floor(log10(peakAbs))`、`rmsOrder=floor(log10(rms))` 四项结构摘要。两侧对每项使用 `|got-want| <= 1e-6 * max(|want|, 1e-9)` 判定。该扫描约束合法参数域中的有限性、活动输出与幅值数量级；旧 72 组音频向量继续承担逐样本波形约束。

## 冻结与门禁

`node scripts/phase4-param-scan.mjs` 默认只验证，夹具缺失或内容漂移均失败；仅首次创建可显式传 `--write`。TS 测试与 Rust `hse-parity` 必须同时消费该夹具，任一 case 失败或夹具缺失均进入综合非零退出码。
