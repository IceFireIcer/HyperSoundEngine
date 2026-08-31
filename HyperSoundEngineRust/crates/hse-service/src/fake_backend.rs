//! 内存假后端（单测/集成测试支撑）：纯内存模拟捕获源与渲染宿，不碰真实设备。
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use hse_wasapi::{AccessMode, BackendError, DeviceInfo, DeviceKind, OpenOptions, StreamFormat};

use crate::backend::{BackendFactory, CaptureOpener, CaptureSource, RenderOpener, RenderSink};

/// 确定性测试样本：无超越函数、跨平台一致的伪斜坡。
fn test_sample(idx: u64) -> f32 {
    ((idx % 997) as f32 / 997.0) * 2.0 - 1.0
}

/// fake 捕获 readiness：测试可显式发放事件，不依赖墙钟推进数据面。
#[derive(Default)]
struct FakeCaptureReady {
    permits: Mutex<u64>,
    changed: Condvar,
    automatic: AtomicBool,
    waits: AtomicU64,
    pulls: AtomicU64,
}

impl FakeCaptureReady {
    fn automatic() -> Arc<Self> {
        Arc::new(Self {
            automatic: AtomicBool::new(true),
            ..Self::default()
        })
    }

    fn signal(&self, count: u64) {
        let mut permits = self.permits.lock().unwrap();
        *permits = permits.saturating_add(count);
        self.changed.notify_all();
    }
}

/// 假捕获源：每次 pull 产出确定斜坡帧；readiness 可自动就绪或由测试显式发放，
/// 静音模式恒返回 0 帧，另支持计划性暂停。
pub struct FakeCapture {
    cursor: u64,
    period: Duration,
    format: StreamFormat,
    silent: bool,
    enabled: Arc<AtomicBool>,
    ready: Arc<FakeCaptureReady>,
}

impl FakeCapture {
    pub fn new(period: Duration, format: StreamFormat) -> Self {
        Self::gated(
            period,
            format,
            false,
            Arc::new(AtomicBool::new(true)),
            FakeCaptureReady::automatic(),
        )
    }

    /// 静音捕获源：pull 恒返回 0 帧。
    pub fn silent(period: Duration, format: StreamFormat) -> Self {
        Self::gated(
            period,
            format,
            true,
            Arc::new(AtomicBool::new(true)),
            FakeCaptureReady::automatic(),
        )
    }

    fn gated(
        period: Duration,
        format: StreamFormat,
        silent: bool,
        enabled: Arc<AtomicBool>,
        ready: Arc<FakeCaptureReady>,
    ) -> Self {
        Self {
            cursor: 0,
            period,
            format,
            silent,
            enabled,
            ready,
        }
    }
}

impl CaptureSource for FakeCapture {
    fn start(&mut self) -> Result<StreamFormat, BackendError> {
        Ok(self.format)
    }

    fn wait_ready(&mut self, timeout: Duration) -> Result<bool, BackendError> {
        self.ready.waits.fetch_add(1, Ordering::Relaxed);
        if !self.enabled.load(Ordering::Relaxed) {
            let permits = self.ready.permits.lock().unwrap();
            let _ = self.ready.changed.wait_timeout(permits, timeout).unwrap();
            return Ok(false);
        }
        if self.ready.automatic.load(Ordering::Relaxed) {
            if !self.period.is_zero() {
                std::thread::sleep(self.period.min(timeout));
            }
            return Ok(true);
        }
        let permits = self.ready.permits.lock().unwrap();
        let (mut permits, _) = self
            .ready
            .changed
            .wait_timeout_while(permits, timeout, |permits| *permits == 0)
            .unwrap();
        if *permits == 0 {
            return Ok(false);
        }
        *permits -= 1;
        Ok(true)
    }

