//! 数据面线程编排：捕获线程 → rtrb 入环 → DSP 线程 → rtrb 出环 → 渲染线程。
//!
//! - 双环容量按 blockSize 整数倍预分配（RING_BLOCKS 倍），启动时一次分配；
//! - WASAPI 句柄不跨线程：开流在各数据面线程内经 opener 完成，随后通过
//!   有界握手通道把协商格式回报引擎；引擎装好初始链并置 ready 旗后数据面
//!   才开始搬运音频（避免启动瞬态污染 xrun 计数）；
//! - DSP 线程稳态零分配、零锁、零系统调用：只做环读写、planar 过链、原子
//!   计数与自旋提示；参数热更换经命令环在块边界整链换入；
//! - 捕获/渲染线程是 I/O 线程：允许阻塞与系统调用；xrun 走原子累加，事件
//!   经有界通道 try_send 转发（满则丢事件不丢计数）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hse_wasapi::StreamFormat;
use rtrb::{Consumer, Producer, RingBuffer};

use crate::backend::{CaptureOpener, RenderOpener};
use crate::dsp_chain;
use crate::dsp_chain::PilotSubchain;
use crate::state::{ServiceEvent, StatsAtomic};

/// 环容量 = blockSize × 该倍数（约 128ms @48kHz/256 帧）。
const RING_BLOCKS: usize = 24;
/// 同方向 xrun 事件的最小发送间隔（限频防风暴；计数器本身不受影响）。
const XRUN_EVENT_INTERVAL_MS: u64 = 100;
/// 捕获空轮询退避：后端 pull 为非阻塞尽力语义（返回实际帧数，0=暂无），
/// 无数据时按此间隔休眠再询，避免忙转（I/O 线程允许系统调用）。
const CAPTURE_IDLE_POLL_MS: u64 = 10;

/// 线程族共享依赖（各线程持有克隆）。
pub struct WorkerDeps {
    pub run_flag: Arc<AtomicBool>,
    pub err_flag: Arc<AtomicBool>,
    /// 引擎完成握手并装入初始链后置 true，数据面才开始搬运音频。
    pub ready_gate: Arc<AtomicBool>,
    pub stats: Arc<StatsAtomic>,
    pub events: SyncSender<ServiceEvent>,
}

/// 拉起三个数据面线程的产物：句柄 + 两条握手通道。
pub struct ThreadHandles {
    pub handles: Vec<JoinHandle<()>>,
    pub capture_ready: Receiver<Result<StreamFormat, String>>,
    pub render_ready: Receiver<Result<StreamFormat, String>>,
}

/// 环容量样本数（整块整数倍）。
pub fn ring_capacity_samples(block_frames: usize) -> usize {
    block_frames * RING_BLOCKS * 2
}

/// 构造数据面双环（入环/出环，容量均为 blockSize 整数倍的样本数）。
pub fn build_rings(block_frames: usize) -> (Producer<f32>, Consumer<f32>, Producer<f32>, Consumer<f32>) {
    let cap = ring_capacity_samples(block_frames);
    let (ip, ic) = RingBuffer::<f32>::new(cap);
    let (op, oc) = RingBuffer::<f32>::new(cap);
    (ip, ic, op, oc)
}

/// 构造参数热更换命令环（控制面生产、DSP 线程消费）。
pub fn build_command_ring() -> (Producer<PilotSubchain>, Consumer<PilotSubchain>) {
    RingBuffer::<PilotSubchain>::new(4)
}

