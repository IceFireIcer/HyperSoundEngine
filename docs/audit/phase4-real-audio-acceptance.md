# Phase 4 真机音频验收作业书

> `hse-real-audio-check` 是底层 WASAPI 诊断：直接连接 capture/render，不经过 `ServiceEngineChain`、输入环、输出环或服务工作线程。报告 `diagnosticScope="low-level-wasapi"`，并将结果放在 `measurements.lowLevelWasapi`；`measurements.servicePipeline.status="not-measured"` 不得解释为完整服务端到端结果。完整服务路径由纯内存 readiness 许可门禁覆盖。
>
> 工具只提供验收能力，本轮未运行真实音频。所有真实开流命令必须由操作者显式设置 `HSE_ALLOW_REAL_AUDIO=1`，并同时传入 `--run`。缺任一项均为 dry-run。

## 1. 工具与判定

从 `HyperSoundEngineRust/` 运行：

```powershell
cargo run -p hse-service --bin hse-real-audio-check -- inspect --pretty
```

`inspect` 只枚举 WASAPI 端点、检测友好名中的 VB-CABLE，并校验默认配置。JSON 包含：

- `config.shareMode`、输入/输出设备 ID 与名称、`sampleRate`、`blockSizeFrames`、固定总帧数和脉冲数；
- `vbCable.detected` 与命中的端点；
- `topology`：`wasapi-loopback`、`vb-cable`、`external-loopback-confirmed` 或 `external-loopback-required`；
- 真机结果的 `latency.frames|milliseconds.p50/p95/max`、`xruns.capture/render/total`；
- `diagnosticScope="low-level-wasapi"`、`path` 与 `excludedPath` 明确测量边界；
- `measurements.lowLevelWasapi` 保存底层延迟、xrun 与进程性能，`measurements.servicePipeline` 明确为独立自动门禁且此工具不测；
- `performance.cpuPercent`（整个底层诊断进程的 kernel+user CPU / 墙钟）、`framesPerSecond`、`realtimeFactor`。

退出码：0 为 dry-run 配置有效或真机通过；2 为参数错误；3 为门控缺失、外部闭环未确认、相关不足或后端错误。`latency:null` 表示没有有效相关结果，不能按 0ms 解读。

工具发送幅度 0.8 的双声道脉冲，以固定脉冲数和固定帧数终止。事件等待上限由帧预算和块长推导，不以“运行 N 秒”作为结束条件。共享模式目标为 p95 ≤30ms；独占 capture/render 的目标为 p95 ≤10ms。WASAPI loopback 本身不支持独占。

## 2. 正式播放器与 VB-CABLE

1. 安装 VB-CABLE 后关闭并重开播放器，先执行 `inspect --pretty`，确认同时看到渲染端 `CABLE Input` 与捕获端 `CABLE Output`，记录完整设备 ID。
2. 将正式播放器（浏览器、foobar2000 等）的输出设备设为 `CABLE Input`，播放可确认的非静音素材。
3. 启动服务并将输入配置为 `capture/CABLE Output`、输出配置为真实扬声器或耳机。不要把输出再指回 `CABLE Input`，否则会构成反馈环。
4. 用 `hse-cli get-state` 记录配置、`framesProcessed` 和 xrun；热改一个可听参数，再确认真实输出持续非零且无异常停机。
5. 停止服务后恢复播放器输出设备。该步骤证明正式播放器经 VB-CABLE 进入服务全链，但不把听感冒充延迟测量。

命令模板：

```powershell
cargo run -p hse-service
cargo run -p hse-cli -- configure --mode capture --share-mode shared --input-device "<CABLE Output id>" --output-device "<headphone id>" --rate 48000 --block 128
cargo run -p hse-cli -- start
cargo run -p hse-cli -- get-state
cargo run -p hse-cli -- stop
```

## 3. 脉冲相关延迟

### 3.1 同一渲染端点的共享 loopback

先 dry-run，核对 JSON 中输入和输出 ID 相同且 `topology="wasapi-loopback"`：

```powershell
cargo run -p hse-service --bin hse-real-audio-check -- measure --source loopback --share-mode shared --input-device "<render id>" --output-device "<same render id>" --rate 48000 --block 128 --pulses 12 --pulse-interval 4800 --frames 67200 --max-latency 9600 --pretty
```

