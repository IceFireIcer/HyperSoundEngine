//! 推流入口：会话表、二进制帧解析、每会话有界队列（drop-oldest 背压）与混合前级。
//!
//! 规格：`specs/service/push-stream.md`（Phase 3）。要点：
//!
//! - **会话**：`openSession`/`closeSession` 分配 u32 id，从 1 起全局单调递增、永不复用，耗尽报后端失败；
//!   会话与引擎相位解耦（任意相位可开、非运行相位照常入队＋淘汰）；
//! - **二进制帧**（与控制面同端口复用，按 WS opcode 分流）：`sessionId u32 LE + seq u64 LE + 交错 f32LE 立体声载荷`，
//!   载荷须为 8 的倍数（≥8）且 ≤ 1 MiB、sessionId≠0，违规帧与未知会话帧**统一静默丢弃**（不回错误、不发事件、不断连接）；
//! - **背压**：每会话独立有界队列（rtrb SPSC，元素 = 一条 WS 帧的载荷样本），入队遇满丢队首最旧块腾位；
//!   每丢弃一个旧块 xrunsIn +1 并按限频规则上报 `event.xrun {dir:"in"}`（计数器本身精确，通知允许限频/合并）；
//! - **混后处理**（ADR-0002）：`drain_and_mix` 在捕获线程内把各会话块与回环块逐样本求和后一次性写入入环；
//!   累加次序固定 = 回环源最先（若有），其后按 sessionId 升序，保证同输入同会话集重放逐位一致；
//! - **线程归属**：帧入队发生在控制面连接线程（允许分配/加锁/系统调用，control-plane.md §九）；
//!   出队与求和发生在捕获线程（I/O 线程，同前）；两端全部经会话表互斥锁串行——DSP/渲染线程不触碰本模块，
//!   实时纪律不变。锁竞争面：控制面每帧 push、捕获线程每轮 drain 各持锁微秒级，
//!   且本模块从不获取 engine 内部锁（无嵌套锁，无死锁环）。

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rtrb::{Consumer, Producer, RingBuffer};
use serde_json::{json, Value};

use crate::pipeline::XRUN_EVENT_INTERVAL_MS;
use crate::state::{ServiceEvent, StatsAtomic};

/// 帧头字节数：sessionId(u32 LE) + seq(u64 LE)。
pub const FRAME_HEADER_BYTES: usize = 12;
/// 载荷字节上限（1 MiB，specs/service/push-stream.md §四）。
pub const MAX_PAYLOAD_BYTES: usize = 1_048_576;
/// 每会话队列的帧容量（以立体声帧计；2^17 = 任意合法单帧载荷恰好放得下，
/// 48kHz 下约 2.7 秒）。
const MAX_QUEUE_FRAMES: usize = 1 << 17;
/// 每会话队列的块槽数上限（rtrb 槽位预算，防 1 帧小块堆积撑内存；帧预算通常先触顶）。
const MAX_QUEUE_CHUNKS: usize = 2048;

/// 解析一条 WebSocket 二进制消息为帧：返回 (sessionId, seq, 载荷字节)。
///
/// 结构违规（总长不足帧头、载荷非 8 的倍数、载荷 <8 或 >1 MiB、sessionId=0）
/// 返回 None，调用方整帧静默丢弃。seq 仅作诊断线索，服务端不校验连续性。
pub fn parse_frame(frame: &[u8]) -> Option<(u32, u64, &[u8])> {
    if frame.len() < FRAME_HEADER_BYTES {
        return None;
    }
    let session_id = u32::from_le_bytes(frame[0..4].try_into().ok()?);
    let seq = u64::from_le_bytes(frame[4..FRAME_HEADER_BYTES].try_into().ok()?);
    let payload = &frame[FRAME_HEADER_BYTES..];
    if payload.len() < 8 || !payload.len().is_multiple_of(8) || payload.len() > MAX_PAYLOAD_BYTES {
        return None;
    }
    if session_id == 0 {
        return None; // 0 为保留值
    }
    Some((session_id, seq, payload))
}

/// 交错 f32LE 载荷 → 样本向量（小端转主机序；长度为 4 的倍数已由 parse_frame 保证）。
fn payload_to_samples(payload: &[u8]) -> Vec<f32> {
    payload
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().expect("载荷长度为 4 的倍数")))
        .collect()
}

