# 规格：midi —— MIDI 事件接口 / MIDI Learn（控制率参数自动化）

> **规格属性**：双支线共享规格（Phase 3）。行为事实标准 = TS 引擎
> `src/engine/HyperSoundEngine.ts`（MIDI 机制段）+ `src/types.ts`
> （`AutomationTarget` / `MidiEvent` / `MidiBinding` / `AUTOMATABLE_PARAMS`）；
> Rust 支线实现 = `HyperSoundEngineRust/crates/hse-core/src/midi.rs`。
> 本模块**不是** DSP Stage：无音频样本进出，是控制率「事件 → 参数」机器，
> 参数快照以 JSON（服务层 PilotParams 形态）承载。

---

## 一、模块概述

- **定位**：把外部 MIDI 控制流（调音台推子 / 键盘 / DAW CC 自动化轨）接到引擎参数上：
  CC 线性映射到参数范围、note on/off 驱动参数（布尔开关或 min/max）、MIDI Learn 绑定表、
  一阶平滑防 zipper。
- **形态**：三件套，全部预分配、确定性、无随机 / 无时钟 / 无 console：
  1. `MidiEventRing` —— 事件环形队列（生产者入队，块头消费）；
  2. `MidiBindings` —— 绑定表（cc/note → 目标 + 范围 + 平滑）+ 块头消费语义；
  3. 纯函数助手：`write_param_path`（点分路径写回）、`refresh_sections_for_path`
     （路径 → 需刷新 section 映射）、`apply_automation_value`（单值应用）、
     `AUTOMATABLE_PARAMS` / `find_automatable_param`（白名单）。
- **实时安全**：事件环为定长数组（容量常量 `MIDI_QUEUE_CAP = 4096`），消费稳态零分配；
  alpha 缓存 `clear` 保留容量。learn/unlearn/reset 属控制面操作。
- **确定性**：同事件序列 + 同绑定 + 同块长 + 同采样率 → 参数写回逐位一致。
  绑定表以键升序（BTreeMap）迭代；TS 端为插入序（Map）——单绑定数值语义不变，
  仅「多绑定写同一路径」时块内最后写者不同（TS=后插入者，Rust=键大者），
  无向量依赖此顺序（已文档化偏差）。

## 二、事件模型与队列语义

### 2.1 事件类型

引擎只消费三种事件（channel 不参与编码、不入绑定键）：

| 类型 | 编码 t | a | b |
|---|---|---|---|
| cc | 0 | cc 号 | value（0..127） |
| noteOn | 1 | note 号 | velocity |
| noteOff | 2 | note 号 | **恒 0**（编码时置 0） |

- a/b 入环时落 **f32**（镜像 TS Float32Array 存储；合法 MIDI 值 0..127 全为精确整数，
  量化不产生可见差异）。
- channel 在 TS 编码与绑定键中均被丢弃；Rust 事件结构中不存在 channel 字段。

### 2.2 入队与溢出

- **Given** 事件环未满，**When** 入队，**Then** 事件写入 tail，tail 前移（模容量），计数 +1。
- **Given** 事件环已满（count == 4096），**When** 再入队，**Then** **丢弃最旧**事件
  （head 前移、计数 −1）、dropped 计数 +1，新事件照常入队；不抛错、不影响消费侧。
- **Given** 丢弃计数已被观察，**When** `clear()` / 引擎 reset()，**Then** 丢弃计数**保留**
  （自构造起累计；reset 只清 head/tail/count 与平滑状态）。

### 2.3 块头消费

- **Given** 块头队列非空，**When** 处理块开始，**Then** 按到达顺序弹出全部事件逐个应用
  （块速率，非 sample-accurate；同块后到事件后写者生效），随后对全部平滑中绑定再走一步
  收敛（见 §四）。
- **Given** 块头队列为空，**When** 处理块开始，**Then** **本块不做任何 MIDI 事**
  （含平滑收敛不推进）——镜像 TS 引擎 `if (_midiCount > 0)` 守卫。调用方可每块无脑调用
  `consume`，行为仍与引擎逐块一致。
- **Given** 事件的控制号无绑定，**When** 消费，**Then** 安全忽略，无任何副作用。

## 三、绑定模型（MIDI Learn）

### 3.1 绑定条目

| 字段 | 含义 | 缺省 |
|---|---|---|
| cc | 控制号（cc 绑定存 cc 号；note 绑定存 note 号） | — |
| target | builtin（masterGain / stereoWidth）或白名单点分路径 | — |
| min / max | 映射范围：CC 0 / noteOff → min；CC 127 / noteOn → max | builtin → [0, 2]；path → 白名单 `[meta.min, meta.max]`；opts 覆盖 |
| smoothMs | 一阶平滑时间常数 ms | 20；负值入库钳 0 |
| invert | 反向映射（CC 0 → max，CC 127 → min） | false |

### 3.2 绑定键

- cc 绑定键 = cc；note 绑定键 = `0x4000 + note`（共享一张表，以 0x4000 分命名空间；
  cc 键与 note 键互不冲突——**channel 不参与键**）。
