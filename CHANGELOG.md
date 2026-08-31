# Changelog

## [Unreleased]

### Documentation
- **发布就绪度与文档统一**：新增 `docs/RELEASE_READINESS.md`，区分源码 GitHub Release、npm、Windows 二进制与 crates.io 的发布门槛；同步 AGENTS、README、HANDOVER、项目/架构/接入/历史审计文档至 1.5.1 口径，并将第三方来源改为公开上游链接。

## [1.5.1] - 2026-08

### Fixed
- **AudioWorklet 参数替换无缝切换**：新节点 `ready` 后先以零增益接入并预滚一个 128-frame render quantum，再启动新旧路径互补淡变，避免冷启动节点尚未产出时旧路径已衰减；Chromium E2E 以音频线程 barrier 切分采集窗口，排除 MessagePort 中滞留的初始化块。

### Added
- **统一接入指南与 service 客户端示例**：重写 `docs/INTEGRATION.md`，按 TS core、浏览器 TS Host、浏览器 Rust/WASM Host、Rust `hse-service` 与空间 C ABI 明确选择边界、生命周期、参数语义、PCM 帧格式、错误处理和验收清单；新增无副作用的 `examples/hse-service-client.mjs`，并同步 README、API、服务说明与 wire 规格中的当前实现口径。

## [1.5.0] - 2026-08

### Fixed
- **Phase 4 自动门禁与验收口径**：CI 以 release profile 运行 `hse-core` 稳态零分配测试，Windows 增加不枚举/不打开设备的 `hse-wasapi` 方向门禁，服务以显式 readiness 许可覆盖假捕获→输入环→`ServiceEngineChain`→输出环→内存渲染完整路径；`hse-real-audio-check` schema 2 明确仅为 low-level WASAPI diagnostics，并与未测的服务管线字段拆分。双推流工具改用两条独立 WebSocket，通过加性的 `getState.sessions` 分别核验接收/消费/排空和零 xrun，物理输出与双频验证明确返回 `external-output-required`。
- **正式 wasm AudioWorklet 浏览器兼容性**：改用 wasm-bindgen 的命名 `initSync` 导出，避免将异步默认初始化器误用于同步 processor 构造；生产 bundle 在 AudioWorkletGlobalScope 缺少 `TextEncoder` / `TextDecoder` 时前置轻量 UTF-8 codec，使 Chromium 能完成处理器注册、wasm 实例化与 `ready` 握手。
- **TS `processMulti` 共享主链拓扑**：多声道 spatial 开启时改为先由 `SpatialBackend.processMulti` 双耳化全部 3–8 路输入，再让所得 L/R 执行第 1–21 级主链，确保 ch2+ 同样受 EQ、动态器、分析与最终 Limiter 影响；spatial off 继续保持 ch0/ch1 兼容，旧双声道 `process` 仍保持 stage 22 最后的冻结顺序。
- **hrtf-core SOFA 采样率转换**：保留 `sofar default-features=false` 纯 Rust parser，在控制路径以确定性 129-tap Kaiser-windowed sinc 转换 44.1/48/96k HRIR；`Data.Delay` 先按源采样率验证整数采样，再按物理时间换算到目标采样率，输出长度、轴与有限性严格校验。真实 SOFA 测试继续通过 `HSE_TEST_SOFA` ignored 门控，不下载资产。
- **TS/wasm Worklet 参数更新移出渲染线程**：`HyperSoundEngineHost.setParams` 对两个 worklet backend 都在宿主控制路径预建带 `processorOptions.initialParams` 的完整新节点，等待 `ready` 后经双 GainNode 在默认 20ms 内线性交叉淡变；旧链仅在 `AudioContext.currentTime` 到达淡变终点后清理，context 暂停不会被墙钟定时器提前断开。运行期 worklet 不再接受参数重建消息，仅保留 `reset`；替换失败保留当前可听链，并发更新串行化，dispose 立即清理并结束等待。

