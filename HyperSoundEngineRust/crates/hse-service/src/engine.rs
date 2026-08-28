//! 引擎编排核心：控制面可调用操作 + 数据面会话生命周期。
//!
//! 相位机 idle→starting→running→stopping→idle 全部收拢在 EngineInner 互斥锁
//! 内；锁从不跨越 join/sleep——begin_stop 在锁内取走线程句柄并置停机旗，
//! join 一律在锁外进行。数据面线程异常通过 err_flag 原子旗上浮，由事件中枢
//! 线程的 poll_supervision 兜底收尸（异常停机）。
//!
//! 启动时序：starting（可见）→ 数据面线程各自开流并握手回报协商格式 →
//! 引擎校验声道/构建初始子链经命令环送达 → ready 门开启 → running。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hse_wasapi::{DeviceKind, OpenOptions};
use rtrb::Producer;
use serde_json::{json, Map, Value};

use crate::backend::BackendFactory;
use crate::dsp_chain::PilotSubchain;
use crate::params::{parse_pilot_params, PilotParams};
use crate::pipeline;
use crate::state::{Phase, ServiceConfig, ServiceEvent, StatsAtomic};

/// 控制面错误（JSON-RPC 错误码 + 中文消息）。
#[derive(Debug)]
pub struct RpcFault {
    pub code: i64,
    pub message: String,
}

impl RpcFault {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
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
    chain_tx: Producer<PilotSubchain>,
}

struct EngineInner {
    phase: Phase,
    config: Option<ServiceConfig>,
    last_params_value: Option<Value>,
    pilot_params: PilotParams,
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
    inner: Mutex<EngineInner>,
}

impl EngineHandle {
    pub fn new(factory: Arc<dyn BackendFactory>, events: SyncSender<ServiceEvent>) -> Self {
        Self {
            factory,
            events,
            inner: Mutex::new(EngineInner {
                phase: Phase::Idle,
                config: None,
                last_params_value: None,
                pilot_params: PilotParams::default(),
                stats: Arc::new(StatsAtomic::default()),
                started_at: None,
                run_flag: Arc::new(AtomicBool::new(false)),
                err_flag: Arc::new(AtomicBool::new(false)),
                workers: Vec::new(),
                hot: None,
            }),
        }
    }

    fn transition(&self, g: &mut EngineInner, to: Phase) {
        let from = g.phase;
        g.phase = to;
        // 事件通道满则丢弃通知（权威状态以 getState 为准）。
        let _ = self.events.try_send(ServiceEvent::Phase {
            from: from.as_str().to_string(),
            to: to.as_str().to_string(),
        });
    }

    /// listDevices：设备枚举，按类别分列。
    pub fn list_devices(&self) -> Result<Value, RpcFault> {
        let devs = self.factory.list_devices().map_err(|e| RpcFault::backend_failed(e.to_string()))?;
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
        json!({
            "phase": g.phase.as_str(),
            "config": g.config.as_ref().map(|c| c.to_json()).unwrap_or(Value::Null),
            "stats": g.stats.snapshot(g.started_at),
            "lastParams": g.last_params_value.clone().unwrap_or(Value::Null),
        })
    }

    /// xrun 总量（事件序列化用）。
    pub fn xrun_totals(&self) -> (u64, u64) {
        let g = self.inner.lock().unwrap();
        (
            g.stats.xruns_in.load(Ordering::Relaxed),
            g.stats.xruns_out.load(Ordering::Relaxed),
        )
    }

    /// framesProcessed 总量（测试/诊断用）。
    pub fn frames_processed(&self) -> u64 {
        self.inner.lock().unwrap().stats.frames_processed.load(Ordering::Relaxed)
    }

