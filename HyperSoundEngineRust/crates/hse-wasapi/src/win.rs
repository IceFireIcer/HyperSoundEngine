//! win —— WASAPI 真实实现（Phase 2「Windows 服务进程 v1——回环拦截端到端」）。
//!
//! 基于 [`wasapi`] crate（CamillaDSP 作者维护，等号锁定 0.24.0）封装三条数据通路：
//!
//! - [`RenderStream`]：共享模式事件驱动渲染。`push` 以交错立体声 f32 推帧，
//!   缓冲空间不足时在小步事件等待中节流，直至全部写入完成（阻塞语义由调用
//!   线程承担）；欠供（缓冲被完全耗尽）计入 xruns。
//! - [`LoopbackStream`]：在渲染端点上建立 loopback 捕获流，拦截系统混音输出。
//!   wasapi crate 的语义：渲染设备的 AudioClient 按 `Direction::Capture` 初始化
//!   即自动附加 `AUDCLNT_STREAMFLAGS_LOOPBACK`。
//! - [`CaptureStream`]：在捕获端点上建立普通共享模式捕获流；`device_id=None` 选择
//!   系统默认捕获端点。它与 loopback 共用相同的读取实现，仅端点类别不同。
//!
//! 格式策略：优先显式协商立体声 f32（`IsFormatSupported` 共享模式查询；引擎给出
//! 近似格式且仍为立体声 f32 时采纳之），不可行时以 `AUTOCONVERTPCM` 引擎自动
//! 转换兜底打开目标格式，彻底失败才报 [`BackendError::Format`]。`start` 返回的
//! [`StreamFormat`] 即最终生效格式。
//!
//! 线程纪律：本模块方法全部运行在服务控制面线程（WASAPI 调用本身即系统调用，
//! 不受 DSP 线程零系统调用铁律约束）；但热路径仍遵守零分配零 panic——工作缓冲
//! 在 `open` 时按设备缓冲帧数一次性预分配，读写循环只使用既有切片。

use crate::{BackendError, DeviceInfo, DeviceKind, OpenOptions, StreamFormat};
use wasapi::{
    calculate_period_100ns, initialize_mta, AudioCaptureClient, AudioClient, AudioRenderClient,
    DeviceEnumerator, Direction, Handle, SampleType, ShareMode, StreamMode, WasapiError,
    WaveFormat,
};

/// 进程内统一声道数：交错立体声（lib.rs 数据布局契约）。
const CHANNELS: usize = 2;
/// 统一存储位深与有效位深：IEEE f32。
const STORE_BITS: usize = 32;
/// 渲染事件等待的单步超时（毫秒）：限制 push 阻塞路径的最小检查粒度。
const EVENT_STEP_MS: u32 = 25;
/// 单次 push 允许的最长无进展等待（毫秒），超过判定设备失联并报错返回，
/// 避免调用线程被永久挂起。
const PUSH_STALL_LIMIT_MS: u32 = 5000;
/// COM 已处于其他套间（RPC_E_CHANGED_MODE）：线程上已有并发模式，视为可用。
const RPC_E_CHANGED_MODE: i32 = -2147417850;

/// 确保 COM 已在当前线程以多线程套间初始化（幂等）。
fn ensure_com() -> Result<(), BackendError> {
    let hr = initialize_mta();
    if hr.is_ok() || hr.0 == RPC_E_CHANGED_MODE {
        return Ok(());
    }
    Err(BackendError::Stream(format!("COM 初始化失败：{hr}")))
}

/// 把 wasapi 错误映射为带上下文的 [`BackendError::Stream`]。
fn to_backend<T>(result: Result<T, WasapiError>, context: &str) -> Result<T, BackendError> {
    result.map_err(|e| BackendError::Stream(format!("{context}：{e}")))
}

/// 构造目标格式：立体声 IEEE f32、指定采样率、标准立体声声道掩码。
fn target_format(sample_rate: u32) -> WaveFormat {
    WaveFormat::new(
        STORE_BITS,
        STORE_BITS,
        &SampleType::Float,
        sample_rate as usize,
        CHANNELS,
        None,
    )
}