### Added
- **Phase 4 固定种子全链跨语言参数扫描门禁**：复用 40 个合法快照矩阵（44.1/48/96kHz，63/128/257/512 块，8 固定种子 + 最小/最大边界，全部含 17 帧短尾），由 TS 事实实现显式 `--write` 冻结参数、输入种子与输出；默认脚本仅验证且缺失即失败。TS 与 `hse-parity` 同调度重放并按 1e-6 相对容差比对有限率、非零率及峰值/RMS 数量级摘要，综合退出码覆盖新门禁；既有 72 组逐样本音频向量保持不变。
- **Rust EngineChain stage 22 world/stage 参数对等**：`hse-core` 完整消费 world listener position/yaw/pitch/roll、sources/trajectories/playhead/occlusion 与相邻快照确定速度，并按 source id 使用确定性稳定 slot；stage 对齐四套 preset、seat、roomSize、reverbAmount、customSources 与 ambience。`hrtf-core` 新增完整欧拉几何及 Doppler、遮挡、声源大小方向模糊/右耳去相关，保持中性配置和 spatial off 旧路径不变；service 与 wasm 构造路径同步覆盖，新增短块、reset 和稳态零分配门禁。world-listener 共享夹具在保留原 12 case 的基础上追加 pitch/roll 两例，综合空间门禁为 28/28。
- **正式 wasm AudioWorklet Chromium E2E 门禁**：新增基于 `playwright-core` 的 headless Chromium 测试，按 wasm32 release → wasm-bindgen web glue → core/Host bundle → 生产 worklet bundle 的顺序构建，并从 localhost 加载真实产物；以不连接扬声器或麦克风的 Web Audio 图验证 `ready`、spatial off 的 1–21 级非静音处理、构造失败静音和参数节点替换淡变。Firefox 尚未纳入自动门禁。
- **Rust stage 22 接入正式 wasm Host 路径**：`hse-wasm::HseEngine` 新增 SOFA bytes 与预解析规则 grid 构造入口，在 AudioWorklet 构造控制阶段统一调用 `EngineChainStage::from_params_with_hrtf_grid`；默认 spatial off 保持兼容，非 off 无 HRTF 明确失败。`HyperSoundEngineHost` 新增互斥的 `hrtfUrl` / `hrtf` 选项，主线程 fetch 并缓存 SOFA bytes、编译 wasm module，参数替换沿现有 crossfade 预建节点并复用两项资源，render callback 不解析 HRTF。
- **Rust stage 22 接入 hse-service 正式路径**：控制面新增向后兼容的 `loadHrtf{path}`，仅在 idle 从本地绝对 `.sofa` 普通文件按已配置采样率调用 `hrtf-core::load_sofa_file`，成功后将网格与规范路径存入 `EngineHandle`；`getState.hrtf` 加性暴露加载状态和网格元数据。`start` 与运行态 `setParams` 统一在控制线程预建带 grid 的 1–22 级链，DSP 线程仅在块边界换链；无 grid 的非 off spatial 明确失败，改配采样率清除旧 grid，非法/解析失败路径保持旧状态。
- **Rust HRTF 分区卷积与 wasm ABI**：`hrtf-core::BinauralRenderer` 新增 `ConvolutionMode::Time | Partitioned`；partitioned 按 `RenderProfile` 使用固定 64/128 样本分区，在 prepare/控制路径预计算网格 HRIR 频谱及对象工作区，每对象每分区一次输入 FFT 复用于左右耳，稳态 render 零分配/零锁。模式切换重建并清状态，延迟准确上报；`hse-wasm::spatial_set_convolution_mode(handle, 1)` 现可实际启用，不再返回 unsupported。
- **Phase 4 真机验收工具**：新增默认 dry-run 的 `hse-real-audio-check`，枚举设备并检测 VB-CABLE，真实开流要求 `measure --run` 与 `HSE_ALLOW_REAL_AUDIO=1` 双门控；以固定帧数/脉冲数生成并相关脉冲，JSON 输出 shared/exclusive、设备 ID、采样率/块长、延迟 p50/p95/max、xrun、进程 CPU 与帧吞吐。无法证明 capture/render 物理闭环时明确返回 `external-loopback-required` 且不伪造延迟；另附同门控、固定帧数的双推流客户端和正式播放器/VB-CABLE/非零回环验收作业书。
- **TS spatial 多声道实时输入产品路径**：`AudioEngine.processMulti` 接受 caller-owned 的 3–8 路非交错缓冲，经预分配引用视图把 6/8 路 discrete 输入送入 `SpatialBackend.processMulti` 并固定输出双耳；spatial off 保持 ch0/ch1 兼容下混。浏览器 Host 新增 `inputChannelCount: 2 | 6 | 8`，TS AudioWorklet 以 explicit/discrete 协商输入且始终 2 路输出，ScriptProcessor 回退同步支持；热路径不走会分配的 `processBus`。
- **WASAPI 捕获事件等待与服务路径基准**：`CaptureSource` 新增有界 readiness 等待，Windows loopback/直捕复用已持有的 WASAPI event handle，服务捕获线程移除固定 10ms 空轮询；fake 后端提供显式事件许可与调用计数，测试不以固定睡眠时长断言。新增 `bench_service_path`，纯内存覆盖 deinterleave、三会话混音、完整 DSP、interleave、双环搬运与串联块路径，不打开真实音频设备。
- **WASAPI shared/exclusive 访问模式**：`OpenOptions`/服务配置新增稳定 `AccessMode`，控制面与 CLI 支持可选 `shareMode:"shared"|"exclusive"`；省略时保持 shared 行为和旧回显形态。普通 capture/render 的 exclusive 路径使用 `EventsExclusive`、要求设备原生支持目标立体声 f32 格式并禁用自动转换，loopback+exclusive 在 configure 阶段以 -32602 拒绝且不回退 shared。

## [1.3.0] - 2026-08

### Added
- **Windows 捕获/渲染独立选路**：控制面 `configure` 新增 `mode:"capture"`、`captureDeviceId` 与可选 `outputDeviceId`；保留旧 `mode:"loopback"` + `renderDeviceId` 请求和四字段回显。`hse-wasapi` 新增默认/显式 capture 端点直捕，服务启动时分别构造捕获与渲染流，并拒绝两端协商采样率不一致。

### Fixed
- **推流集成测试跨平台时序稳定性**：双会话混后处理测试改为在 idle 期预装低于 capture→DSP 输入环容量的等长时间线，并以完整非零渲染序列作为消费屏障；不再依赖微秒级线程调度推断吞吐，避免 Linux/Windows CI 偶发将输入环积压误判为会话丢帧。

## [1.2.0] - 2026-08

### Added
- **Spatial Slice 1：world-listener 几何双绿**：新增共享规格、严格 JSON Schema 与 12 个结构化 case，冻结右手世界坐标、listener position/yaw、方位 `[-180,180)`、仰角、距离和平移/整圈等价语义；TS 将几何函数作为公共 API 导出并补引擎第 22 级 world/yaw 集成回归。
- **Rust `hrtf-core` 起步**：新增平台无关、无生产依赖的 f64 world-listener 几何 crate；`hse-parity` 在原音频 72/72 之外强制执行空间 12/12，缺少或无效空间夹具即门禁失败。此版本不包含 Rust HRIR、卷积、房间或主链 stage 22。