    fn pull(&mut self, out: &mut [f32]) -> Result<usize, BackendError> {
        self.ready.pulls.fetch_add(1, Ordering::Relaxed);
        if !self.enabled.load(Ordering::Relaxed) || self.silent {
            return Ok(0);
        }
        let frames = out.len() / 2;
        for f in 0..frames {
            out[f * 2] = test_sample(self.cursor + f as u64);
            out[f * 2 + 1] = test_sample(self.cursor + f as u64 + 500_000);
        }
        self.cursor += frames as u64;
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

struct FakeLoopbackOpener {
    opts: OpenOptions,
    period: Duration,
    silent: bool,
    opened: Arc<Mutex<Vec<(Option<String>, AccessMode)>>>,
    sample_rate: Option<u32>,
    capture_enabled: Arc<AtomicBool>,
    capture_ready: Arc<FakeCaptureReady>,
}
impl CaptureOpener for FakeLoopbackOpener {
    fn open(self: Box<Self>) -> Result<Box<dyn CaptureSource>, BackendError> {
        self.opened
            .lock()
            .unwrap()
            .push((self.opts.device_id.clone(), self.opts.access_mode));
        if let Some(m) = &self.opts.device_id {
            if m == "force-error" {
                return Err(BackendError::DeviceNotFound(m.clone()));
            }
        }
        let format = StreamFormat {
            sample_rate: self.sample_rate.unwrap_or(self.opts.sample_rate),
            channels: 2,
        };
        let src = FakeCapture::gated(
            self.period,
            format,
            self.silent,
            self.capture_enabled,
            self.capture_ready,
        );
        Ok(Box::new(src))
    }
}

struct FakeRenderOpener {
    opts: OpenOptions,
    period: Duration,
    received: Arc<Mutex<Vec<f32>>>,
    push_calls: Arc<AtomicU64>,
    open_error: Option<String>,
    opened: Arc<Mutex<Vec<(Option<String>, AccessMode)>>>,
    sample_rate: Option<u32>,
}
impl RenderOpener for FakeRenderOpener {
    fn open(self: Box<Self>) -> Result<Box<dyn RenderSink>, BackendError> {
        self.opened
            .lock()
            .unwrap()
            .push((self.opts.device_id.clone(), self.opts.access_mode));
        if let Some(m) = &self.open_error {
            return Err(BackendError::Stream(m.clone()));
        }
        Ok(Box::new(FakeRender {
            received: self.received,
            push_calls: self.push_calls,
            period: self.period,
            format: StreamFormat {
                sample_rate: self.sample_rate.unwrap_or(self.opts.sample_rate),
                channels: 2,
            },
        }))
    }
}

/// 假工厂：固定设备表 + 可配置节拍/静音捕获/开流失败。
pub struct FakeFactory {
    pub devices: Vec<DeviceInfo>,
    pub render_received: Arc<Mutex<Vec<f32>>>,
    pub push_calls: Arc<AtomicU64>,
    pub opened_loopback: Arc<Mutex<Vec<(Option<String>, AccessMode)>>>,
    pub opened_capture: Arc<Mutex<Vec<(Option<String>, AccessMode)>>>,
    pub opened_render: Arc<Mutex<Vec<(Option<String>, AccessMode)>>>,
    pub open_error: Option<String>,
    pub capture_period: Duration,
    pub render_period: Duration,
    pub capture_silent: bool,
    pub capture_sample_rate: Option<u32>,
    pub render_sample_rate: Option<u32>,
    pub capture_enabled: Arc<AtomicBool>,
    capture_ready: Arc<FakeCaptureReady>,
}

impl FakeFactory {
    /// 可用工厂：两个渲染端点（其一默认）+ 两个捕获端点（其一默认）。
    pub fn working(capture_period: Duration, render_period: Duration) -> Arc<Self> {
        Arc::new(Self {
            devices: vec![
                DeviceInfo {
                    kind: DeviceKind::Render,
                    id: "render-default".into(),
                    name: "扬声器（默认）".into(),
                    is_default: true,
                },
                DeviceInfo {
                    kind: DeviceKind::Render,
                    id: "render-headphone".into(),
                    name: "耳机".into(),
                    is_default: false,
                },
                DeviceInfo {
                    kind: DeviceKind::Capture,
                    id: "capture-loopback".into(),
                    name: "扬声器（回环）".into(),
                    is_default: true,
                },
                DeviceInfo {
                    kind: DeviceKind::Capture,
                    id: "cable-output".into(),
                    name: "CABLE Output".into(),
                    is_default: false,
                },
            ],
            render_received: Arc::new(Mutex::new(Vec::new())),
            push_calls: Arc::new(AtomicU64::new(0)),
            opened_loopback: Arc::new(Mutex::new(Vec::new())),
            opened_capture: Arc::new(Mutex::new(Vec::new())),
            opened_render: Arc::new(Mutex::new(Vec::new())),
            open_error: None,
            capture_period,
            render_period,
            capture_silent: false,
            capture_sample_rate: None,
            render_sample_rate: None,
            capture_enabled: Arc::new(AtomicBool::new(true)),
            capture_ready: FakeCaptureReady::automatic(),
        })
    }

    /// 与 working 相同，但回环捕获恒静音（pull 恒返回 0 帧）：
    /// 供推流混合测试隔离回环源，使渲染输出只含会话混音。
    pub fn silent_loopback(capture_period: Duration, render_period: Duration) -> Arc<Self> {
        let mut f = Self::working(capture_period, render_period);
        Arc::get_mut(&mut f).expect("引用计数为 1").capture_silent = true;
        f
    }

    /// 开流必失败的工厂（模拟无声卡/系统拒绝）。
    pub fn broken(message: impl Into<String>) -> Arc<Self> {
        Arc::new(Self {
            devices: Vec::new(),
            render_received: Arc::new(Mutex::new(Vec::new())),
            push_calls: Arc::new(AtomicU64::new(0)),
            opened_loopback: Arc::new(Mutex::new(Vec::new())),
            opened_capture: Arc::new(Mutex::new(Vec::new())),
            opened_render: Arc::new(Mutex::new(Vec::new())),
            open_error: Some(message.into()),
            capture_period: Duration::ZERO,
            render_period: Duration::ZERO,
            capture_silent: false,
            capture_sample_rate: None,
            render_sample_rate: None,
            capture_enabled: Arc::new(AtomicBool::new(true)),
            capture_ready: FakeCaptureReady::automatic(),
        })
    }
    pub fn set_capture_enabled(&self, enabled: bool) {
        self.capture_enabled.store(enabled, Ordering::Relaxed);
        self.capture_ready.changed.notify_all();
    }

    /// 切换为显式 readiness 模式；之后只有 signal_capture_ready 才会放行 pull。
    pub fn set_capture_manual(&self) {
        self.capture_ready.automatic.store(false, Ordering::Relaxed);
        *self.capture_ready.permits.lock().unwrap() = 0;
    }

    pub fn signal_capture_ready(&self, count: u64) {
        self.capture_ready.signal(count);
    }

    pub fn capture_waits(&self) -> u64 {
        self.capture_ready.waits.load(Ordering::Relaxed)
    }

    pub fn capture_pulls(&self) -> u64 {
        self.capture_ready.pulls.load(Ordering::Relaxed)
    }

    fn capture_opener_for(
        &self,
        opts: &OpenOptions,
        opened: &Arc<Mutex<Vec<(Option<String>, AccessMode)>>>,
    ) -> Box<dyn CaptureOpener> {
        Box::new(FakeLoopbackOpener {
            opts: opts.clone(),
            period: self.capture_period,
            silent: self.capture_silent,
            opened: Arc::clone(opened),
            sample_rate: self.capture_sample_rate,
            capture_enabled: Arc::clone(&self.capture_enabled),
            capture_ready: Arc::clone(&self.capture_ready),
        })
    }
}

impl BackendFactory for FakeFactory {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>, BackendError> {
        Ok(self.devices.clone())
    }

    fn loopback_opener(&self, opts: &OpenOptions) -> Box<dyn CaptureOpener> {
        self.capture_opener_for(opts, &self.opened_loopback)
    }

    fn capture_opener(&self, opts: &OpenOptions) -> Box<dyn CaptureOpener> {
        self.capture_opener_for(opts, &self.opened_capture)
    }

    fn render_opener(&self, opts: &OpenOptions) -> Box<dyn RenderOpener> {
        Box::new(FakeRenderOpener {
            opts: opts.clone(),
            period: self.render_period,
            received: Arc::clone(&self.render_received),
            push_calls: Arc::clone(&self.push_calls),
            open_error: self.open_error.clone(),
            opened: Arc::clone(&self.opened_render),
            sample_rate: self.render_sample_rate,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 显式readiness许可决定pull推进_无需墙钟断言() {
        let ready = Arc::new(FakeCaptureReady::default());
        let mut capture = FakeCapture::gated(
            Duration::ZERO,
            StreamFormat {
                sample_rate: 48_000,
                channels: 2,
            },
            false,
            Arc::new(AtomicBool::new(true)),
            Arc::clone(&ready),
        );
        capture.start().unwrap();

        assert!(!capture.wait_ready(Duration::ZERO).unwrap());
        assert_eq!(ready.waits.load(Ordering::Relaxed), 1);
        assert_eq!(ready.pulls.load(Ordering::Relaxed), 0);

        ready.signal(1);
        assert!(capture.wait_ready(Duration::ZERO).unwrap());
        let mut out = [0.0_f32; 8];
        assert_eq!(capture.pull(&mut out).unwrap(), 4);
        assert_eq!(ready.waits.load(Ordering::Relaxed), 2);
        assert_eq!(ready.pulls.load(Ordering::Relaxed), 1);
        assert_ne!(out, [0.0; 8]);
        assert!(!capture.wait_ready(Duration::ZERO).unwrap());
    }
}
