//! midi —— MIDI 事件接口 / MIDI Learn（控制率参数自动化，Phase 3）。
//!
//! 行为事实标准：仓库根 `src/engine/HyperSoundEngine.ts`（MIDI 机制段）与
//! `src/types.ts`（AutomationTarget / MidiEvent / MidiBinding / AUTOMATABLE_PARAMS）；
//! 规格：`specs/io/midi.md`。本模块**不是** [`crate::Stage`]——它是控制率机器，
//! 与引擎一样在块头消费事件、把映射值写回参数快照；hse-core 无引擎上下文，
//! 参数快照以 `serde_json::Value`（服务层 PilotParams 形态 JSON）承载，由调用方持有。
//!
//! 与 TS 源码的逐段对应关系（HyperSoundEngine.ts 行号，下称 HE）：
//! - `sendMidi`（HE L939–L958）→ [`MidiEventRing::push`]：事件编码为
//!   type(0=cc/1=noteOn/2=noteOff) + a(cc 号或 note 号) + b(cc value/velocity/noteOff 恒 0)，
//!   **channel 完全不入队不入键**；a/b 落 f32 存储（镜像 `_midiType/_midiA/_midiB` Float32Array）；
//!   队列满时**丢弃最旧**并累计 dropped（HE L944–L948）。
//! - `consumeMidiQueue`（HE L1013–L1072）→ [`MidiBindings::consume`]：块头弹出全部事件；
//!   绑定键 cc 事件取 a、note 事件取 `0x4000 + a`（HE L1025，channel 不参与）；
//!   未绑定控制号安全忽略（HE L1027）；CC 值 clamp [0,127]/127 后线性映射到
//!   `[min,max]`（invert 反向，HE L1033–L1034）；noteOn → max、noteOff → min（布尔参数
//!   由白名单 `boolean` 标志经 0/1 阈值落 JSON 布尔，HE L1087）；smoothMs ≤ 0 直接到位，
//!   否则一阶平滑 `alpha = 1 − exp(−blockSize / fs / (smoothMs/1000))`（HE L1052，运算顺序
//!   原样保留）；队尾对全部平滑中绑定再走一步收敛（HE L1060–L1071）。**镜像 TS 引擎
//!   `if (_midiCount > 0)` 守卫（HE L675–L676）：队列为空的块不调平滑收敛**——
//!   [`MidiBindings::consume`] 对空环直接返回，行为与引擎逐块语义一致。
//! - `applyAutomationValue`（HE L1075–L1090）→ [`apply_automation_value`]：
//!   builtin masterGain / stereoWidth 钳 [0,2] 后**无条件写**顶层 JSON 叶子（镜像 TS 对
//!   `_modMasterGain` / `_params.stereoWidth` 的直接赋值）；path 目标先查白名单（查不到
//!   运行时安全忽略，HE L1084–L1085），值钳到白名单范围、布尔参数按 0.5 阈值离散化，
//!   再走 [`write_param_path`] 点分路径守卫写回。
//! - `writeParamPath`（HE L1093–L1106）→ [`write_param_path`]：逐层下钻，中间层非对象
//!   即放弃；叶子仅接受既有 boolean（→ `value >= 0.5`）/ number（→ value），缺失或
//!   其他类型一律不写。
//! - `refreshModuleForPath`（HE L1109–L1146）→ [`refresh_sections_for_path`]：路径前缀 →
//!   需刷新的参数 section 映射；TS 中 `compressor.enabled` / `reverb.enabled` / `pitch.*`
//!   为快照读 no-op、`stereoWidth` 每块读快照，均映射为空表。
//! - `midiLearn / midiUnlearn / getMidiBindings / refreshMidiMasterBound`
//!   （HE L965–L998、L1149–L1158）→ [`MidiBindings::learn/unlearn/get_bindings/master_bound`]。
//! - `reset()` 的 MIDI 部分（HE L919–L926）→ [`MidiEventRing::clear`] +
//!   [`MidiBindings::reset_runtime`]：绑定（配置）保留，队列与平滑状态（运行时）清零，
//!   **dropped 计数不清零**（TS reset 不复位 `_midiDropped`）。
//!
//! # 绑定键与确定性
//!
//! TS `_midiBindings` 是按**插入序**迭代的 Map；本移植改用 `BTreeMap<i64, _>`
//! （键升序：cc 键在前、note 键在后），使收敛遍历与 `get_bindings` 顺序确定化。
//! 单绑定内数值语义不变；仅当多个绑定写同一参数路径时，块内最后写者以键序为准
//! （TS 为插入序）——控制率参数路径写回无对拍向量依赖此顺序。
//!
//! # 白名单
//!
//! [`AUTOMATABLE_PARAMS`] 逐条复刻 TS `AUTOMATABLE_PARAMS`（38 条，含两条 builtin
//! 顶层路径）；`learn` 对非法路径立即 `Err("unknown automatable path: …")`
//! （HE L966–L969），运行时查不到白名单则安全忽略。
//!
//! # 实时安全
//!
//! 事件环与 alpha 缓存全部预分配（环为定长数组、缓存 `clear` 保留容量），
//! `consume` 稳态零分配；sections 收集与 JSON 叶子写回为控制率路径（TS 同级分配），
//! 不在音频回调内运行。`learn/unlearn/reset_runtime` 属控制面操作，允许分配。