### Fixed
- **world 方位规范化**：修复 TS 世界模式在 yaw 跨越 ±180° 或超出整圈时产生越界方位、进而错误选择左右输入声道的问题；重合点保持全零，非有限坐标在调用边界拒绝。

## [1.1.0] - 2026-08

### Added
- **WAV 双模式兼容**：TS/Rust 核心新增显式 `standard` RIFF/WAVE 小端编码，解码自动识别 standard 与 1.0.0 历史 `legacy` 格式；共享手工标准夹具覆盖 PCM16、Float32 与严格头校验，WaveForge 离线导出统一复用核心 standard 编码。无 format 的编码继续输出 legacy，既有 37 个 golden 保持不变。

### Fixed
- **标准 WAV 输入校验**：standard 路径拒绝截断 RIFF/data、无效采样率及不一致的 byteRate/blockAlign，并将外部 PCM16 最小码 `-32768` 正确解码为 `-1.0`；legacy 路径保留原兼容语义。

## [1.0.0] - 2026-08

### Added
- **Rust 服务完整链统一**：`hse-service` 删除独立维护的旧子链，改为通过兼容 wire 参数适配器构造 `hse-core::EngineChainStage`，实际执行第 1–21 级完整主链（空间级保持 off）；新增全级序、all-bypass 冻结向量投影和三源混后处理确定性测试，完整链 benchmark 同步迁移。
- **参数/场景共享契约与 Rust 双向分享能力**：新增 `specs/engine/params.md`、`scenes.md` 及 3 个由 TS 事实源生成的结构化冻结夹具，固定完整默认参数、12 个内置场景和 14 个 HSE2 编码 case；Rust 新增默认参数/场景生成 API 与 HSE2 encode，覆盖固定键序、Crockford Base32、UTF-16 FNV、未知字段、非 ASCII、负零和“编码保留原值、解码再 clamp”的跨语言逐字符契约。

### Removed
- **MIDI 与 ASIO 范围清理**：项目不再提供或规划 MIDI 事件、MIDI Learn、相关 UI/API/服务控制面规格；删除 `specs/io/midi.md`。Windows 设备 I/O 固定为 WASAPI，不再规划 ASIO 后端；ADR-0004 取代 ADR-0001/0002 中的 ASIO 方向。调制矩阵的 LFO 与 Envelope Follower 保留。

### Fixed
- **服务控制面契约与实时安全加固**：JSON-RPC 显式非法 `id`/`params` 在分派前拒绝且无副作用；start/stop 的 phase 通知在请求连接上严格先于响应，并向其他连接广播一次；`setParams` 运行态候选构链、命令投递与规范化 `lastParams` 改为事务提交，非运行态保留延迟构链兼容语义；公开 stop 不再接受 starting 状态。
- **稳态零分配机械门禁**：Convolver 固定容量 pending 队列移除处理期扩容，服务热换旧链经 SPSC 回收环转交专用非实时线程析构；Rust 分配器测试同时统计 alloc/realloc/dealloc，覆盖卷积、默认链和代表性全开链；TS 调制矩阵新增 `processBlockInto` 供引擎复用预分配结果，同时保留公开 `processBlock` 的独立快照语义。
- **跨平台 CI**：增加本批 Rust 文件格式检查、Clippy 可疑逻辑/性能阻断、全部 benchmark 编译和 `windows-latest` 的 core/service/parity 无声门禁；真机 WASAPI 仍需显式 `HSE_ALLOW_REAL_AUDIO=1`。

## [0.7.0] - 2026-08

### Added
- **Phase 3 真正全链收口**：新增 `specs/engine/chain.md`，以 5 组 `engine-chain` 冻结向量固化 HyperSoundEngine 第 1–21 级组装行为；Rust `EngineChainStage` 完成响度归一化、Surround3D、NightMode、IEQ、分析/LUFS 取样、调制主增益及既有 DSP 模块的全链编排。第 22 级空间音频明确为 `spatial.mode='off'` 契约，不纳入本轮 Rust 全链。共享规格现为 **18 份（17 DSP + 1 engine-chain）**，冻结向量 **72 组 / 144 文件**，Rust 对拍门禁 **72/72 PASS**。
- **Phase 5 wasm32 最小试点**：新增 `hse-wasm` workspace 成员，以 `wasm-bindgen` 暴露单个 `HseBiquad`，使用预分配 planar 缓冲与指针边界接入独立 AudioWorklet 示例；不替换现有 TS worklet，不代表完整 Rust 引擎链 wasm 化。配置消息使用 `requestId` 成功/失败回执；CI 锁定 `wasm-bindgen-cli 0.2.127`，实际生成 web glue 并执行 browser-platform 打包、wasm magic 与 Node builtin 隔离门禁。ASIO 与 Rust `hrtf-core` 尚未启动。
- **Phase 3 收官：LufsMeter 双绿 + 向量 schema 计量读数演进（批次八）**：`specs/schema/vector-case.schema.json` 加性演进（moduleKind stream|meter 双向绑定 + readings 标量读数 {want, tol}，哨兵 "NaN"/"±Infinity" 等值判定）；`hse-core/src/lufs_meter.rs` 移植（K 加权两级 TDF2、BS.1770 双门限、LRA 直方图、4×/24 抽头真峰值多相核；f32 滑窗落盘/f64 统计的精度纪律逐字对齐；7 项规格外 TS 事实固化）；parity harness 扩展 meter 类型（两段输入布局 + readings 绝对容差/哨兵等值判定）。对拍门禁 63→**67 case 全 PASS**（LufsMeter 六读数最大偏差 3.55e-15，比最紧容差低 13 个数量级）。**至此 Phase 3 模块级工作完成**；本版本再以 engine-chain 向量完成 1–21 级组装收口。
- **Phase 4 基准矩阵 + SIMD 评估 + 指标留档**：16 个 criterion bench（全 12 已移植模块 × 块长矩阵 + 全链 60s 离线 + fft/convolver/midi/wav/share_codec）+ `docs/audit/phase4-bench-matrix.md`（热点排名：convolver 385 ns/帧断层第一）+ `docs/audit/phase4-simd-eval.md`（逐位对拍约束分析：零期望样本须逐位、非零样本余量仅 8–10 f32 ulp → 仅通道级 SIMD 安全；自动向量化实测 +6–10%）。**已测三项离线 DSP 性能达标**：全链离线 0.546% realtime = TS 基线的 9.7–10.2×（目标 ≥3×）；默认链 CPU 0.546%（≤5%）；最重场景 10.7%（≤25%）。SIMD 实施缓办；WASAPI 端到端延迟与全链随机参数扫描仍未验证。

