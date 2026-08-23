# hse-wasapi（占位目录，crate 未建）

**一句话职责**（源自《原生化双支线与Windows音频接入规划书》§2.1）：Windows WASAPI 音频后端——事件驱动共享模式渲染 + loopback / 虚拟缆捕获，封装 `wasapi` crate。

**启动节奏：Phase 2+（按需启动）**。当前目录仅含本说明文档，没有 Cargo 清单；启动时再创建 crate 并加入 workspace 的 `members`。