    /// configure：仅 phase=idle 可调；成功后保存配置快照并回显 applied。
    /// 校验顺序对齐 control-plane 规格 GWT-CP-06/07/08：相位(-32001) → 结构(-32602) → 后端枚举(-32000)。
    pub fn configure(&self, params_obj: &Map<String, Value>) -> Result<Value, RpcFault> {
        // 相位守卫最先：非 idle 时无论内容是否合法一律 -32001，且状态（含既有 config）不变
        {
            let g = self.inner.lock().unwrap();
            if g.phase != Phase::Idle {
                return Err(RpcFault::state_forbidden(format!(
                    "phase={} 时禁止 configure（仅 idle 可配）", g.phase.as_str()
                )));
            }
        }
        let mode = match params_obj.get("mode").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => return Err(RpcFault::invalid_params("configure 缺少 mode 字符串")),
        };
        if mode != "loopback" {
            return Err(RpcFault::invalid_params(format!(
                "暂不支持 mode=\"{}\"（当前仅 loopback）", mode
            )));
        }
        let render_device_id = match params_obj.get("renderDeviceId") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.is_empty() => {
                // GWT-CP-08：非 null 引用必须命中当前渲染端点枚举（后端参与校验，失败 -32000 且 config 不变）
                let devs = self
                    .factory
                    .list_devices()
                    .map_err(|e| RpcFault::backend_failed(e.to_string()))?;
                if !devs.iter().any(|d| d.kind == DeviceKind::Render && d.id == *s) {
                    return Err(RpcFault::backend_failed(format!(
                        "renderDeviceId 不在当前渲染端点枚举中：{}", s
                    )));
                }
                Some(s.clone())
            }
            Some(Value::String(_)) => {
                return Err(RpcFault::invalid_params("renderDeviceId 不能为空字符串（用 null 表示默认设备）"));
            }
            Some(_) => return Err(RpcFault::invalid_params("renderDeviceId 必须为字符串或 null")),
        };
        let sample_rate = match params_obj.get("sampleRate").and_then(|v| v.as_u64()) {
            Some(n) if n >= MIN_SAMPLE_RATE as u64 && n <= MAX_SAMPLE_RATE as u64 => n as u32,
            _ => return Err(RpcFault::invalid_params(format!("sampleRate 必须为 {}..={} 的整数", MIN_SAMPLE_RATE, MAX_SAMPLE_RATE))),
        };
        let block_size_frames = match params_obj.get("blockSizeFrames").and_then(|v| v.as_u64()) {
            Some(n) if n >= MIN_BLOCK_FRAMES as u64 && n <= MAX_BLOCK_FRAMES as u64 => n as u32,
            _ => return Err(RpcFault::invalid_params(format!("blockSizeFrames 必须为 {}..={} 的整数", MIN_BLOCK_FRAMES, MAX_BLOCK_FRAMES))),
        };

        // 相位已在开头守卫；控制面串行处理（规格 §九），期间相位不可能变化
        let mut g = self.inner.lock().unwrap();
        let cfg = ServiceConfig { mode, render_device_id, sample_rate, block_size_frames };
        let applied = cfg.to_json();
        g.config = Some(cfg);
        Ok(json!({"applied": applied}))
    }

    /// start：idle → starting（可见）→ 开流握手 → 装链 → running。
    pub fn start(&self) -> Result<Value, RpcFault> {
        // 第一段：校验并置 starting，取出配置与参数快照及共享件克隆。
        let (cfg, pilot, run_flag, err_flag, stats, ready_gate) = {
            let mut g = self.inner.lock().unwrap();
            if g.phase != Phase::Idle {
                return Err(RpcFault::state_forbidden(format!("phase={} 时禁止 start", g.phase.as_str())));
            }
            let cfg = match g.config.clone() {
                Some(c) => c,
                None => return Err(RpcFault::state_forbidden("尚未成功 configure，禁止 start")),
            };
            self.transition(&mut g, Phase::Starting);
            g.err_flag.store(false, Ordering::Relaxed);
            g.run_flag.store(true, Ordering::SeqCst);
            g.started_at = None;
            let gate = Arc::new(AtomicBool::new(false));
            (cfg, g.pilot_params.clone(), Arc::clone(&g.run_flag), Arc::clone(&g.err_flag), Arc::clone(&g.stats), gate)
        };

        // 第二段：锁外完成开流握手、建环、装配初始链。任一步失败即整体回滚。
        match self.launch_pipeline(&cfg, &pilot, &run_flag, &err_flag, &stats, &ready_gate) {
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
                    return Err(RpcFault::state_forbidden("start 过程中收到 stop，已中止本次启动"));
                }
                g.workers = workers;
                g.hot = Some(HotState { sample_rate: negotiated_rate, max_block: block_frames, chain_tx });
                g.started_at = Some(Instant::now());
                ready_gate.store(true, Ordering::SeqCst);
                self.transition(&mut g, Phase::Running);
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
        run_flag: &Arc<AtomicBool>,
        _err_flag: &Arc<AtomicBool>,
        stats: &Arc<StatsAtomic>,
        ready_gate: &Arc<AtomicBool>,
    ) -> Result<(Vec<JoinHandle<()>>, Producer<PilotSubchain>, f64, usize), (String, Vec<JoinHandle<()>>)> {
        let block_frames = cfg.block_size_frames as usize;
        let opts = OpenOptions {
            device_id: cfg.render_device_id.clone(),
            sample_rate: cfg.sample_rate,
            block_size_frames: cfg.block_size_frames,
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
        let capture_opener = self.factory.loopback_opener(&opts);
        let render_opener = self.factory.render_opener(&opts);
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
                format!("仅支持立体声端点（捕获 {}ch / 渲染 {}ch）", cap_fmt.channels, ren_fmt.channels),
                std::mem::take(&mut handles.handles),
            ));
        }

        // 以渲染端协商采样率为 DSP 链基准，构建初始子链并经命令环送达 DSP 线程。
        let negotiated = ren_fmt.sample_rate as f64;
        let chain = match PilotSubchain::build(pilot, negotiated, block_frames) {
            Ok(c) => c,
            Err(e) => {
                return Err(fail(format!("构建试点子链失败：{}", e), std::mem::take(&mut handles.handles)));
            }
        };
        if let Err(rtrb::PushError::Full(_)) = chain_tx.push(chain) {
            // 全新命令环不可能满；防御性兜底。
            return Err(fail("命令环异常（内部错误）".into(), std::mem::take(&mut handles.handles)));
        }
        Ok((std::mem::take(&mut handles.handles), chain_tx, negotiated, block_frames))
    }

    fn abort_start(&self, reason: &str) {
        let mut g = self.inner.lock().unwrap();
        g.run_flag.store(false, Ordering::SeqCst);
        if g.phase == Phase::Starting {
            self.transition(&mut g, Phase::Idle);
        }
        eprintln!("[hse-service] start 失败已回滚：{}", reason);
    }

    /// begin_stop：置 stopping、发停机旗、取走线程句柄（锁内不做 join）。
    fn begin_stop(&self) -> Result<Vec<JoinHandle<()>>, RpcFault> {
        let mut g = self.inner.lock().unwrap();
        match g.phase {
            Phase::Running | Phase::Starting => {}
            Phase::Idle | Phase::Stopping => {
                return Err(RpcFault::state_forbidden(format!("phase={} 时禁止 stop", g.phase.as_str())));
            }
        }
        self.transition(&mut g, Phase::Stopping);
        g.run_flag.store(false, Ordering::SeqCst);
        if let Some(hot) = g.hot.as_ref() {
            // 命令环随 HotState 一并失效；DSP 线程靠 run_flag 退出。
            let _ = hot;
        }
        g.hot = None;
        Ok(std::mem::take(&mut g.workers))
    }

    fn finish_stop(&self) {
        let mut g = self.inner.lock().unwrap();
        if g.phase == Phase::Stopping {
            g.started_at = None;
            self.transition(&mut g, Phase::Idle);
        }
    }

    /// stop：running/starting → stopping → join → idle。
    pub fn stop(&self) -> Result<Value, RpcFault> {
        let workers = self.begin_stop()?;
        for handle in workers {
            let _ = handle.join();
        }
        self.finish_stop();
        Ok(json!({"stopped": true}))
    }

    /// 事件中枢周期调用：数据面异常（err_flag 或线程提前退出）时兜底停机。
    pub fn poll_supervision(&self) {
        let needs_stop = {
            let g = self.inner.lock().unwrap();
            g.phase == Phase::Running
                && (g.err_flag.load(Ordering::Relaxed)
                    || g.workers.iter().any(|h| h.is_finished()))
        };
        if !needs_stop {
            return;
        }
        eprintln!("[hse-service] 数据面异常退出，执行兜底停机");
        if let Ok(workers) = self.begin_stop() {
            for handle in workers {
                let _ = handle.join();
            }
            self.finish_stop();
        }
    }

    /// setParams：解析快照存入状态；running 时构建新链经命令环热换入。
    pub fn set_params(&self, params_value: &Value) -> Result<Value, RpcFault> {
        let (pilot, warnings) = parse_pilot_params(params_value).map_err(RpcFault::invalid_params)?;
        let mut warnings_json: Vec<Value> = warnings.iter().map(|w| json!(w)).collect();
        let mut g = self.inner.lock().unwrap();
        g.pilot_params = pilot;
        g.last_params_value = Some(params_value.clone());
        if g.phase == Phase::Running {
            let (rate, max_block) = match g.hot.as_ref() {
                Some(h) => (h.sample_rate, h.max_block),
                None => return Err(RpcFault::backend_failed("运行态缺失热更换通道（内部错误）")),
            };
            let built = PilotSubchain::build(&g.pilot_params, rate, max_block)
                .map_err(|e| RpcFault::invalid_params(format!("参数无法应用：{}", e)))?;
            if g.hot.as_mut().expect("上方已确认存在").chain_tx.push(built).is_err() {
                // 命令环满（连续快速换参）：丢弃本次快照并如实告警。
                warnings_json.push(json!("参数更换过快，命令环已满：本条快照被丢弃，沿用上一条待生效参数"));
            }
        }
        Ok(json!({"accepted": true, "warnings": warnings_json}))
    }
}

/// 等待一条握手通道给出开流结果。
fn wait_ready(rx: &Receiver<Result<hse_wasapi::StreamFormat, String>>) -> Result<hse_wasapi::StreamFormat, String> {
    match rx.recv_timeout(HANDSHAKE_TIMEOUT) {
        Ok(Ok(fmt)) => Ok(fmt),
        Ok(Err(m)) => Err(m),
        Err(_) => Err(format!("开流握手超时（{} 秒）", HANDSHAKE_TIMEOUT.as_secs())),
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        // 句柄销毁时尽力停掉仍在运行的数据面（避免测试/异常路径泄漏线程）。
        if let Ok(workers) = self.begin_stop() {
            for handle in workers {
                let _ = handle.join();
            }
            self.finish_stop();
        }
    }
}