### Fixed
- **整链 sidechain 语义对齐 TS**：普通双声道 `process` 即使快照开启 `sidechainEnabled` 也保持内部检测；新增显式 `process_with_sidechain`，仅在调用方真实提供外部侧链时驱动 Compressor/Deesser，NightMode 永远使用内部检测。
- **整链卷积路由与参数错误收敛**：`reverb.mode='convolution'` 在无/空 IR 时按 TS 回退算法混响，有效 IR 接入 `ConvolverStage` 并应用 mix/preDelay/dePeriodize；非法 IR 与嵌套参数类型错误返回带路径的 `Err`，不再 panic 或静默换效果。

## [0.6.0] - 2026-08

### Added
- **MIDI 事件接口 + MIDI Learn 移植（Phase 3 第 2 项）**：`hse-core/src/midi.rs`——4096 容量事件环（溢出丢最旧 + dropped 累计计数器，reset 不清零）、绑定表（CC 线性映射 + invert + 防 zipper 一阶平滑，note on/off 布尔目标，键计算 cc / 0x4000+note、channel 不入环）、块头消费 + 空环冻结语义（平滑收敛不推进，与引擎逐块一致）、learn/unlearn/getBindings/masterBound、JSON 路径写回契约（点路径游走 + sections 去重保序返回）；AUTOMATABLE 38 条白名单对齐 TS。28 组移植+补充测试（含 node 黄金位型）。**Phase 3 第 2 项完成**。
- **WAV 文件 I/O 移植（Phase 3 第 2 项）**：`hse-core/src/wav.rs`——encodeWav/decodeWav 逐字节移植（RIFF 头字段大端的历史怪癖、PCM16 半值向 +∞ 取整且刻度 32767、float32 位精确含 denormals、验证顺序逐条固化、畸形文件错误消息与 node 逐字一致）；37 组 node 黄金用例（13 enc + 24 dec）。**多通道 WAV 进出 Rust 支线可用**。
- **ShareCodec Rust 解析（Phase 3 第 4 项：旧分享串在 Rust 支线可解析）**：`hse-core/src/share_codec.rs`——v1 全量载荷与 v2 差异载荷（HSE2/Crockford）双路 decode + 完整白名单清洗（~400 行 TS sanitizeParams 全量移植：clamp/枚举/截断/注入防护/spatial 深度清洗）+ V8 fdlibm 三角位级 ts_trig 复用；43 组 node 黄金用例对拍（Rust 解码结果与 node decodeShareCode 逐字一致，含错误消息与多字节 fnv 的 UTF-16 码元语义）。
- **服务链全模块插入（Phase 3 服务层集成）**：hse-service 引擎子链重建为全序 12 级（midSide → biquad → eqChain → deesser → compressor → modEffects 五效果 → 混响三路路由 simple|fdn|convolver|off → bassEnhancer → loudnessComp → dynamicEq → modMatrix 控制率 → limiter，镜像 TS 22 级引擎相对顺序；NightMode/IEQ/分析级为引擎内部组合跳过并注释，HseStretch 保持链外 getStretch 语义）；控制面 setParams 新增 9 个可选键（eqChain/deesser/modEffects/reverbRoute/fdnReverb/convolver{irRecipe}/loudnessComp/dynamicEq/modMatrix，全部向后兼容 §十.2）；21 个新测试（全默认链逐位直通回归/逐级激活/三路路由切换/delta IR 延迟/热更换零 warnings）。`specs/service/control-plane.md` §一/§5.6/§八 向后兼容修订（§十.5 记录）。
- **Phase 3 批次六：ModulationMatrix/HseStretch 两模块双绿（规划书 §五）**：2 份规格（modulation-matrix 控制率 Stage——LFO 每块推进后采样/包络逐样本联合峰值/masterGain 钳 [0,4]/无路由逐位恒等锚点/输出依赖块长；hse-stretch 块窗映射——变长输出截断补零回填定长网格、rate=1 非逐位（相位声码器重构噪声）按算术调度等价前提实践逐位、跨调用无状态、参数突变=全新实例契约）+ 8 组冻结向量 + hse-core 移植（含 Resampler 逐行移植）；对拍门禁 55→**63 case 全 PASS、maxAbsDiff=0.000e0**。**22 级链的模块对拍主体至此全部双绿**（仅 LufsMeter 因需标量读数向量演进而推迟）。
- **Phase 3 批次五：FFT/Convolver 两模块双绿（规划书 §五）**：2 份规格（fft：非流式变换驱动模型——(L,R)=(Re,Im) 平面单块原位变换，向量覆盖纯基-4 与基-2 尾两条调度路径；convolver：非均匀分区/滑动窗口/冻结窗口尾语义）+ 8 组冻结向量 + hse-core 移植（radix-4 逐级 f32 落点、V8 fdlibm ts_trig 位级复刻、IR 配方 LCG 逐字重建、twiddle 角构造性上界 <3π/2 的边界证明）；对拍门禁 47→**55 case 全 PASS、maxAbsDiff=0.000e0**（Convolver 输出与驱动块长无关六种切分逐位实证）。已知边界记录：3π/2 精确值的 cos 位型未复刻（运行时不可达，#[ignore] 待办测试）。
- **Phase 3 批次四：DynamicEq/ModEffects 两模块双绿（规划书 §五）**：2 份规格（GWT-DY-01..11 / GWT-ME-01..12）+ 8 组冻结向量 + hse-core 移植（全通交叉树 sumsq 不跨调用累积、五效果引擎顺序级联 + enabled 链路门控、chorus/flanger LFO 整块步进、phaser 并级全通 f32 状态落点）；对拍门禁 39→**47 case 全 PASS、maxAbsDiff=0.000e0**。规格实证：DynamicEq 输出依赖驱动分块（任意多余调用边界提前触发控制更新）、chorus/flanger 输出依赖 blockSize、Phaser stages 7≡8 逐位一致（并级非级联）、tremolo rateHz 上界 30 与其他效果 20 不同。
- **Phase 3 批次三：FdnReverb/Deesser/LoudnessComp 三模块双绿（规划书 §五）**：3 份规格（GWT，含实证行为事实：FdnReverb width=0 为 1 ulp 级一致而非逐位、Deesser 内部 Biquad 恒 48000 设计率、LoudnessComp 输出依赖块长且 reset 为钉扎语义、Deesser 阈下为 LR-4 全通重构幅度不变）+ 12 组冻结向量 + hse-core 移植（FDN Householder 混合/素数表、LR-4 交叉、BS.1770 之外的等响度 shelf/peaking 设计）；对拍门禁 27→**39 case 全 PASS、maxAbsDiff=0.000e0**。LufsMeter 显式推迟（仪表类标量读数需向量 schema 演进）。
- **Phase 3 批次二：EqChain 双绿（规划书 §五）**：规格（12 GWT）+ 4 组冻结向量 + hse-core 移植（含级联 Q 补偿 Gauss-Seidel 精确语义：0.8 阻尼/至多 5 轮/单轮 maxErrDb<0.05 提前终止）；对拍门禁 23→**27 case 全 PASS、maxAbsDiff=0.000e0**。规格实证并固化关键行为事实：`processStereo` 立体声共享滤波器状态 → 输出依赖 blockSize（分块不变性仅对单声道 processBlock 成立，GWT-EQ-07/08）；gain=0 全直通为逐位锚点。
- **维护轮**：src/ 全量 57 文件注释质量审计（结论：绝大多数为 A 级）；3 个 B 级文件（modulation/HseAudioBus/ModEffects）补齐出处/许可/确定性声明，纯注释零行为变更。
- 阶段对照与全量验证记录 `docs/audit/phase-status.md`（规划书 §五 逐阶段对照 + 全量门禁证据 + 基准 5.59% realtime）。

