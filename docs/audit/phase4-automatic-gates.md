# Phase 4 自动门禁切片

> 日期：2026-08-31
> 范围：Rust `hse-core` 集成测试、双支线共享参数扫描、`hse-parity`、`hse-benches`、对应 CI 命令与本审计说明；未修改生产 DSP 或真实音频路径。

## 门禁内容

1. `phase4_param_scan.rs` 使用仓库内固定 LCG，不依赖随机源、时钟或外部数据。矩阵为 44.1kHz/63、48kHz/128、48kHz/257、96kHz/512 四组采样率/块长，每组 8 个固定种子和最小/最大边界，共 40 个合法全链快照。
2. 同一 40 case 已提升为 `specs/engine/vectors/phase4-param-scan.json` 双支线共享门禁：`scripts/phase4-param-scan.mjs` 只有显式 `--write` 才能首次创建，默认缺失/漂移失败；TS 与 Rust 按同一 LCG 输入和 17 帧短尾调度重放，以 1e-6 相对容差比对有限率、非零率、峰值与 RMS 数量级摘要。旧 72 组逐样本音频向量不变。
3. 每个扫描 case 检查完整输出有限，并以两条独立实例逐样本位比较确定性重放；全部 case 在 `reset()` 后再次检查输出有限，IEQ 关闭的 case 额外检查 reset 后逐位复现。
4. IEQ 开启时仍参与有限输出和独立实例确定性扫描，但不声明显式 reset 逐位等价：当前 `EngineChainStage::reset()` 清零 IEQ 分析增益，却保留已自适应更新的 IEQ 滤波器系数。该生产语义不在本切片允许修改范围内。
5. `realtime_alloc.rs` 的默认链和代表性全开链从单块扩为连续 64 个 128 帧块；新增 4 秒、192000 taps 固定种子指数衰减 IR，脉冲后连续 1504 块（覆盖完整 IR 长度并留 512 帧余量）静音覆盖 release 调度。计量窗口同时统计 alloc/realloc 与 dealloc，期望均为零。
6. 新增 `bench_hse_stretch`、`bench_lufs_meter`、`bench_engine_param_domain`，补齐变速/变调参数域、计量采样率/块长域、全链默认/旁路边界/全开上边界。CI 只编译 benchmark，不把机器时序阈值作为正确性门禁。

## CI 与复现

CI 显式运行：

```bash
node scripts/phase4-param-scan.mjs
npx vitest run test/phase4-param-scan.test.ts
cargo test -p hse-core --test phase4_param_scan --locked
cargo test --release -p hse-core --test realtime_alloc --locked
cargo test -p hse-service --test pipeline_fake 完整管线由readiness许可推进_经过service_chain与双环 --locked
# windows-latest：仅方向映射，不枚举或打开真实设备
cargo test -p hse-wasapi endpoint_modes_select_expected_device_and_stream_directions --locked
cargo bench --workspace --no-run --locked
```

`realtime_alloc` 必须在 release profile 执行。服务完整管线测试以显式 capture readiness 许可推进，覆盖假捕获 → 输入环 → `ServiceEngineChain` → 输出环 → 内存渲染，并以许可数、块序号和输出指纹判定；超时仅作卡死上限，不以固定运行时长作为成功条件。

本轮本机验证：固定种子参数扫描 1 项、release 零分配 5 项、完整服务 readiness 管线 1 项与 Windows WASAPI 无声方向 1 项通过；`hse-service` 单元测试 74 passed / 1 ignored、`pipeline_fake` 19 passed、`push_stream` 9 passed，workspace all-targets 编译通过。三份新增 benchmark 以定向 `cargo bench -p hse-benches --bench ... --no-run --locked` 编译通过。全程未运行 WASAPI 真实开流、未启用真实音频，也未运行或恢复固定时长测试。
