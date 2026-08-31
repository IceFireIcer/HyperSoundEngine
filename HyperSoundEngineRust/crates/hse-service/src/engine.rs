//! 引擎编排核心：控制面可调用操作 + 数据面会话生命周期。
//!
//! 相位机 idle→starting→running→stopping→idle 全部收拢在 EngineInner 互斥锁
//! 内；锁从不跨越 join/sleep——begin_stop 在锁内取走线程句柄并置停机旗，
//! join 一律在锁外进行。数据面线程异常通过 err_flag 原子旗上浮，由事件中枢
//! 线程的 poll_supervision 兜底收尸（异常停机）。
//!
//! 启动时序：starting（可见）→ 数据面线程各自开流并握手回报协商格式 →
//! 引擎校验声道/构建初始子链经命令环送达 → ready 门开启 → running。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hrtf_core::{load_sofa_file, HrtfGrid, SofaGridOptions};
use hse_wasapi::{AccessMode, DeviceKind, OpenOptions};
use rtrb::Producer;
use serde_json::{json, Map, Value};

use crate::backend::BackendFactory;
use crate::dsp_chain::ServiceEngineChain;
use crate::params::{parse_pilot_params, PilotParams};
use crate::pipeline;
use crate::sessions::{SessionIdExhausted, SessionTable};
use crate::state::{CaptureConfig, Phase, ServiceConfig, ServiceEvent, StatsAtomic};

/// 控制面错误（JSON-RPC 错误码 + 中文消息）。
#[derive(Debug)]
pub struct RpcFault {
    pub code: i64,
    pub message: String,
}

impl RpcFault {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    /// -32602 参数无效。
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(-32602, message)
    }
    /// -32001 状态不允许。
    pub fn state_forbidden(message: impl Into<String>) -> Self {
        Self::new(-32001, message)
    }
    /// -32000 后端失败。
    pub fn backend_failed(message: impl Into<String>) -> Self {
        Self::new(-32000, message)
    }
}

const MIN_SAMPLE_RATE: u32 = 8_000;
const MAX_SAMPLE_RATE: u32 = 384_000;
const MIN_BLOCK_FRAMES: u32 = 16;
const MAX_BLOCK_FRAMES: u32 = 8_192;
/// 开流握手超时：覆盖设备打开/格式协商的合理上界。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

/// 运行期热更换所需的协商信息与命令环生产端。
pub struct HotState {
    sample_rate: f64,
    max_block: usize,
    chain_tx: Producer<ServiceEngineChain>,
}

struct EngineInner {
    phase: Phase,
    config: Option<ServiceConfig>,
    last_params_value: Option<Value>,
    pilot_params: PilotParams,
    hrtf_grid: Option<HrtfGrid>,
    hrtf_path: Option<PathBuf>,
    stats: Arc<StatsAtomic>,
    started_at: Option<Instant>,
    run_flag: Arc<AtomicBool>,
    err_flag: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
    hot: Option<HotState>,
}

/// 引擎句柄：rpc/server 直接持有的唯一入口。
pub struct EngineHandle {
    factory: Arc<dyn BackendFactory>,
    events: SyncSender<ServiceEvent>,
    /// 推流会话表（specs/service/push-stream.md）：与相位解耦，随句柄存活。
    sessions: Arc<SessionTable>,
    inner: Mutex<EngineInner>,
}

impl EngineHandle {
    pub fn new(factory: Arc<dyn BackendFactory>, events: SyncSender<ServiceEvent>) -> Self {
        let stats = Arc::new(StatsAtomic::default());
        let sessions = Arc::new(SessionTable::new(Arc::clone(&stats), events.clone()));
        Self {
            factory,
            events,
            sessions,
            inner: Mutex::new(EngineInner {
                phase: Phase::Idle,
                config: None,
                last_params_value: None,
                pilot_params: PilotParams::default(),
                hrtf_grid: None,
                hrtf_path: None,
                stats,
                started_at: None,
                run_flag: Arc::new(AtomicBool::new(false)),
                err_flag: Arc::new(AtomicBool::new(false)),
                workers: Vec::new(),
                hot: None,
            }),
        }
    }

    fn transition(&self, g: &mut EngineInner, to: Phase, publish: bool) {
        let from = g.phase;
        g.phase = to;
        if publish {
            // 异步数据面跃迁走广播；RPC 同步跃迁由连接线程按响应顺序直接发送。
            let _ = self.events.try_send(ServiceEvent::Phase {
                from: from.as_str().to_string(),
                to: to.as_str().to_string(),
            });
        }
    }

    /// listDevices：设备枚举，按类别分列。
    pub fn list_devices(&self) -> Result<Value, RpcFault> {
        let devs = self
            .factory
            .list_devices()
            .map_err(|e| RpcFault::backend_failed(e.to_string()))?;
        let mut render = Vec::new();
        let mut capture = Vec::new();
        for d in devs {
            let item = json!({"id": d.id, "name": d.name, "isDefault": d.is_default});
            match d.kind {
                DeviceKind::Render => render.push(item),
                DeviceKind::Capture => capture.push(item),
            }
        }
        Ok(json!({"render": render, "capture": capture}))
    }

