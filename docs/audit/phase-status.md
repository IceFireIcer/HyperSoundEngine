# 阶段对照与全量验证记录（Phase 0–5）

> 日期：2026-08-31 · 版本口径：1.5.1 · 依据：《原生化双支线与Windows音频接入规划书》§五、
> 当前代码、共享规格与本批自动门禁；真实设备结论仅在实际验收后成立，浏览器结论仅覆盖已执行的 Chromium 环境。

## 一、阶段实态

| 阶段 | 状态 | 证据与残留 |
|---|---|---|
| **Phase 0** 规格基建 | 完成 | 25 份共享规格（17 DSP + 4 engine + 1 I/O + 3 spatial）；72 组音频冻结向量 / 144 文件、4 个引擎结构夹具、40 case 参数扫描、1 个 standard WAV 夹具、14 个 world-listener case 与 14 个 renderer/ABI case |
| **Phase 1** Rust 核心骨架 | 完成 | `hse-core` Stage 抽象、`hse-parity` 与 criterion 基准 |
| **Phase 2** 服务进程 | 主体完成，出口待验收 | `hse-wasapi` + `hse-service` + 控制面 + CLI + 推流已实现；独立捕获/渲染选路与 shared/exclusive 已落地，正式播放器/VB-CABLE 真机路径仍待用户验收 |
| **Phase 3** 双支线原生化 | 实现完成，出口待验收 | 17 个 DSP 模块、WAV、ShareCodec、推流协议与 `EngineChainStage` 1–21 级已完成，音频门禁 **72/72 PASS**；双推流客户端 + 非零回环联合验收待完成 |
| **Phase 4** 性能冲刺 | 自动实现完成，真机待验收 | 固定参数扫描、release 全时域零分配、服务纯内存 criterion、WASAPI 事件等待、双环排队延迟统计、shared/exclusive 与 `hse-real-audio-check` 已完成；真实设备 shared/exclusive 端到端延迟与整进程 CPU 仍待用户验收 |
| **Phase 5** 可选扩展 | 主体实现，出口仍有缺口 | 完整 1–22 级 wasm 与正式 Host 可选接入；Rust HRTF grid/SOFA、44.1/48/96 kHz 重采样、nearest/spherical、time/partitioned、距离/空气、Doppler、遮挡、声源大小、房间、稳定 slot、8 函数 ABI及第 22 级四模式已实现；空间门禁 **28/28 PASS** |

## 二、1.5.1 实现边界

Rust `EngineChainStage` 对齐 TS HyperSoundEngine 第 1–21 级。音频冻结向量仍只覆盖该主链并固定 `spatial.mode='off'`，共 **72 组 / 144 文件**。

Rust 第 22 级在控制路径注入 HRTF grid 后支持 `instant`、`headLocked`、`world` 与 `stage`。world 消费完整 listener 姿态、轨迹、playhead、遮挡和相邻快照确定速度；stage 对齐 preset/seat/roomSize/reverbAmount/customSources，ambience 同级叠加。`hrtf-core` renderer 还支持 Doppler、声源大小与按稳定 slot 保持对象状态。空间共享门禁由 world-listener 14 case 与 renderer/ABI 14 case 组成，共 **28/28 PASS**；spherical、非零 room、partitioned 和扩展效果由 Rust 行为/分配测试覆盖，不冒充跨语言数值对拍。

`hse-wasm::HseEngine` 已通过默认构造承载兼容的 1–21 级路径，并通过 `withSofaBytes` / `withHrtfGrid` 在 worklet 构造控制阶段接入 stage 22。正式 `HyperSoundEngineHost` 可选择 wasm backend，在主线程 fetch/缓存 HRTF bytes 与编译 module，参数更新复用资源预建新节点、等待 ready 后以零增益接入并预滚一个 128-frame render quantum，再交叉淡变替换；render 不解析 HRTF。headless Chromium E2E 已从 localhost 加载正式 bundle/wasm，并以无系统播放目的节点的 Web Audio 图门禁 ready、spatial off 1–21 级非静音、缺 HRTF 构造失败静音、参数节点替换淡变，以及预解析合成 HRTF grid 下成功的 `instant` stage 22 非静音、双耳不对称渲染；Firefox 尚未纳入自动门禁。空间薄 ABI 精确提供规划中的 8 个函数，外加生命周期、错误查询等辅助符号。

Phase 4 自动门禁覆盖 40 个合法全链参数快照，并已由 TS 冻结参数/输入种子/输出、由 TS 与 Rust 同调度执行共享摘要对拍（40/40 PASS）；另覆盖完整卷积 IR release 期 alloc/realloc/dealloc 为零、服务纯内存数据路径 benchmark 编译、事件驱动捕获与确定性排队帧统计。固定时长测试已删除且不恢复；测试以事件、帧数、块序号或显式超时上限收敛。

## 三、验证口径

| 门禁 | 1.5.1 口径 |
|---|---|
| 冻结音频向量 | 72 个 JSON + 72 个 f32；Rust **72/72 PASS** |
| Spatial fixture | world-listener **14/14** + renderer/ABI **14/14** = **28/28 PASS** |
| Phase 4 参数/分配 | 40 个参数扫描 case；默认/全开/release 稳态零分配自动门禁 |
| 服务性能与统计 | `bench_service_path` 覆盖纯内存服务路径；双环 current/high-water、块序号及 latency p50/p95/max 已实现 |
| wasm | `HseEngine` 默认 1–21 级兼容构造 + SOFA/grid stage 22 构造入口；Host HRTF 资源复用；空间 8 函数 C ABI |
| 自动 CI | `main` 提交 `ad860db` 的 [run 33365076659](https://github.com/IceFireIcer/HyperSoundEngine/actions/runs/33365076659)：`test` / `rust` / `rust-windows-silent` 全部成功 |
| 真机/浏览器 | Chromium wasm AudioWorklet E2E 已自动门禁 off、合成 grid 非 off stage 22 与预滚交叉淡变；Firefox、真实 SOFA 资产兼容性与真实音频设备仍待验收 |

## 四、剩余工作

1. 在真实设备上分别完成 shared/exclusive 端到端延迟、xrun 与整进程 CPU 验收；loopback 不支持 exclusive，不得用 shared 结果替代。
2. 将可再分发的真实 SOFA 资产纳入自动加载/解析/渲染门禁；当前合成夹具不能证明真实资产兼容性。
3. 将 Firefox wasm AudioWorklet 纳入浏览器 E2E；当前自动门禁仅覆盖 Chromium。
4. 物理设备输出仍为立体声双耳；Host 的 2/6/8 输入能力不等于物理 multichannel 输出。
5. 使用两个独立推流客户端与一路非零真实 capture/loopback 完成联合出口验收。