- **Given** 同键重复 learn，**When** 入库，**Then** 覆盖旧绑定并重置平滑状态
  （current/target 归 0）。
- **Given** unlearn，**When** 键存在，**Then** 移除并重算 masterGain 绑定标志，返回 true；
  键不存在则无操作返回 false。

### 3.3 learn 校验（白名单语义）

- **Given** learn 的目标为 path 且不在 `AUTOMATABLE_PARAMS`（38 条，逐条复刻 TS 白名单；
  前两条为顶层路径形态 `masterGain` / `stereoWidth`），**When** learn，**Then**
  立即报错 `unknown automatable path: <path>`，绑定不入库。
- **Given** 目标为 builtin masterGain，**When** learn，**Then** 置位 masterGain 绑定标志
  （供宿主门控 mod-master-gain 级）；unlearn 后重算（全表扫描）。
- 运行时（消费时）查不到白名单 → 安全忽略（learn 时已校验，防御分支）。

### 3.4 reset（运行时状态 vs 配置）

- **Given** 引擎 reset，**When** 复位，**Then** 绑定表**保留**（配置）；事件队列清空、
  各绑定平滑状态 current/target 归 0（运行时状态）；dropped 计数保留。

## 四、映射与平滑语义

### 4.1 目标值计算（块速率）

| 事件 | 目标值 |
|---|---|
| cc | `v = clamp(b, 0, 127) / 127`；`target = min + (max − min) × (invert ? 1 − v : v)` |
| noteOn | `target = max`（布尔参数 → true；数值参数 → max） |
| noteOff | `target = min`（布尔参数 → false；数值参数 → min） |

- 越界 b 值双向钳制 [0, 127] 后参与映射。

### 4.2 一阶平滑（防 zipper）

- **Given** 绑定 smoothMs ≤ 0，**When** 事件应用，**Then** current = target = 目标值，
  立即写回（无平滑）。
- **Given** 绑定 smoothMs > 0，**When** 事件应用，**Then**：
  - `target = 目标值`；平滑状态**初值 current = target = 0**（镜像 TS，绑定创建时归零；
    首次应用从 0 出发走 alpha 步，不取参数现值）；
  - `alpha = 1 − exp(−blockSize / fs / (smoothMs / 1000))`（运算顺序原样保留；
    alpha 按 smoothMs 键缓存，每次消费开头清空）；
  - `current += (target − current) × alpha`，写回 current。
- **Given** 事件耗尽且队列本块非空，**When** 收敛阶段，**Then** 对全部
  `smoothMs > 0` 且 `|current − target| > 1e-9` 的绑定各再走一步并写回。
- **Given** 下一块队列为空，**When** 处理，**Then** 收敛**不推进**（值冻结，
  见 §2.3——镜像 TS 引擎守卫）。

### 4.3 布尔参数离散化

- **Given** 白名单元数据 `boolean = true`（仅 `compressor.enabled` 与 `reverb.enabled`），
  **When** 写回，**Then** 值先钳到白名单范围、再按 0.5 阈值离散化（≥ 0.5 → true，否则 false）
  落 JSON 布尔叶子。CC 驱动布尔参数同样生效（CC ≥ 64 → true）。

## 五、参数路径寻址与 JSON 写回契约（服务层）

### 5.1 点分路径写回（`write_param_path`）

镜像 TS `writeParamPath` 的守卫语义：

- **Given** 路径逐层下钻中某中间层缺失或非对象，**When** 写回，**Then** 整体放弃（不创建、不 panic）。
- **Given** 叶子**已存在**且为 boolean，**When** 写回，**Then** 置 `value >= 0.5`。
- **Given** 叶子已存在且为 number，**When** 写回，**Then** 置 `value`（已钳白名单范围）。
- **Given** 叶子缺失 / 字符串 / 对象 / null / 数组，**When** 写回，**Then** 不动。

### 5.2 builtin 目标写回

- TS builtin masterGain 写引擎内部字段 `_modMasterGain`、stereoWidth 写 `_params.stereoWidth`
  （顶层叶子），值钳 [0, 2]，且**不走** section 刷新（mod-master-gain / M-S 每块读值）。
- JSON 形态契约：builtin → **无条件写**顶层叶子 `masterGain` / `stereoWidth`
  （叶子缺失则创建；服务层每块读该叶子，无需 section 刷新）。
- 白名单顶层路径 `masterGain` 以 **path 形态** learn 时走 §5.1 守卫：TS `_params.masterGain`
  不存在 → 写回 no-op；服务层参数 JSON 若预置了该数值叶子则会正常写回。

### 5.3 section 刷新映射（`refresh_sections_for_path`）

镜像 TS `refreshModuleForPath` 的 if 链，返回需重新下推到 DSP 模块的参数子树键（去重保序）：

