//! 内存假后端（单测/集成测试支撑）：纯内存模拟捕获源与渲染宿，不碰真实设备。
#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hse_wasapi::{BackendError, DeviceInfo, DeviceKind, OpenOptions, StreamFormat};

use crate::backend::{BackendFactory, CaptureOpener, CaptureSource, RenderOpener, RenderSink};

/// 确定性测试样本：无超越函数、跨平台一致的伪斜坡。
fn test_sample(idx: u64) -> f32 {
    ((idx % 997) as f32 / 997.0) * 2.0 - 1.0
}

/// 假捕获源：每次 pull 产出确定斜坡帧；可选节拍（模拟实时到达）与计划性失败。
pub struct FakeCapture {
    cursor: u64,
    period: Duration,
    format: StreamFormat,
    pulls: u64,
}

impl FakeCapture {
    pub fn new(period: Duration, format: StreamFormat) -> Self {
        Self { cursor: 0, period, format, pulls: 0 }
    }
}

impl CaptureSource for FakeCapture {
    fn start(&mut self) -> Result<StreamFormat, BackendError> {
        Ok(self.format)
    }

    fn pull(&mut self, out: &mut [f32]) -> Result<usize, BackendError> {
        self.pulls += 1;
        let frames = out.len() / 2;
        for f in 0..frames {
            out[f * 2] = test_sample(self.cursor + f as u64);
            out[f * 2 + 1] = test_sample(self.cursor + f as u64 + 500_000);
        }
        self.cursor += frames as u64;
        if !self.period.is_zero() {
            std::thread::sleep(self.period);
        }
        Ok(frames)
    }

    fn stop(&mut self) -> Result<(), BackendError> {
        Ok(())
    }

    fn xruns(&self) -> u64 {
        0
    }
}

/// 假渲染宿：记录全部收到的交错样本；可选节拍（模拟设备消费速率）。
pub struct FakeRender {
    pub received: Arc<Mutex<Vec<f32>>>,
    pub push_calls: Arc<AtomicU64>,
    period: Duration,
    format: StreamFormat,
}

impl RenderSink for FakeRender {
    fn start(&mut self) -> Result<StreamFormat, BackendError> {
        Ok(self.format)
    }

    fn push(&mut self, inp: &[f32]) -> Result<(), BackendError> {
        self.received.lock().unwrap().extend_from_slice(inp);
        self.push_calls.fetch_add(1, Ordering::Relaxed);
        if !self.period.is_zero() {
            std::thread::sleep(self.period);
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<(), BackendError> {
        Ok(())
    }

    fn xruns(&self) -> u64 {
        0
    }
}

struct FakeLoopbackOpener { opts: OpenOptions, period: Duration }
impl CaptureOpener for FakeLoopbackOpener {
    fn open(self: Box<Self>) -> Result<Box<dyn CaptureSource>, BackendError> {
        if let Some(m) = &self.opts.device_id {
            if m == "force-error" {
                return Err(BackendError::DeviceNotFound(m.clone()));
            }
        }
        Ok(Box::new(FakeCapture::new(
            self.period,
            StreamFormat { sample_rate: self.opts.sample_rate, channels: 2 },
        )))
    }
}

struct FakeRenderOpener { opts: OpenOptions, period: Duration, received: Arc<Mutex<Vec<f32>>>, push_calls: Arc<AtomicU64>, open_error: Option<String> }
impl RenderOpener for FakeRenderOpener {
    fn open(self: Box<Self>) -> Result<Box<dyn RenderSink>, BackendError> {
        if let Some(m) = &self.open_error {
            return Err(BackendError::Stream(m.clone()));
        }
        Ok(Box::new(FakeRender {
            received: self.received,
            push_calls: self.push_calls,
            period: self.period,
            format: StreamFormat { sample_rate: self.opts.sample_rate, channels: 2 },
        }))
    }
}

/// 假工厂：固定设备表 + 可配置节拍/开流失败。
pub struct FakeFactory {
    pub devices: Vec<DeviceInfo>,
    pub render_received: Arc<Mutex<Vec<f32>>>,
    pub push_calls: Arc<AtomicU64>,
    pub open_error: Option<String>,
    pub capture_period: Duration,
    pub render_period: Duration,
}

impl FakeFactory {
    /// 可用工厂：两个渲染端点（其一默认）+ 两个捕获端点（其一默认）。
    pub fn working(capture_period: Duration, render_period: Duration) -> Arc<Self> {
        Arc::new(Self {
            devices: vec![
                DeviceInfo { kind: DeviceKind::Render, id: "render-default".into(), name: "扬声器（默认）".into(), is_default: true },
                DeviceInfo { kind: DeviceKind::Render, id: "render-headphone".into(), name: "耳机".into(), is_default: false },
                DeviceInfo { kind: DeviceKind::Capture, id: "capture-loopback".into(), name: "扬声器（回环）".into(), is_default: true },
                DeviceInfo { kind: DeviceKind::Capture, id: "cable-output".into(), name: "CABLE Output".into(), is_default: false },
            ],
            render_received: Arc::new(Mutex::new(Vec::new())),
            push_calls: Arc::new(AtomicU64::new(0)),
            open_error: None,
            capture_period,
            render_period,
        })
    }

    /// 开流必失败的工厂（模拟无声卡/系统拒绝）。
    pub fn broken(message: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            devices: Vec::new(),
            render_received: Arc::new(Mutex::new(Vec::new())),
            push_calls: Arc::new(AtomicU64::new(0)),
            open_error: Some(message.into()),
            capture_period: Duration::ZERO,
            render_period: Duration::ZERO,
        })
    }
}

impl BackendFactory for FakeFactory {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>, BackendError> {
        Ok(self.devices.clone())
    }

    fn loopback_opener(&self, opts: &OpenOptions) -> Box<dyn CaptureOpener> {
        Box::new(FakeLoopbackOpener { opts: opts.clone(), period: self.capture_period })
    }

    fn render_opener(&self, opts: &OpenOptions) -> Box<dyn RenderOpener> {
        Box::new(FakeRenderOpener {
            opts: opts.clone(),
            period: self.render_period,
            received: Arc::clone(&self.render_received),
            push_calls: Arc::clone(&self.push_calls),
            open_error: self.open_error.clone(),
        })
    }
}