/// 判定一个协商结果是否满足数据布局契约（立体声 f32）。
fn is_stereo_f32(fmt: &WaveFormat) -> bool {
    fmt.get_bitspersample() == STORE_BITS as u16
        && fmt.get_nchannels() as usize == CHANNELS
        && matches!(fmt.get_subformat(), Ok(SampleType::Float))
}

/// 共享模式显式协商立体声 f32。
///
/// 返回 `(首选格式, 兜底格式, 首选是否开自动转换)`：
///
/// - 引擎完全接受目标格式 → 首选即目标，无需转换；
/// - 引擎给出近似格式且近似格式仍是立体声 f32 → 采纳近似格式；
/// - 其余情形（近似格式破坏布局契约 / 查询本身失败）→ 首选为目标格式并开启
///   `AUTOCONVERTPCM` 由引擎自动转换；若连它都被拒绝，由调用方报 Format。
fn negotiate_format(
    audio_client: &AudioClient,
    requested_rate: u32,
) -> (WaveFormat, WaveFormat, bool) {
    let target = target_format(requested_rate);
    match audio_client.is_supported(&target, &ShareMode::Shared) {
        Ok(None) => (target.clone(), target, false),
        Ok(Some(near)) if is_stereo_f32(&near) => (near, target, false),
        _ => (target.clone(), target, true),
    }
}

/// 打开流程的产物：已按事件驱动共享模式初始化完毕的 AudioClient 及其派生资源。
struct OpenedEndpoint {
    audio_client: AudioClient,
    event_handle: Handle,
    format: StreamFormat,
    buffer_frames: u32,
    blockalign: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndpointMode {
    Render,
    Loopback,
    Capture,
}

impl EndpointMode {
    fn device_direction(self) -> Direction {
        match self {
            Self::Render | Self::Loopback => Direction::Render,
            Self::Capture => Direction::Capture,
        }
    }

