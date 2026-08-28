//! hse-service —— HyperSoundEngine 引擎服务进程（《原生化双支线与Windows音频接入规划书》§2.2）。
//!
//! 进程内三段数据面 + 一段控制面：
//!
//! ```text
//! 捕获线程(loopback.pull → 入环) → DSP 线程(出环 → planar 子链 → 入环) → 渲染线程(出环 → render.push)
//!                                        ↑ 参数热更换（rtrb 命令环整链换入）
//! 控制面线程(WebSocket JSON-RPC) ── 共享状态 / 原子统计 / 有界事件通道 ── 数据面
//! ```
//!
//! # 实时纪律的落点
//!
//! - **DSP 线程**：稳态零分配、零锁、零系统调用——只碰 rtrb 环、预分配缓冲、
//!   原子计数与 core 自旋提示；参数热更新经命令环换入整条新链（新链由控制面
//!   预构建并完成 prepare 预分配，DSP 线程块间仅做所有权移动）。
//! - **捕获/渲染线程**：I/O 线程，允许阻塞与系统调用；xrun 计数走原子累加，
//!   事件经**有界通道** try_send 转发（通道满即丢弃事件，绝不阻塞数据面；
//!   数据面绝不直接碰网络）。
//! - **控制面线程族**：不受实时纪律约束；相位机与运行态全部收拢在
//!   engine::EngineHandle 的互斥锁内，且锁从不跨越 join/sleep 等长等待。

pub mod backend;
pub mod cli;
pub mod dsp_chain;
pub mod engine;
pub mod fake_backend;
pub mod params;
pub mod pipeline;
pub mod rpc;
pub mod server;
pub mod state;