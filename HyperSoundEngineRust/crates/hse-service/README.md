# hse-service —— 引擎服务进程

> 外部项目应先阅读仓库根 [`docs/INTEGRATION.md`](../../../docs/INTEGRATION.md) 的 Rust service 章节，其中给出完整生命周期、JSON-RPC/PCM 示例、错误恢复和接入验收清单。本文件描述服务内部边界；协议事实标准仍是 [`specs/service/control-plane.md`](../../../specs/service/control-plane.md) 与 [`push-stream.md`](../../../specs/service/push-stream.md)。

**一句话职责**（《原生化双支线与Windows音频接入规划书》§2.2）：WASAPI loopback 或捕获端点直捕 + PCM 推流会话 → 混后处理 → rtrb 双环 → DSP 线程（`hse-core::EngineChainStage` 1–22 级；stage 22 可消费控制面预载 HRTF grid）→ WASAPI 独立渲染端点；控制面为 localhost WebSocket JSON-RPC。

## 线程编排（§2.2 架构 + push-stream 混后处理）

```text
┌ 捕获线程(loopback.pull) ─┐
│                          ├→ 逐样本求和(混后处理) → 入环 → DSP 线程(出环 → planar 1–22 级主链) → 出环 → 渲染线程
└ 控制面连接线程(二进制帧) ─┘         ↑ rtrb                          ↑ 参数热更换：rtrb 命令环整链换入
控制面线程(WebSocket：文本=JSON-RPC ｜ 二进制=推流帧，同端口按 opcode 分流)
       ── 共享状态 / 原子统计 / 有界事件通道 / 会话表 ── 数据面
```

- **DSP 线程**稳态零分配、零锁、零系统调用；双环容量按 blockSize 整数倍预分配；
- WASAPI 句柄不跨线程：开流在捕获/渲染线程内经 opener 完成，协商格式经握手通道回报引擎；
- 捕获线程通过后端 readiness 等待推进：WASAPI 使用事件句柄，fake 后端可由测试显式发放许可；等待窗口按块长与采样率推导并设 25ms 上限，以便停机和纯推流会话及时推进；渲染 `push` 由后端自行阻塞节流到实时速率；
- **混后处理**（ADR-0002）：捕获线程同时承担推流会话的混合前级——回环块（若有）先入基线，随后按 sessionId 升序把各会话出队块逐样本求和再入环；无活跃会话时走快路径，与纯回环行为逐字节一致；
- xrun 计数：欠供/溢出的服务侧计数 + 流对象内置 `xruns()` 增量聚合 + 会话环 drop-oldest 丢弃（每旧块 +1），`event.xrun` 通知经有界通道转发（数据面不碰网络，通道满丢通知不丢计数）。

## 控制面契约（JSON-RPC 2.0 over WebSocket）

默认 `ws://127.0.0.1:4780/`；`--port N` 参数或 `HSE_SERVICE_PORT` 环境变量可覆盖。
方法表（9 个）：`listDevices` / `getState` / `configure{mode,renderDeviceId?,captureDeviceId?,outputDeviceId?,shareMode?,sampleRate,blockSizeFrames}` / `loadHrtf{path}` / `start` / `stop` /
`setParams{params}` / `openSession{sampleRate,channels,format}`（未配置图采样率 -32001；协商违规 -32602；id 耗尽 -32000）/
`closeSession{sessionId}`（未知或重复 close → -32602）。
错误码 -32700/-32600/-32601/-32602/-32000/-32001；事件通知 `event.phase`、`event.xrun`。
`shareMode` 可选 `shared|exclusive`，缺省 shared；省略时 applied/config 不新增该键。exclusive 仅支持普通 capture + render，loopback+exclusive 在 configure 返回 -32602；设备不支持目标格式或独占打开失败则 start 返回 -32000，绝不回退 shared。
`loadHrtf` 仅在 idle 且已 configure 时接受本机绝对 `.sofa` 普通文件路径，按配置采样率在控制线程调用 `hrtf-core::load_sofa_file` 构建网格。解析失败不替换旧网格；改配不同采样率会清除旧网格。`start` 与运行态 `setParams` 使用预载网格预建完整链，DSP 线程只在块边界换链，不读取或解析文件。无网格时 `spatial.mode != "off"` 明确报错。
**推流二进制帧**（同连接，音频只进不出）：`sessionId u32 LE + seq u64 LE + 交错 f32LE 立体声载荷`（载荷为 8 的倍数、≥8、≤1 MiB），
违规帧与未知会话帧静默丢弃；每会话有界环（容量按帧计）drop-oldest 背压；连接断开自动关闭其打开的全部会话。
`setParams.params` 当前可识别兼容 wire 键（投影到完整 `EngineChainStage`，子键域见 `specs/service/control-plane.md` §5.6）：
`midSide{width,voiceBalance}`、`biquad{type,f0,q,gainDb}`（键缺省=级不装配）、
`eqChain{bands,bandCount,qCompensation}`（键缺省=级不装配）、`deesser{enabled,centerHz,q,thresholdDb,ratio,attackMs,releaseMs,splitBand,mix,sidechainEnabled}`、
`compressor{…}`、`modEffects{delay,chorus,flanger,phaser,tremolo}`、`reverbSimple{…}`、
`reverbRoute`（simple 缺省 | fdn | convolver | off）、`fdnReverb{roomSize,damping,wet,dry,preDelayMs,width,type,lines}`、
`convolver{irRecipe(delta|expNoise 配方),mix,preDelayMs}`、`bassEnhancer{…}`、
`loudnessComp{mode,preset,bands,volumePercent,maxBoostDb,smoothingSeconds}`（缺省 custom+空带=逐位直通）、
`dynamicEq{enabled,strength,thresholdDb,ratio,attackMs,releaseMs,bands[5]}`（crossover 由服务侧固定注入 [200,800,2500,8000]）、
`modMatrix{routes,lfo,envelope}`（控制率，缺省无路由=恒等）、`limiter{…}`；
其余顶层键忽略并入 warnings。缺省值对齐 TS 支线 `createDefaultParams` 与各模块构造默认；
全键缺省 + reverbSimple 全干 + limiter 禁用 ⇒ 整链逐位直通（回归锚）。