    /// getState：相位 + 配置 + 统计 + 最近参数快照。
    pub fn get_state(&self) -> Value {
        let g = self.inner.lock().unwrap();
        let hrtf = match (&g.hrtf_grid, &g.hrtf_path) {
            (Some(grid), Some(path)) => json!({
                "loaded": true,
                "path": path.to_string_lossy(),
                "sampleRate": grid.sample_rate(),
                "azimuthCount": grid.azimuths().len(),
                "elevationCount": grid.elevations().len(),
                "hrirLength": grid.hrir_length(),
            }),
            _ => json!({"loaded": false}),
        };
        json!({
            "phase": g.phase.as_str(),
            "config": g.config.as_ref().map(|c| c.to_json()).unwrap_or(Value::Null),
            "stats": g.stats.snapshot(g.started_at),
            "sessions": self.sessions.diagnostics(),
            "lastParams": g.last_params_value.clone().unwrap_or(Value::Null),
            "hrtf": hrtf,
        })
    }

    /// loadHrtf：仅 idle 且已配置采样率时，从本地绝对 SOFA 路径构建并提交 grid。
    pub fn load_hrtf(&self, params_obj: &Map<String, Value>) -> Result<Value, RpcFault> {
        let mut g = self.inner.lock().unwrap();
        if g.phase != Phase::Idle {
            return Err(RpcFault::state_forbidden(format!(
                "phase={} 时禁止 loadHrtf（仅 idle 可加载）",
                g.phase.as_str()
            )));
        }
        let sample_rate = g
            .config
            .as_ref()
            .map(|config| config.sample_rate)
            .ok_or_else(|| RpcFault::state_forbidden("尚未成功 configure，禁止 loadHrtf"))?;
        if params_obj.len() != 1 {
            return Err(RpcFault::invalid_params("loadHrtf 只接受 path 字段"));
        }
        let raw_path = params_obj
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcFault::invalid_params("loadHrtf.path 必须为非空绝对路径字符串"))?;
        if raw_path.is_empty() {
            return Err(RpcFault::invalid_params(
                "loadHrtf.path 必须为非空绝对路径字符串",
            ));
        }
        let path = Path::new(raw_path);
        if !path.is_absolute() {
            return Err(RpcFault::invalid_params("loadHrtf.path 必须为绝对路径"));
        }
        if !path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("sofa"))
        {
            return Err(RpcFault::invalid_params(
                "loadHrtf.path 必须使用 .sofa 扩展名",
            ));
        }
        let metadata = std::fs::metadata(path).map_err(|error| {
            RpcFault::invalid_params(format!("loadHrtf.path 不可访问：{error}"))
        })?;
        if !metadata.is_file() {
            return Err(RpcFault::invalid_params("loadHrtf.path 必须指向普通文件"));
        }
        let canonical_path = std::fs::canonicalize(path).map_err(|error| {
            RpcFault::invalid_params(format!("loadHrtf.path 无法规范化：{error}"))
        })?;
        let options = SofaGridOptions {
            sample_rate,
            ..SofaGridOptions::default()
        };
        let grid = load_sofa_file(&canonical_path, &options)
            .map_err(|error| RpcFault::invalid_params(format!("SOFA 加载失败：{error}")))?;
        let result = json!({
            "loaded": true,
            "path": canonical_path.to_string_lossy(),
            "sampleRate": grid.sample_rate(),
            "azimuthCount": grid.azimuths().len(),
            "elevationCount": grid.elevations().len(),
            "hrirLength": grid.hrir_length(),
        });
        g.hrtf_grid = Some(grid);
        g.hrtf_path = Some(canonical_path);
        Ok(result)
    }

    /// xrun 总量（事件序列化用）。
    pub fn xrun_totals(&self) -> (u64, u64) {
        let g = self.inner.lock().unwrap();
        (
            g.stats.xruns_in.load(Ordering::Relaxed),
            g.stats.xruns_out.load(Ordering::Relaxed),
        )
    }

    /// 推流会话表（server 层二进制帧入口与测试使用）。
    pub fn sessions(&self) -> Arc<SessionTable> {
        Arc::clone(&self.sessions)
    }

    /// openSession：打开推流会话（specs/service/push-stream.md §3.1）。
    ///
    /// 校验次序对齐 configure 的先例——状态类先行、结构类随后：
    /// 未 configure（图未配置采样率）→ -32001；参数缺失/类型错/静态域非法
    /// （channels≠2、format≠"f32le"）与 sampleRate≠图采样率 → -32602；
    /// 全部通过才分配 id（被拒请求不消耗 id 空间）；u32 id 空间耗尽 → -32000。
    /// 会话可在任意 phase 打开，与引擎相位解耦。
    pub fn open_session(
        &self,
        params_obj: &Map<String, Value>,
        owner: u64,
    ) -> Result<Value, RpcFault> {
        let graph_rate = {
            let g = self.inner.lock().unwrap();
            g.config.as_ref().map(|c| c.sample_rate).ok_or_else(|| {
                RpcFault::state_forbidden("图未配置采样率（尚未成功 configure），禁止 openSession")
            })?
        };
        let sample_rate = match params_obj.get("sampleRate").and_then(|v| v.as_u64()) {
            Some(n) if n <= u32::MAX as u64 => n as u32,
            _ => return Err(RpcFault::invalid_params("sampleRate 必须为 u32 整数")),
        };
        if sample_rate != graph_rate {
            return Err(RpcFault::invalid_params(format!(
                "sampleRate 必须等于引擎图采样率 {}",
                graph_rate
            )));
        }
        let channels = match params_obj.get("channels").and_then(|v| v.as_u64()) {
            Some(n) if n <= u16::MAX as u64 => n as u16,
            _ => return Err(RpcFault::invalid_params("channels 必须为 u16 整数")),
        };
        if channels != 2 {
            return Err(RpcFault::invalid_params("channels must be 2"));
        }
        let format = match params_obj.get("format").and_then(|v| v.as_str()) {
            Some(f) => f,
            None => return Err(RpcFault::invalid_params("format 必须为字符串")),
        };
        if format != "f32le" {
            return Err(RpcFault::invalid_params("format 必须为 \"f32le\""));
        }
        let session_id = self
            .sessions
            .open(owner)
            .map_err(|SessionIdExhausted| RpcFault::backend_failed("会话 id 空间耗尽"))?;
        Ok(json!({
            "sessionId": session_id,
            "granted": {"sampleRate": sample_rate, "channels": channels, "format": format},
        }))
    }

    /// closeSession：关闭推流会话（specs/service/push-stream.md §3.2）。
    ///
    /// 生效即时：环内未消费块直接丢弃；未知/已关闭 id → -32602（重复 close 不幂等）。
    pub fn close_session(&self, params_obj: &Map<String, Value>) -> Result<Value, RpcFault> {
        let session_id = match params_obj.get("sessionId").and_then(|v| v.as_u64()) {
            Some(n) if n <= u32::MAX as u64 => n as u32,
            _ => return Err(RpcFault::invalid_params("sessionId 必须为 u32 整数")),
        };
        if self.sessions.close(session_id) {
            Ok(json!({"closed": true}))
        } else {
            Err(RpcFault::invalid_params(format!(
                "sessionId {} 不存在或已关闭",
                session_id
            )))
        }
    }

    /// framesProcessed 总量（测试/诊断用）。
    pub fn frames_processed(&self) -> u64 {
        self.inner
            .lock()
            .unwrap()
            .stats
            .frames_processed
            .load(Ordering::Relaxed)
    }

    /// configure：仅 phase=idle 可调；成功后保存配置快照并回显 applied。
    /// 校验顺序对齐 control-plane 规格 GWT-CP-06/07/08：相位(-32001) → 结构(-32602) → 后端枚举(-32000)。
    pub fn configure(&self, params_obj: &Map<String, Value>) -> Result<Value, RpcFault> {
        // 持锁覆盖校验与提交，保证并发 start 不会在设备枚举期间使用旧配置启动。
        let mut g = self.inner.lock().unwrap();
        if g.phase != Phase::Idle {
            return Err(RpcFault::state_forbidden(format!(
                "phase={} 时禁止 configure（仅 idle 可配）",
                g.phase.as_str()
            )));
        }
        let mode = match params_obj.get("mode").and_then(|v| v.as_str()) {
            Some(m @ ("loopback" | "capture")) => m,
            Some(m) => {
                return Err(RpcFault::invalid_params(format!(
                    "暂不支持 mode=\"{}\"（当前支持 loopback/capture）",
                    m
                )))
            }
            None => return Err(RpcFault::invalid_params("configure 缺少 mode 字符串")),
        };
        let parse_device = |key: &str, required: bool| -> Result<Option<String>, RpcFault> {
            match params_obj.get(key) {
                None if required => Err(RpcFault::invalid_params(format!("configure 缺少 {key}"))),
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
                Some(Value::String(_)) => Err(RpcFault::invalid_params(format!(
                    "{key} 不能为空字符串（用 null 表示默认设备）"
                ))),
                Some(_) => Err(RpcFault::invalid_params(format!(
                    "{key} 必须为字符串或 null"
                ))),
            }
        };
        let capture = match mode {
            "loopback" => {
                if params_obj.contains_key("captureDeviceId") {
                    return Err(RpcFault::invalid_params(
                        "loopback 模式不得携带 captureDeviceId",
                    ));
                }
                let render_device_id = parse_device("renderDeviceId", true)?;
                CaptureConfig::Loopback { render_device_id }
            }
            "capture" => {
                if params_obj.contains_key("renderDeviceId") {
                    return Err(RpcFault::invalid_params(
                        "capture 模式不得携带 renderDeviceId",
                    ));
                }
                let capture_device_id = parse_device("captureDeviceId", true)?;
                CaptureConfig::Capture { capture_device_id }
            }
            _ => unreachable!(),
        };
        let output_device_id_explicit = params_obj.contains_key("outputDeviceId");
        let output_device_id = parse_device("outputDeviceId", false)?;
        let access_mode_explicit = params_obj.contains_key("shareMode");
        let access_mode = match params_obj.get("shareMode") {
            None => AccessMode::Shared,
            Some(Value::String(s)) if s == "shared" => AccessMode::Shared,
            Some(Value::String(s)) if s == "exclusive" => AccessMode::Exclusive,
            Some(Value::String(_)) => {
                return Err(RpcFault::invalid_params(
                    "shareMode 仅支持 shared 或 exclusive",
                ))
            }
            Some(_) => return Err(RpcFault::invalid_params("shareMode 必须为字符串")),
        };
        if matches!(capture, CaptureConfig::Loopback { .. }) && access_mode == AccessMode::Exclusive
        {
            return Err(RpcFault::invalid_params(
                "loopback 不支持 shareMode=exclusive",
            ));
        }

        let sample_rate = match params_obj.get("sampleRate").and_then(|v| v.as_u64()) {
            Some(n) if n >= MIN_SAMPLE_RATE as u64 && n <= MAX_SAMPLE_RATE as u64 => n as u32,
            _ => {
                return Err(RpcFault::invalid_params(format!(
                    "sampleRate 必须为 {}..={} 的整数",
                    MIN_SAMPLE_RATE, MAX_SAMPLE_RATE
                )))
            }
        };
        let block_size_frames = match params_obj.get("blockSizeFrames").and_then(|v| v.as_u64()) {
            Some(n) if n >= MIN_BLOCK_FRAMES as u64 && n <= MAX_BLOCK_FRAMES as u64 => n as u32,
            _ => {
                return Err(RpcFault::invalid_params(format!(
                    "blockSizeFrames 必须为 {}..={} 的整数",
                    MIN_BLOCK_FRAMES, MAX_BLOCK_FRAMES
                )))
            }
        };

        let explicit_device = match &capture {
            CaptureConfig::Loopback { render_device_id } => render_device_id.is_some(),
            CaptureConfig::Capture { capture_device_id } => capture_device_id.is_some(),
        } || output_device_id.is_some();
        if explicit_device {
            let devices = self
                .factory
                .list_devices()
                .map_err(|e| RpcFault::backend_failed(e.to_string()))?;
            let validate_device = |id: &Option<String>, kind: DeviceKind, key: &str| {
                if let Some(id) = id {
                    if !devices.iter().any(|d| d.kind == kind && d.id == *id) {
                        return Err(RpcFault::backend_failed(format!(
                            "{key} 不在对应设备类别的当前枚举中：{id}"
                        )));
                    }
                }
                Ok(())
            };
            match &capture {
                CaptureConfig::Loopback { render_device_id } => {
                    validate_device(render_device_id, DeviceKind::Render, "renderDeviceId")?;
                }
                CaptureConfig::Capture { capture_device_id } => {
                    validate_device(capture_device_id, DeviceKind::Capture, "captureDeviceId")?;
                }
            }
            validate_device(&output_device_id, DeviceKind::Render, "outputDeviceId")?;
        }

        // 控制面请求在同一互斥区内完成相位守卫、设备校验与提交。
        let cfg = ServiceConfig {
            capture,
            output_device_id,
            output_device_id_explicit,
            sample_rate,
            block_size_frames,
            access_mode,
            access_mode_explicit,
        };
        let applied = cfg.to_json();
        if g.config
            .as_ref()
            .is_some_and(|previous| previous.sample_rate != cfg.sample_rate)
        {
            g.hrtf_grid = None;
            g.hrtf_path = None;
        }
        g.config = Some(cfg);
        Ok(json!({"applied": applied}))
    }

    /// start：idle → starting（可见）→ 开流握手 → 装链 → running。
    pub fn start(&self) -> Result<Value, RpcFault> {
        // 第一段：校验并置 starting，取出配置与参数快照及共享件克隆。
        let (cfg, pilot, wire_source, hrtf_grid, run_flag, err_flag, stats, ready_gate) = {
            let mut g = self.inner.lock().unwrap();
            if g.phase != Phase::Idle {
                return Err(RpcFault::state_forbidden(format!(
                    "phase={} 时禁止 start",
                    g.phase.as_str()
                )));
            }
            let cfg = match g.config.clone() {
                Some(c) => c,
                None => return Err(RpcFault::state_forbidden("尚未成功 configure，禁止 start")),
            };
            self.transition(&mut g, Phase::Starting, false);
            g.err_flag.store(false, Ordering::Relaxed);
            g.run_flag.store(true, Ordering::SeqCst);
            g.started_at = None;
            let gate = Arc::new(AtomicBool::new(false));
            (
                cfg,
                g.pilot_params.clone(),
                g.last_params_value.clone().unwrap_or_else(|| json!({})),
                g.hrtf_grid.clone(),
                Arc::clone(&g.run_flag),
                Arc::clone(&g.err_flag),
                Arc::clone(&g.stats),
                gate,
            )
        };

        // 第二段：锁外完成开流握手、建环、装配初始链。任一步失败即整体回滚。
        match self.launch_pipeline(
            &cfg,
            &pilot,
            &wire_source,
            hrtf_grid,
            &run_flag,
            &err_flag,
            &stats,
            &ready_gate,
        ) {
            Ok((workers, chain_tx, negotiated_rate, block_frames)) => {
                // 第三段：复查并发 stop 未打断，随后安装运行态并放行数据面。
                let mut g = self.inner.lock().unwrap();
                if g.phase != Phase::Starting {
                    // start 过程中收到 stop：停旗、收线程、维持停机结果。
                    drop(chain_tx);
                    run_flag.store(false, Ordering::SeqCst);
                    for handle in workers {
                        let _ = handle.join();
                    }
                    return Err(RpcFault::state_forbidden(
                        "start 过程中收到 stop，已中止本次启动",
                    ));
                }
                g.workers = workers;
                g.hot = Some(HotState {
                    sample_rate: negotiated_rate,
                    max_block: block_frames,
                    chain_tx,
                });
                g.started_at = Some(Instant::now());
                g.stats.reset_cycle();
                ready_gate.store(true, Ordering::SeqCst);
                self.transition(&mut g, Phase::Running, false);
                Ok(json!({"started": true}))
            }
            Err((message, workers)) => {
                run_flag.store(false, Ordering::SeqCst);
                for handle in workers {
                    let _ = handle.join();
                }
                self.abort_start(&message);
                Err(RpcFault::backend_failed(message))
            }
        }
    }

    /// 第二段实现：拉起线程、握手、验声道、装初始链。失败返回 (原因, 已产出的线程句柄)。
    #[allow(clippy::type_complexity)]
    fn launch_pipeline(
        &self,
        cfg: &ServiceConfig,
        pilot: &PilotParams,
        wire_source: &Value,
        hrtf_grid: Option<HrtfGrid>,
        run_flag: &Arc<AtomicBool>,
        _err_flag: &Arc<AtomicBool>,
        stats: &Arc<StatsAtomic>,
        ready_gate: &Arc<AtomicBool>,
    ) -> Result<
        (
            Vec<JoinHandle<()>>,
            Producer<ServiceEngineChain>,
            f64,
            usize,
        ),
        (String, Vec<JoinHandle<()>>),
    > {
        let block_frames = cfg.block_size_frames as usize;
        let capture_device_id = match &cfg.capture {
            CaptureConfig::Loopback { render_device_id } => render_device_id.clone(),
            CaptureConfig::Capture { capture_device_id } => capture_device_id.clone(),
        };
        let capture_opts = OpenOptions {
            device_id: capture_device_id,
            sample_rate: cfg.sample_rate,
            block_size_frames: cfg.block_size_frames,
            access_mode: cfg.access_mode,
        };
        let render_opts = OpenOptions {
            device_id: cfg.output_device_id.clone(),
            sample_rate: cfg.sample_rate,
            block_size_frames: cfg.block_size_frames,
            access_mode: cfg.access_mode,
        };
        let (in_prod, in_cons, out_prod, out_cons) = pipeline::build_rings(block_frames);
        let (mut chain_tx, chain_rx) = pipeline::build_command_ring();

        let deps = pipeline::WorkerDeps {
            run_flag: Arc::clone(run_flag),
            err_flag: Arc::clone(_err_flag),
            ready_gate: Arc::clone(ready_gate),
            stats: Arc::clone(stats),
            events: self.events.clone(),
        };
        let capture_opener = match &cfg.capture {
            CaptureConfig::Loopback { .. } => self.factory.loopback_opener(&capture_opts),
            CaptureConfig::Capture { .. } => self.factory.capture_opener(&capture_opts),
        };
        let render_opener = self.factory.render_opener(&render_opts);
        let mut handles = pipeline::spawn_workers(
            capture_opener,
            render_opener,
            in_prod,
            in_cons,
            out_prod,
            out_cons,
            chain_rx,
            block_frames,
            deps,
            Arc::clone(&self.sessions),
        );

        // 握手：等待两条流的开流结果（带超时）。失败则交还句柄给调用方收尾。
        let fail = |message: String, handles: Vec<JoinHandle<()>>| (message, handles);
        let cap_fmt = match wait_ready(&handles.capture_ready) {
            Ok(f) => f,
            Err(m) => return Err(fail(m, std::mem::take(&mut handles.handles))),
        };
        let ren_fmt = match wait_ready(&handles.render_ready) {
            Ok(f) => f,
            Err(m) => return Err(fail(m, std::mem::take(&mut handles.handles))),
        };
        if cap_fmt.channels != 2 || ren_fmt.channels != 2 {
            return Err(fail(
                format!(
                    "仅支持立体声端点（捕获 {}ch / 渲染 {}ch）",
                    cap_fmt.channels, ren_fmt.channels
                ),
                std::mem::take(&mut handles.handles),
            ));
        }
        if cap_fmt.sample_rate != cfg.sample_rate || ren_fmt.sample_rate != cfg.sample_rate {
            return Err(fail(
                format!(
                    "捕获与渲染采样率必须等于配置值 {}Hz（捕获 {}Hz / 渲染 {}Hz）",
                    cfg.sample_rate, cap_fmt.sample_rate, ren_fmt.sample_rate
                ),
                std::mem::take(&mut handles.handles),
            ));
        }

        // 以两端一致的协商采样率为 DSP 链基准，构建初始完整链并经命令环送达 DSP 线程。
        let negotiated = ren_fmt.sample_rate as f64;
        let canonical = match pilot.to_canonical_json(wire_source, negotiated) {
            Ok(value) => value,
            Err(e) => {
                return Err(fail(
                    format!("构建完整引擎参数失败：{}", e),
                    std::mem::take(&mut handles.handles),
                ));
            }
        };
        let chain = match ServiceEngineChain::build_with_hrtf_grid(
            &canonical,
            negotiated,
            block_frames,
            hrtf_grid,
        ) {
            Ok(c) => c,
            Err(e) => {
                return Err(fail(
                    format!("构建完整引擎链失败：{}", e),
                    std::mem::take(&mut handles.handles),
                ));
            }
        };
        if let Err(rtrb::PushError::Full(_)) = chain_tx.push(chain) {
            // 全新命令环不可能满；防御性兜底。
            return Err(fail(
                "命令环异常（内部错误）".into(),
                std::mem::take(&mut handles.handles),
            ));
        }
        Ok((
            std::mem::take(&mut handles.handles),
            chain_tx,
            negotiated,
            block_frames,
        ))
    }

    fn abort_start(&self, reason: &str) {
        let mut g = self.inner.lock().unwrap();
        g.run_flag.store(false, Ordering::SeqCst);
        if g.phase == Phase::Starting {
            self.transition(&mut g, Phase::Idle, false);
        }
        eprintln!("[hse-service] start 失败已回滚：{}", reason);
    }

    /// begin_stop：内部停机入口。公开 RPC 只允许 Running；析构可额外清理 Starting。
    fn begin_stop(
        &self,
        publish: bool,
        allow_starting: bool,
    ) -> Result<Vec<JoinHandle<()>>, RpcFault> {
        let mut g = self.inner.lock().unwrap();
        let allowed = g.phase == Phase::Running || (allow_starting && g.phase == Phase::Starting);
        if !allowed {
            return Err(RpcFault::state_forbidden(format!(
                "phase={} 时禁止 stop",
                g.phase.as_str()
            )));
        }
        self.transition(&mut g, Phase::Stopping, publish);
        g.run_flag.store(false, Ordering::SeqCst);
        if let Some(hot) = g.hot.as_ref() {
            // 命令环随 HotState 一并失效；DSP 线程靠 run_flag 退出。
            let _ = hot;
        }
        g.hot = None;
        Ok(std::mem::take(&mut g.workers))
    }

    fn finish_stop(&self, publish: bool) {
        let mut g = self.inner.lock().unwrap();
        if g.phase == Phase::Stopping {
            g.started_at = None;
            g.stats.clear_current_depths();
            self.transition(&mut g, Phase::Idle, publish);
        }
    }

    /// stop：仅 running → stopping → join → idle。
    pub fn stop(&self) -> Result<Value, RpcFault> {
        let workers = self.begin_stop(false, false)?;
        for handle in workers {
            let _ = handle.join();
        }
        self.finish_stop(false);
        Ok(json!({"stopped": true}))
    }

    /// 事件中枢周期调用：数据面异常（err_flag 或线程提前退出）时兜底停机。
    pub fn poll_supervision(&self) {
        let needs_stop = {
            let g = self.inner.lock().unwrap();
            g.phase == Phase::Running
                && (g.err_flag.load(Ordering::Relaxed) || g.workers.iter().any(|h| h.is_finished()))
        };
        if !needs_stop {
            return;
        }
        eprintln!("[hse-service] 数据面异常退出，执行兜底停机");
        if let Ok(workers) = self.begin_stop(true, true) {
            for handle in workers {
                let _ = handle.join();
            }
            self.finish_stop(true);
        }
    }

    /// setParams：候选快照先解析、构链/prepare，并在运行态成功投递后才原子提交状态。
    pub fn set_params(&self, params_value: &Value) -> Result<Value, RpcFault> {
        let (pilot, warnings) =
            parse_pilot_params(params_value).map_err(RpcFault::invalid_params)?;
        let warnings_json: Vec<Value> = warnings.iter().map(|w| json!(w)).collect();
        let mut g = self.inner.lock().unwrap();

        let built = if g.phase == Phase::Running {
            let hot = g
                .hot
                .as_ref()
                .ok_or_else(|| RpcFault::backend_failed("运行态缺失热更换通道（内部错误）"))?;
            let canonical = pilot
                .to_canonical_json(params_value, hot.sample_rate)
                .map_err(|e| RpcFault::invalid_params(format!("参数无法应用：{}", e)))?;
            let previous_canonical = g
                .last_params_value
                .as_ref()
                .map(|value| g.pilot_params.to_canonical_json(value, hot.sample_rate))
                .transpose()
                .map_err(|e| RpcFault::invalid_params(format!("上一参数无法应用：{}", e)))?;
            Some(
                ServiceEngineChain::build_with_hrtf_grid_and_previous(
                    &canonical,
                    hot.sample_rate,
                    hot.max_block,
                    g.hrtf_grid.clone(),
                    previous_canonical.as_ref(),
                )
                .map_err(|e| RpcFault::invalid_params(format!("参数无法应用：{}", e)))?,
            )
        } else {
            None
        };

        if let Some(chain) = built {
            g.hot
                .as_mut()
                .expect("运行态热更换通道已在上方确认")
                .chain_tx
                .push(chain)
                .map_err(|_| RpcFault::backend_failed("参数命令环已满，本条快照未提交"))?;
        }
        let canonical = pilot.to_wire_json(params_value);
        g.pilot_params = pilot;
        g.last_params_value = Some(canonical);
        Ok(json!({"accepted": true, "warnings": warnings_json}))
    }
}