## [0.5.0] - 2026-08

### Added
- **Phase 3 批次一：模块对拍推进三模块双绿（规划书 §五）**：Compressor（13 GWT，sidechain 单声道和派生驱动语义 §4.5）/ BassEnhancer（15 GWT，lowBoostDb 与内部滤波器 48k 设计率事实标准 §4.3）/ MidSide（11 GWT）——规格 + 12 组冻结向量（TS 导出，既有 22 夹具逐字节不变）+ hse-core 移植（f32/f64 量化落点逐位对齐；Math.sign 保号零、Biquad 级联 f64 不落 f32 等 5 项 TS 浮点事实显式复刻）；对拍门禁 11→**23 case 全 PASS、maxAbsDiff=0.000e0**。
- **服务链扩展为六模块引擎子链**：midSide → biquad → compressor → reverbSimple → bassEnhancer → limiter（对齐全链相对顺序 3/4/6/13/14/21）；控制面 setParams 可识别键同步扩展 midSide/compressor/bassEnhancer（向后兼容变更）；`specs/service/control-plane.md` §一/§5.6/§八 同步修订。
- **推流协议落地（规划书 Phase 3 第 3 项，契约 `specs/service/push-stream.md`）**：openSession/closeSession + 同端口二进制 PCM 帧（12 字节帧头 + 交错 f32 立体声载荷）+ 会话表（u32 id 单调不复用、耗尽 -32000）+ 混后处理（回环先、会话按 id 升序累加）+ 背压（drop-oldest、逐旧块 xrunsIn 计数、100ms 限频 event.xrun）+ 断线自动清理会话；新增 20 项测试（会话生命周期/背压/混合次序/文本-二进制分流正交/真实 WS 端到端），全程假后端零出声。
- 真机出声冒烟测试默认跳过（`HSE_ALLOW_REAL_AUDIO=1` 显式启用）——`cargo test` 全量跑不再出声。

## [0.4.0] - 2026-08