## 用法

```powershell
cargo run -p hse-service            # 启动服务（默认端口 4780）
cargo run -p hse-cli -- list-devices [--full]
cargo run -p hse-cli -- configure [--mode loopback|capture] [--share-mode shared|exclusive] [--input-device <id>] [--output-device <id>] [--rate 48000] [--block 256]
cargo run -p hse-cli -- load-hrtf "C:\\hrtf\\subject.sofa"
cargo run -p hse-cli -- start
cargo run -p hse-cli -- get-state
cargo run -p hse-cli -- set-params params.json   # {"biquad":{...},"reverbSimple":{...},"limiter":{...}}
cargo run -p hse-cli -- stop
```

`hse-cli` 支持 `--url ws://host:port` 全局参数与 `HSE_SERVICE_URL` 环境变量。

## Phase 4 真机底层 WASAPI 诊断工具

`hse-real-audio-check` 默认只枚举设备并校验配置，不打开音频流。真实脉冲测量必须同时提供 `measure --run` 与环境变量 `HSE_ALLOW_REAL_AUDIO=1`；若 capture/render 之间的物理连接无法由同端点 loopback 或 VB-CABLE 友好名证明，报告返回 `external-loopback-required`，不会生成延迟数字。该工具直接连接 WASAPI capture/render，明确不经过 `ServiceEngineChain`、输入环或输出环；完整服务路径由 `pipeline_fake` 的纯内存 readiness 许可门禁验证。完整步骤、退出码与 JSON 字段见 `docs/audit/phase4-real-audio-acceptance.md`。

```powershell
cargo run -p hse-service --bin hse-real-audio-check -- inspect --pretty
cargo run -p hse-service --bin hse-real-audio-check -- measure --source loopback --input-device "<render-id>" --output-device "<same-render-id>" --rate 48000 --block 128 --pulses 12 --frames 67200 --pretty
node ../scripts/phase4-dual-push.mjs --rate 48000 --block 128 --frames 48000 --pretty
```

以上三条均为 dry-run；真实运行命令不在默认示例中隐式启用。

## 模块地图（src/）

| 模块 | 职责 |
|------|------|
| `backend.rs` | CaptureSource/RenderSink/opener 抽象 + WasapiFactory 生产适配 |
| `fake_backend.rs` | 内存假后端（测试支撑；含静音回环捕获模式） |
| `dsp_chain.rs` | 包装并驱动 `hse-core::EngineChainStage` 1–22 级主链，以及 planar/交错转换 |
| `params.rs` | setParams 快照解析（识别键/警告/TS 缺省/类型分层校验） |
| `pipeline.rs` | 三条数据面线程与双环/命令环（捕获线程含混后处理前级） |
| `sessions.rs` | 推流会话表：二进制帧解析、每会话有界环（drop-oldest）、混合前级 |
| `engine.rs` | 相位机、本地 SOFA 控制路径加载、openSession/closeSession、整链热更换、兜底停机 |
| `rpc.rs` | JSON-RPC 解析与方法分发表（九方法） |
| `server.rs` | WebSocket 服务（文本/二进制分流）、事件广播、断线清理会话、监督轮询 |
| `cli.rs` + `bin/hse_cli.rs` | 最小调参客户端 |

## 测试

```powershell
cargo test -p hse-service           # 单测+集成测试全绿，不依赖真实音频设备
```