use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

/// 事件环容量（镜像 TS `HyperSoundEngine.MIDI_QUEUE_CAP`）。
pub const MIDI_QUEUE_CAP: usize = 4096;

/// note 绑定键偏移（镜像 TS `0x4000 + cc`：cc 键与 note 键共用一张表，以 0x4000 分命名空间）。
pub const NOTE_KEY_OFFSET: i64 = 0x4000;

/// learn 缺省平滑时间常数 ms（镜像 TS `opts?.smoothMs ?? 20`）。
pub const DEFAULT_SMOOTH_MS: f64 = 20.0;

/// MIDI 事件类别（镜像 TS `MidiEvent` 的三型判别；编码值 0/1/2 仅内部使用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiEventKind {
    /// 控制变化：`a` = cc 号，`b` = value（0..127）
    Cc,
    /// 音符按下：`a` = note 号，`b` = velocity
    NoteOn,
    /// 音符抬起：`a` = note 号，`b` 恒 0（TS sendMidi 对 noteOff 置 0）
    NoteOff,
}

/// 入队事件（镜像 TS `MidiEvent` 的引擎侧编码视图）。
///
/// `a`/`b` 以 f64 接收、入环时落 f32（镜像 TS 写入 Float32Array 的量化落点）；
/// `a` = cc 号或 note 号，`b` = cc value / velocity（noteOff 忽略，内部置 0）。
/// channel 不进入本结构——TS 编码与绑定键均不含 channel。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MidiEventIn {
    pub kind: MidiEventKind,
    pub a: f64,
    pub b: f64,
}

impl MidiEventIn {
    /// cc 事件构造。
    pub fn cc(cc: f64, value: f64) -> Self {
        Self { kind: MidiEventKind::Cc, a: cc, b: value }
    }
    /// noteOn 事件构造。
    pub fn note_on(note: f64, velocity: f64) -> Self {
        Self { kind: MidiEventKind::NoteOn, a: note, b: velocity }
    }
    /// noteOff 事件构造（b 入环时置 0）。
    pub fn note_off(note: f64) -> Self {
        Self { kind: MidiEventKind::NoteOff, a: note, b: 0.0 }
    }
}

/// 预分配 MIDI 事件环形队列（镜像 TS `_midiType/_midiA/_midiB/_midiC` 并行数组 +
/// `_midiHead/_midiTail/_midiCount/_midiDropped`）。
///
/// 溢出语义（HE L944–L948）：队列满时**丢弃最旧**事件（head 前移、计数减一）并
/// 累计 [`MidiEventRing::dropped`]；不抛错、不影响消费侧。
pub struct MidiEventRing {
    t: [u8; MIDI_QUEUE_CAP],
    a: [f32; MIDI_QUEUE_CAP],
    b: [f32; MIDI_QUEUE_CAP],
    head: usize,
    tail: usize,
    count: usize,
    dropped: u64,
}

impl Default for MidiEventRing {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiEventRing {
    /// 构造空环（预分配，无堆分配）。
    pub fn new() -> Self {
        Self { t: [0; MIDI_QUEUE_CAP], a: [0.0; MIDI_QUEUE_CAP], b: [0.0; MIDI_QUEUE_CAP], head: 0, tail: 0, count: 0, dropped: 0 }
    }

    /// 当前排队事件数。
    pub fn len(&self) -> usize {
        self.count
    }

    /// 队列是否为空。
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// 溢出丢弃累计计数（自构造起累计；`clear` 不清零，镜像 TS reset 不复位 `_midiDropped`）。
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// 入队单事件（镜像 TS `sendMidi` 循环体：满则丢最旧 + 计数）。
    pub fn push(&mut self, ev: MidiEventIn) {
        if self.count >= MIDI_QUEUE_CAP {
            // 溢出：丢弃最旧
            self.head = (self.head + 1) % MIDI_QUEUE_CAP;
            self.count -= 1;
            self.dropped += 1;
        }
        // TS L941–L943：t = cc?0 : noteOn?1 : 2；a = cc 或 note；b = value / velocity / 0
        let t = match ev.kind {
            MidiEventKind::Cc => 0u8,
            MidiEventKind::NoteOn => 1u8,
            MidiEventKind::NoteOff => 2u8,
        };
        let i = self.tail;
        self.t[i] = t;
        self.a[i] = ev.a as f32;
        self.b[i] = if ev.kind == MidiEventKind::NoteOff { 0.0 } else { ev.b as f32 };
        self.tail = (self.tail + 1) % MIDI_QUEUE_CAP;
        self.count += 1;
    }

