//! 服务共享状态：相位机、配置快照、原子统计与数据面→控制面事件定义。
//!
//! 统计计数由数据面线程原子累加、控制面读取；事件只承载"增量"信息
//! （相位迁移、xrun 增量），总量以共享计数器为准，序列化在 server 层完成。

use std::sync::atomic::AtomicU64;
use std::time::Instant;

use serde_json::{json, Value};

/// 引擎相位（控制面契约状态机取值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Starting,
    Running,
    Stopping,
}

impl Phase {
    /// 契约字符串形式。
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Starting => "starting",
            Phase::Running => "running",
            Phase::Stopping => "stopping",
        }
    }
}

/// configure 选择的捕获源。
#[derive(Debug, Clone)]
pub enum CaptureConfig {
    /// 捕获渲染端点的 loopback 流；字段名保留既有协议语义。
    Loopback { render_device_id: Option<String> },
    /// 直接打开捕获端点（例如 CABLE Output）。
    Capture { capture_device_id: Option<String> },
}

/// configure 成功后的生效配置快照。
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub capture: CaptureConfig,
    pub output_device_id: Option<String>,
    pub output_device_id_explicit: bool,
    pub sample_rate: u32,
    pub block_size_frames: u32,
}

impl ServiceConfig {
    /// 回显为控制面契约的 config 对象（camelCase 键）。
    pub fn to_json(&self) -> Value {
        let mut value = match &self.capture {
            CaptureConfig::Loopback { render_device_id } => json!({
                "mode": "loopback",
                "renderDeviceId": render_device_id,
                "sampleRate": self.sample_rate,
                "blockSizeFrames": self.block_size_frames,
            }),
            CaptureConfig::Capture { capture_device_id } => json!({
                "mode": "capture",
                "captureDeviceId": capture_device_id,
                "sampleRate": self.sample_rate,
                "blockSizeFrames": self.block_size_frames,
            }),
        };
        if self.output_device_id_explicit {
            value["outputDeviceId"] = json!(self.output_device_id);
        }
        value
    }
}

/// 跨线程统计计数（数据面 Relaxed 累加即可——各计数独立、无顺序依赖）。
#[derive(Default)]
pub struct StatsAtomic {
    pub xruns_in: AtomicU64,
    pub xruns_out: AtomicU64,
    pub frames_processed: AtomicU64,
}

impl StatsAtomic {
    /// getState.stats 形态；running_since 为 None 时 uptime 记 0。
    pub fn snapshot(&self, running_since: Option<Instant>) -> Value {
        let uptime_ms = running_since
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0);
        json!({
            "xrunsIn": self.xruns_in.load(std::sync::atomic::Ordering::Relaxed),
            "xrunsOut": self.xruns_out.load(std::sync::atomic::Ordering::Relaxed),
            "framesProcessed": self.frames_processed.load(std::sync::atomic::Ordering::Relaxed),
            "uptimeMs": uptime_ms,
        })
    }
}

/// 数据面 → 控制面的有界事件。数据面只 try_send，满则丢弃事件本身（计数器仍精确）。
#[derive(Debug, Clone)]
pub enum ServiceEvent {
    /// 相位迁移通知（event.phase）。
    Phase { from: String, to: String },
    /// 欠跑/溢出增量（event.xrun）；dir 取 "in"/"out"。
    Xrun { dir: &'static str, count: u64 },
}