/// 单个活跃会话的队列与消费侧状态（全部字段仅在会话表锁内访问）。
struct SessionEntry {
    /// 打开该会话的连接标识（断线自动清理依据）。
    owner: u64,
    /// 入队端（控制面连接线程经会话表锁使用）。
    prod: Producer<Vec<f32>>,
    /// 出队端（捕获线程经会话表锁使用；drop-oldest 亦从该端弹出）。
    cons: Consumer<Vec<f32>>,
    /// 当前排队帧数（push/drain 同锁维护，无须原子）。
    queued_frames: usize,
    /// WebSocket 数据面成功接收的累计帧数。
    ingested_frames: u64,
    /// 混合前级实际取出的累计帧数。
    consumed_frames: u64,
    /// 消费侧半块余量：上轮未取尽的队首块与已消费帧偏移（FIFO 次序保持）。
    stash: Option<(Vec<f32>, usize)>,
    /// 本会话 xrun 通知的上次发送时刻（限频防风暴；首丢必报）。
    last_emit: Option<Instant>,
}

/// 会话表共享态（锁内）。
struct TableInner {
    /// 下一个待分配会话 id；None = id 空间已耗尽。
    next_id: Option<u32>,
    /// 活跃会话，按 id 升序迭代（混合累加次序的确定性来源）。
    sessions: BTreeMap<u32, SessionEntry>,
}

/// 引擎级会话表：openSession/closeSession、帧入队与混合出队的唯一落点。
pub struct SessionTable {
    inner: Mutex<TableInner>,
    stats: Arc<StatsAtomic>,
    events: SyncSender<ServiceEvent>,
}

/// 会话 id 空间耗尽（映射控制面 -32000）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionIdExhausted;

impl SessionTable {
    /// 构建空表（id 从 1 起分配）。
    pub fn new(stats: Arc<StatsAtomic>, events: SyncSender<ServiceEvent>) -> Self {
        Self::with_initial_next_id(1, stats, events)
    }

    /// 指定起始 id 构建（测试用：模拟 id 空间耗尽）。
    pub fn with_initial_next_id(
        next_id: u32,
        stats: Arc<StatsAtomic>,
        events: SyncSender<ServiceEvent>,
    ) -> Self {
        Self {
            inner: Mutex::new(TableInner {
                next_id: Some(next_id),
                sessions: BTreeMap::new(),
            }),
            stats,
            events,
        }
    }

    /// 分配新会话 id 并建环。id 空间耗尽（u32 用尽）返回 Err。
    pub fn open(&self, owner: u64) -> Result<u32, SessionIdExhausted> {
        let mut g = self.inner.lock().unwrap();
        let id = g.next_id.ok_or(SessionIdExhausted)?;
        g.next_id = id.checked_add(1); // 分配 u32::MAX 后即耗尽（id 永不复用）
                                       // 预分配：块槽位一次到位，会话生命周期内不再分配。
        let (prod, cons) = RingBuffer::<Vec<f32>>::new(MAX_QUEUE_CHUNKS);
        g.sessions.insert(
            id,
            SessionEntry {
                owner,
                prod,
                cons,
                queued_frames: 0,
                ingested_frames: 0,
                consumed_frames: 0,
                stash: None,
                last_emit: None,
            },
        );
        Ok(id)
    }

    /// 关闭会话：立即不再收帧，环内未消费块随条目一并丢弃（不承诺排空）。
    /// 未知 / 已关闭的 id 返回 false（重复 close 不幂等）。
    pub fn close(&self, session_id: u32) -> bool {
        self.inner
            .lock()
            .unwrap()
            .sessions
            .remove(&session_id)
            .is_some()
    }

    /// 关闭某连接打开的全部会话（断线自动清理，防泄漏）。返回关闭数。
    pub fn close_owner(&self, owner: u64) -> usize {
        let mut g = self.inner.lock().unwrap();
        let before = g.sessions.len();
        g.sessions.retain(|_, entry| entry.owner != owner);
        before - g.sessions.len()
    }