    /// 入队多事件（按到达顺序，镜像 TS `sendMidi(events: MidiEvent[])`）。
    pub fn push_slice(&mut self, events: &[MidiEventIn]) {
        for &ev in events {
            self.push(ev);
        }
    }

    /// 弹出最旧事件（镜像 TS `consumeMidiQueue` 的出环三行）。
    pub fn pop(&mut self) -> Option<MidiEventIn> {
        if self.count == 0 {
            return None;
        }
        let i = self.head;
        let kind = match self.t[i] {
            0 => MidiEventKind::Cc,
            1 => MidiEventKind::NoteOn,
            _ => MidiEventKind::NoteOff,
        };
        let ev = MidiEventIn { kind, a: self.a[i] as f64, b: self.b[i] as f64 };
        self.head = (self.head + 1) % MIDI_QUEUE_CAP;
        self.count -= 1;
        Some(ev)
    }

    /// 清空队列（镜像 TS `reset()` L920–L922：head/tail/count 复位；dropped 保留）。
    pub fn clear(&mut self) {
        self.head = 0;
        self.tail = 0;
        self.count = 0;
    }
}

/// builtin 自动化目标（镜像 TS `AutomationTarget` 的 `{ kind: 'builtin', param }` 分支）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinParam {
    /// 调制矩阵主增益（TS 写 `_modMasterGain`；JSON 形态为顶层 `masterGain` 叶子）
    MasterGain,
    /// 立体声宽度（TS 写 `_params.stereoWidth`；JSON 形态为顶层 `stereoWidth` 叶子）
    StereoWidth,
}

/// 参数自动化目标（镜像 TS `AutomationTarget`）。
#[derive(Debug, Clone, PartialEq)]
pub enum AutomationTarget {
    /// 调制矩阵内置目标
    Builtin(BuiltinParam),
    /// 白名单内参数点分路径（如 `compressor.thresholdDb`）
    Path(String),
}

/// MIDI Learn 绑定（镜像 TS `MidiBinding`）。
#[derive(Debug, Clone, PartialEq)]
pub struct MidiBinding {
    /// 控制号：cc 绑定存 cc 号，note 绑定存 note 号（镜像 TS `binding.cc`）
    pub cc: i32,
    pub target: AutomationTarget,
    /// 参数映射下限（CC 0 / noteOff → min）
    pub min: f64,
    /// 参数映射上限（CC 127 / noteOn → max）
    pub max: f64,
    /// 一阶平滑时间常数 ms（防 zipper），缺省 20；负值入库时钳 0
    pub smooth_ms: f64,
    /// 反向映射：CC 0 → max，CC 127 → min
    pub invert: bool,
}

/// learn 选项（镜像 TS `midiLearn` 的 `opts` 形参；None 字段走 TS `??` 缺省）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LearnOpts {
    /// 事件类别：cc（缺省）或 note（镜像 TS `opts.eventType`）
    pub event_type: ControlKind,
    /// 覆盖映射下限（缺省取白名单 / builtin 0）
    pub min: Option<f64>,
    /// 覆盖映射上限（缺省取白名单 / builtin 2）
    pub max: Option<f64>,
    /// 覆盖平滑 ms（缺省 20）
    pub smooth_ms: Option<f64>,
    /// 反向映射（缺省 false）
    pub invert: Option<bool>,
}

/// learn/unlearn 的控制号类别（镜像 TS `opts.eventType: 'cc' | 'note'`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ControlKind {
    /// CC 控制号（键 = cc）
    #[default]
    Cc,
    /// note 音符号（键 = 0x4000 + note）
    Note,
}

/// 可自动化参数元数据（镜像 TS `AutomatableParamMeta`；label 为 UI 显示名，逐条复刻）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutomatableParamMeta {
    pub path: &'static str,
    pub label: &'static str,
    pub min: f64,
    pub max: f64,
    /// true = 布尔参数（note on → true / off → false；CC ≥ 64 → true）
    pub boolean: bool,
}