### Added
- **空间音频（内联级，`src/spatial/`）**（MINOR）：引擎主链第 22 级（Limiter 之后），纯 TS 参考实现——解析 HRTF（球头模型 Woodworth ITD/ILD，72×14 网格 256 抽头，球谐 L=3 插值）→ 每虚拟扬声器双耳分区卷积（512 样本分区，time/partitioned 双卷积模式）→ 房间模拟（镜像源 0-3 阶 + 8 线 Hadamard FDN，7 预设）→ 距离衰减 3 模型/空气吸收/多普勒/声源大小/遮挡（全部可选特性中性值逐位直通）。4 模式：instant 一键空间化 / headLocked 头锁定环绕（布局预设 stereo/5.1/5.1.4/7.1.4/自定义）/ world 世界漫游（听者+声源轨迹）/ stage 舞台影院（4 场景预设）。参数经 `HyperSoundEngineParams.spatial`（默认 mode:'off'=逐位旁路）与 ShareCodec 分享串编解码（整体 JSON 块 + 深度清洗防原型污染，旧分享串解码得默认 off 向后兼容）；配置签名门控防无关参数触发后端状态清零，off→on 先复位后端防旧音频回放；延迟 512 样本经 getLatencySamples() 上报。随附 8 个测试文件 104 用例（位精确旁路/分块不变性/闭式期望输出/物理断言）。此实现作为 Rust hrtf-core（规划书 §3.2 契约）的对拍 ground truth。
- **BassEnhancer 低音下潜 `lowBoostDb`**（MINOR）：低通提取的低频带按 `(10^(lowBoostDb/20)−1)` 真实混回输出，补足谐波路径只提供心理声学感知、无真实低频能量的短板（low-shelf 语义）。参数可选、默认 0=关闭（输出与既有行为逐位一致），越界钳制 -6..12，旧参数快照缺字段按 0 防御（NaN 防护）；ShareCodec 编解码白名单同步（旧分享串解码缺省 0，向后兼容）；API_SPEC 模块 8 与测试同步更新。
- **算法参考文档 `docs/ALGORITHMS.md`**：18 节算法原理速查（RBJ biquad/分区卷积/lookahead 限幅/BS.1770/虚拟低频/相位声码器/YIN 等）+ 三附录（实时性能预算、测试策略、许可合规清单），各节标注对应 `src/dsp/` 实现；README 文档索引同步。
- `test/stretch-signalsmith.test.ts`：signalsmith 可选路径测试——注入缝端到端驱动适配器胶水（分块记账/交织无损/防御回退）+ `skipIf` 门控的同步类接口 DSP 端到端组。实测结论：官方 npm 包（default 导出的 AudioWorklet 工厂）与 `isSignalsmithAvailable` 的同步类接口探测不匹配，适配路径当前不可达、恒走自研相位声码器，该事实由测试固化。

### Changed
- **分享串 v2 紧凑格式（HSE2，`SHARE_CODEC_VERSION`=2）**：载荷只存与默认参数的差异项（sampleRate 强制携带），传输改用 Crockford Base32（去 I/L/O/U 易混字符、大小写不敏感、容忍空白/连字符噪声）并按 5 字符分组；典型分享串从 ~2000 字符量级降至 64–900 字符。**v1 旧串持续可导入**（迁移语义，双向兼容）；新增传输/版本正交解析、Crockford 容错与长度上界回归测试；`docs/VERSIONING.md` 分享串版本说明同步。
- **调音室分享页重做**：分享串随参数自动刷新、字符数/格式提示、导入错误行内展示、串盒聚焦全选；theme 新增 `errorColor` 令牌。
- **移除均衡器页局部 JSON 导入导出**：完整参数分享统一走调音器页「分享串」（v2 兼容旧串，旧 EQ JSON 局部导入入口一并下线）。
- **响度归一化双时间常数平滑**：`externalGainDb` 手动增益分支平滑时间常数 3s→80ms（拖动音量即时跟随、无 zipper），实时 AGC 分支保持 3s 防抽吸语义不变。
- **宿主 setParams 去重**：`HyperSoundEngineHost.setParams` 与上次参数逐字段一致时跳过整链系数重配与 worklet `postMessage`（React 重复渲染/拖拽静止帧零开销）；IR（Float32Array）以引用身份参与指纹，不做逐样本序列化；`dispose` 后指纹复位。
- 移除 optionalDependencies 中从未被引擎代码引用的 `meyda`（特征提取均为自研实现）。
- vitest 配置显式排除本地不入库目录（.gitignore 中的归档/草稿），其中的测试文件不再被主套件扫描。

### Fixed
- **服务控制面 configure 校验对齐规格（GWT-CP-06/08）**：校验顺序改为相位(-32001)→结构(-32602)→后端枚举(-32000)，非 null renderDeviceId 必须命中当前渲染端点枚举；fake 后端补 2 条回归用例。**Phase 2 同设备旧试点链验收脚本 14/14 PASS**（记录见 `docs/audit/service-phase2-acceptance.md`）；原出口要求的独立设备路由、正式播放器与 VB-CABLE 路径当时尚未验收。
- **旁通→重新启用爆音修复**：级从 disabled→enabled 时清空对应模块流状态（延迟线/全通链/卷积缓冲/包络），避免旁通窗口积压的旧音频被回放（pop/串音）；覆盖 Pre-EQ/Deesser/Compressor/NightMode/Delay/Chorus/Flanger/Phaser/Tremolo/混响三路/BassEnhancer/LoudnessComp/IEQ/DynamicEq/Limiter。
- **consumeMidiQueue 稳态零分配**：MIDI 平滑 alpha 缓存 Map 提为实例字段复用（clear 复用不分配），收敛循环去除逐绑定闭包分配，兑现引擎文件头"process() 内零分配"承诺。