| 路径 | sections |
|---|---|
| `compressor.enabled` / `reverb.enabled` / `pitch.*` / `masterGain` / 未知 | `[]`（active() 每块读快照 / 无映射） |
| `compressor.*` | `["compressor"]` |
| `deesser.*` | `["deesser"]` |
| `bassEnhancer.*` | `["bassEnhancer"]` |
| `reverb.algorithmic.*` | `["reverb.algorithmic"]`（TS 同时下推 simple 与 FDN 两个后端——服务层消费该 section 须双后端同源刷新） |
| `modEffects.delay.*` / `chorus.*` / `flanger.*` / `phaser.*` / `tremolo.*` | `["modEffects.<fx>"]` |
| `ieq.*` | `["ieq"]` |
| `dynamicEq.*` | `["dynamicEq"]` |
| `limiter.*` | `["limiter"]` |

- **Given** 一次 consume 触及多条同 section 路径，**When** 返回 sections，**Then** 去重且
  保持首现顺序。
- **Given** 写回被 §5.1 守卫放弃（叶子缺失等），**When** 返回 sections，**Then** 仍按上表
  返回（TS 中 refresh 调用与写回成败无关）。
- `consume` 返回值即本契约的批量形态：服务层按返回的 sections 决定哪些模块参数需要
  重新装配；builtin 写回不出现在返回值中（调用方直接读顶层叶子）。

## 六、接口签名（Rust 事实标准摘录）

```rust
pub const MIDI_QUEUE_CAP: usize = 4096;          // 事件环容量
pub const NOTE_KEY_OFFSET: i64 = 0x4000;         // note 键偏移
pub const DEFAULT_SMOOTH_MS: f64 = 20.0;

pub enum MidiEventKind { Cc, NoteOn, NoteOff }
pub struct MidiEventIn { pub kind: MidiEventKind, pub a: f64, pub b: f64 }   // channel 不入模

pub struct MidiEventRing;   // 定长数组环；push/push_slice/pop/len/is_empty/dropped/clear
                            // 溢出 = 丢弃最旧 + dropped += 1；clear 保留 dropped

pub enum BuiltinParam { MasterGain, StereoWidth }
pub enum AutomationTarget { Builtin(BuiltinParam), Path(String) }
pub struct MidiBinding { pub cc: i32, pub target: AutomationTarget,
                         pub min: f64, pub max: f64, pub smooth_ms: f64, pub invert: bool }
pub struct LearnOpts { pub event_type: ControlKind /* Cc|Note */, pub min: Option<f64>,
                       pub max: Option<f64>, pub smooth_ms: Option<f64>, pub invert: Option<bool> }

pub struct MidiBindings;
impl MidiBindings {
    pub fn learn(&mut self, cc: i32, target: AutomationTarget, opts: LearnOpts) -> Result<(), String>;
    pub fn unlearn(&mut self, cc: i32, event_type: ControlKind) -> bool;
    pub fn get_bindings(&self) -> Vec<MidiBinding>;        // 键升序（确定性；TS 为插入序）
    pub fn master_bound(&self) -> bool;                    // 镜像 _midiMasterBound
    pub fn reset_runtime(&mut self);                       // 平滑状态归 0，绑定保留
    pub fn consume(&mut self, ring: &mut MidiEventRing, params: &mut serde_json::Value,
                   sample_rate: f64, block_size: usize) -> Vec<&'static str>;  // sections
}

pub const AUTOMATABLE_PARAMS: &[AutomatableParamMeta];     // 38 条白名单
pub fn find_automatable_param(path: &str) -> Option<&'static AutomatableParamMeta>;
pub fn apply_automation_value(target: &AutomationTarget, value: f64,
                              params: &mut Value, sections: &mut Vec<&'static str>);
pub fn write_param_path(params: &mut Value, path: &str, value: f64);
pub fn refresh_sections_for_path(path: &str) -> &'static [&'static str];
```

## 七、golden 测试声明

- **TS 行为规格**：TS 侧 `test/midi.test.ts` 14 用例为行为事实标准。
- **Rust 对拍测试**：`HyperSoundEngineRust/crates/hse-core/tests/midi_standalone.rs`
  以 `#[path]` 独立编译 `src/midi.rs`（集成登记前不进 lib.rs），28 用例 =
  TS 14 用例逐一移植（`ts01`–`ts14`；`ts10` builtin masterGain 由音频输出断言适配为
  顶层 `masterGain` JSON 叶子断言）+ 14 条补充契约用例（`extra01`–`extra14`：
  空环冻结语义、逐块单调收敛、布尔阈值 0.5、cc/note 命名空间、section 映射表、
  白名单缺省/钳制/覆盖、写回守卫、master_bound、dropped 保留、f32 落点、
  同块多事件、白名单完整性）。
- **确定性门禁**：`ts12` 要求同事件序列两次运行位型级一致；无随机 / 时钟 / console。
- **禁改条款**：既有冻结对拍向量与本模块无关（控制率机器，无音频向量）；
  行为变更 = 新增向量/用例或走 MAJOR。

## 八、范围外

- MIDI 2.0 / MPE、MIDI 时钟同步、SysEx、运行状态字节（PRD 范围外条款继续有效）；
- sample-accurate 参数自动化（本模块为块速率契约）；
- 合成器触发（note 触发振荡器）与波表；
- 绑定表纳入参数快照 / 分享串（Learn 绑定为引擎内配置，不进 ShareCodec）；
- `hse-service` 的控制面协议接线（由服务层会话按本规格 §5.3 契约消费 sections）。
