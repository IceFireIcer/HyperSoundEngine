//! 服务共享状态：相位机、配置快照、原子统计与数据面→控制面事件定义。
//!
//! 统计计数由数据面线程原子累加、控制面读取；事件只承载"增量"信息
//! （相位迁移、xrun 增量），总量以共享计数器为准，序列化在 server 层完成。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use hse_wasapi::AccessMode;
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
    pub access_mode: AccessMode,
    pub access_mode_explicit: bool,
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
        if self.access_mode_explicit {
            value["shareMode"] = json!(match self.access_mode {
                AccessMode::Shared => "shared",
                AccessMode::Exclusive => "exclusive",
            });
        }
        value
    }
}

const LATENCY_HISTOGRAM_BUCKETS: usize = 64;

/// 跨线程统计计数。数据面仅使用 Relaxed 原子操作，各字段互不建立顺序关系。
pub struct StatsAtomic {
    pub xruns_in: AtomicU64,
    pub xruns_out: AtomicU64,
    pub frames_processed: AtomicU64,
    input_ring_depth_frames: AtomicU64,
    input_ring_high_water_frames: AtomicU64,
    output_ring_depth_frames: AtomicU64,
    output_ring_high_water_frames: AtomicU64,
    block_sequence: AtomicU64,
    latency_current_frames: AtomicU64,
    latency_max_frames: AtomicU64,
    latency_histogram: [AtomicU64; LATENCY_HISTOGRAM_BUCKETS],
    latency_version: AtomicU64,
}

impl Default for StatsAtomic {
    fn default() -> Self {
        Self {
            xruns_in: AtomicU64::new(0),
            xruns_out: AtomicU64::new(0),
            frames_processed: AtomicU64::new(0),
            input_ring_depth_frames: AtomicU64::new(0),
            input_ring_high_water_frames: AtomicU64::new(0),
            output_ring_depth_frames: AtomicU64::new(0),
            output_ring_high_water_frames: AtomicU64::new(0),
            block_sequence: AtomicU64::new(0),
            latency_current_frames: AtomicU64::new(0),
            latency_max_frames: AtomicU64::new(0),
            latency_histogram: std::array::from_fn(|_| AtomicU64::new(0)),
            latency_version: AtomicU64::new(0),
        }
    }
}

impl StatsAtomic {
    fn update_max(target: &AtomicU64, value: u64) {
        target.fetch_max(value, Ordering::Relaxed);
    }

    pub fn observe_input_ring_samples(&self, samples: usize) {
        let frames = (samples / 2) as u64;
        self.input_ring_depth_frames
            .store(frames, Ordering::Relaxed);
        Self::update_max(&self.input_ring_high_water_frames, frames);
    }

    pub fn observe_output_ring_samples(&self, samples: usize) {
        let frames = (samples / 2) as u64;
        self.output_ring_depth_frames
            .store(frames, Ordering::Relaxed);
        Self::update_max(&self.output_ring_high_water_frames, frames);
    }

    /// DSP 块的确定性驻留估算：块进入处理时的输入排队帧、当前块帧与
    /// 处理完成后的输出排队帧之和。只依赖环占用，不读取实时线程时钟。
    pub fn record_processed_block(
        &self,
        input_queued_frames: u64,
        block_frames: u64,
        output_queued_frames: u64,
    ) {
        self.latency_version.fetch_add(1, Ordering::Release);
        let latency = input_queued_frames
            .saturating_add(block_frames)
            .saturating_add(output_queued_frames);
        Self::update_max(&self.latency_max_frames, latency);
        self.latency_current_frames
            .store(latency, Ordering::Relaxed);
        let bucket = if latency == 0 {
            0
        } else {
            (u64::BITS - latency.leading_zeros()) as usize
        }
        .min(LATENCY_HISTOGRAM_BUCKETS - 1);
        self.latency_histogram[bucket].fetch_add(1, Ordering::Relaxed);
        self.block_sequence.fetch_add(1, Ordering::Relaxed);
        self.latency_version.fetch_add(1, Ordering::Release);
    }