/// 拉起三个数据面线程。开流发生在各线程内；协商格式经握手通道回报。
#[allow(clippy::too_many_arguments)]
pub fn spawn_workers(
    capture_opener: Box<dyn CaptureOpener>,
    render_opener: Box<dyn RenderOpener>,
    in_prod: Producer<f32>,
    in_cons: Consumer<f32>,
    out_prod: Producer<f32>,
    out_cons: Consumer<f32>,
    chain_rx: Consumer<PilotSubchain>,
    block_frames: usize,
    deps: WorkerDeps,
) -> ThreadHandles {
    let run = Arc::clone(&deps.run_flag);
    let err = Arc::clone(&deps.err_flag);
    let gate = Arc::clone(&deps.ready_gate);
    let stats = Arc::clone(&deps.stats);
    let ev_cap = deps.events.clone();
    let ev_ren = deps.events.clone();

    let (cap_tx, cap_rx) = std::sync::mpsc::channel::<Result<StreamFormat, String>>();
    let (ren_tx, ren_rx) = std::sync::mpsc::channel::<Result<StreamFormat, String>>();

    let run_cap = Arc::clone(&run);
    let err_cap = Arc::clone(&err);
    let gate_cap = Arc::clone(&gate);
    let stats_cap = Arc::clone(&stats);
    let h_cap = std::thread::Builder::new()
        .name("hse-capture".into())
        .spawn(move || {
            capture_loop(capture_opener, in_prod, block_frames, cap_tx, &run_cap, &err_cap, &gate_cap, &stats_cap, &ev_cap);
        })
        .expect("捕获线程创建失败");

    let run_ren = Arc::clone(&run);
    let err_ren = Arc::clone(&err);
    let gate_ren = Arc::clone(&gate);
    let stats_ren = Arc::clone(&stats);
    let h_ren = std::thread::Builder::new()
        .name("hse-render".into())
        .spawn(move || {
            render_loop(render_opener, out_cons, block_frames, ren_tx, &run_ren, &err_ren, &gate_ren, &stats_ren, &ev_ren);
        })
        .expect("渲染线程创建失败");

    let h_dsp = std::thread::Builder::new()
        .name("hse-dsp".into())
        .spawn(move || dsp_loop(in_cons, out_prod, chain_rx, block_frames, &run, &err, &stats))
        .expect("DSP 线程创建失败");

    ThreadHandles { handles: vec![h_cap, h_dsp, h_ren], capture_ready: cap_rx, render_ready: ren_rx }
}

fn maybe_emit_xrun(events: &SyncSender<ServiceEvent>, last_emit: &mut Instant, dir: &'static str, count: u64) {
    if count == 0 {
        return;
    }
    if last_emit.elapsed().as_millis() as u64 >= XRUN_EVENT_INTERVAL_MS {
        *last_emit = Instant::now();
        // 有界通道：满则丢弃事件（统计计数已在共享原子中，总量不失真）。
        let _ = events.try_send(ServiceEvent::Xrun { dir, count });
    }
}

/// 把流对象内置的 xrun 计数增量聚合进共享统计（单调比较，容忍复位）。
fn aggregate_backend_xruns(
    dir: &'static str,
    last_seen: &mut u64,
    observed: u64,
    stats: &StatsAtomic,
    events: &SyncSender<ServiceEvent>,
    last_emit: &mut Instant,
) {
    if observed > *last_seen {
        let delta = observed - *last_seen;
        *last_seen = observed;
        match dir {
            "in" => stats.xruns_in.fetch_add(delta, Ordering::Relaxed),
            _ => stats.xruns_out.fetch_add(delta, Ordering::Relaxed),
        };
        maybe_emit_xrun(events, last_emit, dir, delta);
    } else if observed < *last_seen {
        *last_seen = observed; // 后端计数被复位（如流重开）
    }
}