    fn stream_direction(self) -> Direction {
        match self {
            Self::Render => Direction::Render,
            Self::Loopback | Self::Capture => Direction::Capture,
        }
    }
}

fn open_endpoint(opts: &OpenOptions, mode: EndpointMode) -> Result<OpenedEndpoint, BackendError> {
    ensure_com()?;
    if opts.sample_rate == 0 {
        return Err(BackendError::Format("采样率不能为 0".into()));
    }

    let enumerator = DeviceEnumerator::new()
        .map_err(|e| BackendError::Stream(format!("创建设备枚举器失败：{e}")))?;
    let device_direction = mode.device_direction();
    let device = match &opts.device_id {
        Some(id) => {
            let device = enumerator
                .get_device(id)
                .map_err(|_| BackendError::DeviceNotFound(id.clone()))?;
            if device.get_direction() != device_direction {
                return Err(BackendError::DeviceNotFound(format!(
                    "设备类别不匹配：{id} 不是 {device_direction} 端点"
                )));
            }
            device
        }
        None => enumerator
            .get_default_device(&device_direction)
            .map_err(|e| {
                BackendError::DeviceNotFound(format!("默认 {device_direction} 端点不可用：{e}"))
            })?,
    };
    let mut audio_client = to_backend(device.get_iaudioclient(), "获取 AudioClient 失败")?;

    let (mut wave, fallback_wave, mut autoconvert) =
        negotiate_format(&audio_client, opts.sample_rate);

    // 缓冲时长：请求块长换算为 100ns 周期作为下限，且不低于设备默认周期的两倍，
    // 给 push 的阻塞等待留出节流余量。
    let (default_period, _) = to_backend(audio_client.get_device_period(), "获取设备周期失败")?;
    let wanted_hns = calculate_period_100ns(
        opts.block_size_frames.max(1) as i64,
        i64::from(wave.get_samplespersec()),
    );
    let buffer_duration_hns = wanted_hns.max(default_period * 2);

    // 渲染设备按 Capture 初始化时，wasapi crate 自动附加 LOOPBACK 标志；捕获设备
    // 按 Capture 初始化则是普通直捕。
    let stream_direction = mode.stream_direction();
    let init_once =
        |ac: &mut AudioClient, fmt: &WaveFormat, convert: bool| -> Result<(), BackendError> {
            let mode = StreamMode::EventsShared {
                autoconvert: convert,
                buffer_duration_hns,
            };
            to_backend(
                ac.initialize_client(fmt, &stream_direction, &mode),
                "初始化音频流失败",
            )
        };
    if init_once(&mut audio_client, &wave, autoconvert).is_err() && !autoconvert {
        // 显式协商结果被引擎拒绝：以目标格式 + 自动转换兜底重试一次。
        autoconvert = true;
        wave = fallback_wave;
        init_once(&mut audio_client, &wave, autoconvert)
            .map_err(|_| BackendError::Format("立体声 f32 共享模式无法在该端点打开".into()))?;
    }

    if wave.get_nchannels() as usize != CHANNELS {
        return Err(BackendError::Format(format!(
            "协商结果声道数为 {}，违反立体声布局契约",
            wave.get_nchannels()
        )));
    }

    let event_handle = to_backend(audio_client.set_get_eventhandle(), "设置流事件句柄失败")?;
    let buffer_frames = to_backend(audio_client.get_buffer_size(), "获取缓冲帧数失败")?;

    Ok(OpenedEndpoint {
        format: StreamFormat {
            sample_rate: wave.get_samplespersec(),
            channels: wave.get_nchannels(),
        },
        audio_client,
        event_handle,
        buffer_frames,
        blockalign: wave.get_blockalign() as usize,
    })
}

// ---------------------------------------------------------------------------
// 渲染流
// ---------------------------------------------------------------------------

/// 共享模式事件驱动渲染流。
pub struct RenderStream {
    audio_client: AudioClient,
    render_client: AudioRenderClient,
    event_handle: Handle,
    format: StreamFormat,
    /// 设备缓冲总帧数（Initialize 后固定）。
    buffer_frames: u32,
    /// 字节/帧 = 声道数 × 4（f32）。
    blockalign: usize,
    /// 预分配写缓冲：容量 = buffer_frames × blockalign，热路径复用不分配。
    scratch: Vec<u8>,
    started: bool,
    xruns: u64,
}

impl RenderStream {
    pub fn open(opts: &OpenOptions) -> Result<Self, BackendError> {
        let opened = open_endpoint(opts, EndpointMode::Render)?;
        let render_client = to_backend(
            opened.audio_client.get_audiorenderclient(),
            "获取渲染客户端失败",
        )?;
        Ok(Self {
            scratch: vec![0u8; opened.buffer_frames as usize * opened.blockalign],
            audio_client: opened.audio_client,
            render_client,
            event_handle: opened.event_handle,
            format: opened.format,
            buffer_frames: opened.buffer_frames,
            blockalign: opened.blockalign,
            started: false,
            xruns: 0,
        })
    }

    /// 启动流：先预填整段静音保证起播即有有效缓冲，再启动；返回协商后的格式。
    pub fn start(&mut self) -> Result<StreamFormat, BackendError> {
        if self.started {
            return Err(BackendError::Stream("渲染流已启动，不可重复 start".into()));
        }
        let space = to_backend(
            self.audio_client.get_available_space_in_frames(),
            "查询可用空间失败",
        )? as usize;
        if space > 0 {
            let bytes = space * self.blockalign;
            self.scratch[..bytes].fill(0);
            to_backend(
                self.render_client
                    .write_to_device(space, &self.scratch[..bytes], None),
                "预填静音失败",
            )?;
        }
        to_backend(self.audio_client.start_stream(), "启动渲染流失败")?;
        self.started = true;
        Ok(self.format)
    }