/// 引擎可自动化参数白名单（逐条复刻 TS `AUTOMATABLE_PARAMS`，38 条）。
///
/// 前两条为顶层路径形态的 builtin 目标（`masterGain` / `stereoWidth`）；
/// 其余为参数快照点分路径。learn 非法路径立即报错（白名单语义）。
pub const AUTOMATABLE_PARAMS: &[AutomatableParamMeta] = &[
    AutomatableParamMeta { path: "masterGain", label: "主增益（调制矩阵）", min: 0.0, max: 2.0, boolean: false },
    AutomatableParamMeta { path: "stereoWidth", label: "立体声宽度", min: 0.0, max: 2.0, boolean: false },
    AutomatableParamMeta { path: "compressor.enabled", label: "压缩开关", min: 0.0, max: 1.0, boolean: true },
    AutomatableParamMeta { path: "compressor.thresholdDb", label: "压缩阈值", min: -60.0, max: 0.0, boolean: false },
    AutomatableParamMeta { path: "compressor.ratio", label: "压缩比率", min: 1.0, max: 20.0, boolean: false },
    AutomatableParamMeta { path: "compressor.attackMs", label: "压缩起始", min: 0.0, max: 100.0, boolean: false },
    AutomatableParamMeta { path: "compressor.releaseMs", label: "压缩释放", min: 10.0, max: 1000.0, boolean: false },
    AutomatableParamMeta { path: "compressor.makeupDb", label: "补偿增益", min: 0.0, max: 12.0, boolean: false },
    AutomatableParamMeta { path: "deesser.thresholdDb", label: "齿音阈值", min: -50.0, max: 0.0, boolean: false },
    AutomatableParamMeta { path: "deesser.mix", label: "齿音混合", min: 0.0, max: 1.0, boolean: false },
    AutomatableParamMeta { path: "bassEnhancer.cutoffHz", label: "低音截止", min: 20.0, max: 250.0, boolean: false },
    AutomatableParamMeta { path: "bassEnhancer.harmonicGain", label: "低音谐波", min: 0.0, max: 1.0, boolean: false },
    AutomatableParamMeta { path: "bassEnhancer.mix", label: "低音混合", min: 0.0, max: 1.0, boolean: false },
    AutomatableParamMeta { path: "reverb.enabled", label: "混响开关", min: 0.0, max: 1.0, boolean: true },
    AutomatableParamMeta { path: "reverb.algorithmic.wet", label: "混响湿声", min: 0.0, max: 1.0, boolean: false },
    AutomatableParamMeta { path: "reverb.algorithmic.dry", label: "混响干声", min: 0.0, max: 1.0, boolean: false },
    AutomatableParamMeta { path: "reverb.algorithmic.roomSize", label: "混响空间", min: 0.0, max: 1.0, boolean: false },
    AutomatableParamMeta { path: "reverb.algorithmic.damping", label: "混响阻尼", min: 0.0, max: 1.0, boolean: false },
    AutomatableParamMeta { path: "reverb.algorithmic.preDelayMs", label: "混响预延迟", min: 0.0, max: 200.0, boolean: false },
    AutomatableParamMeta { path: "modEffects.delay.delayMs", label: "延迟时间", min: 0.0, max: 2000.0, boolean: false },
    AutomatableParamMeta { path: "modEffects.delay.feedback", label: "延迟反馈", min: 0.0, max: 0.9, boolean: false },
    AutomatableParamMeta { path: "modEffects.delay.mix", label: "延迟混合", min: 0.0, max: 1.0, boolean: false },
    AutomatableParamMeta { path: "modEffects.chorus.rateHz", label: "合唱速率", min: 0.1, max: 10.0, boolean: false },
    AutomatableParamMeta { path: "modEffects.chorus.mix", label: "合唱混合", min: 0.0, max: 1.0, boolean: false },
    AutomatableParamMeta { path: "modEffects.flanger.rateHz", label: "镶边速率", min: 0.05, max: 10.0, boolean: false },
    AutomatableParamMeta { path: "modEffects.flanger.mix", label: "镶边混合", min: 0.0, max: 1.0, boolean: false },
    AutomatableParamMeta { path: "modEffects.phaser.rateHz", label: "移相速率", min: 0.05, max: 10.0, boolean: false },
    AutomatableParamMeta { path: "modEffects.phaser.mix", label: "移相混合", min: 0.0, max: 1.0, boolean: false },
    AutomatableParamMeta { path: "modEffects.tremolo.rateHz", label: "颤音速率", min: 0.1, max: 20.0, boolean: false },
    AutomatableParamMeta { path: "modEffects.tremolo.depth", label: "颤音深度", min: 0.0, max: 1.0, boolean: false },
    AutomatableParamMeta { path: "ieq.strength", label: "智能均衡强度", min: 0.0, max: 1.0, boolean: false },
    AutomatableParamMeta { path: "dynamicEq.strength", label: "动态均衡强度", min: 0.0, max: 1.0, boolean: false },
    AutomatableParamMeta { path: "dynamicEq.thresholdDb", label: "动态均衡阈值", min: -80.0, max: 0.0, boolean: false },
    AutomatableParamMeta { path: "dynamicEq.ratio", label: "动态均衡比率", min: 1.0, max: 20.0, boolean: false },
    AutomatableParamMeta { path: "limiter.thresholdDb", label: "限幅阈值", min: -12.0, max: 0.0, boolean: false },
    AutomatableParamMeta { path: "pitch.semitones", label: "变调半音", min: -10.0, max: 10.0, boolean: false },
    AutomatableParamMeta { path: "pitch.rate", label: "变速速率", min: 0.25, max: 3.0, boolean: false },
    AutomatableParamMeta { path: "pitch.voiceBalance", label: "人声比例", min: -1.0, max: 1.0, boolean: false },
];