    /// 每次成功启动前复位周期型统计；进程级 xrun/处理帧累计保持旧语义。
    pub fn reset_cycle(&self) {
        self.latency_version.fetch_add(1, Ordering::Release);
        self.input_ring_depth_frames.store(0, Ordering::Relaxed);
        self.input_ring_high_water_frames
            .store(0, Ordering::Relaxed);
        self.output_ring_depth_frames.store(0, Ordering::Relaxed);
        self.output_ring_high_water_frames
            .store(0, Ordering::Relaxed);
        self.block_sequence.store(0, Ordering::Relaxed);
        self.latency_current_frames.store(0, Ordering::Relaxed);
        self.latency_max_frames.store(0, Ordering::Relaxed);
        for bucket in &self.latency_histogram {
            bucket.store(0, Ordering::Relaxed);
        }
        self.latency_version.fetch_add(1, Ordering::Release);
    }

    pub fn clear_current_depths(&self) {
        self.input_ring_depth_frames.store(0, Ordering::Relaxed);
        self.output_ring_depth_frames.store(0, Ordering::Relaxed);
    }

    fn percentile_frames(&self, samples: u64, numerator: u64, denominator: u64) -> u64 {
        if samples == 0 {
            return 0;
        }
        let target = samples.saturating_mul(numerator).div_ceil(denominator);
        let mut cumulative = 0_u64;
        for (index, bucket) in self.latency_histogram.iter().enumerate() {
            cumulative = cumulative.saturating_add(bucket.load(Ordering::Relaxed));
            if cumulative >= target {
                return if index == 0 { 0 } else { 1_u64 << (index - 1) };
            }
        }
        self.latency_max_frames.load(Ordering::Relaxed)
    }

    /// getState.stats 形态；running_since 为 None 时 uptime 记 0。
    pub fn snapshot(&self, running_since: Option<Instant>) -> Value {
        let uptime_ms = running_since
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(0);
        loop {
            let before = self.latency_version.load(Ordering::Acquire);
            if before & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let samples = self.block_sequence.load(Ordering::Relaxed);
            let input_depth = self.input_ring_depth_frames.load(Ordering::Relaxed);
            let input_high_water = self.input_ring_high_water_frames.load(Ordering::Relaxed);
            let output_depth = self.output_ring_depth_frames.load(Ordering::Relaxed);
            let output_high_water = self.output_ring_high_water_frames.load(Ordering::Relaxed);
            let current = self.latency_current_frames.load(Ordering::Relaxed);
            let p50 = self.percentile_frames(samples, 50, 100);
            let p95 = self.percentile_frames(samples, 95, 100);
            let maximum = self.latency_max_frames.load(Ordering::Relaxed);
            if before != self.latency_version.load(Ordering::Acquire) {
                continue;
            }
            return json!({
                "xrunsIn": self.xruns_in.load(Ordering::Relaxed),
                "xrunsOut": self.xruns_out.load(Ordering::Relaxed),
                "framesProcessed": self.frames_processed.load(Ordering::Relaxed),
                "uptimeMs": uptime_ms,
                "inputRingDepthFrames": input_depth,
                "inputRingHighWaterFrames": input_high_water,
                "outputRingDepthFrames": output_depth,
                "outputRingHighWaterFrames": output_high_water,
                "blockSequence": samples,
                "latencyFrames": {
                    "current": current,
                    "p50": p50,
                    "p95": p95,
                    "max": maximum,
                    "samples": samples,
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 帧延迟分布与周期复位保持进程累计计数() {
        let stats = StatsAtomic::default();
        stats.xruns_in.store(3, Ordering::Relaxed);
        stats.frames_processed.store(128, Ordering::Relaxed);
        stats.observe_input_ring_samples(16);
        stats.observe_output_ring_samples(32);
        stats.record_processed_block(8, 16, 8); // 32 帧
        stats.record_processed_block(24, 16, 24); // 64 帧

        let before = stats.snapshot(None);
        assert_eq!(before["blockSequence"], 2);
        assert_eq!(before["latencyFrames"]["current"], 64);
        assert_eq!(before["latencyFrames"]["p50"], 32);
        assert_eq!(before["latencyFrames"]["p95"], 64);
        assert_eq!(before["latencyFrames"]["max"], 64);

        stats.reset_cycle();
        let after = stats.snapshot(None);
        assert_eq!(after["xrunsIn"], 3);
        assert_eq!(after["framesProcessed"], 128);
        assert_eq!(after["inputRingHighWaterFrames"], 0);
        assert_eq!(after["outputRingHighWaterFrames"], 0);
        assert_eq!(after["blockSequence"], 0);
        assert_eq!(after["latencyFrames"]["samples"], 0);
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