/// 捕获线程：开流 → 握手 → 就绪门开启后 loopback.pull → 入环。溢出丢弃并计 xrunsIn。
fn capture_loop(
    opener: Box<dyn CaptureOpener>,
    mut prod: Producer<f32>,
    block_frames: usize,
    ready: Sender<Result<StreamFormat, String>>,
    run: &AtomicBool,
    err: &AtomicBool,
    gate: &AtomicBool,
    stats: &StatsAtomic,
    events: &SyncSender<ServiceEvent>,
) {
    let mut src = match opener.open() {
        Ok(s) => s,
        Err(e) => {
            let _ = ready.send(Err(format!("打开回环捕获失败：{}", e)));
            return;
        }
    };
    let format = match src.start() {
        Ok(f) => f,
        Err(e) => {
            let _ = ready.send(Err(format!("回环捕获启动失败：{}", e)));
            return;
        }
    };
    let _ = ready.send(Ok(format));

    let mut buf = vec![0.0_f32; block_frames * 2];
    let mut backend_xruns = src.xruns();
    let mut last_emit = Instant::now();
    loop {
        if !run.load(Ordering::Relaxed) {
            break;
        }
        if !gate.load(Ordering::Relaxed) {
            std::hint::spin_loop();
            continue;
        }
        match src.pull(&mut buf) {
            Ok(nf) => {
                let nf = nf.min(block_frames);
                if nf == 0 {
                    // 非阻塞尽力语义：暂无数据，退避后再询（不忙转）。
                    aggregate_backend_xruns("in", &mut backend_xruns, src.xruns(), stats, events, &mut last_emit);
                    std::thread::sleep(Duration::from_millis(CAPTURE_IDLE_POLL_MS));
                    continue;
                }
                let nsamp = nf * 2;
                let mut sent = 0usize;
                while sent < nsamp {
                    if prod.push(buf[sent]).is_ok() {
                        sent += 1;
                    } else {
                        break; // 环满：丢弃本块剩余样本
                    }
                }
                if sent < nsamp {
                    let lost = ((nsamp - sent) / 2) as u64;
                    stats.xruns_in.fetch_add(lost, Ordering::Relaxed);
                    maybe_emit_xrun(events, &mut last_emit, "in", lost);
                }
                // 后端内部 xrun 计数聚合
                aggregate_backend_xruns("in", &mut backend_xruns, src.xruns(), stats, events, &mut last_emit);
            }
            Err(e) => {
                eprintln!("[hse-capture] 拉取失败，请求停机：{}", e);
                err.store(true, Ordering::Relaxed);
                break;
            }
        }
    }
    let _ = src.stop();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 后端xrun聚合_单调累计且容忍复位() {
        let stats = StatsAtomic::default();
        let (tx, _rx) = std::sync::mpsc::sync_channel::<ServiceEvent>(16);
        let mut last_seen = 0_u64;
        let mut last_emit = Instant::now() - Duration::from_secs(1);
        aggregate_backend_xruns("in", &mut last_seen, 5, &stats, &tx, &mut last_emit);
        aggregate_backend_xruns("in", &mut last_seen, 9, &stats, &tx, &mut last_emit);
        assert_eq!(last_seen, 9);
        assert_eq!(stats.xruns_in.load(Ordering::Relaxed), 9);
        // 计数回退（流重开）：不累计，仅跟随
        aggregate_backend_xruns("in", &mut last_seen, 2, &stats, &tx, &mut last_emit);
        assert_eq!(last_seen, 2);
        assert_eq!(stats.xruns_in.load(Ordering::Relaxed), 9);
        // out 方向走独立计数器
        let mut out_last = 0_u64;
        aggregate_backend_xruns("out", &mut out_last, 7, &stats, &tx, &mut last_emit);
        assert_eq!(out_last, 7);
        assert_eq!(stats.xruns_out.load(Ordering::Relaxed), 7);
    }
}