/// 查找可自动化参数元数据；未知路径返回 `None`（镜像 TS `findAutomatableParam`）。
pub fn find_automatable_param(path: &str) -> Option<&'static AutomatableParamMeta> {
    AUTOMATABLE_PARAMS.iter().find(|m| m.path == path)
}

/// 复刻 JS `Math.min(a, b)` 的 NaN 传播语义（理由同 biquad.rs 的同名函数）。
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a < b {
        a
    } else {
        b
    }
}

/// 复刻 JS `Math.max(a, b)` 的 NaN 传播语义（理由同 js_min）。
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a > b {
        a
    } else {
        b
    }
}

/// 镜像 TS builtin 钳制 `value < 0 ? 0 : value > 2 ? 2 : value`（HE L1078/L1080）。
fn clamp_builtin(v: f64) -> f64 {
    if v < 0.0 {
        0.0
    } else if v > 2.0 {
        2.0
    } else {
        v
    }
}

/// 绑定运行时状态（镜像 TS `_midiBindings` 的 `{ binding, current, target }`；初值均为 0）。
#[derive(Debug, Clone, PartialEq)]
struct BindingState {
    binding: MidiBinding,
    current: f64,
    target: f64,
}

/// MIDI Learn 绑定表 + 块头消费语义（镜像 TS `_midiBindings` + `consumeMidiQueue`）。
///
/// 绑定表为**配置**（reset 保留）；`current/target` 平滑状态与事件队列为**运行时状态**
/// （reset 清零，见 [`MidiBindings::reset_runtime`]）。表键为 i64：
/// cc 绑定键 = cc，note 绑定键 = `0x4000 + note`（镜像 TS 键计算 HE L977/L994/L1025）；
/// 事件侧 channel 不参与键。
pub struct MidiBindings {
    map: BTreeMap<i64, BindingState>,
    /// 是否存在 builtin masterGain 绑定（镜像 TS `_midiMasterBound`，
    /// 供宿主门控 mod-master-gain 级：TS `active() = modulation.enabled || midiMasterBound`）。
    master_bound: bool,
    /// 平滑 alpha 缓存（按 smoothMs 位型键；`clear` 保留容量，镜像 TS `_midiAlphaCache`）。
    alpha_cache: HashMap<u64, f64>,
}

impl Default for MidiBindings {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiBindings {
    /// 构造空绑定表（无堆分配发生——map/cache 空表）。
    pub fn new() -> Self {
        Self { map: BTreeMap::new(), master_bound: false, alpha_cache: HashMap::new() }
    }

    /// 绑定控制号（CC 或 note）到自动化目标。非法路径立即 `Err`（白名单语义，HE L966–L969）。
    ///
    /// 缺省映射范围：builtin → [0, 2]；path → 白名单 `[meta.min, meta.max]`；
    /// `opts.min/max` 覆盖。缺省平滑 20 ms（负值钳 0）；缺省不反向。
    /// 同键重复 learn 覆盖旧绑定并重置平滑状态（镜像 TS `Map.set` 整体替换）。
    pub fn learn(&mut self, cc: i32, target: AutomationTarget, opts: LearnOpts) -> Result<(), String> {
        // TS L966–L969：path 目标必须命中白名单，否则立即抛错
        let meta = match &target {
            AutomationTarget::Builtin(_) => None,
            AutomationTarget::Path(p) => {
                let m = find_automatable_param(p);
                if m.is_none() {
                    return Err(format!("unknown automatable path: {}", p));
                }
                m
            }
        };
        if matches!(target, AutomationTarget::Builtin(BuiltinParam::MasterGain)) {
            // TS L970–L972：learn 直接置位（unlearn 时再全表重算）
            self.master_bound = true;
        }
        // TS L973–L974：builtin（masterGain 与 stereoWidth 同值）缺省 [0, 2]；
        // path 缺省取白名单 [meta.min, meta.max]
        let (default_min, default_max) = match (&target, meta) {
            (AutomationTarget::Builtin(_), _) => (0.0, 2.0),
            (AutomationTarget::Path(_), Some(m)) => (m.min, m.max),
            (AutomationTarget::Path(_), None) => unreachable!("path meta validated above"),
        };
        let min = opts.min.unwrap_or(default_min);
        let max = opts.max.unwrap_or(default_max);
        let smooth_ms_raw = opts.smooth_ms.unwrap_or(DEFAULT_SMOOTH_MS);
        // TS L984：smoothMs < 0 ? 0 : smoothMs
        let smooth_ms = if smooth_ms_raw < 0.0 { 0.0 } else { smooth_ms_raw };
        let key = key_of(cc, opts.event_type);
        self.map.insert(
            key,
            BindingState {
                binding: MidiBinding {
                    cc,
                    target,
                    min,
                    max,
                    smooth_ms,
                    invert: opts.invert.unwrap_or(false),
                },
                current: 0.0,
                target: 0.0,
            },
        );
        Ok(())
    }

