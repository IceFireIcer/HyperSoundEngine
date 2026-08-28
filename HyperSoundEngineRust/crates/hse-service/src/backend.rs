//! 音频后端抽象：面向 hse-wasapi 已锁定 API 面的可注入适配层。
//!
//! 生产路径 WasapiFactory 薄封装 hse-wasapi；测试路径使用 fake_backend 的
//! 内存假后端。服务本体只面向本模块 trait 编码。
//!
//! # 线程模型（关键约束）
//!
//! WASAPI 流对象持有 COM 接口指针（wasapi crate 标记为 !Send），因此**开流
//! 必须发生在最终使用它的数据面线程内**：工厂只产出轻量的"开流器"
//! （opener），捕获/渲染线程各自调用 opener.open() 完成开流与启动，句柄
//! 从不跨线程转移。

use hse_wasapi::{BackendError, DeviceInfo, OpenOptions, StreamFormat};

/// 回环捕获源（对应 hse_wasapi LoopbackStream 的服务侧视图）。
///
/// pull 返回本次实际写入 interleaved_out 的**帧数**（交错立体声，不超过
/// 缓冲长度的一半）。若后端最终语义为"样本数"，只需修改 pipeline 捕获线程
/// 的单处换算点。
pub trait CaptureSource {
    /// 在所属线程上启动流；返回最终协商格式。
    fn start(&mut self) -> Result<StreamFormat, BackendError>;
    fn pull(&mut self, interleaved_out: &mut [f32]) -> Result<usize, BackendError>;
    fn stop(&mut self) -> Result<(), BackendError>;
    fn xruns(&self) -> u64;
}

/// 渲染宿（对应 hse_wasapi RenderStream 的服务侧视图）。
pub trait RenderSink {
    /// 在所属线程上启动流；返回最终协商格式。
    fn start(&mut self) -> Result<StreamFormat, BackendError>;
    fn push(&mut self, interleaved_in: &[f32]) -> Result<(), BackendError>;
    fn stop(&mut self) -> Result<(), BackendError>;
    fn xruns(&self) -> u64;
}

/// 回环开流器：open 在捕获线程内调用。
pub trait CaptureOpener: Send {
    fn open(self: Box<Self>) -> Result<Box<dyn CaptureSource>, BackendError>;
}

/// 渲染开流器：open 在渲染线程内调用。
pub trait RenderOpener: Send {
    fn open(self: Box<Self>) -> Result<Box<dyn RenderSink>, BackendError>;
}

/// 后端工厂：设备枚举 + 两类开流器。实现须可在多线程间共享（Send + Sync）。
pub trait BackendFactory: Send + Sync {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>, BackendError>;
    fn loopback_opener(&self, opts: &OpenOptions) -> Box<dyn CaptureOpener>;
    fn render_opener(&self, opts: &OpenOptions) -> Box<dyn RenderOpener>;
}

/// 生产后端：直接委托 hse-wasapi。
pub struct WasapiFactory;

/// Windows 适配器：开流器在工作线程内构造具体流类型。
#[cfg(windows)]
mod adapters {
    use super::{CaptureOpener, CaptureSource, RenderOpener, RenderSink};
    use hse_wasapi::win::{LoopbackStream, RenderStream};
    use hse_wasapi::{BackendError, OpenOptions, StreamFormat};

    pub struct WasapiCapture(pub LoopbackStream);
    impl CaptureSource for WasapiCapture {
        fn start(&mut self) -> Result<StreamFormat, BackendError> { self.0.start() }
        fn pull(&mut self, out: &mut [f32]) -> Result<usize, BackendError> { self.0.pull(out) }
        fn stop(&mut self) -> Result<(), BackendError> { self.0.stop() }
        fn xruns(&self) -> u64 { self.0.xruns() }
    }

    pub struct WasapiRender(pub RenderStream);
    impl RenderSink for WasapiRender {
        fn start(&mut self) -> Result<StreamFormat, BackendError> { self.0.start() }
        fn push(&mut self, inp: &[f32]) -> Result<(), BackendError> { self.0.push(inp) }
        fn stop(&mut self) -> Result<(), BackendError> { self.0.stop() }
        fn xruns(&self) -> u64 { self.0.xruns() }
    }

    pub struct WasapiLoopbackOpener(pub OpenOptions);
    impl CaptureOpener for WasapiLoopbackOpener {
        fn open(self: Box<Self>) -> Result<Box<dyn CaptureSource>, BackendError> {
            Ok(Box::new(WasapiCapture(hse_wasapi::open_loopback(&self.0)?)))
        }
    }

    pub struct WasapiRenderOpener(pub OpenOptions);
    impl RenderOpener for WasapiRenderOpener {
        fn open(self: Box<Self>) -> Result<Box<dyn RenderSink>, BackendError> {
            Ok(Box::new(WasapiRender(hse_wasapi::open_render(&self.0)?)))
        }
    }
}

impl BackendFactory for WasapiFactory {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>, BackendError> {
        hse_wasapi::list_devices()
    }

    #[cfg(windows)]
    fn loopback_opener(&self, opts: &OpenOptions) -> Box<dyn CaptureOpener> {
        Box::new(adapters::WasapiLoopbackOpener(opts.clone()))
    }

    #[cfg(not(windows))]
    fn loopback_opener(&self, _opts: &OpenOptions) -> Box<dyn CaptureOpener> {
        Box::new(UnsupportedCaptureOpener)
    }

    #[cfg(windows)]
    fn render_opener(&self, opts: &OpenOptions) -> Box<dyn RenderOpener> {
        Box::new(adapters::WasapiRenderOpener(opts.clone()))
    }

    #[cfg(not(windows))]
    fn render_opener(&self, _opts: &OpenOptions) -> Box<dyn RenderOpener> {
        Box::new(UnsupportedRenderOpener)
    }
}

/// 非 Windows 占位开流器：open 必然报平台不支持。
#[cfg(not(windows))]
struct UnsupportedCaptureOpener;
#[cfg(not(windows))]
impl CaptureOpener for UnsupportedCaptureOpener {
    fn open(self: Box<Self>) -> Result<Box<dyn CaptureSource>, BackendError> {
        Err(BackendError::UnsupportedPlatform("非 Windows 平台无 WASAPI"))
    }
}

#[cfg(not(windows))]
struct UnsupportedRenderOpener;
#[cfg(not(windows))]
impl RenderOpener for UnsupportedRenderOpener {
    fn open(self: Box<Self>) -> Result<Box<dyn RenderSink>, BackendError> {
        Err(BackendError::UnsupportedPlatform("非 Windows 平台无 WASAPI"))
    }
}