/// 渲染线程：开流 → 握手 → 就绪门开启后 出环 → render.push。欠供补零并计 xrunsOut。
fn render_loop(
    opener: Box<dyn RenderOpener>,
    mut cons: Consumer<f32>,
    block_frames: usize,
    ready: Sender<Result<StreamFormat, String>>,
    run: &AtomicBool,
    err: &AtomicBool,
    gate: &AtomicBool,
    stats: &StatsAtomic,
    events: &SyncSender<ServiceEvent>,
) {
    let mut sink = match opener.open() {
        Ok(s) => s,
        Err(e) => {
            let _ = ready.send(Err(format!("打开渲染端点失败：{}", e)));
            return;
        }
    };
    let format = match sink.start() {
        Ok(f) => f,
        Err(e) => {
            let _ = ready.send(Err(format!("渲染端点启动失败：{}", e)));
            return;
        }
    };
    let _ = ready.send(Ok(format));

    let mut outbuf = vec![0.0_f32; block_frames * 2];
    let mut backend_xruns = sink.xruns();
    let mut last_emit = Instant::now();
    let want = block_frames * 2;
    loop {
        if !run.load(Ordering::Relaxed) {
            break;
        }
        if !gate.load(Ordering::Relaxed) {
            std::hint::spin_loop();
            continue;
        }
        let avail = cons.slots().min(want);
        for i in 0..avail {
            outbuf[i] = cons.pop().unwrap_or(0.0);
        }
        if avail < want {
            for slot in outbuf[avail..want].iter_mut() {
                *slot = 0.0;
            }
            let missed = ((want - avail) / 2) as u64;
            stats.xruns_out.fetch_add(missed, Ordering::Relaxed);
            maybe_emit_xrun(events, &mut last_emit, "out", missed);
        }
        // 后端内部 xrun 计数聚合（渲染侧）
        aggregate_backend_xruns("out", &mut backend_xruns, sink.xruns(), stats, events, &mut last_emit);
        if let Err(e) = sink.push(&outbuf) {
            eprintln!("[hse-render] 推帧失败，请求停机：{}", e);
            err.store(true, Ordering::Relaxed);
            break;
        }
    }
    let _ = sink.stop();
}

/// DSP 线程：出环 → planar 子链 → 入环；块间取用热更换链。稳态零分配零锁零系统调用。
fn dsp_loop(
    mut cons_in: Consumer<f32>,
    mut prod_out: Producer<f32>,
    mut chain_rx: Consumer<PilotSubchain>,
    block_frames: usize,
    run: &AtomicBool,
    err: &AtomicBool,
    stats: &StatsAtomic,
) {
    // 全部缓冲一次性预分配（线程启动期，属非稳态）。
    let mut staging = vec![0.0_f32; block_frames * 2];
    let mut left = vec![0.0_f32; block_frames];
    let mut right = vec![0.0_f32; block_frames];
    let mut filled = 0usize;
    let total = block_frames * 2;
    // 初始链由引擎在握手完成后经命令环送达。
    let mut chain: Option<PilotSubchain> = None;
    loop {
        if !run.load(Ordering::Relaxed) || err.load(Ordering::Relaxed) {
            break;
        }
        // 参数热更换：块边界整链换入（仅所有权移动）。
        while let Ok(next_chain) = chain_rx.pop() {
            chain = Some(next_chain);
        }
        let Some(active) = chain.as_mut() else {
            std::hint::spin_loop();
            continue;
        };
        // 从入环取样本，攒满一块再处理（短读跨块续攒，DSP 恒定整块语义）。
        while filled < total {
            match cons_in.pop() {
                Ok(sample) => {
                    staging[filled] = sample;
                    filled += 1;
                }
                Err(_) => break,
            }
        }
        if filled < total {
            std::hint::spin_loop();
            continue;
        }
        dsp_chain::deinterleave(&staging, &mut left, &mut right);
        active.process_planar(&mut left, &mut right);
        dsp_chain::interleave(&left, &right, &mut staging);
        let mut pushed = 0usize;
        while pushed < total {
            if prod_out.push(staging[pushed]).is_ok() {
                pushed += 1;
            } else {
                if !run.load(Ordering::Relaxed) || err.load(Ordering::Relaxed) {
                    break;
                }
                std::hint::spin_loop();
            }
        }
        if pushed == total {
            stats.frames_processed.fetch_add(block_frames as u64, Ordering::Relaxed);
        }
        filled = 0;
    }
}