    /// 解除绑定（cc 或 note）；无绑定则无操作。返回是否解除成功（镜像 TS `midiUnlearn`）。
    pub fn unlearn(&mut self, cc: i32, event_type: ControlKind) -> bool {
        let removed = self.map.remove(&key_of(cc, event_type)).is_some();
        self.refresh_master_bound();
        removed
    }

    /// 当前全部绑定（副本；键升序：cc 键在前、note 键在后——确定性遍历，
    /// TS 为插入序，此处为已文档化的顺序偏差）。
    pub fn get_bindings(&self) -> Vec<MidiBinding> {
        self.map.values().map(|st| st.binding.clone()).collect()
    }

    /// 绑定条数。
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// 是否无绑定。
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 是否存在 builtin masterGain 绑定（镜像 TS `_midiMasterBound`）。
    pub fn master_bound(&self) -> bool {
        self.master_bound
    }

    /// 重算 `master_bound`（镜像 TS `refreshMidiMasterBound` HE L1149–L1158）。
    fn refresh_master_bound(&mut self) {
        self.master_bound = self
            .map
            .values()
            .any(|st| matches!(st.binding.target, AutomationTarget::Builtin(BuiltinParam::MasterGain)));
    }

    /// 复位运行时状态、保留绑定（镜像 TS `reset()` L923–L926：current/target 归 0；
    /// 事件队列由 [`MidiEventRing::clear`] 单独清）。
    pub fn reset_runtime(&mut self) {
        for st in self.map.values_mut() {
            st.current = 0.0;
            st.target = 0.0;
        }
    }

    /// 消费事件环全部事件并应用自动化（块头调用；镜像 TS `consumeMidiQueue`）。
    ///
    /// 契约：
    /// - **空环早退**：镜像 TS 引擎 `if (_midiCount > 0)` 守卫（HE L675–L676）——
    ///   队列为空的块不做任何事（含平滑收敛）；调用方可以每块无脑调用，
    ///   行为仍与引擎逐块一致。
    /// - 事件按到达顺序弹出：CC 值 clamp [0,127]/127 线性映射 `[min,max]`（invert 反向）；
    ///   noteOn → max；noteOff → min；未绑定控制号安全忽略。
    /// - `smooth_ms <= 0`：值直接到位；`smooth_ms > 0`：一阶平滑向 target 走一步
    ///   （alpha = `1 − exp(−block_size / sample_rate / (smooth_ms/1000))`，运算顺序与
    ///   TS L1052 逐项一致）；绑定平滑状态初值 current = target = 0（镜像 TS）。
    /// - 事件耗尽后，对全部平滑中绑定（`|current − target| > 1e-9`）再各走一步
    ///   （HE L1060–L1071）。
    /// - 返回本块被触及的参数 section 去重表（首现顺序；来自
    ///   [`refresh_sections_for_path`]，builtin 写回不入表——镜像 TS builtin 分支
    ///   提前返回、不走 `refreshModuleForPath`）。
    pub fn consume(
        &mut self,
        ring: &mut MidiEventRing,
        params: &mut Value,
        sample_rate: f64,
        block_size: usize,
    ) -> Vec<&'static str> {
        // 镜像引擎守卫：块头队列为空 → 本块不消费、不收敛（HE L675–L676）
        if ring.is_empty() {
            return Vec::new();
        }
        // alpha 缓存实例级复用（clear 不分配）：消费稳态零分配（HE L1015–L1016）
        self.alpha_cache.clear();
        let mut sections: Vec<&'static str> = Vec::new();
        while let Some(ev) = ring.pop() {
            // TS L1025：key = t === 0 ? a : 0x4000 + a；非整/越域键不可能来自 learn，安全忽略
            let key = match event_key(ev.kind, ev.a) {
                Some(k) => k,
                None => continue,
            };
            let Some(st) = self.map.get_mut(&key) else {
                continue; // 未绑定控制号：安全忽略（HE L1027）
            };
            let target_value = target_value_for(&st.binding, ev.kind, ev.b);
            if st.binding.smooth_ms <= 0.0 {
                // smoothMs<=0 直接应用（HE L1044–L1047）
                st.current = target_value;
                st.target = target_value;
                apply_automation_value(&st.binding.target, target_value, params, &mut sections);
            } else {
                // 一阶平滑（防 zipper）：current += (target − current) × alpha（HE L1049–L1056）
                st.target = target_value;
                let alpha = Self::alpha_for(&mut self.alpha_cache, st.binding.smooth_ms, sample_rate, block_size);
                st.current += (st.target - st.current) * alpha;
                apply_automation_value(&st.binding.target, st.current, params, &mut sections);
            }
        }
        // 无事件但存在平滑中绑定：继续向 target 收敛（HE L1060–L1071；
        // BTreeMap 键升序遍历，确定性——见模块头「绑定键与确定性」）
        for st in self.map.values_mut() {
            let smooth_ms = st.binding.smooth_ms;
            if smooth_ms > 0.0 && (st.current - st.target).abs() > 1e-9 {
                let alpha = Self::alpha_for(&mut self.alpha_cache, smooth_ms, sample_rate, block_size);
                st.current += (st.target - st.current) * alpha;
                let target = &st.binding.target;
                apply_automation_value(target, st.current, params, &mut sections);
            }
        }
        sections
    }

