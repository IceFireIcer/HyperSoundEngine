//! hse-wasapi —— HyperSoundEngine 的 Windows 音频后端封装。
//!
//! 职责（规划书 §2.2）：共享模式渲染 + 输出设备 loopback 捕获 + 输入设备直捕。
//! 实时路径纪律：数据面只经预分配环形缓冲与本 crate 的流对象，不加锁、不分配
//! （调用方预先给足缓冲）。
//!
//! 平台策略：本文件全部类型与函数签名在所有平台编译；真实 WASAPI 实现在 win
//! 模块（仅 Windows 编译，由 Phase 2 后端代理落地）。非 Windows 平台所有入口
//! 返回 BackendError::UnsupportedPlatform——保证 CI（ubuntu）可编译可测。
//!
//! 数据布局契约：进程内统一使用交错立体声 f32（L,R,L,R,…），帧数 = 样本数/2；
//! 与 DSP 层 planar 布局的转换只发生在 DSP 线程边界。

use std::fmt;

/// 设备种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Render,
    Capture,
}

/// 枚举到的音频端点。
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub kind: DeviceKind,
    /// 稳定设备标识（Windows IMMDevice ID 字符串）。
    pub id: String,
    /// 友好名（如扬声器/耳机名、CABLE Output 等）。
    pub name: String,
    /// 是否该类别的系统默认端点。
    pub is_default: bool,
}

/// 协商后的流格式（Phase 2 固定立体声 f32 共享模式）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

/// 打开流的选项；device_id=None 表示该类别系统默认端点。
#[derive(Debug, Clone)]
pub struct OpenOptions {
    pub device_id: Option<String>,
    pub sample_rate: u32,
    /// 期望的每块帧数（事件驱动轮询周期）。
    pub block_size_frames: u32,
}

/// 后端错误。
#[derive(Debug)]
pub enum BackendError {
    UnsupportedPlatform(&'static str),
    DeviceNotFound(String),
    Format(String),
    Stream(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackendError::UnsupportedPlatform(why) => write!(f, "平台不支持：{why}"),
            BackendError::DeviceNotFound(id) => write!(f, "设备不存在：{id}"),
            BackendError::Format(msg) => write!(f, "格式协商失败：{msg}"),
            BackendError::Stream(msg) => write!(f, "流错误：{msg}"),
        }
    }
}

impl std::error::Error for BackendError {}

// Windows：真实实现位于 win 模块（Phase 2 后端代理落地）。
#[cfg(windows)]
pub mod win;

#[cfg(windows)]
pub fn list_devices() -> Result<Vec<DeviceInfo>, BackendError> {
    win::list_devices()
}

#[cfg(windows)]
pub fn open_loopback(opts: &OpenOptions) -> Result<win::LoopbackStream, BackendError> {
    win::LoopbackStream::open(opts)
}

#[cfg(windows)]
pub fn open_capture(opts: &OpenOptions) -> Result<win::CaptureStream, BackendError> {
    win::CaptureStream::open(opts)
}

#[cfg(windows)]
pub fn open_render(opts: &OpenOptions) -> Result<win::RenderStream, BackendError> {
    win::RenderStream::open(opts)
}

// 非 Windows：占位实现（保证 ubuntu CI 编译与单测可跑）。
#[cfg(not(windows))]
mod unsupported_shim {
    /// 占位类型：不会在任何平台上产生实例。
    pub struct LoopbackStream;
    /// 占位类型：不会在任何平台上产生实例。
    pub struct CaptureStream;
    /// 占位类型：不会在任何平台上产生实例。
    pub struct RenderStream;
}

#[cfg(not(windows))]
pub fn list_devices() -> Result<Vec<DeviceInfo>, BackendError> {
    Err(BackendError::UnsupportedPlatform(
        "非 Windows 平台无 WASAPI",
    ))
}

#[cfg(not(windows))]
pub fn open_loopback(
    _opts: &OpenOptions,
) -> Result<unsupported_shim::LoopbackStream, BackendError> {
    Err(BackendError::UnsupportedPlatform(
        "非 Windows 平台无 WASAPI",
    ))
}

#[cfg(not(windows))]
pub fn open_capture(_opts: &OpenOptions) -> Result<unsupported_shim::CaptureStream, BackendError> {
    Err(BackendError::UnsupportedPlatform(
        "非 Windows 平台无 WASAPI",
    ))
}

#[cfg(not(windows))]
pub fn open_render(_opts: &OpenOptions) -> Result<unsupported_shim::RenderStream, BackendError> {
    Err(BackendError::UnsupportedPlatform(
        "非 Windows 平台无 WASAPI",
    ))
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    fn options() -> OpenOptions {
        OpenOptions {
            device_id: None,
            sample_rate: 48_000,
            block_size_frames: 480,
        }
    }

    #[test]
    fn all_stream_entry_points_report_unsupported_platform() {
        let opts = options();
        assert!(matches!(
            open_render(&opts),
            Err(BackendError::UnsupportedPlatform(_))
        ));
        assert!(matches!(
            open_loopback(&opts),
            Err(BackendError::UnsupportedPlatform(_))
        ));
        assert!(matches!(
            open_capture(&opts),
            Err(BackendError::UnsupportedPlatform(_))
        ));
    }
}