确认会真实出声后，操作者才可在当前终端显式执行：

```powershell
$env:HSE_ALLOW_REAL_AUDIO="1"
cargo run -p hse-service --bin hse-real-audio-check -- measure --run --source loopback --share-mode shared --input-device "<render id>" --output-device "<same render id>" --rate 48000 --block 128 --pulses 12 --pulse-interval 4800 --frames 67200 --max-latency 9600 --pretty
Remove-Item Env:HSE_ALLOW_REAL_AUDIO
```

保存 stdout JSON。只有 `status="pass"` 且 `pulsesCorrelated=12` 才可使用延迟分位数。

### 3.2 VB-CABLE 或物理闭环

VB-CABLE 自动可识别的配对为输出 `CABLE Input`、捕获 `CABLE Output`：

```powershell
$env:HSE_ALLOW_REAL_AUDIO="1"
cargo run -p hse-service --bin hse-real-audio-check -- measure --run --source capture --share-mode shared --input-device "<CABLE Output id>" --output-device "<CABLE Input id>" --rate 48000 --block 128 --pulses 12 --pulse-interval 4800 --frames 67200 --max-latency 9600 --pretty
Remove-Item Env:HSE_ALLOW_REAL_AUDIO
```

普通声卡输出到线路输入、混音台回送等拓扑无法由枚举自动证明。未加确认参数时工具必须返回 `status="external-loopback-required"`，保持 `latency:null` 且不开流。完成物理接线并检查电平后，显式增加 `--external-loopback-confirmed` 才能测量。

独占模式仅用于普通 capture + render，并要求两端原生支持目标立体声 f32 格式：

```powershell
$env:HSE_ALLOW_REAL_AUDIO="1"
cargo run -p hse-service --bin hse-real-audio-check -- measure --run --source capture --share-mode exclusive --input-device "<physical capture id>" --output-device "<render id>" --external-loopback-confirmed --rate 48000 --block 128 --pulses 12 --pulse-interval 4800 --frames 67200 --max-latency 4800 --pretty
Remove-Item Env:HSE_ALLOW_REAL_AUDIO
```

## 4. 双推流加非零回环

该项验收服务的三源混合：一路真实非零 capture/loopback，加两个 WebSocket PCM 会话。推流客户端默认 dry-run，并按每会话固定帧数终止。

1. 用正式播放器向已配置的 loopback 或 `CABLE Output` 持续提供非零音频。
2. 启动 `hse-service`，记录开始前 `get-state`；确认 `framesProcessed` 正增长且真实输出非零。
3. 先运行脚本 dry-run，核对 URL、采样率、块长和每会话帧数。
4. 显式门控后发送两个会话。脚本建立两条独立 WebSocket，每条连接各自 `openSession`，每会话发送精确 48000 帧，分别为 997Hz/1499Hz 低幅正弦；完成后要求两条会话各自 `ingestedFrames=48000`、`consumedFrames>=48000`、`queuedFrames=0`，且 `xrunsIn=xrunsOut=0`，再关闭会话。不按时间停止。
5. 脚本报告固定包含 `outputVerification.status="external-output-required"`：控制协议不回传渲染 PCM，无法自行验证频率或物理输出。必须另取输出录音或外部分析，确认同时存在播放器内容、997Hz 与 1499Hz；否则不得签署“三源混合通过”。

```powershell
node ../scripts/phase4-dual-push.mjs --url ws://127.0.0.1:4780/ --rate 48000 --block 128 --frames 48000 --pretty
$env:HSE_ALLOW_REAL_AUDIO="1"
node ../scripts/phase4-dual-push.mjs --url ws://127.0.0.1:4780/ --rate 48000 --block 128 --frames 48000 --pretty --run
Remove-Item Env:HSE_ALLOW_REAL_AUDIO
```

该脚本只发送推流，不启动或配置服务，也不声称能独自证明非零回环。若没有播放器/物理源和输出观测证据，只能记录“双推流发送完成”，不能记录“双推流+非零回环通过”。

## 5. 留档要求

每种模式保存：工具版本、完整命令、JSON 报告、设备 ID/名称、声卡驱动与系统信息。共享/独占分别判定，不用共享结果替代独占。任何 `external-loopback-required`、`insufficient-correlation`、`latency:null`、格式协商错误或 xrun 非零都必须原样留档，不补写估算值。