## [0.3.0] - 2026-08

### Changed
- **命名规范审计与整改**（规则见 `docs/VERSIONING.md` 公开标识符命名分层）：泛词类加 `Hse` 前缀并同步文件名——`AudioBus`→`HseAudioBus`、`Stretch`/`StretchParams`→`HseStretch`/`HseStretchParams`、`HearingTest`→`HseHearingTest`、`SeparationQueue`→`HseSeparationQueue`、`AudioEffectsProcessor`→`HseAudioEffectsProcessor`；DSP 行业域名类（Biquad/Compressor/Convolver 等）与工具函数（createEngine/encodeWav 等）按规则保留原名。
- **版本谱系重置**：废止旧 WaveForge v1/v2/v3 引擎谱系描述，现引擎定名 **HyperSoundEngine v1**（版本策略见 `docs/VERSIONING.md`）。
- 适配层 `attachV3Engine.ts` 重命名为 `attachEngine.ts`；导出 `attachV3Engine/detachV3Engine/getV3Bridge/isV3Attached/setV3SystemVolume/exportV3Wav/V3GraphHandle` 相应改为 `attachEngine/detachEngine/getBridge/isAttached/setSystemVolume/exportWav/EngineGraphHandle`。
- 调音室 UI 移除引擎版本切换入口（`engineVersion`/`onSwitchEngine` props 与 v1/v2/v3 切换器）。
- 命名去版本化：`v3HearingPlay`→`hseHearingPlay`、存储键 `hypersound:v3-*`/`waveforge:v3-params`→`hse-*`、worklet URL `v3-worklet*`→`hse-worklet*`、CSS 动画 `v3-*`→`hse-*`、WAV 导出文件名前缀 `waveforge-v3-`→`waveforge-hse-`。

### Added
- **Rust 支线试点三模块双绿（规划书 Phase 1）**：`hse-core` 逐行移植 `biquad` / `Limiter` / `ReverbSimple` 并全部通过冻结向量对拍——`cargo run -p hse-parity` **11/11 用例 PASS，全程 maxAbsDiff=0.000e0**（与 TS 基线逐位一致，远优于 1e-6 容差）。关键移植纪律：TS Number(f64) 中间量全 f64 复刻、Float32Array 落点精确区分 f32（含 limiter 队列峰值/真峰值插值系数、reverb combStore 状态）、JS `Math.round` 半值向上与 `min/max` NaN 语义显式复刻。
- **biquad.case4 多采样率向量**（MINOR 追加）：44100Hz / blockSize=441 / highshelf +3dB@8kHz Q0.707，补齐 shelf 类型与多采样率覆盖；冻结向量现共 **11 组**。
- **criterion 基准雏形 `benches/`（成员 crate hse-benches）**：parity_biquad（含 128/256/512 块长矩阵）/ parity_limiter（真峰值开关对照）/ parity_reverb_simple，口径对齐 TS benchmark（48kHz / 立体声 / 128 帧），确定性合成激励。
- CI 新增 `rust` job：`cargo test --workspace` + 对拍门禁 `cargo run -p hse-parity`（specs/ 冻结向量全部 PASS 才算过，双绿门禁 Rust 半边正式接入）。
- **双支线规格基建（规划书 Phase 0）**：新增共享规格目录 `specs/` —— 总纲与向量格式契约 `specs/README.md`、用例元数据 Schema `specs/schema/vector-case.schema.json`（draft-07）、试点模块规格 `specs/dsp/biquad.md` / `limiter.md` / `reverb-simple.md`（GWT 条款 + 参数 clamp 表 + 边界条件）。
- **10 组冻结对拍向量**（`specs/dsp/vectors/`：biquad×3 / limiter×4 / reverb-simple×3，JSON 元数据 + 小端四段 f32 夹具）与导出工具 `scripts/export-vectors.mjs`（优先 Node 原生 type-stripping 加载 TS 模块，esbuild 打包兜底；重跑逐字节比对，不一致拒写——机制性冻结守卫）。
- TS 侧对拍门禁测试 `test/spec-vectors.test.ts`（21 用例：向量目录缺失/为空显式失败，元数据契约逐一校验）及配套最小 Node 内置类型声明 `test/node-builtin-types.d.ts`。
- **Rust 支线骨架 `HyperSoundEngineRust/`**：Cargo workspace（edition 2021，license CC-BY-NC-ND-4.0）+ `crates/hse-core`（`Stage` trait 与 `StageChain`，语义对齐 `ProcessingStage`/`StereoProcessor` 与实时安全铁律）+ `crates/hse-parity` 对拍 harness（自动定位 `specs/dsp/vectors`，分块重放 + 统一容差比对；直通假实现期数值 FAIL 属预期，待 Phase 1 真实模块按规格落地后转绿）+ `hse-wasapi` / `hse-napi` / `hse-service` 占位说明。
- 版本策略文档 `docs/VERSIONING.md`（生成代号 ↔ semver 映射、bump 规则、向量纪律、命名规范）。
- `npm run benchmark:scenes`：接入场景化基准脚本（卷积/FDN 混响、DynamicEq；原 `scripts/benchmark-optimized.mjs` 无人引用，本次纳入 npm scripts）。