    /// 当前活跃会话 id（升序）。测试/诊断用。
    pub fn active_ids(&self) -> Vec<u32> {
        self.inner
            .lock()
            .unwrap()
            .sessions
            .keys()
            .copied()
            .collect()
    }

    /// 是否无活跃会话（捕获线程快路径判断）。
    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().sessions.is_empty()
    }

    /// 某会话当前排队帧数；未知会话返回 None。测试/诊断用。
    pub fn queued_frames(&self, session_id: u32) -> Option<usize> {
        self.inner
            .lock()
            .unwrap()
            .sessions
            .get(&session_id)
            .map(|e| e.queued_frames)
    }

    /// 会话级消费诊断快照。用于验收工具机械证明每条独立连接的会话均被消费。
    pub fn diagnostics(&self) -> Value {
        let g = self.inner.lock().unwrap();
        Value::Array(
            g.sessions
                .iter()
                .map(|(&session_id, entry)| {
                    json!({
                        "sessionId": session_id,
                        "queuedFrames": entry.queued_frames,
                        "ingestedFrames": entry.ingested_frames,
                        "consumedFrames": entry.consumed_frames,
                    })
                })
                .collect(),
        )
    }

    /// 输入侧 xrun 累计（与 getState.stats.xrunsIn 同源；测试/诊断用）。
    pub fn xruns_in_total(&self) -> u64 {
        self.stats.xruns_in.load(Ordering::Relaxed)
    }

    /// 测试钩子：改写下一个待分配 id（模拟 id 空间耗尽等场景）。
    #[doc(hidden)]
    pub fn force_next_session_id(&self, next_id: u32) {
        self.inner.lock().unwrap().next_id = Some(next_id);
    }

    /// 音频入口：解析一条二进制帧并入对应会话环。
    ///
    /// 违规帧 / 未知会话 → 整帧静默丢弃并返回 false（GWT-PS-08/09：不回错误、
    /// 不发事件、不断连接）；成功入队返回 true。帧按 sessionId 路由，与发送
    /// 连接无关（单客户端假设下二者一致；规格未把会话绑定到连接）。
    pub fn ingest_frame(&self, frame: &[u8]) -> bool {
        let Some((session_id, _seq, payload)) = parse_frame(frame) else {
            return false;
        };
        // 控制面线程：允许分配（样本向量走堆，帧载荷上限 1 MiB）。
        let samples = payload_to_samples(payload);
        let mut g = self.inner.lock().unwrap();
        let Some(entry) = g.sessions.get_mut(&session_id) else {
            return false;
        };
        push_entry(entry, samples, &self.stats, &self.events);
        true
    }

    /// 混合前级（捕获线程每轮调用）：把各会话本轮取出的块逐样本累加进 mix。
    ///
    /// - `mix[0..loopback_frames*2]` 已含回环样本（回环源最先参与求和）；
    /// - 各会话按 id 升序累加，每会话本轮最多取 `cap_frames` 帧（超长块留余量到
    ///   下轮，FIFO 次序不变）；尚未被覆盖的槽位覆盖写、已覆盖槽位加写，
    ///   累加次序固定 → 同输入同会话集重放逐位一致（GWT-PS-12）；
    /// - 返回本轮混合总帧数 = max(回环帧数, 各会话取出帧数)。
    pub fn drain_and_mix(
        &self,
        mix: &mut [f32],
        loopback_frames: usize,
        cap_frames: usize,
    ) -> usize {
        debug_assert!(mix.len() >= cap_frames * 2, "mix 缓冲须容得下本轮配额");
        let mut g = self.inner.lock().unwrap();
        let mut covered = loopback_frames; // mix 前 covered 帧已初始化
        let mut total = loopback_frames;
        for entry in g.sessions.values_mut() {
            let mut pos = 0usize; // 本会话本轮已写入的帧数（从 0 起连续推进）
            while pos < cap_frames {
                let (chunk, off) = match entry.stash.take() {
                    Some(pair) => pair,
                    None => match entry.cons.pop() {
                        Ok(c) => {
                            entry.queued_frames -= c.len() / 2;
                            (c, 0)
                        }
                        Err(_) => break, // 队列已空
                    },
                };
                let chunk_frames = chunk.len() / 2;
                if chunk_frames == off {
                    continue; // 空块防御（合法帧载荷 ≥1 帧，正常不可达）
                }
                let take = (chunk_frames - off).min(cap_frames - pos);
                for i in 0..take {
                    let dst = (pos + i) * 2;
                    let src = (off + i) * 2;
                    if pos + i < covered {
                        mix[dst] += chunk[src];
                        mix[dst + 1] += chunk[src + 1];
                    } else {
                        mix[dst] = chunk[src];
                        mix[dst + 1] = chunk[src + 1];
                    }
                }
                pos += take;
                entry.consumed_frames = entry.consumed_frames.saturating_add(take as u64);
                covered = covered.max(pos);
                total = total.max(pos);
                let consumed = off + take;
                if consumed < chunk_frames {
                    entry.stash = Some((chunk, consumed)); // 本轮配额用尽，余量留下轮
                    break;
                }
            }
        }
        total
    }
}

