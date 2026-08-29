# hse-service —— 引擎服务进程

**一句话职责**（《原生化双支线与Windows音频接入规划书》§2.2）：回环拦截端到端——WASAPI loopback 捕获 → rtrb 双环 → DSP 线程（引擎子链全序装配 midSide → biquad → eqChain → deesser → compressor → modEffects → reverb 三路路由(simple|fdn|convolver|off) → bassEnhancer → loudnessComp → dynamicEq → modMatrix(控制率) → limiter）→ rtrb → WASAPI 渲染；控制面为 localhost WebSocket JSON-RPC；Phase 3 起同一连接上的二进制帧承载**推流入口**（specs/service/push-stream.md）。

## 线程编排（§2.2 架构 + push-stream 混后处理）

```text
┌ 捕获线程(loopback.pull) ─┐
│                          ├→ 逐样本求和(混后处理) → 入环 → DSP 线程(出环 → planar 子链) → 出环 → 渲染线程
└ 控制面连接线程(二进制帧) ─┘         ↑ rtrb                          ↑ 参数热更换：rtrb 命令环整链换入
控制面线程(WebSocket：文本=JSON-RPC ｜ 二进制=推流帧，同端口按 opcode 分流)
       ── 共享状态 / 原子统计 / 有界事件通道 / 会话表 ── 数据面
```

- **DSP 线程**稳态零分配、零锁、零系统调用；双环容量按 blockSize 整数倍预分配；
- WASAPI 句柄不跨线程：开流在捕获/渲染线程内经 opener 完成，协商格式经握手通道回报引擎；
- 捕获 `pull` 为非阻塞尽力语义（返回实际帧数，0=暂无）：空轮询按 ~10ms 退避，避免忙转；渲染 `push` 由后端自行阻塞节流到实时速率；
- **混后处理**（ADR-0002）：捕获线程同时承担推流会话的混合前级——回环块（若有）先入基线，随后按 sessionId 升序把各会话出队块逐样本求和再入环；无活跃会话时走快路径，与纯回环行为逐字节一致；
- xrun 计数：欠供/溢出的服务侧计数 + 流对象内置 `xruns()` 增量聚合 + 会话环 drop-oldest 丢弃（每旧块 +1），`event.xrun` 通知经有界通道转发（数据面不碰网络，通道满丢通知不丢计数）。

## 控制面契约（JSON-RPC 2.0 over WebSocket）

默认 `ws://127.0.0.1:4780/`；`--port N` 参数或 `HSE_SERVICE_PORT` 环境变量可覆盖。
方法表（8 个）：`listDevices` / `getState` / `configure{mode,renderDeviceId,sampleRate,blockSizeFrames}` / `start` / `stop` /
`setParams{params}` / `openSession{sampleRate,channels,format}`（未配置图采样率 -32001；协商违规 -32602；id 耗尽 -32000）/
`closeSession{sessionId}`（未知或重复 close → -32602）。
错误码 -32700/-32600/-32601/-32602/-32000/-32001；事件通知 `event.phase`、`event.xrun`。
**推流二进制帧**（同连接，音频只进不出）：`sessionId u32 LE + seq u64 LE + 交错 f32LE 立体声载荷`（载荷为 8 的倍数、≥8、≤1 MiB），
违规帧与未知会话帧静默丢弃；每会话有界环（容量按帧计）drop-oldest 背压；连接断开自动关闭其打开的全部会话。
`setParams.params` 当前可识别键（Phase 3 全序链，完整子键域见 `specs/service/control-plane.md` §5.6）：
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
cargo run -p hse-cli -- configure [--device <id>] [--rate 48000] [--block 256]
cargo run -p hse-cli -- start
cargo run -p hse-cli -- get-state
cargo run -p hse-cli -- set-params params.json   # {"biquad":{...},"reverbSimple":{...},"limiter":{...}}
cargo run -p hse-cli -- stop
```

`hse-cli` 支持 `--url ws://host:port` 全局参数与 `HSE_SERVICE_URL` 环境变量。

## 模块地图（src/）

| 模块 | 职责 |
|------|------|
| `backend.rs` | CaptureSource/RenderSink/opener 抽象 + WasapiFactory 生产适配 |
| `fake_backend.rs` | 内存假后端（测试支撑；含静音回环捕获模式） |
| `dsp_chain.rs` | 引擎子链全序装配（13 级 + reverb 三路路由 + 控制率 modMatrix）与 planar/交错转换 |
| `params.rs` | setParams 快照解析（识别键/警告/TS 缺省/类型分层校验） |
| `pipeline.rs` | 三条数据面线程与双环/命令环（捕获线程含混后处理前级） |
| `sessions.rs` | 推流会话表：二进制帧解析、每会话有界环（drop-oldest）、混合前级 |
| `engine.rs` | 相位机、openSession/closeSession、热更换、兜底停机 |
| `rpc.rs` | JSON-RPC 解析与方法分发表（八方法） |
| `server.rs` | WebSocket 服务（文本/二进制分流）、事件广播、断线清理会话、监督轮询 |
| `cli.rs` + `bin/hse_cli.rs` | 最小调参客户端 |

## 测试

```powershell
cargo test -p hse-service           # 单测+集成测试全绿，不依赖真实音频设备
```