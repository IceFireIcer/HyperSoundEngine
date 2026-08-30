# Phase 2 真机部分验收记录 —— 同一默认设备上的旧试点链

> 日期：2026-08-29（北京时间）
> 对象：`HyperSoundEngineRust/crates/hse-service`（release 构建，含本次修复）+ `hse-wasapi`
> 契约依据：[`specs/service/control-plane.md`](../../specs/service/control-plane.md)（GWT-CP-01..19）、
> [规划书 §五 Phase 2](../../原生化双支线与Windows音频接入规划书.md)
> 环境：Windows 11 / Realtek Audio（唯一渲染端点，即系统默认）/ 48kHz / blockSizeFrames=256 / 控制面 ws://127.0.0.1:4780
> 验收方式：自动化客户端（Node 内置 WebSocket，脚本本地 `.scratch/`，不入库）对真实服务进程执行
> 控制面 GWT 序列；音频源为 PowerShell SoundPlayer 播放 48kHz 扫频 WAV 至默认渲染设备，
> 服务以 loopback 拦截同设备 → 试点子链（biquad → reverb-simple → limiter）→ 渲染回默认设备。

## 一、本验收脚本结果：14/14 PASS（不等于 Phase 2 出口通过）

| # | 验收项 | 结果 | 证据 |
|---|--------|------|------|
| 1 | GWT-CP-01 枚举结构完备（render/capture 数组、字段类型、默认端点唯一） | PASS | render=1（默认"扬声器 (Realtek(R) Audio)"）capture=1 |
| 2 | 前提：存在默认渲染端点（回环拦截拦截源） | PASS | 同上 |
| 3 | GWT-CP-03 getState 字段完备（phase/config/stats 四计数/lastParams） | PASS | phase=idle |
| 4 | GWT-CP-08 未知 renderDeviceId → -32000 且 config 不变 | PASS | code=-32000（**首轮 FAIL，已修复，见 §三**） |
| 5 | GWT-CP-07 结构非法（mode/capture、sampleRate=0）→ -32602 | PASS | 两例均 -32602 |
| 6 | GWT-CP-05 合法 configure → applied 原样回显 + getState.config 相等 | PASS | applied 四字段逐字段相等 |
| 7 | GWT-CP-09 启动全序：event.phase(idle→starting→running) 先于 started:true | PASS | 事件先于响应，序正确 |
| 8 | GWT-CP-16 setParams 未知顶层键/子键 → warnings 字典序 ["biquad.order","myPluginKey"] 且被忽略 | PASS | 恰为该二元素 |
| 9 | GWT-CP-17 快照整体替换（lastParams 不残留上一快照键） | PASS | 仅剩 limiter，thresholdDb=-2 |
| 10 | GWT-CP-06 非 idle 拒绝 configure（-32001） | PASS | running 态拒绝 |
| 11 | **端到端：真实音频流经旧试点链**——播放 8s 扫频期间 DSP 累计处理帧数增长 | PASS | ΔframesProcessed=258,816（≈5.4s @48k；差值系 SoundPlayer 供块抖动，见 §四） |
| 12 | GWT-CP-04 计数器单调不减（四计数器两次采样） | PASS | 全部 ≥ |
| 13 | xrun 上报机制正确：event.xrun 通知与 stats 计数器同源单调 | PASS | 会话内 198 条通知，totalIn/totalOut 与 getState 对账一致 |
| 14 | GWT-CP-13 停止全序：running→stopping→idle 事件 + stopped:true + config/lastParams 保留 | PASS | 全序正确 |

fake 后端单测/集成同步全绿（含本次新增 2 条 GWT-CP-06/08 用例）；`cargo test --workspace` 全绿。

## 二、真实设备信息（Phase 2 第 5 项对账）

- 本机未安装 VB-CABLE（`listDevices` 无 CABLE 端点）→ **虚拟缆直捕路径未验收**；
- 因只有一个渲染端点，本次回环拦截源 = 渲染出口 = 同一物理设备（存在同设备回授，
  由试点链干声增益 <1 与限幅器 -1dBFS 保证回路稳定，实测无自激）。

## 三、验收中发现并修复的缺陷

1. **configure 缺少渲染端点枚举校验 + 校验顺序不符规格**（GWT-CP-08/CP-06）：
   - 症状：未知 renderDeviceId 被 configure 接受（回显 applied），且结构校验先于相位守卫；
   - 修复：`engine.rs` configure 改为「相位(-32001) → 结构(-32602) → 后端枚举(-32000)」三级顺序，
     非 null renderDeviceId 必须命中 `factory.list_devices()` 的渲染枚举；
   - 回归：fake 后端补 2 条用例（未知设备拒绝且 config 不变；非 idle 拒绝优先于结构校验）。

## 四、出口判据对账（Phase 2，规划书 §五）

| 出口判据 | 状态 | 说明 |
|---|---|---|
| 任意播放器输出到指定设备 → 经引擎全链 → 真实设备出声 | ⏳ 未验收 | 本次只证明同一默认设备上的 loopback→旧试点链→同设备渲染；当前 1–21 级链、正式播放器、异设备输出均未做真机验收 |
| 控制面可热改参数 | ✅ | running 态 setParams 快照替换 + 热应用（fake 侧另有块边界生效测试） |
| 虚拟缆直捕路径 | ⏳ 待 VB-CABLE | 独立捕获/渲染选路已在代码和 fake 后端测试中落地；本机未安装 VB-CABLE，真机路径尚未验收 |

**xrun 数据的解读**：短窗实测 xrunsOut≈4.7s 当量——归因于 SoundPlayer（WaveOut 大缓冲）
供块极不平滑 + 同设备回授场景；事件通知（198 条）与计数器单调对账正确，证明 xrun 上报链路
（规划书 §六风险缓解项）按设计工作。该数据只描述本次测试拓扑，不构成正式播放器或异设备路径的性能结论。

## 五、5 分钟运行观测

见会话记录：60s 间隔采样 ×5，phase 恒 running、framesProcessed 持续增长、计数器单调；
数据以提交时的会话日志为准（本文件不嵌实时数据）。

## 六、复现

```bash
cd HyperSoundEngineRust && cargo build -q -p hse-service --release
./target/release/hse-service.exe --port 4780          # 终端 A
node .scratch/phase2-acceptance.mjs                   # 终端 B（脚本不入库，随会话提供）
```
