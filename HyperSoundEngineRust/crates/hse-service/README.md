# hse-service —— 引擎服务进程

**一句话职责**（《原生化双支线与Windows音频接入规划书》§2.2）：回环拦截端到端——WASAPI loopback 捕获 → rtrb 双环 → DSP 线程（试点子链 biquad → reverb-simple → limiter）→ rtrb → WASAPI 渲染；控制面为 localhost WebSocket JSON-RPC。

## 线程编排（§2.2 架构）

```text
捕获线程(loopback.pull → 入环) → DSP 线程(出环 → planar 子链 → 入环) → 渲染线程(出环 → render.push)
                                       ↑ 参数热更换：rtrb 命令环整链换入
控制面线程(WebSocket JSON-RPC) ── 共享状态 / 原子统计 / 有界事件通道 ── 数据面
```

- **DSP 线程**稳态零分配、零锁、零系统调用；双环容量按 blockSize 整数倍预分配；
- WASAPI 句柄不跨线程：开流在捕获/渲染线程内经 opener 完成，协商格式经握手通道回报引擎；
- 捕获 `pull` 为非阻塞尽力语义（返回实际帧数，0=暂无）：空轮询按 ~10ms 退避，避免忙转；渲染 `push` 由后端自行阻塞节流到实时速率；
- xrun 计数：欠供/溢出的服务侧计数 + 流对象内置 `xruns()` 增量聚合，`event.xrun` 通知经有界通道转发（数据面不碰网络，通道满丢通知不丢计数）。

## 控制面契约（JSON-RPC 2.0 over WebSocket）

默认 `ws://127.0.0.1:4780/`；`--port N` 参数或 `HSE_SERVICE_PORT` 环境变量可覆盖。
方法表：`listDevices` / `getState` / `configure{mode,renderDeviceId,sampleRate,blockSizeFrames}` / `start` / `stop` / `setParams{params}`；
错误码 -32700/-32600/-32601/-32602/-32000/-32001；事件通知 `event.phase`、`event.xrun`。
`setParams.params` 当前可识别键：`biquad{type,f0,q,gainDb}`、`reverbSimple{roomSize,damping,wet,dry,preDelayMs,width,type}`、
`limiter{enabled,thresholdDb,lookaheadMs,attackMs,releaseMs,truePeak}`；其余顶层键忽略并入 warnings。缺省值对齐 TS 支线
`createDefaultParams` 与各模块构造默认（biquad 未配置即直通）。

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
| `fake_backend.rs` | 内存假后端（测试支撑） |
| `dsp_chain.rs` | 试点子链装配与 planar/交错转换 |
| `params.rs` | setParams 快照解析（识别键/警告/TS 缺省） |
| `pipeline.rs` | 三条数据面线程与双环/命令环 |
| `engine.rs` | 相位机、会话生命周期、热更换、兜底停机 |
| `rpc.rs` | JSON-RPC 解析与方法分发表 |
| `server.rs` | WebSocket 服务、事件广播、监督轮询 |
| `cli.rs` + `bin/hse_cli.rs` | 最小调参客户端 |

## 测试

```powershell
cargo test -p hse-service           # 单测+集成测试全绿，不依赖真实音频设备
```