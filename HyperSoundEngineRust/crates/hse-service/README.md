# hse-service（占位目录，bin 未建）

**一句话职责**（源自《原生化双支线与Windows音频接入规划书》§2.1/§2.2）：引擎服务进程（独立常驻 bin）——线程编排 + rtrb 环形缓冲 + WebSocket JSON-RPC 控制面，独占音频设备对外提供处理服务。

**启动节奏：Phase 2+（按需启动）**。当前目录仅含本说明文档，没有 Cargo 清单；启动时再创建 bin crate 并加入 workspace 的 `members`。
