# ADR-0004：Windows 音频仅支持 WASAPI，不提供 ASIO 或 MIDI

- 状态：已接受
- 日期：2026-08-30
- 取代：ADR-0001 与 ADR-0002 中关于 ASIO 的方向；其余决策保持有效

## 背景

HyperSoundEngine 的 Windows 服务路径已经围绕 WASAPI shared/exclusive 普通 capture/render、shared-only loopback、虚拟音频缆直捕和独立渲染建立实现与自动化测试口径，真机组合路径仍需设备验收。继续保留 ASIO 作为可选路线会引入第二套设备枚举、生命周期、格式协商、许可和测试矩阵，但当前产品目标不需要该后端。

MIDI 事件、MIDI Learn 及其 UI/API/控制面描述也不属于当前项目范围。参数调制仍是引擎 DSP 能力，但其来源限定为内部 LFO 与 Envelope Follower，不依赖外部 MIDI 控制协议。

## 决策

1. Windows 音频设备 I/O 仅支持 WASAPI。回环拦截、虚拟音频缆直捕和推流会话混合后的最终渲染均走 `hse-wasapi`；捕获源与渲染端点可独立配置。
2. 不实现、不规划 ASIO 后端，不保留 `hse-asio` crate、feature flag、许可决策或 Phase 待办。
3. 不提供 MIDI 事件入口、MIDI Learn、MIDI 控制面方法、MIDI UI 或共享 MIDI 行为规格。
4. 保留 modulation matrix 的 LFO、Envelope Follower 及其到 `masterGain` / `stereoWidth` 的路由。
5. ADR-0001 与 ADR-0002 的历史正文保持原样；其中涉及 ASIO 的方向由本 ADR 取代。

## 后果

- Windows 音频实现、测试和运维口径统一为 WASAPI，减少后端分叉与许可负担。
- 推流协议仍是音频入口协议，不因设备后端收敛而改变；双入口与混后处理决策继续有效。
- 外部控制器或宿主若需要自动化，应在宿主侧转换为现有参数快照或控制面 `setParams`，项目本身不承担 MIDI 协议解析与绑定。
- 不再把 ASIO 或 MIDI 列入当前能力、路线、待办、UI、API、集成文档或服务规格。
