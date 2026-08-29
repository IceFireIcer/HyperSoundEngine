# 阶段对照与全量验证记录（Phase 0–5）

> 日期：2026-08-29 · 依据：《原生化双支线与Windows音频接入规划书》§五阶段计划、
> 《架构书》§14、`CHANGELOG.md`。本文件回答"双线进行到哪一步、哪些已全量验证、下一阶段是什么"。

## 一、阶段对照表（规划书 §五 → 实态）

| 阶段 | 内容 | 状态 | 证据 / 残留 |
|---|---|---|---|
| **Phase 0** 规格基建 | specs/ 规范 + 向量 schema + TS 导出工具 + Rust 骨架 + CI | ✅ 完成 | `specs/`（6 模块规格 / 23 组冻结向量）；`d9f82ca` |
| **Phase 1** Rust 核心骨架 | hse-core stage 抽象 + 试点 3 模块双绿 + benches | ✅ 完成 | 对拍 11/11 逐位一致；`97e472d` |
| **Phase 2** 服务进程 v1（回环拦截端到端） | hse-wasapi + hse-service + 控制面 + CLI + xrun 上报 | ✅ 代码与真机验收完成 | 真机 GWT 14/14（`docs/audit/service-phase2-acceptance.md`，`c709143`）；**残留**：8h 零 xrun 长跑（需正式播放器）、VB-CABLE 安装引导（设备未装） |
| **Phase 3** 模块对拍推进 + 推流协议 | 22 级链逐模块规格化+Rust 双绿；MIDI/WAV I/O；推流协议；ShareCodec 兼容 | 🔶 **进行中（批次一、二）** | ✅ 批次一：Compressor/BassEnhancer/MidSide 双绿（对拍 23/23 逐位）、服务链六模块、推流协议全量落地（0.5.0）；✅ 批次二：EqChain 双绿（对拍 **27/27** 逐位，规格实证 processStereo 立体声共享状态 → 输出依赖块长，按事实固化为 GWT-EQ-07）；**批次三完成**：FdnReverb/Deesser/LoudnessComp 双绿（对拍 **39/39** 逐位，含 LoudnessComp 块长依赖轨迹与 FdnReverb width=0 1 ulp 案例）；**批次五完成**：FFT/Convolver 双绿（对拍 **55/55** 逐位；FFT 非流式驱动模型 = (L,R)=(Re,Im) 平面单块原位变换，schema 零改动；Convolver 输出与驱动块长无关六种切分逐位实证）；**未开始**：调制矩阵/HseStretch、MIDI/WAV I/O、ShareCodec Rust 解析、LufsMeter（推迟：仪表类观测量为 LUFS 标量读数而非音频输出，需向量 schema 先支持标量读数通道，属格式演进而非模块移植）、服务链插入 Deesser/LoudnessComp/FdnReverb(mode=fdn 路由)（模块双绿已就绪，插入属服务层批次） |
| **Phase 4** 性能冲刺 | 基准矩阵 + SIMD + §三指标实测留档 + 8h 压测 | ⬜ 未开始 | 依赖 Phase 3 全链双绿 |
| **Phase 5** 可选扩展 | ASIO（许可决策）/ wasm32 / 空间音频 Rust 核 | ⬜ 未开始 | TS 侧空间音频参考已就位（0.4.0，`src/spatial/`），为 Rust hrtf-core 的对拍 ground truth |

## 二、全量验证记录（2026-08-29 本轮实测）

| 门禁 | 结果 |
|---|---|
| `npm run typecheck` + `typecheck:ui` | 0 错误 |
| `node scripts/export-vectors.mjs`（幂等重跑） | 23 case / 46 夹具全部字节级一致 |
| `npm test`（vitest 全量） | **50 文件 / 572 用例全绿**（568 passed + 4 skipped=可选依赖未装） |
| `npm run build` | types + core + worklet 单文件包正常 |
| `cargo test --workspace` | 13 个测试套件全 ok（core 78 / parity 22 / service 48 / wasapi 2 / benches 3） |
| `cargo run -q -p hse-parity`（对拍门禁） | **55/55 PASS，全程 maxAbsDiff=0.000e0**（批次五后复测） |
| `npm run benchmark`（默认全链 48kHz/128） | **5.59% realtime**（5000ms 音频 279.61ms 处理完）——规划目标 ≤5% 单核（Ryzen 5 / i5 参考机），本机实测接近达标，Phase 4 正式留档 |

部署面：`hse-service.exe`（release）真机回环端到端验收 14/14（Phase 2 记录）；CI 双 job（test/rust）全绿于最近每次推送。

## 三、"逐文件验证 + 算法优化 + 注释"维护轮（本轮）

- **算法优化边界**：冻结向量约束下，biquad/limiter/reverb-simple/compressor/bass-enhancer/mid-side
  六模块的数值行为不可变（逐位一致门禁）；历史上 DSP 审计已确认 TS 支线在
  FFT（radix-4）、EqChain（块级向量化）、Convolver（非均匀分区）、Limiter/ReverbSimple（内联）
  上为优化后的实现——本轮"优化"以**零输出变化的纯性能项**为限，并以基准复测守门。
- **注释维护**：src/ 全量逐文件审计（头注释完整性/关键机制/非显而易见逻辑的行内说明），
  纯注释零行为变更，vitest 全量跑作为零行为变更的机械证明（本轮子代理执行，结果见其报告）。
- Rust 侧（hse-core/hse-service）注释在移植与实现时即按既有纪律书写（模块头 + 浮点事实标注），
  本轮抽查补缺。

## 四、下一步（依规划书顺序）

1. **Phase 3 批次二**：EqChain 双绿（进行中）→ 后续批次 FFT → Convolver → FdnReverb →
   DynamicEq → ModEffects/调制矩阵 → LufsMeter/LoudnessComp → HseStretch；每批 =
   规格 + 向量 + hse-core + （可选）服务链插入，一次推送。
2. **Phase 3 收尾项**：MIDI 环形队列 + WAV I/O（`.scratch/midi-and-wav-io/PRD.md` 为输入，
   注意该目录已 gitignore，PRD 内容需先固化进 specs/）；推流客户端联调。
3. **Phase 2 残留**：正式播放器（foobar2000/浏览器）+ 8h 零 xrun 长跑；VB-CABLE 安装引导。
4. 全部模块双绿后进入 **Phase 4** 性能冲刺（基准矩阵/SIMD/§三指标留档/8h 压测）。