/// 等待一条握手通道给出开流结果。
fn wait_ready(
    rx: &Receiver<Result<hse_wasapi::StreamFormat, String>>,
) -> Result<hse_wasapi::StreamFormat, String> {
    match rx.recv_timeout(HANDSHAKE_TIMEOUT) {
        Ok(Ok(fmt)) => Ok(fmt),
        Ok(Err(m)) => Err(m),
        Err(_) => Err(format!(
            "开流握手超时（{} 秒）",
            HANDSHAKE_TIMEOUT.as_secs()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_backend::FakeFactory;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    use std::sync::mpsc::sync_channel;

    static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn configured_engine() -> EngineHandle {
        let (events, _event_rx) = sync_channel(16);
        let engine =
            EngineHandle::new(FakeFactory::working(Duration::ZERO, Duration::ZERO), events);
        engine
            .configure(
                json!({"mode":"loopback","renderDeviceId":null,"sampleRate":48000,"blockSizeFrames":64})
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        engine
    }

    fn temporary_sofa_path() -> PathBuf {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, AtomicOrdering::Relaxed);
        std::env::temp_dir().join(format!(
            "hse-service-invalid-{}-{sequence}.sofa",
            std::process::id()
        ))
    }

    fn test_grid() -> HrtfGrid {
        HrtfGrid::new(
            48_000,
            vec![-30.0, 30.0],
            vec![0.0],
            3,
            vec![1.0, 0.5, 0.0, 0.25, 0.0, 0.0],
            vec![0.25, 0.0, 0.0, 1.0, 0.5, 0.0],
        )
        .unwrap()
    }

    #[test]
    fn 预载grid贯通start与运行态块边界热换() {
        let engine = configured_engine();
        {
            let mut inner = engine.inner.lock().unwrap();
            inner.hrtf_grid = Some(test_grid());
            inner.hrtf_path = Some(PathBuf::from("test-grid.sofa"));
        }
        engine
            .set_params(&json!({"spatial":{"mode":"instant"}}))
            .unwrap();
        engine.start().expect("start 应使用预载 grid 构建 stage 22");
        engine
            .set_params(
                &json!({"spatial":{"mode":"world","convolution":"time","world":{
                    "listener":{"position":{"x":0,"y":1.6,"z":0},"yaw":0,"pitch":0,"roll":0},
                    "sources":[{"id":"lead","position":{"x":0,"y":1.6,"z":4},"gain":1,"size":0.4}],
                    "playhead":1,"trajectories":[],"occlusion":0.2
                }}}),
            )
            .expect("running setParams 应预建 world 链并投递到块边界");
        engine
            .set_params(
                &json!({"spatial":{"mode":"world","convolution":"time","world":{
                    "listener":{"position":{"x":1,"y":1.6,"z":0},"yaw":15,"pitch":5,"roll":-2},
                    "sources":[{"id":"lead","position":{"x":0,"y":1.6,"z":4},"gain":1,"size":0.4}],
                    "playhead":2,"trajectories":[],"occlusion":0.2
                }}}),
            )
            .expect("后续 world 快照应使用上一快照推导确定速度");
        engine
            .set_params(
                &json!({"spatial":{"mode":"stage","convolution":"time","stage":{
                    "preset":"cinema","seat":"back","roomSize":1.5,"reverbAmount":0.6,
                    "customSources":[]
                }}}),
            )
            .expect("running setParams 应预建 stage 链并投递到块边界");
        assert_eq!(engine.get_state()["lastParams"]["spatial"]["mode"], "stage");
        engine.stop().unwrap();
    }

    #[test]
    fn configure_采样率变化清除旧grid_同采样率保留() {
        let engine = configured_engine();
        {
            let mut inner = engine.inner.lock().unwrap();
            inner.hrtf_grid = Some(test_grid());
            inner.hrtf_path = Some(PathBuf::from("test-grid.sofa"));
        }
        engine
            .configure(
                json!({"mode":"loopback","renderDeviceId":null,"sampleRate":48000,"blockSizeFrames":128})
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(engine.get_state()["hrtf"]["loaded"], true);

        engine
            .configure(
                json!({"mode":"loopback","renderDeviceId":null,"sampleRate":44100,"blockSizeFrames":128})
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(engine.get_state()["hrtf"], json!({"loaded":false}));
    }

    #[test]
    fn load_hrtf_拒绝相对路径与临时非法文件且不提交状态() {
        let engine = configured_engine();
        let relative = json!({"path":"fixture.sofa"});
        let error = engine.load_hrtf(relative.as_object().unwrap()).unwrap_err();
        assert_eq!(error.code, -32602);
        assert!(error.message.contains("绝对路径"));
        assert_eq!(engine.get_state()["hrtf"], json!({"loaded":false}));

        let path = temporary_sofa_path();
        fs::write(&path, b"not a sofa file").unwrap();
        let request = json!({"path":path.to_string_lossy()});
        let error = engine.load_hrtf(request.as_object().unwrap()).unwrap_err();
        fs::remove_file(&path).unwrap();
        assert_eq!(error.code, -32602);
        assert!(error.message.contains("SOFA 加载失败"));
        assert_eq!(engine.get_state()["hrtf"], json!({"loaded":false}));
    }

    #[test]
    fn load_hrtf_未配置与运行态按状态拒绝() {
        let (events, _event_rx) = sync_channel(16);
        let engine =
            EngineHandle::new(FakeFactory::working(Duration::ZERO, Duration::ZERO), events);
        let path = std::env::temp_dir().join("unused.sofa");
        let request = json!({"path":path.to_string_lossy()});
        assert_eq!(
            engine
                .load_hrtf(request.as_object().unwrap())
                .unwrap_err()
                .code,
            -32001
        );

        engine
            .configure(
                json!({"mode":"loopback","renderDeviceId":null,"sampleRate":48000,"blockSizeFrames":64})
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        engine.inner.lock().unwrap().phase = Phase::Running;
        assert_eq!(
            engine
                .load_hrtf(request.as_object().unwrap())
                .unwrap_err()
                .code,
            -32001
        );
        engine.inner.lock().unwrap().phase = Phase::Idle;
    }

    #[test]
    #[ignore = "需要 HSE_TEST_SOFA 指向本地真实 SimpleFreeFieldHRIR 文件"]
    fn load_hrtf_真实sofa环境变量验收() {
        let path = std::env::var("HSE_TEST_SOFA").expect("请设置 HSE_TEST_SOFA");
        let engine = configured_engine();
        let result = engine
            .load_hrtf(json!({"path":path}).as_object().unwrap())
            .expect("真实 SOFA 应在控制路径加载成功");
        assert_eq!(result["loaded"], true);
        assert_eq!(result["sampleRate"], 48_000);
        assert_eq!(engine.get_state()["hrtf"]["loaded"], true);
    }

    #[test]
    fn starting状态公开stop必须拒绝() {
        let (events, _event_rx) = sync_channel(16);
        let engine =
            EngineHandle::new(FakeFactory::working(Duration::ZERO, Duration::ZERO), events);
        engine.inner.lock().unwrap().phase = Phase::Starting;

        let err = engine.stop().unwrap_err();
        assert_eq!(err.code, -32001);
        assert_eq!(engine.get_state()["phase"], "starting");
    }

    #[test]
    fn set_params_命令投递失败不提交候选状态() {
        let (events, _event_rx) = sync_channel(16);
        let engine =
            EngineHandle::new(FakeFactory::working(Duration::ZERO, Duration::ZERO), events);
        let old_value = json!({"reverbRoute": "off"});
        let (old_params, _) = parse_pilot_params(&old_value).unwrap();
        let (mut chain_tx, _chain_rx) = pipeline::build_command_ring();
        for _ in 0..4 {
            let canonical = old_params.to_canonical_json(&old_value, 48_000.0).unwrap();
            let filler = ServiceEngineChain::build(&canonical, 48_000.0, 64).unwrap();
            assert!(chain_tx.push(filler).is_ok());
        }
        {
            let mut g = engine.inner.lock().unwrap();
            g.phase = Phase::Running;
            g.pilot_params = old_params;
            g.last_params_value = Some(old_value.clone());
            g.hot = Some(HotState {
                sample_rate: 48_000.0,
                max_block: 64,
                chain_tx,
            });
        }

        let candidate = json!({"limiter": {"enabled": false}});
        let err = engine.set_params(&candidate).unwrap_err();
        assert_eq!(err.code, -32000);
        assert_eq!(engine.get_state()["lastParams"], old_value);
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        // 句柄销毁时尽力停掉仍在运行的数据面（避免测试/异常路径泄漏线程）。
        if let Ok(workers) = self.begin_stop(true, true) {
            for handle in workers {
                let _ = handle.join();
            }
            self.finish_stop(true);
        }
    }
}