    /// 取（必要时计算）平滑 alpha（镜像 TS alphaCache 语义，键 = smoothMs 的位型）。
    ///
    /// 以关联函数形态接收缓存切片：调用点持有 `self.map` 的可变借用，
    /// 借用按字段互斥拆分（`alpha_cache` 与 `map` 不相交），与方法接收器形态等价。
    fn alpha_for(cache: &mut HashMap<u64, f64>, smooth_ms: f64, sample_rate: f64, block_size: usize) -> f64 {
        let key = smooth_ms.to_bits();
        match cache.get(&key) {
            Some(&a) => a,
            None => {
                // TS L1052：1 − Math.exp(−blockSize / fs / (smoothMs / 1000))，运算顺序原样
                let alpha = 1.0 - (-(block_size as f64) / sample_rate / (smooth_ms / 1000.0)).exp();
                cache.insert(key, alpha);
                alpha
            }
        }
    }
}

/// 绑定键计算（镜像 TS L977/L994：cc → cc；note → 0x4000 + cc）。
fn key_of(cc: i32, event_type: ControlKind) -> i64 {
    match event_type {
        ControlKind::Cc => cc as i64,
        ControlKind::Note => NOTE_KEY_OFFSET + cc as i64,
    }
}

/// 事件侧键计算（镜像 TS L1025 `key = t === 0 ? a : 0x4000 + a`）。
///
/// TS 键为 JS number；非整数或超 i64 域的键不可能来自 `learn`（cc/note 为整数），
/// 返回 `None` 由消费侧安全忽略——与 TS Map 查不到等价。
fn event_key(kind: MidiEventKind, a: f64) -> Option<i64> {
    let key = match kind {
        MidiEventKind::Cc => a,
        _ => NOTE_KEY_OFFSET as f64 + a,
    };
    if key.is_finite() && key.fract() == 0.0 && key >= i64::MIN as f64 && key <= i64::MAX as f64 {
        Some(key as i64)
    } else {
        None
    }
}

/// 事件 → 目标值（镜像 TS L1031–L1041）：
/// CC 值 clamp [0,127]/127 线性映射（invert 反向）；noteOn → max；noteOff → min。
fn target_value_for(binding: &MidiBinding, kind: MidiEventKind, b: f64) -> f64 {
    match kind {
        MidiEventKind::Cc => {
            let v = js_min(127.0, js_max(0.0, b)) / 127.0;
            binding.min + (binding.max - binding.min) * (if binding.invert { 1.0 - v } else { v })
        }
        MidiEventKind::NoteOn => binding.max,
        MidiEventKind::NoteOff => binding.min,
    }
}