### Removed
- **移除引擎侧 soundtouchjs（LGPL）全部相关物**：该可选路径备而未用（`createStretchLgplAdapter` 全库零调用方，实际变速变调走自研 `HseStretch`）。删除 `optionalDependencies` 条目、`src/dsp/StretchLgplAdapter.ts`、`src/dsp/soundtouchjs.d.ts`、`test/stretchlgpl.test.ts`、`vendor/soundtouchjs/` 原包副本，及 build 脚本 external 与各文档引用。**引擎包现零 LGPL 依赖**；WaveForge 宿主侧的 `@soundtouchjs/audio-worklet` 不受影响。
- 死文件审计（入口可达性分析 54/55 全可达）：删除 `.hse-bench/` 实验脚手架（含优化前 `old/` 算法副本，结论已记录于本 CHANGELOG）；修正 integration 测试引用不存在的 `test/setup.ts` 的过时注释。

## [0.2.0] - 2026-08

### Added
- 独立引擎包 `hypersoundengine`（HyperSoundEngine v1 架构）。
- 核心/浏览器/Worklet 三个子路径导出。
- `AudioEngine` 接口新增 `getParams()` 与 `prepare(maxBlockSize)`。
- `ProcessingStage` 处理链抽象。
- 独立接入示例（Node 离线 / 浏览器 Host）。
- 接口文档（API / ARCHITECTURE / INTEGRATION）。
- WaveForge 适配层独立到 `adapters/waveforge/`。
- 性能基准脚本 `npm run benchmark` 与性能冒烟测试。
- GitHub Actions CI。
- 自定义处理阶段注册：`registerStage()` / `unregisterStage()` / `getStages()`。
- 差距分析文档 `docs/GAP_ANALYSIS.md`。
- Sidechain 输入：`process(inputs, outputs, sidechain?)` 第三参数，Compressor/Deesser 支持外部信号驱动包络（`sidechainEnabled`）。
- 参数调制矩阵：`dsp/modulation.ts`（LFO 四种波形 + Envelope Follower），路由到 masterGain / stereoWidth。
- 多通道 HseAudioBus：`dsp/HseAudioBus.ts` 非交错 N 通道缓冲抽象 + `processBus()` 便利入口（当前内核立体声，上下混兼容）。
- 调制类效果：`dsp/ModEffects.ts` —— Delay / Chorus / Flanger / Phaser / Tremolo 五个新处理阶段。
- 调制类效果 + Sidechain UI：效果页新增 延迟/合唱/镶边/移相/颤音 五卡片与参数调制矩阵卡片（`ui/modalsModulation.tsx`）；Compressor/Deesser 弹窗新增外部 Sidechain 开关。
- HseAudioBus 多通道工具：`create/fromInterleaved/toInterleaved/copyTo/fill/applyGain/mixFrom/extract/downmixToMono`。
- `processBus()` 新增 `perChannelPair` 模式：按立体声对逐对独立处理（子引擎池），支持 5.1/7.1 各通道独立 DSP。
- MIDI 事件接口 / MIDI Learn：`sendMidi(events)` 预分配环形队列 + `process()` 块头消费；`midiLearn(cc, target, opts?)` / `midiUnlearn(cc)` / `getMidiBindings()` / `getMidiDroppedCount()`；`AutomationTarget`（builtin masterGain/stereoWidth 或任意参数路径白名单）+ CC/Note → 范围映射 + 一阶平滑（防 zipper）。
- WAV 文件 I/O：`src/io/wav.ts` —— `encodeWav(channels, sampleRate, opts?)` / `decodeWav(buffer)`，支持 16-bit PCM 与 32-bit Float、多通道、严格 RIFF 校验（防注入）。
- UI MIDI Learn 面板：调音室新增 MIDI 页签（`ui/midiPanel.tsx`），参数路径下拉 + CC/Note 绑定 + 绑定表 + 测试发送；bridge 可选 `midi` 对象（HyperSoundEngine 后端探测填充）。
- **Convolver 非均匀分区卷积**：两级分区（短分区=partitionSize 默认 512 / 长分区默认 4096），长 IR 每块耗时降约 77%，延迟语义不变。
- **FFT 基-4 蝶形**：N=1024/2048 提速 32-34%（±j 免乘、stage 数减半，数值容差内一致）。
- **ReverbSimple 热循环内联**：14 次/样本方法调用消除，提速约 17%（逐位一致）。
- **Limiter 真峰值插值优化**：相位对称合并 + 全展开，提速约 18%（逐位一致）。
- **FDN 混响（算法创新）**：`dsp/FdnReverb.ts` —— 反馈延迟网络（Jot 1991），素数互质延迟线 + Householder 正交反馈矩阵（O(N) 快速应用，无条件稳定）；引擎 `reverb.mode='fdn'` 接线。
- **自适应动态均衡（算法创新）**：`dsp/DynamicEq.ts` —— 全通交叉分带（5 带，单位增益精确重建）+ 块级 RMS 分析 + 软拐点压缩 + attack/release 平滑；引擎 `dynamicEq` 参数组接线。

### Changed
- LICENSE 改为 CC BY-NC-ND 4.0。
- 引擎目录重构为 `HyperSoundEngine`。
- 核心类名统一为 `HyperSoundEngine` / `HyperSoundEngineHost`。
- Worklet 处理器名：`hypersoundengine`。
- DSP 内部去重：Deesser/BassEnhancer 共用 `dsp/biquad.ts`，HseStretch 共用 `dsp/fft.ts`。
- `EqChain.processStereo` 改为块处理，减少每样本方法调用开销。
- 宿主/Worklet 在创建引擎时预分配工作缓冲。

### Fixed
- 保持 331 测试全绿。

## [0.1.0] - 2026-08