    /// 推送一块交错立体声 f32 数据，直至全部写入设备缓冲才返回。
    ///
    /// 缓冲空间不足时分多次写入：每次写满当前可用空间后等待渲染事件（硬件时钟
    /// 节拍释放空间），期间发现缓冲被完全耗尽即记一次欠供（xrun）。最长无进展
    /// 等待受 [`PUSH_STALL_LIMIT_MS`] 约束，超时报错而非永久挂起。
    pub fn push(&mut self, interleaved_in: &[f32]) -> Result<(), BackendError> {
        if !self.started {
            return Err(BackendError::Stream("渲染流未启动".into()));
        }
        if interleaved_in.len() % CHANNELS != 0 {
            return Err(BackendError::Stream(
                "输入样本长度未按立体声帧对齐（应为偶数个样本）".into(),
            ));
        }
        let total_frames = interleaved_in.len() / CHANNELS;
        let mut offset_frames = 0usize;
        let mut stalled_ms = 0u32;
        while offset_frames < total_frames {
            let space = to_backend(
                self.audio_client.get_available_space_in_frames(),
                "查询可用空间失败",
            )? as usize;
            if space >= self.buffer_frames as usize {
                // 整段缓冲空闲：设备已把此前数据播完，正在播放空气 —— 记一次欠供。
                self.xruns += 1;
            }
            let writable = space.min(total_frames - offset_frames);
            if writable > 0 {
                let src_lo = offset_frames * CHANNELS;
                let src_hi = (offset_frames + writable) * CHANNELS;
                let bytes = writable * self.blockalign;
                {
                    let dst = &mut self.scratch[..bytes];
                    for (frame_dst, sample) in
                        dst.chunks_exact_mut(4).zip(&interleaved_in[src_lo..src_hi])
                    {
                        frame_dst.copy_from_slice(&sample.to_le_bytes());
                    }
                }
                to_backend(
                    self.render_client
                        .write_to_device(writable, &self.scratch[..bytes], None),
                    "写入设备失败",
                )?;
                offset_frames += writable;
                stalled_ms = 0;
            }
            if offset_frames < total_frames {
                match self.event_handle.wait_for_event(EVENT_STEP_MS) {
                    Ok(()) => {}
                    Err(_) => {
                        stalled_ms += EVENT_STEP_MS;
                        if stalled_ms >= PUSH_STALL_LIMIT_MS {
                            return Err(BackendError::Stream(
                                "渲染事件持续超时，设备可能已失联".into(),
                            ));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// 停止并复位流；幂等，重复调用无害。
    pub fn stop(&mut self) -> Result<(), BackendError> {
        if !self.started {
            return Ok(());
        }
        self.started = false;
        // 先停后排：丢弃残余排队数据，避免下次会话播出旧内容。
        to_backend(self.audio_client.stop_stream(), "停止渲染流失败")?;
        to_backend(self.audio_client.reset_stream(), "复位渲染流失败")?;
        Ok(())
    }

    /// 欠供次数。
    pub fn xruns(&self) -> u64 {
        self.xruns
    }
}

impl Drop for RenderStream {
    fn drop(&mut self) {
        if self.started {
            let _ = self.audio_client.stop_stream();
            self.started = false;
        }
    }
}

// ---------------------------------------------------------------------------
// 捕获流共享实现
// ---------------------------------------------------------------------------

struct CaptureStreamImpl {
    audio_client: AudioClient,
    capture_client: AudioCaptureClient,
    format: StreamFormat,
    /// 设备缓冲总帧数。
    buffer_frames: u32,
    /// 字节/帧。
    blockalign: usize,
    /// 预分配读缓冲：单次读取不超过一个完整设备缓冲。
    byte_scratch: Vec<u8>,
    started: bool,
    xruns: u64,
}

impl CaptureStreamImpl {
    fn open(opts: &OpenOptions, mode: EndpointMode) -> Result<Self, BackendError> {
        let opened = open_endpoint(opts, mode)?;
        let capture_client = to_backend(
            opened.audio_client.get_audiocaptureclient(),
            "获取捕获客户端失败",
        )?;
        Ok(Self {
            byte_scratch: vec![0u8; opened.buffer_frames as usize * opened.blockalign],
            audio_client: opened.audio_client,
            capture_client,
            format: opened.format,
            buffer_frames: opened.buffer_frames,
            blockalign: opened.blockalign,
            started: false,
            xruns: 0,
        })
    }

    /// 启动捕获流；返回协商后的格式。
    fn start(&mut self) -> Result<StreamFormat, BackendError> {
        if self.started {
            return Err(BackendError::Stream("捕获流已启动，不可重复 start".into()));
        }
        to_backend(self.audio_client.start_stream(), "启动捕获流失败")?;
        self.started = true;
        Ok(self.format)
    }

    /// 尽力拉取交错立体声 f32 数据：把当前所有就绪包读出填入 `interleaved_out`，
    /// 返回实际写入的帧数（0 表示暂无数据）。不阻塞、不等待事件。
    ///
    /// xrun 来源有二：读到数据不连续标志（上游曾断流），或单个包超出剩余输出
    /// 容量被迫截断丢弃（下游消费过慢）。
    fn pull(&mut self, interleaved_out: &mut [f32]) -> Result<usize, BackendError> {
        if !self.started {
            return Err(BackendError::Stream("捕获流未启动".into()));
        }
        if interleaved_out.is_empty() {
            return Ok(0);
        }
        if interleaved_out.len() % CHANNELS != 0 {
            return Err(BackendError::Stream(
                "输出切片长度未按立体声帧对齐（应为偶数个样本）".into(),
            ));
        }
        let capacity_frames = interleaved_out.len() / CHANNELS;
        let mut filled_frames = 0usize;
        while filled_frames < capacity_frames {
            let packet_frames =
                match to_backend(self.capture_client.get_next_packet_size(), "查询包大小失败")?
                {
                    None | Some(0) => break,
                    Some(n) => n as usize,
                };
            // 共享模式下单包不超过设备缓冲帧数；越界说明状态异常，拒绝读取。
            if packet_frames > self.buffer_frames as usize {
                return Err(BackendError::Stream(
                    "捕获包大小超出预分配读缓冲，内部状态异常".into(),
                ));
            }
            let need_bytes = packet_frames * self.blockalign;
            let (read_frames, info) = to_backend(
                self.capture_client
                    .read_from_device(&mut self.byte_scratch[..need_bytes]),
                "读取捕获数据失败",
            )?;
            if info.flags.data_discontinuity {
                self.xruns += 1;
            }
            let read_frames = read_frames as usize;
            let take_frames = read_frames.min(capacity_frames - filled_frames);
            let silent = info.flags.silent;
            {
                let src = &self.byte_scratch[..take_frames * self.blockalign];
                let dst_base = filled_frames * CHANNELS;
                for (chunk, dst) in src.chunks_exact(4).zip(&mut interleaved_out[dst_base..]) {
                    let value = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    *dst = if silent { 0.0 } else { value };
                }
            }
            filled_frames += take_frames;
            if take_frames < read_frames {
                // 输出容量吃不下整个包：剩余帧随包一起被丢弃，计一次过载。
                self.xruns += 1;
                break;
            }
        }
        Ok(filled_frames)
    }

    /// 停止并复位流；幂等，重复调用无害。
    fn stop(&mut self) -> Result<(), BackendError> {
        if !self.started {
            return Ok(());
        }
        self.started = false;
        to_backend(self.audio_client.stop_stream(), "停止捕获流失败")?;
        to_backend(self.audio_client.reset_stream(), "复位捕获流失败")?;
        Ok(())
    }

    /// xrun（断流 + 过载丢包）计数。
    fn xruns(&self) -> u64 {
        self.xruns
    }
}

impl Drop for CaptureStreamImpl {
    fn drop(&mut self) {
        if self.started {
            let _ = self.audio_client.stop_stream();
            self.started = false;
        }
    }
}

macro_rules! capture_stream {
    ($name:ident, $mode:expr, $doc:literal) => {
        #[doc = $doc]
        pub struct $name(CaptureStreamImpl);

        impl $name {
            pub fn open(opts: &OpenOptions) -> Result<Self, BackendError> {
                CaptureStreamImpl::open(opts, $mode).map(Self)
            }

            pub fn start(&mut self) -> Result<StreamFormat, BackendError> {
                self.0.start()
            }

            pub fn pull(&mut self, interleaved_out: &mut [f32]) -> Result<usize, BackendError> {
                self.0.pull(interleaved_out)
            }

            pub fn stop(&mut self) -> Result<(), BackendError> {
                self.0.stop()
            }

            pub fn xruns(&self) -> u64 {
                self.0.xruns()
            }
        }
    };
}

capture_stream!(
    LoopbackStream,
    EndpointMode::Loopback,
    "系统渲染端点上的共享模式 loopback 捕获流。"
);
capture_stream!(
    CaptureStream,
    EndpointMode::Capture,
    "系统捕获端点上的共享模式直接捕获流。"
);

// ---------------------------------------------------------------------------
// 设备枚举
// ---------------------------------------------------------------------------

/// 枚举系统全部输出与输入端点，标记各类别的默认端点。
pub fn list_devices() -> Result<Vec<DeviceInfo>, BackendError> {
    ensure_com()?;
    let enumerator = DeviceEnumerator::new()
        .map_err(|e| BackendError::Stream(format!("创建设备枚举器失败：{e}")))?;
    let default_render_id = enumerator
        .get_default_device(&Direction::Render)
        .ok()
        .and_then(|d| d.get_id().ok());
    let default_capture_id = enumerator
        .get_default_device(&Direction::Capture)
        .ok()
        .and_then(|d| d.get_id().ok());

    let mut devices = Vec::new();
    for (direction, kind, default_id) in [
        (
            Direction::Render,
            DeviceKind::Render,
            default_render_id.as_deref(),
        ),
        (
            Direction::Capture,
            DeviceKind::Capture,
            default_capture_id.as_deref(),
        ),
    ] {
        let collection = to_backend(
            enumerator.get_device_collection(&direction),
            "获取设备集合失败",
        )?;
        let count = to_backend(collection.get_nbr_devices(), "读取设备数量失败")?;
        for index in 0..count {
            let dev = to_backend(collection.get_device_at_index(index), "读取设备失败")?;
            let id = to_backend(dev.get_id(), "读取设备 ID 失败")?;
            let name = dev
                .get_friendlyname()
                .unwrap_or_else(|_| "<未知设备名>".to_string());
            devices.push(DeviceInfo {
                kind,
                is_default: default_id == Some(id.as_str()),
                name,
                id,
            });
        }
    }
    Ok(devices)
}

// ---------------------------------------------------------------------------
// 测试（真实设备冒烟）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// 就地填充一段指定相位的 1kHz 正弦（交错双声道同相）。
    fn fill_sine(buf: &mut [f32], phase: &mut f64, sample_rate: u32, amplitude: f32) {
        let step = 2.0 * std::f64::consts::PI * 1000.0 / f64::from(sample_rate);
        for slot in buf.iter_mut() {
            *phase += step;
            *slot = amplitude * phase.sin() as f32;
        }
    }

    #[test]
    fn endpoint_modes_select_expected_device_and_stream_directions() {
        assert_eq!(EndpointMode::Render.device_direction(), Direction::Render);
        assert_eq!(EndpointMode::Render.stream_direction(), Direction::Render);
        assert_eq!(EndpointMode::Loopback.device_direction(), Direction::Render);
        assert_eq!(
            EndpointMode::Loopback.stream_direction(),
            Direction::Capture
        );
        assert_eq!(EndpointMode::Capture.device_direction(), Direction::Capture);
        assert_eq!(EndpointMode::Capture.stream_direction(), Direction::Capture);
    }

    #[test]
    fn list_devices_finds_render_and_single_default() {
        let devices = list_devices().expect("list_devices 不应失败");
        assert!(
            !devices.is_empty(),
            "本机未枚举到任何音频端点；请在有声卡的环境运行"
        );
        let renders: Vec<&DeviceInfo> = devices
            .iter()
            .filter(|d| d.kind == DeviceKind::Render)
            .collect();
        assert!(!renders.is_empty(), "至少应发现一个渲染端点");
        let defaults = renders.iter().filter(|d| d.is_default).count();
        assert_eq!(defaults, 1, "渲染类别应有且仅有一个默认端点");
        for device in &devices {
            assert!(!device.id.is_empty(), "设备 ID 不应为空");
            assert!(!device.name.is_empty(), "设备名不应为空");
        }
        eprintln!("=== list_devices 输出（{} 个端点）===", devices.len());
        for device in &devices {
            eprintln!(
                "  [{:>7}] 默认={} 名={} id={}",
                format!("{:?}", device.kind),
                device.is_default,
                device.name,
                device.id
            );
        }
    }

    #[test]
    fn loopback_smoke_captures_own_render_output() {
        // 真机出声冒烟：默认跳过（cargo test 全量跑不得出声——CI/日常开发均静音）；
        // 仅在显式设置 HSE_ALLOW_REAL_AUDIO=1 时运行。
        if std::env::var("HSE_ALLOW_REAL_AUDIO").as_deref() != Ok("1") {
            eprintln!(
                "skip loopback_smoke_captures_own_render_output：真机出声测试需显式 HSE_ALLOW_REAL_AUDIO=1"
            );
            return;
        }
        let sample_rate = 48000u32;
        let block = 480u32; // 10ms @ 48kHz

        // ① 打开并启动默认渲染端点（xrun 计数初始必须为 0）
        let mut render = RenderStream::open(&OpenOptions {
            device_id: None,
            sample_rate,
            block_size_frames: block,
        })
        .expect("打开渲染流失败");
        assert_eq!(render.xruns(), 0, "渲染流 xrun 初始必须为 0");
        let render_format = render.start().expect("启动渲染流失败");
        assert_eq!(render_format.channels, 2, "渲染协商必须是立体声");

        // ② 打开并启动同一渲染端点的 loopback 捕获
        let mut loopback = LoopbackStream::open(&OpenOptions {
            device_id: None,
            sample_rate: render_format.sample_rate,
            block_size_frames: block,
        })
        .expect("打开回环流失败");
        assert_eq!(loopback.xruns(), 0, "回环流 xrun 初始必须为 0");
        let loop_format = loopback.start().expect("启动回环流失败");
        assert_eq!(loop_format.channels, 2, "回环协商必须是立体声");
        assert_eq!(
            loop_format.sample_rate, render_format.sample_rate,
            "回环与渲染应在同一采样率"
        );

        // ③ 交替“推一块正弦 + 尽力拉取”，模拟服务进程拦截-回注节奏；
        //    连续推送会让渲染缓冲填满，从而覆盖 push 的事件等待节流路径。
        let push_target_frames = render_format.sample_rate as usize * 6 / 10; // 600ms
        let mut phase = 0.0f64;
        let mut pushed_frames = 0usize;
        let mut collected_samples: Vec<f32> = Vec::new();
        let mut peak = 0f32;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut rx_block = [0f32; 960]; // 480 帧 × 2 声道
        while Instant::now() < deadline {
            if pushed_frames < push_target_frames {
                let mut tx_block = vec![0f32; block as usize * 2];
                fill_sine(&mut tx_block, &mut phase, render_format.sample_rate, 0.3);
                render.push(&tx_block).expect("push 失败");
                pushed_frames += block as usize;
            }
            match loopback.pull(&mut rx_block).expect("pull 失败") {
                0 => std::thread::sleep(Duration::from_millis(2)),
                got_frames => {
                    for &sample in &rx_block[..got_frames * 2] {
                        peak = peak.max(sample.abs());
                    }
                    collected_samples.extend_from_slice(&rx_block[..got_frames * 2]);
                }
            }
            if pushed_frames >= push_target_frames
                && collected_samples.len() >= push_target_frames * 2
            {
                break;
            }
        }

        // ④ stop 幂等性验证
        render.stop().expect("停止渲染流失败");
        loopback.stop().expect("停止回环流失败");
        render.stop().expect("渲染流二次 stop 必须无害");
        loopback.stop().expect("回环流二次 stop 必须无害");

        let captured_frames = collected_samples.len() / 2;
        eprintln!(
            "smoke 结果：pushed={pushed_frames} 帧, captured={captured_frames} 帧              ({:.1} ms), peak={peak:.4}, render_xruns={}, loop_xruns={}, 格式={render_format:?}/{loop_format:?}",
            captured_frames as f64 / f64::from(loop_format.sample_rate) * 1000.0,
            render.xruns(),
            loopback.xruns(),
        );

        let min_frames = push_target_frames / 4;
        assert!(
            captured_frames >= min_frames,
            "捕获帧数不足：{captured_frames} < 下限 {min_frames}"
        );
        assert!(
            peak > 0.01,
            "捕获信号峰值过低（{peak}）：疑似系统主音量为 0 或未捕到本进程回环声"
        );
    }
}