/// 应用自动化值到目标（镜像 TS `applyAutomationValue` HE L1075–L1090）。
///
/// - builtin：钳 [0,2] 后**无条件写**顶层 JSON 叶子 `masterGain` / `stereoWidth`
///   （JSON 形态映射：TS builtin 写的是引擎内部 `_modMasterGain` 与 `_params.stereoWidth`
///   顶层叶子；服务层按同名顶层叶子读取，每块生效，不需要 section 刷新）。
/// - path：白名单查不到 → 安全忽略（learn 时已校验，运行时防御）；值钳到白名单范围，
///   `boolean` 元数据按 0.5 阈值离散化为 0/1 再写（布尔叶子落 `value >= 0.5`）；
///   写回后把 [`refresh_sections_for_path`] 给出的 section 追加入 `sections`。
pub fn apply_automation_value(
    target: &AutomationTarget,
    value: f64,
    params: &mut Value,
    sections: &mut Vec<&'static str>,
) {
    match target {
        AutomationTarget::Builtin(param) => {
            let v = clamp_builtin(value);
            let key = match param {
                BuiltinParam::MasterGain => "masterGain",
                BuiltinParam::StereoWidth => "stereoWidth",
            };
            if let Some(obj) = params.as_object_mut() {
                obj.insert(key.to_string(), serde_json::json!(v));
            }
        }
        AutomationTarget::Path(path) => {
            let Some(meta) = find_automatable_param(path) else {
                return; // learn 时已校验；运行时安全忽略（HE L1084–L1085）
            };
            // TS L1086–L1087：钳白名单范围；boolean 按 0.5 阈值离散化
            let mut v = if value < meta.min {
                meta.min
            } else if value > meta.max {
                meta.max
            } else {
                value
            };
            if meta.boolean {
                v = if v >= 0.5 { 1.0 } else { 0.0 };
            }
            write_param_path(params, path, v);
            for s in refresh_sections_for_path(path) {
                if !sections.contains(s) {
                    sections.push(s);
                }
            }
        }
    }
}

/// 写回参数快照叶子字段（点分路径；镜像 TS `writeParamPath` HE L1093–L1106）。
///
/// 逐层下钻：中间层缺失或非对象 → 整体放弃；叶子仅当**已存在**且为 boolean
/// （→ `value >= 0.5`）或 number（→ `value`）时写入，其他类型 / 缺失一律不动。
pub fn write_param_path(params: &mut Value, path: &str, value: f64) {
    let mut it = path.split('.');
    // 无点路径：唯一段即叶子（如白名单顶层路径 'masterGain'）
    let Some(leaf) = it.next_back() else { return };
    let Some(mut obj) = params.as_object_mut() else { return };
    for k in it {
        let Some(next) = obj.get_mut(k) else { return };
        // TS：typeof next !== 'object' || next === null → return
        if !next.is_object() {
            return;
        }
        obj = next.as_object_mut().unwrap();
    }
    match obj.get(leaf) {
        Some(Value::Bool(_)) => {
            obj.insert(leaf.to_string(), Value::Bool(value >= 0.5));
        }
        Some(Value::Number(_)) => {
            obj.insert(leaf.to_string(), serde_json::json!(value));
        }
        _ => {} // 缺失 / 字符串 / 对象 / null / 数组：不写
    }
}

/// 参数路径 → 需刷新的 section 映射（镜像 TS `refreshModuleForPath` HE L1109–L1146）。
///
/// 返回的 section 是参数快照 JSON 内需要重新下推到 DSP 模块的子树键：
/// - 空表 = TS 中该路径**无需 setter**（`compressor.enabled` / `reverb.enabled` /
///   `pitch.*` 的 active() 每块读快照；`stereoWidth` 每块读快照；未知路径无映射）；
/// - `reverb.algorithmic` 在 TS 同时下推 simple 与 FDN 两个后端（HE L1115–L1119），
///   服务层消费该 section 时须对两个后端同源刷新。
pub fn refresh_sections_for_path(path: &str) -> &'static [&'static str] {
    if path == "compressor.enabled" {
        return &[]; // active() 读快照，无需 setter（HE L1111）
    }
    if path.starts_with("compressor.") {
        return &["compressor"];
    }
    if path.starts_with("deesser.") {
        return &["deesser"];
    }
    if path.starts_with("bassEnhancer.") {
        return &["bassEnhancer"];
    }
    if path.starts_with("reverb.algorithmic.") {
        // TS 同时 setParams _reverbSimple 与 _fdnReverb（HE L1115–L1119）
        return &["reverb.algorithmic"];
    }
    if path == "reverb.enabled" {
        return &[]; // active() 读快照（HE L1120）
    }
    if path.starts_with("modEffects.delay.") {
        return &["modEffects.delay"];
    }
    if path.starts_with("modEffects.chorus.") {
        return &["modEffects.chorus"];
    }
    if path.starts_with("modEffects.flanger.") {
        return &["modEffects.flanger"];
    }
    if path.starts_with("modEffects.phaser.") {
        return &["modEffects.phaser"];
    }
    if path.starts_with("modEffects.tremolo.") {
        return &["modEffects.tremolo"];
    }
    if path.starts_with("ieq.") {
        return &["ieq"];
    }
    if path.starts_with("dynamicEq.") {
        return &["dynamicEq"];
    }
    if path.starts_with("limiter.") {
        return &["limiter"];
    }
    if path.starts_with("pitch.") {
        return &[]; // M/S 每块读快照；离线用（HE L1144）
    }
    // stereoWidth 顶层路径（builtin）与未知路径：M/S 每块读快照 / 无映射
    &[]
}