/// 入队（会话表锁内调用）：满则丢队首旧块腾位（drop-oldest），
/// 每丢弃一个旧块 xrunsIn +1 并按限频规则上报 event.xrun {dir:"in"}。
fn push_entry(
    entry: &mut SessionEntry,
    samples: Vec<f32>,
    stats: &StatsAtomic,
    events: &SyncSender<ServiceEvent>,
) {
    let frames = samples.len() / 2;
    // 容量约束 = 帧预算 + 块槽数，二者任一触顶都丢最旧块。
    while entry.queued_frames + frames > MAX_QUEUE_FRAMES || entry.prod.slots() == 0 {
        match entry.cons.pop() {
            Ok(old) => {
                entry.queued_frames -= old.len() / 2;
                stats.xruns_in.fetch_add(1, Ordering::Relaxed);
                emit_xrun_throttled(events, &mut entry.last_emit, 1);
            }
            Err(_) => break, // 队列已空仍放不下（单块超预算防御，理论不可达）：整帧丢弃
        }
    }
    if entry.prod.push(samples).is_ok() {
        entry.queued_frames += frames;
        entry.ingested_frames = entry.ingested_frames.saturating_add(frames as u64);
    }
}

/// 限频上报（与数据面同窗口；首丢必报，窗口内丢弃只计计数不发通知，
/// 总量以共享计数器为准——control-plane.md §七允许合并）。
fn emit_xrun_throttled(
    events: &SyncSender<ServiceEvent>,
    last_emit: &mut Option<Instant>,
    count: u64,
) {
    if count == 0 {
        return;
    }
    let due = match last_emit {
        Some(t) => t.elapsed().as_millis() as u64 >= XRUN_EVENT_INTERVAL_MS,
        None => true,
    };
    if due {
        *last_emit = Some(Instant::now());
        // 有界事件通道：满则丢通知不丢计数。
        let _ = events.try_send(ServiceEvent::Xrun { dir: "in", count });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    /// 构造 12 字节帧头 + 载荷的完整帧。
    fn frame(session_id: u32, seq: u64, samples: &[f32]) -> Vec<u8> {
        let mut f = Vec::with_capacity(FRAME_HEADER_BYTES + samples.len() * 4);
        f.extend_from_slice(&session_id.to_le_bytes());
        f.extend_from_slice(&seq.to_le_bytes());
        for s in samples {
            f.extend_from_slice(&s.to_le_bytes());
        }
        f
    }

    fn table() -> (SessionTable, std::sync::mpsc::Receiver<ServiceEvent>) {
        let (tx, rx) = sync_channel::<ServiceEvent>(256);
        let t = SessionTable::new(Arc::new(StatsAtomic::default()), tx);
        (t, rx)
    }

    #[test]
    fn 帧解析_合法与各类违规() {
        // 合法：1 帧载荷
        let f = frame(7, 42, &[0.5, -0.25]);
        assert_eq!(parse_frame(&f), Some((7, 42, &f[12..])));
        // 不足帧头 / 无载荷
        assert_eq!(parse_frame(&f[..11]), None);
        assert_eq!(parse_frame(&f[..12]), None);
        // 载荷非 8 的倍数（8+4=12 字节 = 1.5 帧）
        let mut bad = f.clone();
        bad.extend_from_slice(&[0u8; 4]);
        assert_eq!(parse_frame(&bad), None);
        // 载荷恰好 1 MiB 合法；再多样本即超限
        let huge = frame(1, 0, &[0.0; 262144]);
        assert_eq!(
            parse_frame(&huge).map(|(id, _, p)| (id, p.len())),
            Some((1, MAX_PAYLOAD_BYTES))
        );
        let mut over = frame(1, 0, &[0.0; 262144]);
        over.extend_from_slice(&1.0f32.to_le_bytes());
        assert_eq!(parse_frame(&over), None, "超过 1 MiB 须拒收");
        // sessionId 0 保留
        assert_eq!(parse_frame(&frame(0, 0, &[0.0, 0.0])), None);
    }

    #[test]
    fn 开关会话_id单调不复用_重复close不幂等() {
        let (t, _rx) = table();
        let a = t.open(9).unwrap();
        let b = t.open(9).unwrap();
        assert_eq!((a, b), (1, 2), "id 从 1 起严格递增");
        assert!(t.close(a));
        assert!(!t.close(a), "重复 close 不幂等");
        assert!(!t.close(999_999), "未知 id 拒绝");
        let c = t.open(0).unwrap();
        assert!(c > b, "新 id 须大于历史全部 id（永不复用）");
        assert_eq!(t.active_ids(), vec![b, c]);
        assert_eq!(t.close_owner(9), 1, "只清理属于该连接的会话");
        assert_eq!(t.active_ids(), vec![c]);
    }

    #[test]
    fn id空间耗尽报错() {
        let (tx, _rx) = sync_channel::<ServiceEvent>(4);
        let t = SessionTable::with_initial_next_id(u32::MAX, Arc::new(StatsAtomic::default()), tx);
        assert_eq!(t.open(0).unwrap(), u32::MAX, "最后一个 id 仍可分配");
        assert_eq!(t.open(0), Err(SessionIdExhausted), "耗尽后拒绝分配");
    }

    #[test]
    fn 帧入队_违规与未知会话静默_seq不校验连续() {
        let (t, _rx) = table();
        let s = t.open(0).unwrap();
        // 违规帧：返回 false 且不入队（不足帧头 / 无载荷 / sessionId=0）
        assert!(!t.ingest_frame(&frame(s, 0, &[0.5, -0.25])[..8]));
        assert!(!t.ingest_frame(&frame(s, 0, &[0.5, -0.25])[..12]));
        assert!(!t.ingest_frame(&[0u8; 16]));
        // 未知会话（从未分配 999999）
        assert!(!t.ingest_frame(&frame(999_999, 0, &[0.5, 0.5])));
        assert_eq!(t.queued_frames(s), Some(0));
        // 合法帧 ×10（seq 0..9），再跳号推 seq=20 → 一律入队（GWT-PS-10）
        for seq in 0..10u64 {
            assert!(t.ingest_frame(&frame(s, seq, &[0.25, 0.75])));
        }
        assert!(t.ingest_frame(&frame(s, 20, &[0.25, 0.75])));
        assert_eq!(t.queued_frames(s), Some(11), "每帧 1 立体声帧，共 11 帧");
    }

    #[test]
    fn 关闭后帧静默丢弃且未消费块随之销毁() {
        let (t, _rx) = table();
        let s = t.open(0).unwrap();
        assert!(t.ingest_frame(&frame(s, 0, &[0.5, 0.5])));
        assert!(t.close(s));
        assert_eq!(t.queued_frames(s), None, "关闭即丢弃未消费块");
        assert!(
            !t.ingest_frame(&frame(s, 1, &[0.5, 0.5])),
            "关闭后帧按未知会话静默丢弃"
        );
    }

    #[test]
    fn 背压_丢队首旧块_每块计一次xrun_首丢发通知() {
        let (t, rx) = table();
        let s = t.open(0).unwrap();
        let frames_per_chunk = 512usize;
        let full_chunks = MAX_QUEUE_FRAMES / frames_per_chunk; // 256 块恰好装满预算
                                                               // 先推一块可识别的“最旧”内容，再灌 300 块新内容（总 301 > 256）；
                                                               // 每块前两个样本（第 0 帧 L/R）携带块号 k 作指纹。
        let mut oldest = vec![0.5_f32; frames_per_chunk * 2];
        oldest[0] = 7.5;
        oldest[1] = 7.5;
        assert!(t.ingest_frame(&frame(s, 0, &oldest)));
        for k in 1..=300u32 {
            let mut c = vec![0.5_f32; frames_per_chunk * 2];
            c[0] = k as f32;
            c[1] = k as f32;
            assert!(t.ingest_frame(&frame(s, k as u64, &c)));
        }
        // 301 - 256 = 45 次丢弃，每块 +1；稳态排队恰为帧预算
        assert_eq!(t.queued_frames(s), Some(MAX_QUEUE_FRAMES));
        assert_eq!(
            t.xruns_in_total(),
            (301 - full_chunks) as u64,
            "每丢弃一个旧块 xrunsIn 恰 +1"
        );
        // 首丢必发通知（dir="in"）
        let mut seen = false;
        while let Ok(ev) = rx.try_recv() {
            if let ServiceEvent::Xrun { dir, count } = ev {
                assert_eq!((dir, count), ("in", 1));
                seen = true;
            }
        }
        assert!(seen, "首次丢弃应产生 event.xrun(dir=in)");
        // 丢的是队首旧块：标记块与 k=1..44 被弃，队首应从 k=45 开始
        let mut mix = vec![0.0_f32; MAX_QUEUE_FRAMES * 2];
        let total = t.drain_and_mix(&mut mix, 0, MAX_QUEUE_FRAMES);
        assert_eq!(total, MAX_QUEUE_FRAMES);
        assert_eq!(
            (mix[0], mix[1]),
            (45.0, 45.0),
            "队首最旧块被丢弃，消费侧取得时间上更近的数据"
        );
        assert_eq!((mix[2], mix[3]), (0.5, 0.5), "块内非指纹样本为填充值");
        assert_eq!((mix[1024], mix[1025]), (46.0, 46.0), "下一块从第 512 帧起");
    }

    #[test]
    fn 混合次序确定_回环先_会话按id升序() {
        let (t, _rx) = table();
        let a = t.open(0).unwrap(); // id 1
        let b = t.open(0).unwrap(); // id 2
                                    // 回环块 3 帧；A 块 2 帧（L/R 相异）；B 块 4 帧；本轮配额 4 帧
        let loopback_l = [0.1_f32, 0.2, 0.3];
        let a_l = [0.5_f32, 0.25];
        let a_r = [-0.5_f32, -0.25];
        let b_val = 1.0_f32;
        let mut a_frame = frame(a, 0, &[]);
        for i in 0..2 {
            a_frame.extend_from_slice(&a_l[i].to_le_bytes());
            a_frame.extend_from_slice(&a_r[i].to_le_bytes());
        }
        let b_frame = frame(b, 0, &[b_val; 8]); // 4 立体声帧
        assert!(t.ingest_frame(&a_frame));
        assert!(t.ingest_frame(&b_frame));

        let mut mix = vec![0.0_f32; 8];
        mix[..6].copy_from_slice(&[
            loopback_l[0],
            loopback_l[0],
            loopback_l[1],
            loopback_l[1],
            loopback_l[2],
            loopback_l[2],
        ]);
        let total = t.drain_and_mix(&mut mix, 3, 4);
        assert_eq!(total, 4, "B 取满 4 帧，混合长度取最大者");
        // 期望（f32 加法次序固定）：回环最先，随后 A、B 按 id 升序
        for i in 0..4usize {
            let base = if i < 3 { loopback_l[i] } else { 0.0 };
            let mut exp_l = base;
            let mut exp_r = base;
            if i < a_l.len() {
                exp_l += a_l[i];
                exp_r += a_r[i];
            }
            if i < 4 {
                exp_l += b_val;
                exp_r += b_val;
            }
            assert_eq!(mix[i * 2], exp_l, "第 {i} 帧左声道应逐位等于固定次序累加");
            assert_eq!(mix[i * 2 + 1], exp_r);
        }
        // 重放逐位一致（GWT-PS-12）：重建同内容会话后二次混合结果相同
        let a2 = t.open(0).unwrap();
        let b2 = t.open(0).unwrap();
        let mut a2_frame = frame(a2, 0, &[]);
        for i in 0..2 {
            a2_frame.extend_from_slice(&a_l[i].to_le_bytes());
            a2_frame.extend_from_slice(&a_r[i].to_le_bytes());
        }
        assert!(t.ingest_frame(&a2_frame));
        assert!(t.ingest_frame(&frame(b2, 0, &[b_val; 8])));
        let mut mix2 = vec![0.0_f32; 8];
        mix2[..6].copy_from_slice(&[
            loopback_l[0],
            loopback_l[0],
            loopback_l[1],
            loopback_l[1],
            loopback_l[2],
            loopback_l[2],
        ]);
        let total2 = t.drain_and_mix(&mut mix2, 3, 4);
        assert_eq!(total2, total);
        assert_eq!(
            &mix2[..total * 2],
            &mix[..total * 2],
            "同输入同会话集重放须逐位一致"
        );
    }

    #[test]
    fn 超长块跨轮余量保持_fifo次序不乱() {
        let (t, _rx) = table();
        let s = t.open(0).unwrap();
        // 块1 = 8 帧，块2 = 2 帧；每轮配额 3 帧
        t.ingest_frame(&frame(
            s,
            0,
            &[
                1.0, 1.0, 2.0, 2.0, 3.0, 3.0, 4.0, 4.0, 5.0, 5.0, 6.0, 6.0, 7.0, 7.0, 8.0, 8.0,
            ],
        ));
        t.ingest_frame(&frame(s, 1, &[9.0, 9.0, 10.0, 10.0]));
        let mut mix = vec![0.0_f32; 6];
        assert_eq!(t.drain_and_mix(&mut mix, 0, 3), 3);
        assert_eq!(&mix[..6], &[1.0, 1.0, 2.0, 2.0, 3.0, 3.0]);
        let mut mix2 = vec![0.0_f32; 6];
        assert_eq!(t.drain_and_mix(&mut mix2, 0, 3), 3);
        assert_eq!(&mix2[..6], &[4.0, 4.0, 5.0, 5.0, 6.0, 6.0]);
        let mut mix3 = vec![0.0_f32; 8];
        assert_eq!(
            t.drain_and_mix(&mut mix3, 0, 3),
            3,
            "余量 7,8 与下一块首帧 9"
        );
        assert_eq!(&mix3[..6], &[7.0, 7.0, 8.0, 8.0, 9.0, 9.0]);
        let mut mix4 = vec![0.0_f32; 6];
        assert_eq!(t.drain_and_mix(&mut mix4, 0, 3), 1);
        assert_eq!(&mix4[..2], &[10.0, 10.0]);
        assert_eq!(t.queued_frames(s), Some(0), "全部消费完毕");
    }

    #[test]
    fn 空表drain为直通_回环样本不被触碰() {
        let (t, _rx) = table();
        let mut mix = vec![0.5_f32; 16];
        assert_eq!(t.drain_and_mix(&mut mix, 2, 8), 2);
        assert_eq!(&mix[..4], &[0.5, 0.5, 0.5, 0.5]);
    }

    #[test]
    fn 恰好等于帧预算的最大合法单帧可入队() {
        let (t, _rx) = table();
        let s = t.open(0).unwrap();
        let f = frame(s, 0, &[0.25_f32; 262144]); // 载荷 1 MiB = 131072 帧 = 预算上限
        assert_eq!(f.len(), FRAME_HEADER_BYTES + MAX_PAYLOAD_BYTES);
        assert!(t.ingest_frame(&f), "预算容得下最大合法单帧");
        assert_eq!(t.queued_frames(s), Some(MAX_QUEUE_FRAMES));
        assert_eq!(t.xruns_in_total(), 0);
    }

    #[test]
    fn 非有限样本照常入队透传() {
        let (t, _rx) = table();
        let s = t.open(0).unwrap();
        let weird = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.0];
        assert!(
            t.ingest_frame(&frame(s, 0, &weird)),
            "规格未要求过滤，透传给链内 limiter"
        );
        assert_eq!(t.queued_frames(s), Some(2));
    }
}
