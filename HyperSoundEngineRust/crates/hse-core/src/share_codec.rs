//! share_codec —— 分享串编解码管线（Phase 3 批次二）。
//!
//! 行为事实标准是 TS 支线 `src/engine/ShareCodec.ts` 的解码路径（`decodeShareCode`），
//! 本模块为其逐语义移植：v2 编码、两代传输解码、信封布局、校验和、差异载荷还原
//! （rehydrate）+ `sanitizeParams` 全量白名单清洗（含 spatial 深度清洗），错误消息逐字对齐。
//!
//! # 序列化格式（两代并存，解码端全收；与 TS 头注释一致）
//!
//! - **v2（当前编码输出）**：`HSE2-<Crockford Base32 分组串>`。载荷 =
//!   `<version>:<8位fnv1a hex>:<deltaJson>`，version=2；deltaJson 只存与默认参数的
//!   差异项（sampleRate 强制携带）。Crockford Base32：0-9 + 去易混字符的大写字母
//!   （无 I/L/O/U），大小写不敏感，解码端把 I/L→1、O→0、U→V 归一；每 5 字符一组
//!   以 `-` 分隔仅为可读性，解码前剥掉全部分隔符与空白。
//! - **v1（旧串，只解码）**：`base64url("<version>:<checksum>:<json>")`，json 为
//!   全量参数快照（无填充；长度 %4==1 非法）。
//! - checksum 为覆盖 `<version>:<json>` 的 FNV-1a 32 位值（8 位小写十六进制），
//!   固定长度置于版本号之后，保证 json 内含 `:` 也能无歧义解析。
//! - 两代 json 均为"去 IR 数组"后的参数：卷积混响 IR 数组不参与序列化，仅保留
//!   irName 引用，解码后 ir 恒为 null，由调用方按 irName 重新加载。
//!
//! # v2 差异载荷还原（rehydrate over 默认骨架）
//!
//! 解码端以 `default_params_skeleton(sample_rate)`（镜像 TS
//! `toShareObject(createDefaultParams(sampleRate))` 的**逐字段缺省值**，含 spatial
//! 默认块与 5.1 布局扬声器表）为底、delta 覆盖：数组与叶子整体替换、未知键静默
//! 丢弃（delta 中不存在于骨架的键不还原），随后与 v1 走同一套白名单清洗。
//! 骨架采样率取 delta 的 `sampleRate`（缺失/非有限数回落 48000；越界由白名单
//! clamp 最终兜底——骨架采样率只影响还原底座，不做独立校验）。
//!
//! # 白名单清洗（sanitize_params）
//!
//! 只读取已知字段：未知字段（含 `__proto__`/`constructor`/`prototype` 注入键）
//! 一律丢弃；非有限数/类型不符回落默认值；越界数值钳到白名单区间；字符串枚举
//! 外回落默认；超长字符串按 UTF-16 码元长度截断（与 JS `slice` 语义一致）。
//! 各 section 的字段/区间/缺省值与 TS `sanitizeParams` 逐字对齐（eq 10/20 段
//! proBands 白名单重建、loudnessCompensation 32 段上限、modulation 16 条路由上限、
//! dynamicEq 5 带固定重建等）。
//!
//! # spatial 深度清洗（decodeSpatial 语义）
//!
//! spatial 段不做逐字段白名单（避免与 spatial/types 双份维护漂移），而是：
//! 递归只保留 boolean/有限 number/string/plain array/plain object（限深 12、单
//! 对象键数 256；`__proto__`/`constructor`/`prototype` 键直接丢弃；任一子项非法
//! 则整体拒绝），再校验 mode 白名单 + 五个子对象（instant/headLocked/world/stage/
//! ambience）存在性——任一缺失整体回落 `createDefaultSpatialSettings()`（mode
//! 'off'）。通过后以默认骨架合并（JS spread 语义：默认键在前、清洗后的键覆盖、
//! **子对象内的额外键保留**、顶层额外键丢弃），refDistance/maxDistance 钳位。
//!
//! # 数值序列化纪律
//!
//! TS 侧全部数值为 f64，`JSON.stringify` 对整值输出不带小数点。本模块所有输出
//! 数值经 [`jnum`] 归一（整值→i64、-0→0、非整值→f64 最短表示），保证与 TS
//! 序列化形态一致；对比测试按结构化（数值按 f64 相等）比对，不依赖键序。
//!
//! # 与 TS 的已知微差（均为病态构造输入，可安全忽略）
//!
//! - JSON 浮点溢出（如 `1e999`）：JS `JSON.parse` 得 `Infinity`（随后被白名单
//!   回落默认值）；serde_json 解析报错 → 本模块报 malformed JSON。
//! - 截断恰好把代理对切成两半时：JS 产生孤立代理项（`JSON.stringify` 转义保留），
//!   本模块以 U+FFFD 等价表达（serde 解析孤立代理转义同样归一为 U+FFFD）。
//! - 输入前缀含非 ASCII 大小写映射歧义/`\u{85}`（NEL）等空白归一的极端差异。
//!
//! 确定性：纯函数、无随机/时钟/控制台输出；同串解码结果唯一。
//! 本模块只做参数 JSON 解析与清洗，不涉及音频设备与实时回调（无实时安全约束）。

use serde_json::{json, Map, Number, Value};

/// 当前分享串格式版本（编码输出）；载荷 2 = 仅存与默认参数的差异项。
pub const SHARE_CODEC_VERSION: u32 = 2;
/// 旧版全量载荷版本（只解码不编码；旧分享串持续可导入）。
pub const SHARE_CODEC_LEGACY_VERSION: u32 = 1;

const B64_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const B32_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// HSE2 前缀（v2 传输哨兵；纯 ASCII，跨输入法/剪贴板稳定）。
const SHARE_CODE_PREFIX: &str = "HSE2";

// ---------------------------------------------------------------------------
// FNV-1a 32 位（公开算法，公有领域）
// ---------------------------------------------------------------------------

/// FNV-1a 32 位，按 **UTF-16 码元**散列——JS `charCodeAt` 语义。
///
/// TS 侧载荷 json 可能含非 ASCII 字符（如中文 sceneId 原样留在 JSON 文本里），
/// 校验和按 UTF-16 码元（BMP 一元、增补平面代理对两元）推进；按 UTF-8 字节散列
/// 将得到不同结果，故此处显式 `encode_utf16`。`Math.imul` 即 32 位环绕乘法，
/// 与 `u32::wrapping_mul` 逐位一致（末尾 `>>> 0` 对 u32 恒等）。
fn fnv1a32_utf16(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for unit in s.encode_utf16() {
        h ^= unit as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

fn checksum_of(version: &str, json: &str) -> String {
    format!("{:08x}", fnv1a32_utf16(&format!("{}:{}", version, json)))
}

// ---------------------------------------------------------------------------
// base64url（RFC 4648 §5，URL 安全字母表，无填充）
// ---------------------------------------------------------------------------

/// base64url → 字节；非法字符/非法长度返回 Err（防注入入口）。
///
/// JS 版按 UTF-16 码元迭代（`t[i]`）；ASCII 输入下与按字符迭代逐位一致，
/// 含非 ASCII 字符的输入两侧都在字母表查找处失败，仅错误分支可能不同
/// （接收/拒绝判定一致）。长度校验按 UTF-16 码元数（`s.length`）。
fn base64url_to_bytes(s: &str) -> Result<Vec<u8>, String> {
    let chars: Vec<char> = s.chars().collect();
    let utf16_len: usize = chars.iter().map(|c| c.len_utf16()).sum();
    if utf16_len % 4 == 1 {
        return Err("invalid share code: bad base64url length".to_string());
    }
    // 兼容带填充输入（本实现生成时无填充）
    let mut padded = chars;
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    let mut out = Vec::with_capacity(padded.len() / 4 * 3);
    for g in padded.chunks(4) {
        let idx = |ch: char| -> i32 {
            // JS：B64_ALPHABET.indexOf——'=' 不在字母表 → -1（错误）
            B64_ALPHABET
                .iter()
                .position(|&b| b == ch as u8)
                .map_or(-1, |p| p as i32)
        };
        let c0 = idx(g[0]);
        let c1 = idx(g[1]);
        let c2 = if g[2] == '=' { 0 } else { idx(g[2]) };
        let c3 = if g[3] == '=' { 0 } else { idx(g[3]) };
        if c0 < 0 || c1 < 0 || c2 < 0 || c3 < 0 {
            return Err("invalid share code: bad base64url character".to_string());
        }
        out.push(((c0 << 2) | (c1 >> 4)) as u8);
        if g[2] != '=' {
            out.push((((c1 & 0x0f) << 4) | (c2 >> 2)) as u8);
        }
        if g[3] != '=' {
            out.push((((c2 & 0x03) << 6) | c3) as u8);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Crockford Base32（0-9 + 去易混字符的大写字母）
// ---------------------------------------------------------------------------

/// JS 正则 `[-\s]` 的字符集（ECMAScript `\s`，含 NBSP/ZWNBSP 等）。
fn is_js_regex_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

fn bytes_to_base32_crockford(bytes: &[u8]) -> String {
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    let mut out = String::new();
    for &byte in bytes {
        acc = (acc << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            out.push(B32_ALPHABET[((acc >> (bits - 5)) & 0x1f) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(B32_ALPHABET[((acc << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

fn group_code(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 5);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && i % 5 == 0 {
            out.push('-');
        }
        out.push(ch);
    }
    out
}

/// Crockford Base32 → 字节；剥离由调用方完成，这里做大小写归一
/// （I/L→1、O→0、U→V），非法字符返回 Err。JS `for..of` 按**码点**迭代、
/// 对整串 `toUpperCase()` 后再查表——此处 `char` 迭代与全串 `to_uppercase`
/// 等价（单字符展开如 ß→SS 两侧都会因查表失败而报错）。
fn base32_crockford_to_bytes(s: &str) -> Result<Vec<u8>, String> {
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    let mut out = Vec::new();
    for ch0 in s.to_uppercase().chars() {
        let ch = match ch0 {
            'I' | 'L' => '1',
            'O' => '0',
            'U' => 'V',
            other => other,
        };
        let v = B32_ALPHABET
            .iter()
            .position(|&b| b == ch as u8)
            .ok_or_else(|| format!("invalid share code: bad base32 character {}", ch0))?;
        acc = (acc << 5) | v as u32;
        bits += 5;
        if bits >= 8 {
            out.push(((acc >> (bits - 8)) & 0xff) as u8);
            bits -= 8;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 差异载荷还原（shareRehydrate）
// ---------------------------------------------------------------------------

fn share_delta(base: &Value, full: &Value) -> Option<Value> {
    match (base, full) {
        (Value::Object(b), Value::Object(f)) => {
            let mut out = Map::new();
            for (k, fv) in f {
                match b.get(k).and_then(|bv| share_delta(bv, fv)) {
                    Some(delta) => {
                        out.insert(k.clone(), delta);
                    }
                    None if !b.contains_key(k) => {
                        out.insert(k.clone(), fv.clone());
                    }
                    None => {}
                }
            }
            (!out.is_empty()).then_some(Value::Object(out))
        }
        _ if js_json_equal(base, full) => None,
        _ => Some(full.clone()),
    }
}

fn js_json_equal(a: &Value, b: &Value) -> bool {
    js_stringify(a) == js_stringify(b)
}

fn ordered_keys(path: &[&str]) -> Option<&'static [&'static str]> {
    Some(match path {
        [] => &[
            "sampleRate",
            "eq",
            "deesser",
            "compressor",
            "nightMode",
            "bassEnhancer",
            "reverb",
            "surround3d",
            "loudnessCompensation",
            "loudnessNormalization",
            "limiter",
            "ieq",
            "dynamicEq",
            "pitch",
            "modulation",
            "modEffects",
            "hearing",
            "spatial",
            "stereoWidth",
            "sceneId",
            "customized",
        ],
        ["eq"] => &[
            "enabled",
            "mode",
            "simpleBands",
            "proBands",
            "bandCount",
            "qCompensation",
            "locked",
        ],
        ["eq", "proBands", "*"] => &["frequency", "gain", "q"],
        ["deesser"] => &[
            "enabled",
            "centerHz",
            "q",
            "thresholdDb",
            "ratio",
            "attackMs",
            "releaseMs",
            "splitBand",
            "mix",
            "sidechainEnabled",
        ],
        ["compressor"] => &[
            "enabled",
            "thresholdDb",
            "ratio",
            "kneeDb",
            "attackMs",
            "releaseMs",
            "makeupDb",
            "outputGain",
            "sidechainEnabled",
        ],
        ["nightMode"] => &["enabled", "amount"],
        ["bassEnhancer"] => &[
            "enabled",
            "cutoffHz",
            "q",
            "harmonicType",
            "harmonicGain",
            "mix",
            "levelDb",
            "lowBoostDb",
        ],
        ["reverb"] => &["enabled", "mode", "algorithmic", "convolution"],
        ["reverb", "algorithmic"] => &[
            "type",
            "roomSize",
            "damping",
            "wet",
            "dry",
            "preDelayMs",
            "width",
        ],
        ["reverb", "convolution"] => &["irName", "mix", "preDelayMs", "dePeriodize"],
        ["surround3d"] => &["enabled", "distance", "speed", "angle", "direction"],
        ["loudnessCompensation"] => &[
            "enabled",
            "mode",
            "preset",
            "bands",
            "volumePercent",
            "maxBoostDb",
            "smoothingSeconds",
        ],
        ["loudnessCompensation", "bands", "*"] => &["frequency", "gain"],
        ["loudnessNormalization"] => &[
            "enabled",
            "targetLufs",
            "maxGainDb",
            "minGainDb",
            "useRealtimeMeter",
            "externalGainDb",
        ],
        ["limiter"] => &[
            "enabled",
            "thresholdDb",
            "lookaheadMs",
            "attackMs",
            "releaseMs",
            "truePeak",
        ],
        ["ieq"] => &["enabled", "strength", "targetCurve", "timeConstantSec"],
        ["dynamicEq"] => &[
            "enabled",
            "strength",
            "thresholdDb",
            "ratio",
            "attackMs",
            "releaseMs",
            "bands",
        ],
        ["dynamicEq", "bands", "*"] => &["enabled", "targetGainDb"],
        ["pitch"] => &["enabled", "semitones", "rate", "voiceBalance"],
        ["modulation"] => &["enabled", "lfo", "envelope", "routes"],
        ["modulation", "lfo"] => &["enabled", "shape", "rateHz", "depth"],
        ["modulation", "envelope"] => &["enabled", "attackMs", "releaseMs", "amount"],
        ["modulation", "routes", "*"] => &["source", "target", "amount", "offset"],
        ["modEffects"] => &["delay", "chorus", "flanger", "phaser", "tremolo"],
        ["modEffects", "delay"] => &["enabled", "delayMs", "feedback", "mix"],
        ["modEffects", "chorus"] => &["enabled", "rateHz", "depthMs", "mix"],
        ["modEffects", "flanger"] => &["enabled", "rateHz", "depthMs", "feedback", "mix"],
        ["modEffects", "phaser"] => &["enabled", "rateHz", "depth", "feedback", "mix", "stages"],
        ["modEffects", "tremolo"] => &["enabled", "rateHz", "depth", "mix"],
        ["hearing"] => &["enabled"],
        ["spatial"] => &[
            "mode",
            "masterGain",
            "instant",
            "headLocked",
            "world",
            "stage",
            "ambience",
            "convolution",
            "hrtfInterp",
            "distanceModel",
            "refDistance",
            "maxDistance",
        ],
        ["spatial", "instant"] => &[
            "spreadDeg",
            "amount",
            "room",
            "roomAmount",
            "multichannelAuto",
        ],
        ["spatial", "headLocked"] => {
            &["layout", "speakers", "heightLayer", "bottomLayer", "routes"]
        }
        ["spatial", "headLocked", "speakers", "*"] => &[
            "azimuthDeg",
            "elevationDeg",
            "distance",
            "gain",
            "size",
            "muted",
        ],
        ["spatial", "world"] => &[
            "moveSpeed",
            "listener",
            "sources",
            "playhead",
            "trajectories",
            "occlusion",
        ],
        ["spatial", "world", "listener"] => &["position", "yaw", "pitch", "roll"],
        ["spatial", "world", "listener", "position"] => &["x", "y", "z"],
        ["spatial", "world", "sources", "*"] | ["spatial", "stage", "customSources", "*"] => {
            &["id", "position", "gain", "size"]
        }
        ["spatial", "world", "sources", "*", "position"]
        | ["spatial", "stage", "customSources", "*", "position"] => &["x", "y", "z"],
        ["spatial", "world", "trajectories", "*"] => &["sourceId", "keyframes"],
        ["spatial", "world", "trajectories", "*", "keyframes", "*"] => &["t", "position"],
        ["spatial", "world", "trajectories", "*", "keyframes", "*", "position"] => &["x", "y", "z"],
        ["spatial", "stage"] => &[
            "preset",
            "seat",
            "roomSize",
            "reverbAmount",
            "customSources",
        ],
        ["spatial", "ambience"] => &["enabled", "amount"],
        _ => return None,
    })
}

fn write_js_json(value: &Value, path: &mut Vec<&'static str>, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
        Value::Number(n) => {
            let x = n.as_f64().unwrap_or(0.0);
            if x == 0.0 {
                out.push('0');
            } else {
                out.push_str(&n.to_string());
            }
        }
        Value::String(s) => {
            out.push_str(&serde_json::to_string(s).expect("字符串 JSON 序列化不可失败"))
        }
        Value::Array(values) => {
            out.push('[');
            for (i, item) in values.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                path.push("*");
                write_js_json(item, path, out);
                path.pop();
            }
            out.push(']');
        }
        Value::Object(obj) => {
            out.push('{');
            let mut first = true;
            let path_slice = path.as_slice();
            if let Some(keys) = ordered_keys(path_slice) {
                for key in keys {
                    if let Some(item) = obj.get(*key) {
                        if !first {
                            out.push(',');
                        }
                        first = false;
                        out.push_str(&serde_json::to_string(key).unwrap());
                        out.push(':');
                        path.push(key);
                        write_js_json(item, path, out);
                        path.pop();
                    }
                }
            } else {
                for (key, item) in obj {
                    if !first {
                        out.push(',');
                    }
                    first = false;
                    out.push_str(&serde_json::to_string(key).unwrap());
                    out.push(':');
                    write_js_json(item, path, out);
                }
            }
            out.push('}');
        }
    }
}

fn js_stringify(value: &Value) -> String {
    let mut out = String::new();
    write_js_json(value, &mut Vec::new(), &mut out);
    out
}

fn js_stringify_delta(value: &Value) -> String {
    let Some(obj) = value.as_object() else {
        return js_stringify(value);
    };
    let mut out = String::from("{");
    let mut first = true;
    for key in ordered_keys(&[]).expect("顶层键序已定义") {
        if *key == "sampleRate" {
            continue;
        }
        if let Some(item) = obj.get(*key) {
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str(&serde_json::to_string(key).unwrap());
            out.push(':');
            write_js_json(item, &mut vec![key], &mut out);
        }
    }
    if let Some(sample_rate) = obj.get("sampleRate") {
        if !first {
            out.push(',');
        }
        out.push_str("\"sampleRate\":");
        write_js_json(sample_rate, &mut vec!["sampleRate"], &mut out);
    }
    out.push('}');
    out
}

fn to_share_object(params: &Value) -> Result<Value, String> {
    let sample_rate = params
        .get("sampleRate")
        .and_then(Value::as_f64)
        .filter(|x| x.is_finite())
        .unwrap_or(48_000.0);
    let skeleton = default_params_skeleton(sample_rate);
    let mut share = share_rehydrate(&skeleton, params);
    if let Some(spatial) = params.get("spatial") {
        share["spatial"] = spatial.clone();
    }
    if let Some(conv) = share
        .pointer_mut("/reverb/convolution")
        .and_then(Value::as_object_mut)
    {
        conv.remove("ir");
    }
    Ok(share)
}

/// 差异载荷还原：以 base（默认参数骨架）为底、delta 覆盖；数组与叶子整体替换；
/// 未知键与 v1 白名单语义一致——静默丢弃（篡改另有校验和兜底）。
///
/// JS 版的 `__proto__`/`constructor` 等注入键在 base（纯 JSON 对象骨架）中
/// 不存在 → 按未知键跳过；`__proto__` 赋值触发的原型 setter 对白名单读取不可见，
/// 故直接跳过与 TS 的可观察行为一致。
fn share_rehydrate(base: &Value, delta: &Value) -> Value {
    let (base_obj, delta_obj) = match (base, delta) {
        (Value::Object(b), Value::Object(d)) => (b, d),
        _ => return delta.clone(),
    };
    let mut out = base_obj.clone();
    for (k, dv) in delta_obj {
        if let Some(bv) = base_obj.get(k) {
            out.insert(k.clone(), share_rehydrate(bv, dv));
        }
    }
    Value::Object(out)
}

// ---------------------------------------------------------------------------
// 数值序列化归一（对齐 JS JSON.stringify 的整数/浮点形态）
// ---------------------------------------------------------------------------

/// f64 → JSON 数值：整值（|x| ≤ 2^53）以 i64 表达、-0 归一为 0、其余按 f64
/// 最短表示。对齐 TS：全部数值为 f64，`JSON.stringify` 对整值不带小数点。
fn jnum(x: f64) -> Value {
    debug_assert!(x.is_finite(), "清洗输出恒为有限数（输入侧已过滤）");
    if x == 0.0 {
        return Value::Number(Number::from(0i64));
    }
    if x.fract() == 0.0 && x.abs() <= 9.007_199_254_740_992e15 {
        return Value::Number(Number::from(x as i64));
    }
    Value::Number(Number::from_f64(x).unwrap_or_else(|| Number::from(0i64)))
}

// ---------------------------------------------------------------------------
// 白名单清洗助手（TS num/bool/str/oneOf/numOneOf/numArray 的移植）
// ---------------------------------------------------------------------------

/// 数值 clamp：非有限数/类型不符 → 默认值；越界 → 钳到 [min,max]。
fn clamp_num(v: Option<&Value>, min: f64, max: f64, def: f64) -> Value {
    match v.and_then(Value::as_f64) {
        Some(x) if x.is_finite() => {
            let c = if x < min {
                min
            } else if x > max {
                max
            } else {
                x
            };
            jnum(c)
        }
        _ => jnum(def),
    }
}

/// 布尔白名单：类型不符 → 默认值。
fn bool_or(v: Option<&Value>, def: bool) -> bool {
    v.and_then(Value::as_bool).unwrap_or(def)
}

/// 字符串白名单：仅接受 string（防注入），超长按 **UTF-16 码元**截断
/// （对齐 JS `slice`）。截断把代理对切成两半时以 U+FFFD 等价表达（见模块注释）。
fn str_or(v: Option<&Value>, def: Option<&str>, max_len: usize) -> Option<String> {
    match v {
        Some(Value::String(s)) => {
            let mut out = String::new();
            let mut units = 0usize;
            for ch in s.chars() {
                let cu = ch.len_utf16();
                if units + cu <= max_len {
                    out.push(ch);
                    units += cu;
                } else {
                    if units < max_len {
                        out.push('\u{FFFD}');
                    }
                    break;
                }
            }
            Some(out)
        }
        _ => def.map(str::to_string),
    }
}

/// 字符串枚举白名单：枚举外回落默认值。
fn one_of_str(v: Option<&Value>, allowed: &[&str], def: &str) -> String {
    match v.and_then(Value::as_str) {
        Some(s) if allowed.contains(&s) => s.to_string(),
        _ => def.to_string(),
    }
}

/// 数字枚举白名单（bandCount / direction）：枚举外回落默认值。
fn num_one_of(v: Option<&Value>, allowed: &[f64], def: f64) -> Value {
    match v.and_then(Value::as_f64) {
        Some(x) if allowed.contains(&x) => jnum(x),
        _ => jnum(def),
    }
}

/// 数值数组（clamp 后截断到 maxLen；非数值元素跳过）；缺失时返回默认值副本。
fn num_array(v: Option<&Value>, min: f64, max: f64, def: &[f64], max_len: usize) -> Vec<Value> {
    match v.and_then(Value::as_array) {
        Some(arr) => {
            let mut out: Vec<Value> = Vec::with_capacity(arr.len().min(max_len));
            for x in arr {
                if out.len() >= max_len {
                    break;
                }
                if let Some(xv) = x.as_f64().filter(|f| f.is_finite()) {
                    let c = if xv < min {
                        min
                    } else if xv > max {
                        max
                    } else {
                        xv
                    };
                    out.push(jnum(c));
                }
            }
            out
        }
        None => def.iter().copied().map(jnum).collect(),
    }
}

/// 专业 10 段默认频点（与 src/types.ts PRO_EQ_DEFAULT_BANDS 一致）。
fn default_eq_pro_bands() -> Vec<Value> {
    [
        31.5, 63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
    ]
    .iter()
    .map(|&f| json!({ "frequency": jnum(f), "gain": jnum(0.0), "q": jnum(1.1) }))
    .collect()
}

/// 取 section：raw[key] 为对象则返回之，否则返回空对象（TS `isObj(raw.x) ? raw.x : {}`）。
fn sec<'a>(
    obj: &'a Map<String, Value>,
    empty: &'a Map<String, Value>,
    key: &str,
) -> &'a Map<String, Value> {
    obj.get(key).and_then(Value::as_object).unwrap_or(empty)
}

// ---------------------------------------------------------------------------
// 空间音频：深度清洗 + 形状校验 + 默认骨架合并（decodeSpatial 语义）
// ---------------------------------------------------------------------------

/// 递归深度上限（防恶意深度嵌套栈溢出）。
const SPATIAL_MAX_DEPTH: usize = 12;
/// 单对象键数上限（防超大 payload）。
const SPATIAL_MAX_KEYS: usize = 256;

/// 递归深度清洗：仅保留 boolean/有限 number/string/plain array/plain object，
/// 限深 [`SPATIAL_MAX_DEPTH`]、单对象键数限 [`SPATIAL_MAX_KEYS`]。任何非法/超限
/// → None（调用方整体回落默认）。危险键（`__proto__`/`constructor`/`prototype`）
/// 直接丢弃（防原型污染）。
fn deep_sanitize_spatial(v: &Value, depth: usize) -> Option<Value> {
    if depth > SPATIAL_MAX_DEPTH {
        return None;
    }
    match v {
        Value::Null => None,
        Value::Bool(b) => Some(Value::Bool(*b)),
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite()).map(jnum),
        Value::String(s) => Some(Value::String(s.clone())),
        Value::Array(a) => {
            if a.len() > SPATIAL_MAX_KEYS {
                return None;
            }
            let mut out = Vec::with_capacity(a.len());
            for item in a {
                // 子项非法 → 整体拒绝（保守，防半残结构）
                out.push(deep_sanitize_spatial(item, depth + 1)?);
            }
            Some(Value::Array(out))
        }
        Value::Object(o) => {
            if o.len() > SPATIAL_MAX_KEYS {
                return None;
            }
            let mut out = Map::new();
            for (k, val) in o {
                if k == "__proto__" || k == "constructor" || k == "prototype" {
                    continue;
                }
                out.insert(k.clone(), deep_sanitize_spatial(val, depth + 1)?);
            }
            Some(Value::Object(out))
        }
    }
}

/// 空间音频默认设置（镜像 TS `createDefaultSpatialSettings()`——由
/// `createDefaultSpatialParams()`（mode 'off'、perfMode 'balanced'）投影：
/// hrtfInterp='nearest'、distanceModel='inverse'、refDistance=1、maxDistance=50；
/// headLocked 默认 5.1 布局扬声器表（layouts.ts 单事实源，距离 1.5m）。
pub fn default_spatial_settings() -> Value {
    let speaker = |az: f64| json!({ "azimuthDeg": jnum(az), "elevationDeg": jnum(0.0), "distance": jnum(1.5), "gain": jnum(1.0), "size": jnum(0.0) });
    let source = |id: &str, x: f64, y: f64, z: f64, gain: f64, size: f64| json!({ "id": id, "position": { "x": jnum(x), "y": jnum(y), "z": jnum(z) }, "gain": jnum(gain), "size": jnum(size) });
    json!({
        "mode": "off",
        "masterGain": jnum(0.9),
        "instant": {
            "spreadDeg": jnum(60.0),
            "amount": jnum(0.7),
            "room": "studio",
            "roomAmount": jnum(0.15),
            "multichannelAuto": false,
        },
        "headLocked": {
            "layout": "51",
            "speakers": [speaker(0.0), speaker(-30.0), speaker(30.0), speaker(-110.0), speaker(110.0)],
            "heightLayer": true,
            "bottomLayer": true,
            "routes": [],
        },
        "world": {
            "moveSpeed": jnum(2.0),
            "listener": {
                "position": { "x": jnum(0.0), "y": jnum(1.6), "z": jnum(0.0) },
                "yaw": jnum(0.0),
                "pitch": jnum(0.0),
                "roll": jnum(0.0),
            },
            "sources": [
                source("vocal", -2.0, 1.6, 4.0, 1.0, 0.0),
                source("guitar", -5.0, 1.6, 6.0, 1.0, 0.0),
                source("drums", 3.0, 1.6, 7.0, 1.0, 0.0),
                source("ambience", 0.0, 2.5, 10.0, 0.6, 0.5),
            ],
            "playhead": jnum(0.0),
            "trajectories": [],
            "occlusion": jnum(0.0),
        },
        "stage": {
            "preset": "stage",
            "seat": "middle",
            "roomSize": jnum(1.0),
            "reverbAmount": jnum(0.35),
            "customSources": [],
        },
        "ambience": { "enabled": false, "amount": jnum(0.3) },
        "convolution": "partitioned",
        "hrtfInterp": "nearest",
        "distanceModel": "inverse",
        "refDistance": jnum(1.0),
        "maxDistance": jnum(50.0),
    })
}

/// JS spread 合并：默认键在前、o 的键覆盖（**o 内额外键保留**，与
/// `{ ...def, ...o }` 语义一致）。
fn merge_spread(def: &Map<String, Value>, o: &Map<String, Value>) -> Value {
    let mut out = def.clone();
    for (k, v) in o {
        out.insert(k.clone(), v.clone());
    }
    Value::Object(out)
}

/// 解码 SpatialSettings：raw 为对象 → 深度清洗 → 期望形状校验（mode 白名单 +
/// 五个子对象存在性）→ 以默认骨架合并；非法/缺省 → `default_spatial_settings()`
/// （mode 'off'，即引擎旁路的逐位直通形态）。
pub fn decode_spatial(raw: &Value) -> Value {
    let Some(Value::Object(o)) = deep_sanitize_spatial(raw, 0) else {
        return default_spatial_settings();
    };
    let mode = one_of_str(
        o.get("mode"),
        &["off", "instant", "headLocked", "world", "stage"],
        "off",
    );
    // 关键子对象存在性校验：缺任一 → 默认（防半残结构进入引擎）
    for k in ["instant", "headLocked", "world", "stage", "ambience"] {
        if !matches!(o.get(k), Some(Value::Object(_))) {
            return default_spatial_settings();
        }
    }
    let def = default_spatial_settings();
    let d = def.as_object().expect("默认 spatial 恒为对象");
    let mut out = Map::new();
    out.insert("mode".to_string(), Value::String(mode));
    out.insert(
        "masterGain".to_string(),
        match o.get("masterGain").and_then(Value::as_f64) {
            Some(x) if x.is_finite() => jnum(x),
            _ => d["masterGain"].clone(),
        },
    );
    for k in ["instant", "headLocked", "world", "stage", "ambience"] {
        let sub = o[k].as_object().expect("子对象存在性已校验");
        out.insert(
            k.to_string(),
            merge_spread(d[k].as_object().expect("默认子对象"), sub),
        );
    }
    out.insert(
        "convolution".to_string(),
        one_of_str(
            o.get("convolution"),
            &["partitioned", "time"],
            "partitioned",
        )
        .into(),
    );
    out.insert(
        "hrtfInterp".to_string(),
        one_of_str(o.get("hrtfInterp"), &["nearest", "spherical"], "nearest").into(),
    );
    out.insert(
        "distanceModel".to_string(),
        one_of_str(
            o.get("distanceModel"),
            &["inverse", "linear", "exponential"],
            "inverse",
        )
        .into(),
    );
    out.insert(
        "refDistance".to_string(),
        clamp_num(o.get("refDistance"), 0.1, 100.0, 1.0),
    );
    out.insert(
        "maxDistance".to_string(),
        clamp_num(o.get("maxDistance"), 1.0, 200.0, 50.0),
    );
    Value::Object(out)
}

// ---------------------------------------------------------------------------
// 默认参数骨架（镜像 TS toShareObject(createDefaultParams(sampleRate))）
// ---------------------------------------------------------------------------

/// v2 还原骨架：与 TS `toShareObject(createDefaultParams(sampleRate))` 逐字段
/// 一致的 plain JSON（卷积混响只含 irName 引用、无 ir 数组键；spatial 为默认块）。
///
/// 骨架键的存在性决定 v2 delta 的可还原面（delta 中不存在于骨架的键一律静默
/// 丢弃，与 TS `shareRehydrate` 的 `k in b` 语义一致），故每个 section/子键/
/// spatial 子结构都必须与 TS 骨架同形同值。
pub fn default_params_skeleton(sample_rate: f64) -> Value {
    json!({
        "sampleRate": jnum(sample_rate),
        "eq": {
            "enabled": true,
            "mode": "pro",
            "simpleBands": [jnum(0.0), jnum(0.0), jnum(0.0), jnum(0.0), jnum(0.0)],
            "proBands": default_eq_pro_bands(),
            "bandCount": jnum(10.0),
            "qCompensation": true,
            "locked": false,
        },
        "deesser": {
            "enabled": false,
            "centerHz": jnum(6000.0),
            "q": jnum(0.7),
            "thresholdDb": jnum(-30.0),
            "ratio": jnum(8.0),
            "attackMs": jnum(1.0),
            "releaseMs": jnum(80.0),
            "splitBand": true,
            "mix": jnum(1.0),
            "sidechainEnabled": false,
        },
        "compressor": {
            "enabled": false,
            "thresholdDb": jnum(-20.0),
            "ratio": jnum(4.0),
            "kneeDb": jnum(6.0),
            "attackMs": jnum(10.0),
            "releaseMs": jnum(150.0),
            "makeupDb": jnum(0.0),
            "outputGain": jnum(1.0),
            "sidechainEnabled": false,
        },
        "nightMode": { "enabled": false, "amount": jnum(0.0) },
        "bassEnhancer": {
            "enabled": false,
            "cutoffHz": jnum(90.0),
            "q": jnum(0.7),
            "harmonicType": "odd",
            "harmonicGain": jnum(0.6),
            "mix": jnum(0.5),
            "levelDb": jnum(0.0),
            "lowBoostDb": jnum(0.0),
        },
        "reverb": {
            "enabled": false,
            "mode": "algorithmic",
            "algorithmic": {
                "type": "hall",
                "roomSize": jnum(0.5),
                "damping": jnum(0.5),
                "wet": jnum(0.3),
                "dry": jnum(0.7),
                "preDelayMs": jnum(0.0),
                "width": jnum(1.0),
            },
            // 去 IR 数组：卷积 IR 只保留 irName 引用，ir 不参与序列化（骨架无 ir 键）
            "convolution": {
                "irName": Value::Null,
                "mix": jnum(0.3),
                "preDelayMs": jnum(0.0),
                "dePeriodize": true,
            },
        },
        "surround3d": {
            "enabled": false,
            "distance": jnum(0.5),
            "speed": jnum(1.0),
            "angle": jnum(0.0),
            "direction": jnum(1.0),
        },
        "loudnessCompensation": {
            "enabled": false,
            "mode": "auto",
            "preset": "flat",
            "bands": [],
            "volumePercent": jnum(80.0),
            "maxBoostDb": jnum(12.0),
            "smoothingSeconds": jnum(0.2),
        },
        "loudnessNormalization": {
            "enabled": false,
            "targetLufs": jnum(-14.0),
            "maxGainDb": jnum(9.0),
            "minGainDb": jnum(-9.0),
            "useRealtimeMeter": true,
            "externalGainDb": jnum(0.0),
        },
        "limiter": {
            "enabled": true,
            "thresholdDb": jnum(-1.0),
            "lookaheadMs": jnum(5.0),
            "attackMs": jnum(0.5),
            "releaseMs": jnum(150.0),
            "truePeak": true,
        },
        "ieq": {
            "enabled": false,
            "strength": jnum(0.5),
            "targetCurve": "flat",
            "timeConstantSec": jnum(3.0),
        },
        "dynamicEq": {
            "enabled": false,
            "strength": jnum(0.5),
            "thresholdDb": jnum(-20.0),
            "ratio": jnum(2.0),
            "attackMs": jnum(20.0),
            "releaseMs": jnum(200.0),
            "bands": (0..5).map(|_| json!({ "enabled": true, "targetGainDb": jnum(0.0) })).collect::<Vec<Value>>(),
        },
        "pitch": {
            "enabled": false,
            "semitones": jnum(0.0),
            "rate": jnum(1.0),
            "voiceBalance": jnum(0.0),
        },
        "modulation": {
            "enabled": false,
            "lfo": {
                "enabled": false,
                "shape": "sine",
                "rateHz": jnum(1.0),
                "depth": jnum(0.5),
            },
            "envelope": {
                "enabled": false,
                "attackMs": jnum(10.0),
                "releaseMs": jnum(200.0),
                "amount": jnum(0.5),
            },
            "routes": [],
        },
        "modEffects": {
            "delay": { "enabled": false, "delayMs": jnum(250.0), "feedback": jnum(0.3), "mix": jnum(0.3) },
            "chorus": { "enabled": false, "rateHz": jnum(1.0), "depthMs": jnum(3.0), "mix": jnum(0.4) },
            "flanger": { "enabled": false, "rateHz": jnum(0.5), "depthMs": jnum(2.0), "feedback": jnum(0.4), "mix": jnum(0.5) },
            "phaser": { "enabled": false, "rateHz": jnum(0.5), "depth": jnum(0.5), "feedback": jnum(0.4), "mix": jnum(0.5), "stages": jnum(4.0) },
            "tremolo": { "enabled": false, "rateHz": jnum(5.0), "depth": jnum(0.5), "mix": jnum(1.0) },
        },
        "hearing": { "enabled": false },
        "spatial": default_spatial_settings(),
        "stereoWidth": jnum(1.0),
        "sceneId": Value::Null,
        "customized": false,
    })
}

// ---------------------------------------------------------------------------
// 白名单重建：只读取已知字段，未知字段（含 __proto__ 等注入键）一律丢弃
// ---------------------------------------------------------------------------

/// `sanitizeParams` 全量移植：白名单字段 + 数值 clamp + 枚举回落 + 字符串截断。
/// 非 JSON 对象输入 → Err("invalid share code payload")。
pub fn sanitize_params(raw: &Value) -> Result<Value, String> {
    let Some(obj) = raw.as_object() else {
        return Err("invalid share code payload".to_string());
    };
    let empty = Map::new();

    let eq_raw = sec(obj, &empty, "eq");
    let deesser_raw = sec(obj, &empty, "deesser");
    let comp_raw = sec(obj, &empty, "compressor");
    let night_raw = sec(obj, &empty, "nightMode");
    let bass_raw = sec(obj, &empty, "bassEnhancer");
    let rev_raw = sec(obj, &empty, "reverb");
    let rev_alg_raw = sec(rev_raw, &empty, "algorithmic");
    let rev_conv_raw = sec(rev_raw, &empty, "convolution");
    let sur_raw = sec(obj, &empty, "surround3d");
    let lc_raw = sec(obj, &empty, "loudnessCompensation");
    let ln_raw = sec(obj, &empty, "loudnessNormalization");
    let lim_raw = sec(obj, &empty, "limiter");
    let ieq_raw = sec(obj, &empty, "ieq");
    let dynamic_eq_raw = sec(obj, &empty, "dynamicEq");
    let pitch_raw = sec(obj, &empty, "pitch");
    let mod_raw = sec(obj, &empty, "modulation");
    let mod_lfo_raw = sec(mod_raw, &empty, "lfo");
    let mod_env_raw = sec(mod_raw, &empty, "envelope");
    let mod_fx_raw = sec(obj, &empty, "modEffects");
    let delay_raw = sec(mod_fx_raw, &empty, "delay");
    let chorus_raw = sec(mod_fx_raw, &empty, "chorus");
    let flanger_raw = sec(mod_fx_raw, &empty, "flanger");
    let phaser_raw = sec(mod_fx_raw, &empty, "phaser");
    let tremolo_raw = sec(mod_fx_raw, &empty, "tremolo");
    let hearing_raw = sec(obj, &empty, "hearing");

    // sampleRate：先 clamp 再 Math.round（clamp 后恒 ≥8000，两侧半值取整语义一致）
    let sample_rate = match obj.get("sampleRate").and_then(Value::as_f64) {
        Some(x) if x.is_finite() => {
            let c = if x < 8000.0 {
                8000.0
            } else if x > 192000.0 {
                192000.0
            } else {
                x
            };
            c.round()
        }
        _ => 48000.0,
    };

    // eq.simpleBands：5 段，缺失补 0
    let mut simple = num_array(
        eq_raw.get("simpleBands"),
        -20.0,
        20.0,
        &[0.0, 0.0, 0.0, 0.0, 0.0],
        5,
    );
    while simple.len() < 5 {
        simple.push(jnum(0.0));
    }

    // eq.proBands：白名单 {frequency,gain,q}；字段缺失 → 默认 10 段；显式空数组 → 保留空
    let pro_bands: Vec<Value> = match eq_raw.get("proBands").and_then(Value::as_array) {
        Some(arr) => {
            let mut parsed: Vec<Value> = Vec::new();
            for b in arr {
                if parsed.len() >= 20 {
                    break;
                }
                if let Some(bo) = b.as_object() {
                    parsed.push(json!({
                        "frequency": clamp_num(bo.get("frequency"), 20.0, 20000.0, 1000.0),
                        "gain": clamp_num(bo.get("gain"), -20.0, 20.0, 0.0),
                        "q": clamp_num(bo.get("q"), 0.1, 10.0, 1.1),
                    }));
                }
            }
            parsed
        }
        None => default_eq_pro_bands(),
    };

    // loudnessCompensation.bands：白名单 {frequency,gain}，最多 32 段
    let mut lc_bands: Vec<Value> = Vec::new();
    if let Some(arr) = lc_raw.get("bands").and_then(Value::as_array) {
        for b in arr {
            if lc_bands.len() >= 32 {
                break;
            }
            if let Some(bo) = b.as_object() {
                lc_bands.push(json!({
                    "frequency": clamp_num(bo.get("frequency"), 20.0, 20000.0, 1000.0),
                    "gain": clamp_num(bo.get("gain"), -20.0, 20.0, 0.0),
                }));
            }
        }
    }

    // modulation.routes：白名单路由，最多 16 条
    let mut mod_routes: Vec<Value> = Vec::new();
    if let Some(arr) = mod_raw.get("routes").and_then(Value::as_array) {
        for r in arr {
            if mod_routes.len() >= 16 {
                break;
            }
            let Some(ro) = r.as_object() else { continue };
            mod_routes.push(json!({
                "source": one_of_str(ro.get("source"), &["lfo", "envelope"], "lfo"),
                "target": one_of_str(ro.get("target"), &["masterGain", "stereoWidth"], "masterGain"),
                "amount": clamp_num(ro.get("amount"), -1.0, 1.0, 0.0),
                "offset": clamp_num(ro.get("offset"), -2.0, 2.0, 0.0),
            }));
        }
    }

    // dynamicEq.bands：数组则取前 5 个元素逐带重建（元素非对象 → 该带默认值），
    // 缺失/非数组 → 默认 5 带
    let dynamic_eq_bands: Vec<Value> = match dynamic_eq_raw.get("bands").and_then(Value::as_array) {
        Some(arr) => arr
            .iter()
            .take(5)
            .map(|b| {
                let bo = b.as_object();
                json!({
                    "enabled": bo.and_then(|o| o.get("enabled")).and_then(Value::as_bool).unwrap_or(true),
                    "targetGainDb": clamp_num(bo.and_then(|o| o.get("targetGainDb")), -12.0, 12.0, 0.0),
                })
            })
            .collect(),
        None => (0..5).map(|_| json!({ "enabled": true, "targetGainDb": jnum(0.0) })).collect(),
    };

    Ok(json!({
        "sampleRate": jnum(sample_rate),
        "eq": {
            "enabled": bool_or(eq_raw.get("enabled"), true),
            "mode": one_of_str(eq_raw.get("mode"), &["simple", "pro"], "pro"),
            "simpleBands": simple,
            "proBands": pro_bands,
            "bandCount": num_one_of(eq_raw.get("bandCount"), &[10.0, 20.0], 10.0),
            "qCompensation": bool_or(eq_raw.get("qCompensation"), true),
            "locked": bool_or(eq_raw.get("locked"), false),
        },
        "deesser": {
            "enabled": bool_or(deesser_raw.get("enabled"), false),
            "centerHz": clamp_num(deesser_raw.get("centerHz"), 100.0, 16000.0, 6000.0),
            "q": clamp_num(deesser_raw.get("q"), 0.1, 10.0, 0.7),
            "thresholdDb": clamp_num(deesser_raw.get("thresholdDb"), -60.0, 0.0, -30.0),
            "ratio": clamp_num(deesser_raw.get("ratio"), 1.0, 50.0, 8.0),
            "attackMs": clamp_num(deesser_raw.get("attackMs"), 0.0, 100.0, 1.0),
            "releaseMs": clamp_num(deesser_raw.get("releaseMs"), 0.0, 2000.0, 80.0),
            "splitBand": bool_or(deesser_raw.get("splitBand"), true),
            "mix": clamp_num(deesser_raw.get("mix"), 0.0, 1.0, 1.0),
            "sidechainEnabled": bool_or(deesser_raw.get("sidechainEnabled"), false),
        },
        "compressor": {
            "enabled": bool_or(comp_raw.get("enabled"), false),
            "thresholdDb": clamp_num(comp_raw.get("thresholdDb"), -60.0, 0.0, -20.0),
            "ratio": clamp_num(comp_raw.get("ratio"), 1.0, 50.0, 4.0),
            "kneeDb": clamp_num(comp_raw.get("kneeDb"), 0.0, 24.0, 6.0),
            "attackMs": clamp_num(comp_raw.get("attackMs"), 0.0, 500.0, 10.0),
            "releaseMs": clamp_num(comp_raw.get("releaseMs"), 0.0, 3000.0, 150.0),
            "makeupDb": clamp_num(comp_raw.get("makeupDb"), -24.0, 24.0, 0.0),
            "outputGain": clamp_num(comp_raw.get("outputGain"), 0.0, 2.0, 1.0),
            "sidechainEnabled": bool_or(comp_raw.get("sidechainEnabled"), false),
        },
        "nightMode": {
            "enabled": bool_or(night_raw.get("enabled"), false),
            "amount": clamp_num(night_raw.get("amount"), 0.0, 10.0, 0.0),
        },
        "bassEnhancer": {
            "enabled": bool_or(bass_raw.get("enabled"), false),
            "cutoffHz": clamp_num(bass_raw.get("cutoffHz"), 20.0, 500.0, 90.0),
            "q": clamp_num(bass_raw.get("q"), 0.1, 10.0, 0.7),
            "harmonicType": one_of_str(bass_raw.get("harmonicType"), &["odd", "even", "atan", "soft"], "odd"),
            "harmonicGain": clamp_num(bass_raw.get("harmonicGain"), 0.0, 1.0, 0.6),
            "mix": clamp_num(bass_raw.get("mix"), 0.0, 1.0, 0.5),
            "levelDb": clamp_num(bass_raw.get("levelDb"), -6.0, 6.0, 0.0),
            "lowBoostDb": clamp_num(bass_raw.get("lowBoostDb"), -6.0, 12.0, 0.0),
        },
        "reverb": {
            "enabled": bool_or(rev_raw.get("enabled"), false),
            "mode": one_of_str(rev_raw.get("mode"), &["convolution", "algorithmic", "fdn", "off"], "algorithmic"),
            "algorithmic": {
                "type": one_of_str(rev_alg_raw.get("type"), &["hall", "room", "plate", "spring", "stage"], "hall"),
                "roomSize": clamp_num(rev_alg_raw.get("roomSize"), 0.0, 1.0, 0.5),
                "damping": clamp_num(rev_alg_raw.get("damping"), 0.0, 1.0, 0.5),
                "wet": clamp_num(rev_alg_raw.get("wet"), 0.0, 1.0, 0.3),
                "dry": clamp_num(rev_alg_raw.get("dry"), 0.0, 1.0, 0.7),
                "preDelayMs": clamp_num(rev_alg_raw.get("preDelayMs"), 0.0, 500.0, 0.0),
                "width": clamp_num(rev_alg_raw.get("width"), 0.0, 2.0, 1.0),
            },
            "convolution": {
                // IR 数组不进入分享串；解码后恒为 null，由调用方按 irName 重新加载
                "ir": Value::Null,
                "irName": str_or(rev_conv_raw.get("irName"), None, 256).map(Value::String).unwrap_or(Value::Null),
                "mix": clamp_num(rev_conv_raw.get("mix"), 0.0, 1.0, 0.3),
                "preDelayMs": clamp_num(rev_conv_raw.get("preDelayMs"), 0.0, 500.0, 0.0),
                "dePeriodize": bool_or(rev_conv_raw.get("dePeriodize"), true),
            },
        },
        "surround3d": {
            "enabled": bool_or(sur_raw.get("enabled"), false),
            "distance": clamp_num(sur_raw.get("distance"), 0.0, 10.0, 0.5),
            "speed": clamp_num(sur_raw.get("speed"), 0.0, 10.0, 1.0),
            "angle": clamp_num(sur_raw.get("angle"), -360.0, 360.0, 0.0),
            "direction": num_one_of(sur_raw.get("direction"), &[1.0, -1.0], 1.0),
        },
        "loudnessCompensation": {
            "enabled": bool_or(lc_raw.get("enabled"), false),
            "mode": one_of_str(lc_raw.get("mode"), &["auto", "preset", "custom"], "auto"),
            "preset": one_of_str(lc_raw.get("preset"), &["flat", "bass", "vocal", "warm", "bright", "night"], "flat"),
            "bands": lc_bands,
            "volumePercent": clamp_num(lc_raw.get("volumePercent"), 0.0, 100.0, 80.0),
            "maxBoostDb": clamp_num(lc_raw.get("maxBoostDb"), 0.0, 24.0, 12.0),
            "smoothingSeconds": clamp_num(lc_raw.get("smoothingSeconds"), 0.01, 10.0, 0.2),
        },
        "loudnessNormalization": {
            "enabled": bool_or(ln_raw.get("enabled"), false),
            "targetLufs": clamp_num(ln_raw.get("targetLufs"), -40.0, 0.0, -14.0),
            "maxGainDb": clamp_num(ln_raw.get("maxGainDb"), 0.0, 24.0, 9.0),
            "minGainDb": clamp_num(ln_raw.get("minGainDb"), -24.0, 0.0, -9.0),
            "useRealtimeMeter": bool_or(ln_raw.get("useRealtimeMeter"), true),
            "externalGainDb": clamp_num(ln_raw.get("externalGainDb"), -24.0, 24.0, 0.0),
        },
        "limiter": {
            "enabled": bool_or(lim_raw.get("enabled"), true),
            "thresholdDb": clamp_num(lim_raw.get("thresholdDb"), -60.0, 0.0, -1.0),
            "lookaheadMs": clamp_num(lim_raw.get("lookaheadMs"), 0.0, 50.0, 5.0),
            "attackMs": clamp_num(lim_raw.get("attackMs"), 0.0, 50.0, 0.5),
            "releaseMs": clamp_num(lim_raw.get("releaseMs"), 0.0, 2000.0, 150.0),
            "truePeak": bool_or(lim_raw.get("truePeak"), true),
        },
        "ieq": {
            "enabled": bool_or(ieq_raw.get("enabled"), false),
            "strength": clamp_num(ieq_raw.get("strength"), 0.0, 1.0, 0.5),
            "targetCurve": one_of_str(ieq_raw.get("targetCurve"), &["flat", "warm", "bright", "vocal"], "flat"),
            "timeConstantSec": clamp_num(ieq_raw.get("timeConstantSec"), 0.1, 10.0, 3.0),
        },
        "dynamicEq": {
            "enabled": bool_or(dynamic_eq_raw.get("enabled"), false),
            "strength": clamp_num(dynamic_eq_raw.get("strength"), 0.0, 1.0, 0.5),
            "thresholdDb": clamp_num(dynamic_eq_raw.get("thresholdDb"), -80.0, 0.0, -20.0),
            "ratio": clamp_num(dynamic_eq_raw.get("ratio"), 1.0, 20.0, 2.0),
            "attackMs": clamp_num(dynamic_eq_raw.get("attackMs"), 1.0, 500.0, 20.0),
            "releaseMs": clamp_num(dynamic_eq_raw.get("releaseMs"), 10.0, 2000.0, 200.0),
            "bands": dynamic_eq_bands,
        },
        "pitch": {
            "enabled": bool_or(pitch_raw.get("enabled"), false),
            "semitones": clamp_num(pitch_raw.get("semitones"), -10.0, 10.0, 0.0),
            "rate": clamp_num(pitch_raw.get("rate"), 0.25, 3.0, 1.0),
            "voiceBalance": clamp_num(pitch_raw.get("voiceBalance"), -1.0, 1.0, 0.0),
        },
        "modulation": {
            "enabled": bool_or(mod_raw.get("enabled"), false),
            "lfo": {
                "enabled": bool_or(mod_lfo_raw.get("enabled"), false),
                "shape": one_of_str(mod_lfo_raw.get("shape"), &["sine", "triangle", "square", "saw"], "sine"),
                "rateHz": clamp_num(mod_lfo_raw.get("rateHz"), 0.01, 20.0, 1.0),
                "depth": clamp_num(mod_lfo_raw.get("depth"), 0.0, 1.0, 0.5),
            },
            "envelope": {
                "enabled": bool_or(mod_env_raw.get("enabled"), false),
                "attackMs": clamp_num(mod_env_raw.get("attackMs"), 0.1, 1000.0, 10.0),
                "releaseMs": clamp_num(mod_env_raw.get("releaseMs"), 0.1, 5000.0, 200.0),
                "amount": clamp_num(mod_env_raw.get("amount"), 0.0, 1.0, 0.5),
            },
            "routes": mod_routes,
        },
        "modEffects": {
            "delay": {
                "enabled": bool_or(delay_raw.get("enabled"), false),
                "delayMs": clamp_num(delay_raw.get("delayMs"), 0.0, 2000.0, 250.0),
                "feedback": clamp_num(delay_raw.get("feedback"), 0.0, 0.98, 0.3),
                "mix": clamp_num(delay_raw.get("mix"), 0.0, 1.0, 0.3),
            },
            "chorus": {
                "enabled": bool_or(chorus_raw.get("enabled"), false),
                "rateHz": clamp_num(chorus_raw.get("rateHz"), 0.01, 20.0, 1.0),
                "depthMs": clamp_num(chorus_raw.get("depthMs"), 0.0, 50.0, 3.0),
                "mix": clamp_num(chorus_raw.get("mix"), 0.0, 1.0, 0.4),
            },
            "flanger": {
                "enabled": bool_or(flanger_raw.get("enabled"), false),
                "rateHz": clamp_num(flanger_raw.get("rateHz"), 0.01, 20.0, 0.5),
                "depthMs": clamp_num(flanger_raw.get("depthMs"), 0.0, 50.0, 2.0),
                "feedback": clamp_num(flanger_raw.get("feedback"), 0.0, 0.98, 0.4),
                "mix": clamp_num(flanger_raw.get("mix"), 0.0, 1.0, 0.5),
            },
            "phaser": {
                "enabled": bool_or(phaser_raw.get("enabled"), false),
                "rateHz": clamp_num(phaser_raw.get("rateHz"), 0.01, 20.0, 0.5),
                "depth": clamp_num(phaser_raw.get("depth"), 0.0, 1.0, 0.5),
                "feedback": clamp_num(phaser_raw.get("feedback"), 0.0, 0.98, 0.4),
                "mix": clamp_num(phaser_raw.get("mix"), 0.0, 1.0, 0.5),
                "stages": clamp_num(phaser_raw.get("stages"), 2.0, 8.0, 4.0),
            },
            "tremolo": {
                "enabled": bool_or(tremolo_raw.get("enabled"), false),
                "rateHz": clamp_num(tremolo_raw.get("rateHz"), 0.01, 30.0, 5.0),
                "depth": clamp_num(tremolo_raw.get("depth"), 0.0, 1.0, 0.5),
                "mix": clamp_num(tremolo_raw.get("mix"), 0.0, 1.0, 1.0),
            },
        },
        "hearing": {
            "enabled": bool_or(hearing_raw.get("enabled"), false),
        },
        // 空间音频：raw 缺 spatial 字段 → decodeSpatial 返回默认 off；
        // 旧分享串（无 spatial）往返得默认 off，行为与历史一致
        "spatial": decode_spatial(obj.get("spatial").unwrap_or(&Value::Null)),
        "stereoWidth": clamp_num(obj.get("stereoWidth"), 0.0, 2.0, 1.0),
        "sceneId": str_or(obj.get("sceneId"), None, 64).map(Value::String).unwrap_or(Value::Null),
        "customized": bool_or(obj.get("customized"), false),
    }))
}

// ---------------------------------------------------------------------------
// 公开 API：v2 编码 + v1/v2 解码
// ---------------------------------------------------------------------------

/// 序列化为 v2 分享串。v1 保持只解码，不提供编码入口。
pub fn encode_share_code(params: &Value) -> Result<String, String> {
    let full = to_share_object(params)?;
    let sample_rate = full
        .get("sampleRate")
        .and_then(Value::as_f64)
        .unwrap_or(48_000.0);
    let base = default_params_skeleton(sample_rate);
    let mut delta = share_delta(&base, &full)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let sample_rate = full
        .get("sampleRate")
        .cloned()
        .unwrap_or_else(|| jnum(48_000.0));
    delta.insert("sampleRate".to_string(), sample_rate);
    let json = js_stringify_delta(&Value::Object(delta));
    let payload = format!(
        "{}:{}:{}",
        SHARE_CODEC_VERSION,
        checksum_of("2", &json),
        json
    );
    Ok(format!(
        "{}-{}",
        SHARE_CODE_PREFIX,
        group_code(&bytes_to_base32_crockford(payload.as_bytes()))
    ))
}

/// 反序列化：HSE2（v2 差异载荷）与 v1 旧串（base64url 全量载荷）双路全收；
/// 版本/校验和验证 + 白名单字段 + 数值 clamp；非法输入返回 Err（消息与 TS 逐字一致）。
///
/// 返回值为规范化的参数 JSON（serde_json 对象），可直接交给后续消费者
/// （如 hse-service 的参数解析层）做模块级映射。
pub fn decode_share_code(s: &str) -> Result<Value, String> {
    if s.is_empty() {
        return Err("invalid share code: empty input".to_string());
    }
    let trimmed = s.trim();
    let chars: Vec<char> = trimmed.chars().collect();

    let text: String = if chars.len() >= 4 && is_hse2_prefix(&chars[..4]) {
        // v2 传输：HSE2- 分组 Crockford（剥前缀与全部分隔符/空白）
        let rest: String = chars[4..].iter().collect();
        let stripped: String = rest
            .chars()
            .filter(|c| *c != '-' && !is_js_regex_whitespace(*c))
            .collect();
        let bytes = base32_crockford_to_bytes(&stripped)?;
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        // v1 传输：base64url（错误消息经一次再包裹，与 TS catch 语义一致——双重前缀）
        match base64url_to_bytes(trimmed) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(msg) => return Err(format!("invalid share code: {}", msg)),
        }
    };

    // 信封布局：<version>:<8位校验和>:<json>；json 起点 = 版本冒号 + 1 + 校验和(8) + 1
    let first_colon = match text.find(':') {
        None | Some(0) => return Err("invalid share code: missing version".to_string()),
        Some(i) => i,
    };
    let version = &text[..first_colon];
    if version != "1" && version != "2" {
        return Err(format!("unsupported share code version: {}", version));
    }
    let bytes = text.as_bytes();
    let cs_start = first_colon + 1;
    let checksum: &[u8] = if bytes.len() >= cs_start + 8 {
        &bytes[cs_start..cs_start + 8]
    } else {
        &[]
    };
    let checksum_valid = checksum.len() == 8
        && checksum
            .iter()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if !checksum_valid {
        return Err("invalid share code: bad checksum format".to_string());
    }
    // json 起点：跳过 8 位校验和之后的一个 UTF-16 码元（ASCII 1 字节；多字节字符
    // 按整字跳过——与 JS `slice(firstColon + 10)` 的码元推进语义对齐）
    let mut json_start = cs_start + 8;
    if json_start < bytes.len() {
        if bytes[json_start] < 0x80 {
            json_start += 1;
        } else if let Some(ch) = text[json_start..].chars().next() {
            json_start += ch.len_utf8();
        }
    }
    let json = &text[json_start..];

    if checksum_of(version, json) != String::from_utf8_lossy(checksum) {
        return Err("share code checksum mismatch".to_string());
    }

    let raw: Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Err("invalid share code: malformed JSON".to_string()),
    };

    if version == "1" {
        return sanitize_params(&raw); // v1：全量参数快照
    }

    // v2：差异载荷 → 以默认参数（发送方采样率）为骨架还原后走同一套白名单清洗。
    // （v2 载荷强制携带 sampleRate；骨架采样率仅是缺省兜底，越界/缺失由
    //   sanitizeParams 的白名单 clamp 最终兜底。）
    let sample_rate = raw
        .as_object()
        .and_then(|o| o.get("sampleRate"))
        .and_then(Value::as_f64)
        .filter(|x| x.is_finite())
        .unwrap_or(48000.0);
    let skeleton = default_params_skeleton(sample_rate);
    let rehydrated = share_rehydrate(&skeleton, &raw);
    sanitize_params(&rehydrated)
}

/// 前缀判定：前 4 个字符整体 `toUpperCase()` 后等于 "HSE2"（对齐 JS
/// `slice(0, 4).toUpperCase() === 'HSE2'`；'ħ'→'H' 等单字符映射两侧一致）。
fn is_hse2_prefix(chars: &[char]) -> bool {
    let mut upper = String::with_capacity(8);
    for c in chars {
        upper.extend(c.to_uppercase());
    }
    upper == SHARE_CODE_PREFIX
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 结构化深度比较（键序无关；数值按 f64 相等——对齐"TS 全数值为 f64"语义，
    /// 规避 i64/f64 与 -0/0、1e2/100 等表示差异）。
    fn json_eq(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
                (Some(p), Some(q)) => p == q,
                _ => false,
            },
            (Value::Array(x), Value::Array(y)) => {
                x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| json_eq(p, q))
            }
            (Value::Object(x), Value::Object(y)) => {
                x.len() == y.len()
                    && x.iter()
                        .all(|(k, v)| y.get(k).map_or(false, |w| json_eq(v, w)))
            }
            (Value::Null, Value::Null) => true,
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::String(x), Value::String(y)) => x == y,
            _ => false,
        }
    }

    // ------------------------- 单元级对拍（手工向量） -------------------------

    #[test]
    fn fnv1a32_按utf16码元推进_与ts_charCodeAt一致() {
        // 空串/ASCII/含非 ASCII（UTF-16 码元 ≠ UTF-8 字节）三类向量，
        // 期望值由 node 端独立实现（Math.imul + charCodeAt）计算
        assert_eq!(fnv1a32_utf16(""), 0x811c_9dc5);
        assert_eq!(fnv1a32_utf16("hello"), 0x4f9f_2cab);
        assert_eq!(fnv1a32_utf16("2:{\"sampleRate\":48000}"), 0x9edd_696f);
        assert_eq!(
            fnv1a32_utf16("2:{\"sceneId\":\"爵士 Club 🎵\"}"),
            0xa3fd_9a03
        );
        assert_eq!(fnv1a32_utf16("1:{\"sampleRate\":48000,\"deesser\":{\"enabled\":true,\"thresholdDb\":-40},\"limiter\":{\"thresholdDb\":-0.5}}"), 0xd2e2_cebb);
    }

    #[test]
    fn base64url_解码_含无填充与填充输入() {
        // "hello" 的 base64url 为 "aGVsbG8"（无填充）；带填充输入兼容
        assert_eq!(base64url_to_bytes("aGVsbG8").unwrap(), b"hello");
        assert_eq!(base64url_to_bytes("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64url_to_bytes("").unwrap(), b"");
        assert_eq!(base64url_to_bytes("aGVsbG").unwrap(), b"hell");
        assert!(base64url_to_bytes("aaaaa").is_err()); // 长度 %4==1 → 非法
        assert!(base64url_to_bytes("!!!!").is_err()); // 非法字符
        assert!(base64url_to_bytes("A===").is_err()); // '=' 不在字母表（位置 0/1）
    }

    #[test]
    fn base32_crockford_易混字符归一与非法字符() {
        // "10" → 5+5 位取 8 位：acc=(1<<5)|0=32 → (32>>2)&0xff = 8
        assert_eq!(base32_crockford_to_bytes("10").unwrap(), vec![8u8]);
        // 大小写不敏感 + I/L→1、O→0、U→V
        assert_eq!(
            base32_crockford_to_bytes("iLoU").unwrap(),
            base32_crockford_to_bytes("110V").unwrap()
        );
        assert!(base32_crockford_to_bytes("!!!!").is_err());
        assert_eq!(
            base32_crockford_to_bytes("übel").unwrap_err(),
            "invalid share code: bad base32 character Ü"
        );
    }

    #[test]
    fn jnum_整数浮点与负零归一() {
        assert_eq!(serde_json::to_string(&jnum(48000.0)).unwrap(), "48000");
        assert_eq!(serde_json::to_string(&jnum(-30.0)).unwrap(), "-30");
        assert_eq!(serde_json::to_string(&jnum(0.5)).unwrap(), "0.5");
        assert_eq!(serde_json::to_string(&jnum(-0.0)).unwrap(), "0");
        assert_eq!(serde_json::to_string(&jnum(0.0)).unwrap(), "0");
        assert_eq!(serde_json::to_string(&jnum(3.5)).unwrap(), "3.5");
    }

    #[test]
    fn share_rehydrate_数组叶子整体替换_未知键丢弃() {
        let base = json!({"a": 1, "arr": [1, 2], "sub": {"x": 1, "y": 2}});
        let delta = json!({"a": 9, "arr": [3], "sub": {"x": 5}, "unknown": 1});
        let out = share_rehydrate(&base, &delta);
        assert_eq!(out, json!({"a": 9, "arr": [3], "sub": {"x": 5, "y": 2}}));
    }

    // ------------------------- golden 对拍（node 生成，冻结常量） -------------------------

    /// golden 套件：由 node（esbuild 打包 TS 支线 ShareCodec.ts）生成，
    /// 期望值为 node `decodeShareCode` 的解码结果（ok）或错误消息（err）。
    /// 每个 case：{ kind: "ok"|"err", code: 分享串, want: 解码 JSON | 错误消息 }。
    /// 对拍按结构化比较（键序无关、数值按 f64 相等），规避键序/数值表示差异。
    const GOLDENS_JSON: &str = r##"{"err_aaa": {"code": "aaa", "kind": "err", "want": "invalid share code: missing version"}, "err_bad_b64_char": {"code": "!!!!!not-base64!!!!!", "kind": "err", "want": "invalid share code: invalid share code: bad base64url character"}, "err_bad_b64_len": {"code": "abcde", "kind": "err", "want": "invalid share code: invalid share code: bad base64url length"}, "err_empty": {"code": "", "kind": "err", "want": "invalid share code: empty input"}, "err_hse2_badchar": {"code": "HSE2-!!!!!", "kind": "err", "want": "invalid share code: bad base32 character !"}, "err_hse2_badchar_lower": {"code": "hse2-üüü", "kind": "err", "want": "invalid share code: bad base32 character Ü"}, "err_hse2_only": {"code": "HSE2", "kind": "err", "want": "invalid share code: missing version"}, "err_version_3": {"code": "Mzo3MThiNGQxNjp7InNhbXBsZVJhdGUiOjQ4MDAwfQ", "kind": "err", "want": "unsupported share code version: 3"}, "err_version_3_v2transport": {"code": "HSE2-6CX30-C1G60-R30C1-G79XJ-4WV1D-NR6RS-AJC5T-6A8HT-6GW30-C1GFM", "kind": "err", "want": "unsupported share code version: 3"}, "err_version_empty": {"code": ":dd1e5d0d:{}", "kind": "err", "want": "invalid share code: invalid share code: bad base64url character"}, "err_ws_only": {"code": "   ", "kind": "err", "want": "invalid share code: missing version"}, "v1_array_payload": {"code": "MTplZGJiNjUzMzpbMSwyXQ", "kind": "err", "want": "invalid share code payload"}, "v1_clamps_edge": {"code": "MTowZWIzOTM5Yjp7InNhbXBsZVJhdGUiOjk5OTk5OSwiaWVxIjp7InN0cmVuZ3RoIjo5LCJ0aW1lQ29uc3RhbnRTZWMiOjk5fSwicGl0Y2giOnsic2VtaXRvbmVzIjo5OSwicmF0ZSI6OSwidm9pY2VCYWxhbmNlIjo5fSwibW9kRWZmZWN0cyI6eyJkZWxheSI6eyJkZWxheU1zIjo5OTk5LCJmZWVkYmFjayI6OSwibWl4Ijo5fSwidHJlbW9sbyI6eyJyYXRlSHoiOjk5LCJkZXB0aCI6OSwibWl4Ijo5fX0sImxvdWRuZXNzTm9ybWFsaXphdGlvbiI6eyJ0YXJnZXRMdWZzIjo5LCJtYXhHYWluRGIiOjk5LCJtaW5HYWluRGIiOi05OSwiZXh0ZXJuYWxHYWluRGIiOjk5fSwibG91ZG5lc3NDb21wZW5zYXRpb24iOnsidm9sdW1lUGVyY2VudCI6OTk5LCJtYXhCb29zdERiIjo5OSwic21vb3RoaW5nU2Vjb25kcyI6OTl9LCJuaWdodE1vZGUiOnsiYW1vdW50Ijo5OX0sInN1cnJvdW5kM2QiOnsiZGlzdGFuY2UiOjk5LCJzcGVlZCI6OTksImFuZ2xlIjo5OTl9LCJiYXNzRW5oYW5jZXIiOnsiY3V0b2ZmSHoiOjk5OTksInEiOjk5LCJoYXJtb25pY0dhaW4iOjksIm1peCI6OSwibGV2ZWxEYiI6OTksImxvd0Jvb3N0RGIiOjk5fSwicmV2ZXJiIjp7ImFsZ29yaXRobWljIjp7InJvb21TaXplIjo5LCJkYW1waW5nIjo5LCJ3ZXQiOjksImRyeSI6OSwicHJlRGVsYXlNcyI6OTk5OSwid2lkdGgiOjl9LCJjb252b2x1dGlvbiI6eyJtaXgiOjksInByZURlbGF5TXMiOjk5OTl9fSwiZGVlc3NlciI6eyJjZW50ZXJIeiI6OTk5OTksInEiOjk5LCJ0aHJlc2hvbGREYiI6OTksInJhdGlvIjo5OTksImF0dGFja01zIjo5OTksInJlbGVhc2VNcyI6OTk5OSwibWl4Ijo5fSwibGltaXRlciI6eyJ0aHJlc2hvbGREYiI6OSwibG9va2FoZWFkTXMiOjk5OSwiYXR0YWNrTXMiOjk5LCJyZWxlYXNlTXMiOjk5OTl9LCJkeW5hbWljRXEiOnsiYmFuZHMiOltudWxsLHsiZW5hYmxlZCI6ZmFsc2V9LCJub3Rhbm9iamVjdCIseyJ0YXJnZXRHYWluRGIiOi05OX0se30seyJlbmFibGVkIjp0cnVlLCJ0YXJnZXRHYWluRGIiOjN9LHsiZW5hYmxlZCI6dHJ1ZX1dfX0", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 500, "enabled": false, "harmonicGain": 1, "harmonicType": "odd", "levelDb": 6, "lowBoostDb": 12, "mix": 1, "q": 10}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 100, "centerHz": 16000, "enabled": false, "mix": 1, "q": 10, "ratio": 50, "releaseMs": 2000, "sidechainEnabled": false, "splitBand": true, "thresholdDb": 0}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": false, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": -12}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 1, "targetCurve": "flat", "timeConstantSec": 10}, "limiter": {"attackMs": 50, "enabled": true, "lookaheadMs": 50, "releaseMs": 2000, "thresholdDb": 0, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 24, "mode": "auto", "preset": "flat", "smoothingSeconds": 10, "volumePercent": 100}, "loudnessNormalization": {"enabled": false, "externalGainDb": 24, "maxGainDb": 24, "minGainDb": -24, "targetLufs": 0, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 2000, "enabled": false, "feedback": 0.98, "mix": 1}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 1, "enabled": false, "mix": 1, "rateHz": 30}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 10, "enabled": false}, "pitch": {"enabled": false, "rate": 3, "semitones": 10, "voiceBalance": 1}, "reverb": {"algorithmic": {"damping": 1, "dry": 1, "preDelayMs": 500, "roomSize": 1, "type": "hall", "wet": 1, "width": 2}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 1, "preDelayMs": 500}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 192000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 360, "direction": 1, "distance": 10, "enabled": false, "speed": 10}}}, "v1_clamps_repo": {"code": "MTpjMjQzMmI3ODp7InNhbXBsZVJhdGUiOjk5OTk5OSwiZXEiOnsicHJvQmFuZHMiOlt7ImZyZXF1ZW5jeSI6NTAsImdhaW4iOjk5OSwicSI6MH0seyJmcmVxdWVuY3kiOjEwLCJnYWluIjotOTk5LCJxIjo5OX1dfSwiZGVlc3NlciI6eyJlbmFibGVkIjoieWVzIiwidGhyZXNob2xkRGIiOjEyfSwicmV2ZXJiIjp7ImNvbnZvbHV0aW9uIjp7Im1peCI6NX19LCJzdGVyZW9XaWR0aCI6LTN9", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": 0}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 50, "gain": 20, "q": 0.1}, {"frequency": 20, "gain": -20, "q": 10}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 1, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 192000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 0, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v1_emoji_truncate": {"code": "MTo0N2YxNjY1Mzp7InNhbXBsZVJhdGUiOjQ4MDAwLCJzY2VuZUlkIjoi8J-OtfCfjrXwn4618J-OtfCfjrXwn4618J-OtfCfjrXwn4618J-OtfCfjrXwn4618J-OtfCfjrXwn4618J-OtfCfjrXwn4618J-OtfCfjrXwn4618J-OtfCfjrXwn4618J-OtfCfjrXwn4618J-OtfCfjrXwn4618J-OtfCfjrXwn4618J-OtfCfjrXwn4618J-OtfCfjrXwn4618J-OtSJ9", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": "🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵🎵", "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v1_enums": {"code": "MTowMmQ4YTU2Nzp7InNhbXBsZVJhdGUiOjQ4MDAwLCJyZXZlcmIiOnsibW9kZSI6Im51Y2xlYXIifSwiYmFzc0VuaGFuY2VyIjp7Imhhcm1vbmljVHlwZSI6InjCsyJ9LCJzdXJyb3VuZDNkIjp7ImRpcmVjdGlvbiI6N30sImVxIjp7ImJhbmRDb3VudCI6OTl9fQ", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v1_lc_bands_40": {"code": "MTowMDc2ODhkOTp7InNhbXBsZVJhdGUiOjQ4MDAwLCJsb3VkbmVzc0NvbXBlbnNhdGlvbiI6eyJiYW5kcyI6W3siZnJlcXVlbmN5IjoxMDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5IjoyMDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5IjozMDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5Ijo0MDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5Ijo1MDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5Ijo2MDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5Ijo3MDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5Ijo4MDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5Ijo5MDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5IjoxMDAwLCJnYWluIjoyfSx7ImZyZXF1ZW5jeSI6MTEwMCwiZ2FpbiI6Mn0seyJmcmVxdWVuY3kiOjEyMDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5IjoxMzAwLCJnYWluIjoyfSx7ImZyZXF1ZW5jeSI6MTQwMCwiZ2FpbiI6Mn0seyJmcmVxdWVuY3kiOjE1MDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5IjoxNjAwLCJnYWluIjoyfSx7ImZyZXF1ZW5jeSI6MTcwMCwiZ2FpbiI6Mn0seyJmcmVxdWVuY3kiOjE4MDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5IjoxOTAwLCJnYWluIjoyfSx7ImZyZXF1ZW5jeSI6MjAwMCwiZ2FpbiI6Mn0seyJmcmVxdWVuY3kiOjIxMDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5IjoyMjAwLCJnYWluIjoyfSx7ImZyZXF1ZW5jeSI6MjMwMCwiZ2FpbiI6Mn0seyJmcmVxdWVuY3kiOjI0MDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5IjoyNTAwLCJnYWluIjoyfSx7ImZyZXF1ZW5jeSI6MjYwMCwiZ2FpbiI6Mn0seyJmcmVxdWVuY3kiOjI3MDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5IjoyODAwLCJnYWluIjoyfSx7ImZyZXF1ZW5jeSI6MjkwMCwiZ2FpbiI6Mn0seyJmcmVxdWVuY3kiOjMwMDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5IjozMTAwLCJnYWluIjoyfSx7ImZyZXF1ZW5jeSI6MzIwMCwiZ2FpbiI6Mn0seyJmcmVxdWVuY3kiOjMzMDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5IjozNDAwLCJnYWluIjoyfSx7ImZyZXF1ZW5jeSI6MzUwMCwiZ2FpbiI6Mn0seyJmcmVxdWVuY3kiOjM2MDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5IjozNzAwLCJnYWluIjoyfSx7ImZyZXF1ZW5jeSI6MzgwMCwiZ2FpbiI6Mn0seyJmcmVxdWVuY3kiOjM5MDAsImdhaW4iOjJ9LHsiZnJlcXVlbmN5Ijo0MDAwLCJnYWluIjoyfV19fQ", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [{"frequency": 100, "gain": 2}, {"frequency": 200, "gain": 2}, {"frequency": 300, "gain": 2}, {"frequency": 400, "gain": 2}, {"frequency": 500, "gain": 2}, {"frequency": 600, "gain": 2}, {"frequency": 700, "gain": 2}, {"frequency": 800, "gain": 2}, {"frequency": 900, "gain": 2}, {"frequency": 1000, "gain": 2}, {"frequency": 1100, "gain": 2}, {"frequency": 1200, "gain": 2}, {"frequency": 1300, "gain": 2}, {"frequency": 1400, "gain": 2}, {"frequency": 1500, "gain": 2}, {"frequency": 1600, "gain": 2}, {"frequency": 1700, "gain": 2}, {"frequency": 1800, "gain": 2}, {"frequency": 1900, "gain": 2}, {"frequency": 2000, "gain": 2}, {"frequency": 2100, "gain": 2}, {"frequency": 2200, "gain": 2}, {"frequency": 2300, "gain": 2}, {"frequency": 2400, "gain": 2}, {"frequency": 2500, "gain": 2}, {"frequency": 2600, "gain": 2}, {"frequency": 2700, "gain": 2}, {"frequency": 2800, "gain": 2}, {"frequency": 2900, "gain": 2}, {"frequency": 3000, "gain": 2}, {"frequency": 3100, "gain": 2}, {"frequency": 3200, "gain": 2}], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v1_legacy": {"code": "MTpkMmUyY2ViYjp7InNhbXBsZVJhdGUiOjQ4MDAwLCJkZWVzc2VyIjp7ImVuYWJsZWQiOnRydWUsInRocmVzaG9sZERiIjotNDB9LCJsaW1pdGVyIjp7InRocmVzaG9sZERiIjotMC41fX0", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": true, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -40}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -0.5, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v1_multibyte_fnv": {"code": "MTo2NTYwMjk3Mzp7InNhbXBsZVJhdGUiOjQ4MDAwLCJzY2VuZUlkIjoi54i15aOrIENsdWIg8J-OtSIsInJldmVyYiI6eyJjb252b2x1dGlvbiI6eyJpck5hbWUiOiLmlZnloILCt2hhbGwifX19", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": "教堂·hall", "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": "爵士 Club 🎵", "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v1_null_payload": {"code": "MTowZDYzZTAxMzpudWxs", "kind": "err", "want": "invalid share code payload"}, "v1_number_payload": {"code": "MTo3ZWVhMzA2MDo0Mg", "kind": "err", "want": "invalid share code payload"}, "v1_probands_25": {"code": "MToyYjZiODFkNTp7InNhbXBsZVJhdGUiOjQ4MDAwLCJlcSI6eyJtb2RlIjoicHJvIiwiYmFuZENvdW50IjoyMCwicHJvQmFuZHMiOlt7ImZyZXF1ZW5jeSI6MTAwLCJnYWluIjotMS41LCJxIjoxLjF9LHsiZnJlcXVlbmN5IjoyMDAsImdhaW4iOjEuNSwicSI6MS4xfSx7ImZyZXF1ZW5jeSI6MzAwLCJnYWluIjotMS41LCJxIjoxLjF9LHsiZnJlcXVlbmN5Ijo0MDAsImdhaW4iOjEuNSwicSI6MS4xfSx7ImZyZXF1ZW5jeSI6NTAwLCJnYWluIjotMS41LCJxIjoxLjF9LHsiZnJlcXVlbmN5Ijo2MDAsImdhaW4iOjEuNSwicSI6MS4xfSx7ImZyZXF1ZW5jeSI6NzAwLCJnYWluIjotMS41LCJxIjoxLjF9LHsiZnJlcXVlbmN5Ijo4MDAsImdhaW4iOjEuNSwicSI6MS4xfSx7ImZyZXF1ZW5jeSI6OTAwLCJnYWluIjotMS41LCJxIjoxLjF9LHsiZnJlcXVlbmN5IjoxMDAwLCJnYWluIjoxLjUsInEiOjEuMX0seyJmcmVxdWVuY3kiOjExMDAsImdhaW4iOi0xLjUsInEiOjEuMX0seyJmcmVxdWVuY3kiOjEyMDAsImdhaW4iOjEuNSwicSI6MS4xfSx7ImZyZXF1ZW5jeSI6MTMwMCwiZ2FpbiI6LTEuNSwicSI6MS4xfSx7ImZyZXF1ZW5jeSI6MTQwMCwiZ2FpbiI6MS41LCJxIjoxLjF9LHsiZnJlcXVlbmN5IjoxNTAwLCJnYWluIjotMS41LCJxIjoxLjF9LHsiZnJlcXVlbmN5IjoxNjAwLCJnYWluIjoxLjUsInEiOjEuMX0seyJmcmVxdWVuY3kiOjE3MDAsImdhaW4iOi0xLjUsInEiOjEuMX0seyJmcmVxdWVuY3kiOjE4MDAsImdhaW4iOjEuNSwicSI6MS4xfSx7ImZyZXF1ZW5jeSI6MTkwMCwiZ2FpbiI6LTEuNSwicSI6MS4xfSx7ImZyZXF1ZW5jeSI6MjAwMCwiZ2FpbiI6MS41LCJxIjoxLjF9LHsiZnJlcXVlbmN5IjoyMTAwLCJnYWluIjotMS41LCJxIjoxLjF9LHsiZnJlcXVlbmN5IjoyMjAwLCJnYWluIjoxLjUsInEiOjEuMX0seyJmcmVxdWVuY3kiOjIzMDAsImdhaW4iOi0xLjUsInEiOjEuMX0seyJmcmVxdWVuY3kiOjI0MDAsImdhaW4iOjEuNSwicSI6MS4xfSx7ImZyZXF1ZW5jeSI6MjUwMCwiZ2FpbiI6LTEuNSwicSI6MS4xfV19fQ", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 20, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 100, "gain": -1.5, "q": 1.1}, {"frequency": 200, "gain": 1.5, "q": 1.1}, {"frequency": 300, "gain": -1.5, "q": 1.1}, {"frequency": 400, "gain": 1.5, "q": 1.1}, {"frequency": 500, "gain": -1.5, "q": 1.1}, {"frequency": 600, "gain": 1.5, "q": 1.1}, {"frequency": 700, "gain": -1.5, "q": 1.1}, {"frequency": 800, "gain": 1.5, "q": 1.1}, {"frequency": 900, "gain": -1.5, "q": 1.1}, {"frequency": 1000, "gain": 1.5, "q": 1.1}, {"frequency": 1100, "gain": -1.5, "q": 1.1}, {"frequency": 1200, "gain": 1.5, "q": 1.1}, {"frequency": 1300, "gain": -1.5, "q": 1.1}, {"frequency": 1400, "gain": 1.5, "q": 1.1}, {"frequency": 1500, "gain": -1.5, "q": 1.1}, {"frequency": 1600, "gain": 1.5, "q": 1.1}, {"frequency": 1700, "gain": -1.5, "q": 1.1}, {"frequency": 1800, "gain": 1.5, "q": 1.1}, {"frequency": 1900, "gain": -1.5, "q": 1.1}, {"frequency": 2000, "gain": 1.5, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v1_routes_20": {"code": "MTpmZGUzNTdmODp7InNhbXBsZVJhdGUiOjQ4MDAwLCJtb2R1bGF0aW9uIjp7InJvdXRlcyI6W3sic291cmNlIjoibGZvIiwidGFyZ2V0IjoibWFzdGVyR2FpbiIsImFtb3VudCI6MC41LCJvZmZzZXQiOjB9LHsic291cmNlIjoiZW52ZWxvcGUiLCJ0YXJnZXQiOiJtYXN0ZXJHYWluIiwiYW1vdW50IjowLjUsIm9mZnNldCI6MH0seyJzb3VyY2UiOiJsZm8iLCJ0YXJnZXQiOiJtYXN0ZXJHYWluIiwiYW1vdW50IjowLjUsIm9mZnNldCI6MH0seyJzb3VyY2UiOiJlbnZlbG9wZSIsInRhcmdldCI6Im1hc3RlckdhaW4iLCJhbW91bnQiOjAuNSwib2Zmc2V0IjowfSx7InNvdXJjZSI6ImxmbyIsInRhcmdldCI6Im1hc3RlckdhaW4iLCJhbW91bnQiOjAuNSwib2Zmc2V0IjowfSx7InNvdXJjZSI6ImVudmVsb3BlIiwidGFyZ2V0IjoibWFzdGVyR2FpbiIsImFtb3VudCI6MC41LCJvZmZzZXQiOjB9LHsic291cmNlIjoibGZvIiwidGFyZ2V0IjoibWFzdGVyR2FpbiIsImFtb3VudCI6MC41LCJvZmZzZXQiOjB9LHsic291cmNlIjoiZW52ZWxvcGUiLCJ0YXJnZXQiOiJtYXN0ZXJHYWluIiwiYW1vdW50IjowLjUsIm9mZnNldCI6MH0seyJzb3VyY2UiOiJsZm8iLCJ0YXJnZXQiOiJtYXN0ZXJHYWluIiwiYW1vdW50IjowLjUsIm9mZnNldCI6MH0seyJzb3VyY2UiOiJlbnZlbG9wZSIsInRhcmdldCI6Im1hc3RlckdhaW4iLCJhbW91bnQiOjAuNSwib2Zmc2V0IjowfSx7InNvdXJjZSI6ImxmbyIsInRhcmdldCI6Im1hc3RlckdhaW4iLCJhbW91bnQiOjAuNSwib2Zmc2V0IjowfSx7InNvdXJjZSI6ImVudmVsb3BlIiwidGFyZ2V0IjoibWFzdGVyR2FpbiIsImFtb3VudCI6MC41LCJvZmZzZXQiOjB9LHsic291cmNlIjoibGZvIiwidGFyZ2V0IjoibWFzdGVyR2FpbiIsImFtb3VudCI6MC41LCJvZmZzZXQiOjB9LHsic291cmNlIjoiZW52ZWxvcGUiLCJ0YXJnZXQiOiJtYXN0ZXJHYWluIiwiYW1vdW50IjowLjUsIm9mZnNldCI6MH0seyJzb3VyY2UiOiJsZm8iLCJ0YXJnZXQiOiJtYXN0ZXJHYWluIiwiYW1vdW50IjowLjUsIm9mZnNldCI6MH0seyJzb3VyY2UiOiJlbnZlbG9wZSIsInRhcmdldCI6Im1hc3RlckdhaW4iLCJhbW91bnQiOjAuNSwib2Zmc2V0IjowfSx7InNvdXJjZSI6ImxmbyIsInRhcmdldCI6Im1hc3RlckdhaW4iLCJhbW91bnQiOjAuNSwib2Zmc2V0IjowfSx7InNvdXJjZSI6ImVudmVsb3BlIiwidGFyZ2V0IjoibWFzdGVyR2FpbiIsImFtb3VudCI6MC41LCJvZmZzZXQiOjB9LHsic291cmNlIjoibGZvIiwidGFyZ2V0IjoibWFzdGVyR2FpbiIsImFtb3VudCI6MC41LCJvZmZzZXQiOjB9LHsic291cmNlIjoiZW52ZWxvcGUiLCJ0YXJnZXQiOiJtYXN0ZXJHYWluIiwiYW1vdW50IjowLjUsIm9mZnNldCI6MH1dfX0", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": [{"amount": 0.5, "offset": 0, "source": "lfo", "target": "masterGain"}, {"amount": 0.5, "offset": 0, "source": "envelope", "target": "masterGain"}, {"amount": 0.5, "offset": 0, "source": "lfo", "target": "masterGain"}, {"amount": 0.5, "offset": 0, "source": "envelope", "target": "masterGain"}, {"amount": 0.5, "offset": 0, "source": "lfo", "target": "masterGain"}, {"amount": 0.5, "offset": 0, "source": "envelope", "target": "masterGain"}, {"amount": 0.5, "offset": 0, "source": "lfo", "target": "masterGain"}, {"amount": 0.5, "offset": 0, "source": "envelope", "target": "masterGain"}, {"amount": 0.5, "offset": 0, "source": "lfo", "target": "masterGain"}, {"amount": 0.5, "offset": 0, "source": "envelope", "target": "masterGain"}, {"amount": 0.5, "offset": 0, "source": "lfo", "target": "masterGain"}, {"amount": 0.5, "offset": 0, "source": "envelope", "target": "masterGain"}, {"amount": 0.5, "offset": 0, "source": "lfo", "target": "masterGain"}, {"amount": 0.5, "offset": 0, "source": "envelope", "target": "masterGain"}, {"amount": 0.5, "offset": 0, "source": "lfo", "target": "masterGain"}, {"amount": 0.5, "offset": 0, "source": "envelope", "target": "masterGain"}]}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v1_spatial_missing_sub": {"code": "MTo3NDg4ZTFhMDp7InNhbXBsZVJhdGUiOjQ4MDAwLCJzcGF0aWFsIjp7Im1vZGUiOiJpbnN0YW50In19", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v1_spatial_nonobj": {"code": "MTpkYjE1ZTk5ODp7InNhbXBsZVJhdGUiOjQ4MDAwLCJzcGF0aWFsIjoibm9wZSJ9", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v1_spatial_null_in_array": {"code": "MTo4Njk3YjFhYjp7InNhbXBsZVJhdGUiOjQ4MDAwLCJzcGF0aWFsIjp7Im1vZGUiOiJvZmYiLCJpbnN0YW50Ijp7InNwcmVhZERlZyI6NjB9LCJleHRyYSI6WzEsbnVsbCwzXX19", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v1_spatial_rich": {"code": "MTo1NjU5N2RhZTp7InNhbXBsZVJhdGUiOjQ4MDAwLCJzcGF0aWFsIjp7Im1vZGUiOiJzdGFnZSIsIm1hc3RlckdhaW4iOjAuOSwiaW5zdGFudCI6eyJleHRyYUtleSI6ImtlcHQiLCJjb25zdHJ1Y3RvciI6ImRhbmdlciJ9LCJoZWFkTG9ja2VkIjp7fSwid29ybGQiOnt9LCJzdGFnZSI6e30sImFtYmllbmNlIjp7fSwiZXh0cmFUb3AiOnsieCI6MX19fQ", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "extraKey": "kept", "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "stage", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v1_truncate": {"code": "MTo0OGY2MjhiMjp7InNhbXBsZVJhdGUiOjQ4MDAwLCJyZXZlcmIiOnsiY29udm9sdXRpb24iOnsiaXJOYW1lIjoieHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eCJ9fX0", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx", "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v1_whitelist": {"code": "MTo1OGE4YWQyZDp7InNhbXBsZVJhdGUiOjQ4MDAwLCJldmlsIjoiZHJvcC1tZSIsImVxIjp7ImVuYWJsZWQiOmZhbHNlLCJoYWNrZWQiOjk5OX19", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": false, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v2_bad_checksum_fmt": {"code": "HSE2-68X7M-YKTF9-X7MYK-T79XJ-4WV1D-NR6RS-AJC5T-6A8HT-6GW30-C1GFM", "kind": "err", "want": "invalid share code: bad checksum format"}, "v2_checksum_mismatch": {"code": "HSE2-68X30-C1G60-R30C1-G79XJ-4WV1D-NR6RS-AJC5T-6A8HT-6GW30-C1GFM", "kind": "err", "want": "share code checksum mismatch"}, "v2_default_44100": {"code": "HSE2-68X3E-CSHCN-H64SH-P79XJ-4WV1D-NR6RS-AJC5T-6A8HT-6GT32-C1GFM", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 44100, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v2_default_48000": {"code": "HSE2-68X3J-SB4CG-V3JDK-679XJ-4WV1D-NR6RS-AJC5T-6A8HT-6GW30-C1GFM", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v2_malformed_json": {"code": "HSE2-68X36-S1HCD-J32RB-279XP-WVVM4-1N76V-VE", "kind": "err", "want": "invalid share code: malformed JSON"}, "v2_neg_zero": {"code": "HSE2-68X3J-SB4CG-V3JDK-679XJ-4WV1D-NR6RS-AJC5T-6A8HT-6GW30-C1GFM", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v2_noisy": {"code": "hse2 68x3j sb4cg v3jdk 679xj 4wv1d nr6rs ajc5t 6a8ht 6gw3o c1gfm", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v2_payload_array": {"code": "HSE2-68X66-RHNC8-SP6RV-579DK-2B1JB-M", "kind": "err", "want": "invalid share code payload"}, "v2_payload_null": {"code": "HSE2-68X3A-D32CG-S66C3-379Q7-AV3C", "kind": "err", "want": "invalid share code payload"}, "v2_rate_string": {"code": "HSE2-68X3J-CV4CR-RK8RB-179XJ-4WV1D-NR6RS-AJC5T-6A8HT-48T3G-C1G60-H2R8K-3ENSQ-8VVDD-5X6AS-1279T-74XB5-FM", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": true, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v2_rich": {"code": "HSE2-68X32-RK36D-J6CDV-279XJ-4SBH4-8X7P8-KGE9Q-M4RBE-CHSJ4-EJVFC-H6CWK-5E5TP-AVK3F-4H3MD-1G5GH-6ERB9-DRH3M-CSE6M-P24W9-278R2-WE3X5-HXJ4S-KJCNR-QASBE-CDWJ4-EHP6C-P24SV-1D5Q2-4EHG5-GH728-HT64Q-32Z9C-FCH6C-WK5E5-TPAVK-3F4H3-MC9J6-MP24S-V1D5Q-24EHG-5GH72-8HT64-Q32Z9-CFCH6-CWK5E-5TPAV-K3F4H-3MCHN-60P24-SV1D5-Q24EH-G5GH7-28HT6-4Q32Z-9CFCH-6CWK5-E5TPA-VK3F4-H3MD9-G60P2-4SV1D-5Q24E-HG5GH-728HT-64Q32-Z9CFC-H6CWK-5E5TP-AVK3F-4H3MC-9G60R-2R8K7-C5MPW-8HT60-P24W9-278RJ-WCBX5-HXJ4S-KJCNR-QASBE-CDWJ4-EHJ60-R30B1-2CXGP-JVH27-8R2R8-KH48X-32BHH-FMP7P-8K6E9-JQ2XB-5DSHQ-J8HT6-GR30C-1C49K-P2TBE-48X30-B12E4-H3MC9-E65YJ-RYS2C-SS6AW-BNCNQ-66Y92-78W30-C1G5G-H6ERB-9DRH3-MC1C4-9RJ4E-HH5RR-QTB3V-49K74-SBHEN-JPWRV-S48X3-2DHG6-0R2R8-K7C5M-PW8HT-60P24-W9278-RJWCB-XBNYJ-R8K4C-NJQ6W-V5E8H-3MYS2-CNQ62-RKCCN-J24EK-ME9TP-AB12E-HM74S-BKD1Q-PRS24-C8H3M-B9M61-YJR8K-2C5SQ-6HBED-1GPWR-V5E8H-3MYS2-CNQ62-RKCCN-J24EK-ME9TP-AB12D-HQQEG-KFDXS-Q8H32-48X38-Z9C49-S6AXK-5E9H2-4EKV4-9JPWR-B2DHJ-P88HT-EHS7A-S9C49-PPYS3-548X2-4RVFD-SV6YV-3NEHM-PYVH2-5GH66-VVEES-QPRXB-MD5QP-W8HTF-CH6JW-JEC5P-PA8HT-49M62-V3C5N-HP2X3-8CNJ7-4RBC4-9YQTB-12DHQ-QAS3E-CNSQ6-KKFE9-PP2V3-9F9GQ-8TBFD-RH3MY-S2EHG-Q4SV5-EH67A-SKK48-X2TC9-PFMP2-4WVGC-5T6JR-BC48X-7P8KD-DXJ6A-8HT49-MPWWV-MC5Q7-88KX5-GH76X-35E9J-PYNV9-CHT6G-8HT64-Q38B1-2EDHP-AVK59-5J24E-H2D9G-QMYH2-5GH66-XBKEH-QPTTB-TCNJ2-4EKME-9TPAB-12EDG-PTW3C-CN962-X3548-X38E1-G60R7-T", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": true, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 4, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": true, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": true, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -40}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 40, "gain": 3.5, "q": 0.8}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -16, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": "hall-cathedral", "mix": 0.3, "preDelayMs": 0}, "enabled": true, "mode": "convolution"}, "sampleRate": 48000, "sceneId": "jazz", "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "instant", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1.4, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v2_tampered": {"code": "HSE2-68X3JSB4CGV3JDK679XJ4WV1DNR6RSAJC5T6A8HT6GW30C1GFA", "kind": "err", "want": "share code checksum mismatch"}, "v2_unknown_keys": {"code": "HSE2-68X64-S9QCG-W62DB-179XJ-4WV1D-NR6RS-AJC5T-6A8HT-6GW30-C1G5G-H6AXK-9DGH3-MC9C4-9P6JV-B9EHJ-Q48HT-FCH78-T3JCN-SPGVV-CCH26-48HT5-MSJR8-K8C5H-PPSB4-48X32-ZBX", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -3, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}, "v2_unknown_top_only": {"code": "HSE2-68X6C-RK36H-H3JCK-579XJ-4WV1D-NR6RS-AJC5T-6A8HT-6GW30-C1G5G-H6TYA-BCNWJ-4EKME-9TPAZ-8", "kind": "ok", "want": {"bassEnhancer": {"cutoffHz": 90, "enabled": false, "harmonicGain": 0.6, "harmonicType": "odd", "levelDb": 0, "lowBoostDb": 0, "mix": 0.5, "q": 0.7}, "compressor": {"attackMs": 10, "enabled": false, "kneeDb": 6, "makeupDb": 0, "outputGain": 1, "ratio": 4, "releaseMs": 150, "sidechainEnabled": false, "thresholdDb": -20}, "customized": false, "deesser": {"attackMs": 1, "centerHz": 6000, "enabled": false, "mix": 1, "q": 0.7, "ratio": 8, "releaseMs": 80, "sidechainEnabled": false, "splitBand": true, "thresholdDb": -30}, "dynamicEq": {"attackMs": 20, "bands": [{"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}, {"enabled": true, "targetGainDb": 0}], "enabled": false, "ratio": 2, "releaseMs": 200, "strength": 0.5, "thresholdDb": -20}, "eq": {"bandCount": 10, "enabled": true, "locked": false, "mode": "pro", "proBands": [{"frequency": 31.5, "gain": 0, "q": 1.1}, {"frequency": 63, "gain": 0, "q": 1.1}, {"frequency": 125, "gain": 0, "q": 1.1}, {"frequency": 250, "gain": 0, "q": 1.1}, {"frequency": 500, "gain": 0, "q": 1.1}, {"frequency": 1000, "gain": 0, "q": 1.1}, {"frequency": 2000, "gain": 0, "q": 1.1}, {"frequency": 4000, "gain": 0, "q": 1.1}, {"frequency": 8000, "gain": 0, "q": 1.1}, {"frequency": 16000, "gain": 0, "q": 1.1}], "qCompensation": true, "simpleBands": [0, 0, 0, 0, 0]}, "hearing": {"enabled": false}, "ieq": {"enabled": false, "strength": 0.5, "targetCurve": "flat", "timeConstantSec": 3}, "limiter": {"attackMs": 0.5, "enabled": true, "lookaheadMs": 5, "releaseMs": 150, "thresholdDb": -1, "truePeak": true}, "loudnessCompensation": {"bands": [], "enabled": false, "maxBoostDb": 12, "mode": "auto", "preset": "flat", "smoothingSeconds": 0.2, "volumePercent": 80}, "loudnessNormalization": {"enabled": false, "externalGainDb": 0, "maxGainDb": 9, "minGainDb": -9, "targetLufs": -14, "useRealtimeMeter": true}, "modEffects": {"chorus": {"depthMs": 3, "enabled": false, "mix": 0.4, "rateHz": 1}, "delay": {"delayMs": 250, "enabled": false, "feedback": 0.3, "mix": 0.3}, "flanger": {"depthMs": 2, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5}, "phaser": {"depth": 0.5, "enabled": false, "feedback": 0.4, "mix": 0.5, "rateHz": 0.5, "stages": 4}, "tremolo": {"depth": 0.5, "enabled": false, "mix": 1, "rateHz": 5}}, "modulation": {"enabled": false, "envelope": {"amount": 0.5, "attackMs": 10, "enabled": false, "releaseMs": 200}, "lfo": {"depth": 0.5, "enabled": false, "rateHz": 1, "shape": "sine"}, "routes": []}, "nightMode": {"amount": 0, "enabled": false}, "pitch": {"enabled": false, "rate": 1, "semitones": 0, "voiceBalance": 0}, "reverb": {"algorithmic": {"damping": 0.5, "dry": 0.7, "preDelayMs": 0, "roomSize": 0.5, "type": "hall", "wet": 0.3, "width": 1}, "convolution": {"dePeriodize": true, "ir": null, "irName": null, "mix": 0.3, "preDelayMs": 0}, "enabled": false, "mode": "algorithmic"}, "sampleRate": 48000, "sceneId": null, "spatial": {"ambience": {"amount": 0.3, "enabled": false}, "convolution": "partitioned", "distanceModel": "inverse", "headLocked": {"bottomLayer": true, "heightLayer": true, "layout": "51", "routes": [], "speakers": [{"azimuthDeg": 0, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 30, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": -110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}, {"azimuthDeg": 110, "distance": 1.5, "elevationDeg": 0, "gain": 1, "size": 0}]}, "hrtfInterp": "nearest", "instant": {"amount": 0.7, "multichannelAuto": false, "room": "studio", "roomAmount": 0.15, "spreadDeg": 60}, "masterGain": 0.9, "maxDistance": 50, "mode": "off", "refDistance": 1, "stage": {"customSources": [], "preset": "stage", "reverbAmount": 0.35, "roomSize": 1, "seat": "middle"}, "world": {"listener": {"pitch": 0, "position": {"x": 0, "y": 1.6, "z": 0}, "roll": 0, "yaw": 0}, "moveSpeed": 2, "occlusion": 0, "playhead": 0, "sources": [{"gain": 1, "id": "vocal", "position": {"x": -2, "y": 1.6, "z": 4}, "size": 0}, {"gain": 1, "id": "guitar", "position": {"x": -5, "y": 1.6, "z": 6}, "size": 0}, {"gain": 1, "id": "drums", "position": {"x": 3, "y": 1.6, "z": 7}, "size": 0}, {"gain": 0.6, "id": "ambience", "position": {"x": 0, "y": 2.5, "z": 10}, "size": 0.5}], "trajectories": []}}, "stereoWidth": 1, "surround3d": {"angle": 0, "direction": 1, "distance": 0.5, "enabled": false, "speed": 1}}}}"##;

    fn goldens() -> Map<String, Value> {
        serde_json::from_str(GOLDENS_JSON).expect("golden 常量必须为合法 JSON")
    }

    fn case(id: &str) -> Value {
        goldens()
            .get(id)
            .cloned()
            .unwrap_or_else(|| panic!("缺少 golden case: {}", id))
    }

    fn assert_ok_case(id: &str) {
        let c = case(id);
        assert_eq!(c["kind"].as_str(), Some("ok"), "case {} 应为 ok", id);
        let got = decode_share_code(c["code"].as_str().unwrap())
            .unwrap_or_else(|e| panic!("[{}] 应解码成功，实际 Err: {}", id, e));
        assert!(
            json_eq(&got, &c["want"]),
            "[{}] 解码结果与 TS golden 不一致\n  got:  {}\n  want: {}",
            id,
            got,
            c["want"]
        );
    }

    fn assert_err_case(id: &str) {
        let c = case(id);
        let raw_code = c["code"].as_str().unwrap();
        match decode_share_code(raw_code) {
            Ok(v) => panic!("[{}] 应报错，实际解码成功: {}", id, v),
            Err(msg) => {
                // 错误消息与 node decodeShareCode 逐字对齐（golden want 即 node 真实消息）
                assert_eq!(
                    msg,
                    c["want"].as_str().unwrap(),
                    "[{}] 错误消息与 TS 不一致",
                    id
                );
            }
        }
    }

    /// 全量 golden 对拍：44 组（node 生成；覆盖 v2/v1/篡改/白名单/clamp/spatial）。
    #[test]
    fn golden_全量对拍_node生成() {
        let g = goldens();
        assert_eq!(g.len(), 43, "golden case 数应与生成脚本一致");
        for id in g.keys() {
            let kind = g[id]["kind"].as_str().unwrap().to_string();
            if kind == "ok" {
                assert_ok_case(id);
            } else {
                assert_err_case(id);
            }
        }
    }

    // 下面为按主题拆分的具名用例（数据同源于 GOLDENS_JSON，便于阅读覆盖面）。

    /// v2（HSE2/Crockford/差异载荷）：默认参数与富参数往返
    #[test]
    fn v2_默认参数与富参数_对拍ts解码() {
        assert_ok_case("v2_default_48000");
        assert_ok_case("v2_default_44100");
        assert_ok_case("v2_rich");
    }

    /// Crockford 容错：小写 / 0↔O / 分隔符→空白，均解码出同一结果
    #[test]
    fn v2_crockford_容错_与干净串同结果() {
        assert_ok_case("v2_noisy");
        let g = goldens();
        let clean = decode_share_code(g["v2_default_48000"]["code"].as_str().unwrap()).unwrap();
        let noisy = decode_share_code(g["v2_noisy"]["code"].as_str().unwrap()).unwrap();
        assert!(json_eq(&clean, &noisy));
    }

    /// v1 旧串兼容（base64url 全量载荷）：缺省 section 由白名单默认值补齐
    #[test]
    fn v1_旧串_部分快照_默认骨架补齐() {
        assert_ok_case("v1_legacy");
        let got = decode_share_code(case("v1_legacy")["code"].as_str().unwrap()).unwrap();
        assert_eq!(got["sampleRate"].as_f64(), Some(48000.0));
        assert_eq!(got["deesser"]["enabled"].as_bool(), Some(true));
        assert_eq!(got["limiter"]["thresholdDb"].as_f64(), Some(-0.5));
        assert_eq!(got["eq"]["enabled"].as_bool(), Some(true));
        // ir 恒为 null（IR 数组不进入分享串）
        assert!(got["reverb"]["convolution"]["ir"].is_null());
    }

    /// 数值 clamp：越界值被钳到白名单区间（含仓库既有 test/codec.test.ts 向量）
    #[test]
    fn clamp_越界值钳到白名单区间() {
        assert_ok_case("v1_clamps_repo");
        assert_ok_case("v1_clamps_edge");
        let got = decode_share_code(case("v1_clamps_repo")["code"].as_str().unwrap()).unwrap();
        assert_eq!(got["sampleRate"].as_f64(), Some(192000.0));
        assert_eq!(got["eq"]["proBands"][0]["gain"].as_f64(), Some(20.0));
        assert_eq!(got["eq"]["proBands"][1]["q"].as_f64(), Some(10.0));
        assert_eq!(got["deesser"]["enabled"].as_bool(), Some(false));
        assert_eq!(got["deesser"]["thresholdDb"].as_f64(), Some(0.0));
        assert_eq!(got["reverb"]["convolution"]["mix"].as_f64(), Some(1.0));
        assert_eq!(got["stereoWidth"].as_f64(), Some(0.0));
    }

    /// 白名单：未知字段（含 __proto__/constructor/prototype 注入键）被丢弃
    #[test]
    fn whitelist_未知字段与注入键丢弃() {
        assert_ok_case("v1_whitelist");
        assert_ok_case("v2_unknown_keys");
        let got = decode_share_code(case("v1_whitelist")["code"].as_str().unwrap()).unwrap();
        assert_eq!(got["eq"]["enabled"].as_bool(), Some(false));
        assert_eq!(got["eq"]["mode"].as_str(), Some("pro"));
        assert!(got.get("evil").is_none());
        assert!(got.get("__proto__").is_none());
    }

    /// 枚举白名单：非法枚举回落默认值
    #[test]
    fn enums_非法枚举回落默认() {
        assert_ok_case("v1_enums");
        let got = decode_share_code(case("v1_enums")["code"].as_str().unwrap()).unwrap();
        assert_eq!(got["reverb"]["mode"].as_str(), Some("algorithmic"));
        assert_eq!(got["bassEnhancer"]["harmonicType"].as_str(), Some("odd"));
        assert_eq!(got["surround3d"]["direction"].as_f64(), Some(1.0));
        assert_eq!(got["eq"]["bandCount"].as_f64(), Some(10.0));
    }

    /// 长字符串截断（防超长注入；按 UTF-16 码元计长）
    #[test]
    fn 字符串截断_utf16码元计长() {
        assert_ok_case("v1_truncate");
        assert_ok_case("v1_emoji_truncate");
        let got = decode_share_code(case("v1_truncate")["code"].as_str().unwrap()).unwrap();
        assert!(
            got["reverb"]["convolution"]["irName"]
                .as_str()
                .unwrap()
                .len()
                <= 256
        );
        // 40 个 emoji（80 个 UTF-16 码元）截到 64 码元 = 32 个 emoji
        let got2 = decode_share_code(case("v1_emoji_truncate")["code"].as_str().unwrap()).unwrap();
        assert_eq!(got2["sceneId"].as_str().unwrap().chars().count(), 32);
    }

    /// 校验和/版本/格式类错误：篡改、坏校验和格式、版本不符、坏 base64url/base32
    #[test]
    fn 恶意输入_校验与格式类错误逐字对齐() {
        assert_err_case("v2_tampered");
        assert_err_case("v2_bad_checksum_fmt");
        assert_err_case("v2_checksum_mismatch");
        assert_err_case("v2_malformed_json");
        assert_err_case("err_version_3");
        assert_err_case("err_version_3_v2transport");
        assert_err_case("err_version_empty");
        assert_err_case("err_empty");
        assert_err_case("err_ws_only");
        assert_err_case("err_bad_b64_char");
        assert_err_case("err_bad_b64_len");
        assert_err_case("err_aaa");
        assert_err_case("err_hse2_only");
        assert_err_case("err_hse2_badchar");
        assert_err_case("err_hse2_badchar_lower");
    }

    /// v2 载荷形态错误：数组/null 载荷 → sanitize 层整体拒绝
    #[test]
    fn v2_载荷形态错误整体拒绝() {
        assert_err_case("v2_payload_array");
        assert_err_case("v2_payload_null");
        assert_err_case("v1_null_payload");
        assert_err_case("v1_array_payload");
        assert_err_case("v1_number_payload");
    }

    /// v2 还原语义：未知顶层键丢弃、sampleRate 字符串回落骨架 48000、-0 归一
    #[test]
    fn v2_还原语义_未知键与采样率兜底() {
        assert_ok_case("v2_unknown_top_only");
        assert_ok_case("v2_rate_string");
        assert_ok_case("v2_neg_zero");
        let got = decode_share_code(case("v2_rate_string")["code"].as_str().unwrap()).unwrap();
        assert_eq!(got["sampleRate"].as_f64(), Some(48000.0));
        assert_eq!(got["customized"].as_bool(), Some(true));
    }

    /// spatial 深度清洗：合法富块按默认骨架合并（子对象额外键保留、顶层额外键
    /// 丢弃、危险键跳过）；数组含 null / 非对象 / 缺子对象 → 整体回落默认 off
    #[test]
    fn spatial_深度清洗与整体回落() {
        assert_ok_case("v1_spatial_rich");
        assert_ok_case("v1_spatial_null_in_array");
        assert_ok_case("v1_spatial_nonobj");
        assert_ok_case("v1_spatial_missing_sub");
        let got =
            decode_share_code(case("v1_spatial_null_in_array")["code"].as_str().unwrap()).unwrap();
        let def = default_spatial_settings();
        assert!(
            json_eq(&got["spatial"], &def),
            "整体非法的 spatial 应回落默认 off"
        );

        let rich = decode_share_code(case("v1_spatial_rich")["code"].as_str().unwrap()).unwrap();
        assert_eq!(rich["spatial"]["mode"].as_str(), Some("stage"));
        assert_eq!(
            rich["spatial"]["instant"]["extraKey"].as_str(),
            Some("kept"),
            "子对象额外键保留"
        );
        assert!(
            rich["spatial"]["instant"].get("constructor").is_none(),
            "危险键丢弃"
        );
        assert!(rich["spatial"].get("extraTop").is_none(), "顶层额外键丢弃");
    }

    /// 数组上限：proBands 20 段 / loudnessCompensation 32 段 / modulation 路由 16 条
    #[test]
    fn 数组上限截断() {
        assert_ok_case("v1_probands_25");
        assert_ok_case("v1_lc_bands_40");
        assert_ok_case("v1_routes_20");
        let got = decode_share_code(case("v1_probands_25")["code"].as_str().unwrap()).unwrap();
        assert_eq!(got["eq"]["proBands"].as_array().unwrap().len(), 20);
        let got2 = decode_share_code(case("v1_lc_bands_40")["code"].as_str().unwrap()).unwrap();
        assert_eq!(
            got2["loudnessCompensation"]["bands"]
                .as_array()
                .unwrap()
                .len(),
            32
        );
        let got3 = decode_share_code(case("v1_routes_20")["code"].as_str().unwrap()).unwrap();
        assert_eq!(got3["modulation"]["routes"].as_array().unwrap().len(), 16);
    }

    /// 多字节字符串的校验和：FNV 按 UTF-16 码元推进（与 charCodeAt 对齐）
    #[test]
    fn 多字节载荷_fnv按utf16码元() {
        assert_ok_case("v1_multibyte_fnv");
        let got = decode_share_code(case("v1_multibyte_fnv")["code"].as_str().unwrap()).unwrap();
        assert_eq!(got["sceneId"].as_str(), Some("爵士 Club 🎵"));
        assert_eq!(
            got["reverb"]["convolution"]["irName"].as_str(),
            Some("教堂·hall")
        );
    }

    /// dynamicEq bands：非对象元素按默认带处理、超出 5 带截断、缺字段回落
    #[test]
    fn dynamicEq_逐带白名单与默认带() {
        // 覆盖于 v1_clamps_edge 内（bands 含 null/字符串/缺字段元素 + 7 个元素）
        assert_ok_case("v1_clamps_edge");
        let got = decode_share_code(case("v1_clamps_edge")["code"].as_str().unwrap()).unwrap();
        let bands = got["dynamicEq"]["bands"].as_array().unwrap();
        assert_eq!(bands.len(), 5);
        assert!(
            bands[0]["enabled"].as_bool().unwrap(),
            "null 元素 → 默认带 enabled=true"
        );
        assert!(!bands[1]["enabled"].as_bool().unwrap());
        assert_eq!(bands[3]["targetGainDb"].as_f64(), Some(-12.0), "越界钳制");
    }

    #[test]
    fn v2编码_默认与未知字段_逐字符命中ts_golden() {
        const DEFAULT: &str = "HSE2-68X3J-SB4CG-V3JDK-679XJ-4WV1D-NR6RS-AJC5T-6A8HT-6GW30-C1GFM";
        let p = crate::params::default_params(48_000.0);
        assert_eq!(encode_share_code(&p).unwrap(), DEFAULT);
        assert!(json_eq(&decode_share_code(DEFAULT).unwrap(), &p));

        let mut with_unknown = p;
        with_unknown["unknownTop"] = json!(123);
        with_unknown["eq"]["unknownNested"] = json!("x");
        assert_eq!(encode_share_code(&with_unknown).unwrap(), DEFAULT);
    }

    #[test]
    fn v2编码_非ascii与负零_逐字符命中ts_golden() {
        const UNICODE: &str = "HSE2-68X38-D35CS-H64RB-579XJ-4WV3C-NQ6AJ-B448X-25SW8-PQJT7-AS08D-P7ARH-0Y2FR-XD925-GH66X-BKEHQ-PTTBT-CNJ24-EKME9-TPAB1-2EDGP-TW3CC-N962X-3548X-38E1G-60R7T";
        let mut unicode = crate::params::default_params(48_000.0);
        unicode["customized"] = json!(true);
        unicode["sceneId"] = json!("爵士 Club 🎵");
        assert_eq!(encode_share_code(&unicode).unwrap(), UNICODE);
        assert!(json_eq(&decode_share_code(UNICODE).unwrap(), &unicode));

        const EDGES: &str = "HSE2-68X30-C9P71-K3ECV-179XJ-4SBH4-8X7P8-KKD5P-Q0V35-89GPW-S3K48-X5PB9-J60P3-4C1C6-0P30B-1K5RT-NTZ9C-49P6J-VB9EH-JQ48H-TFCH7-8T3JC-NSPGV-VCCH2-648HT-5MV30-Z9C49-SQ8SB-JCNQN-ETB4E-HM24E-HG5GH-76RBD-E1P6A-MK1EH-JJ4EH-R60R3-0Z8";
        let mut edges = crate::params::default_params(8_000.0);
        edges["stereoWidth"] = json!(-0.0);
        edges["surround3d"]["angle"] = json!(-0.0);
        edges["eq"]["simpleBands"] = json!([-20, 20, 0, -0.0, 3.5]);
        edges["limiter"]["thresholdDb"] = json!(-60);
        assert_eq!(encode_share_code(&edges).unwrap(), EDGES);
        let decoded = decode_share_code(EDGES).unwrap();
        assert_eq!(decoded["sampleRate"], 8_000);
        assert_eq!(decoded["stereoWidth"].as_f64(), Some(0.0));
        assert_eq!(decoded["limiter"]["thresholdDb"], -60);
    }

    #[test]
    fn v2编码_越界原值保留到解码阶段再钳制() {
        const RAW: &str = "HSE2-68X3J-C9H6G-VK6D1-P79XJ-4V39D-NMQ8S-BJ48X-7P8KM-D1S6A-WV8DX-P68H3-248X2-TE9SF-MP24W-VMCNS-6AVTQ-D5J78-T1278-WJR8K-KC5PQ-0V35A-9GQ8S-9278W-KJE9S-74WQT";
        let mut params = crate::params::default_params(48_000.0);
        params["sampleRate"] = json!(999_999);
        params["stereoWidth"] = json!(9);
        params["limiter"]["thresholdDb"] = json!(-99);
        assert_eq!(encode_share_code(&params).unwrap(), RAW);

        let decoded = decode_share_code(RAW).unwrap();
        assert_eq!(decoded["sampleRate"], 192_000);
        assert_eq!(decoded["stereoWidth"], 2);
        assert_eq!(decoded["limiter"]["thresholdDb"], -60);
    }

    #[test]
    fn v2现有ts_golden_解码后重编码逐字符稳定() {
        for id in [
            "v2_default_44100",
            "v2_default_48000",
            "v2_neg_zero",
            "v2_rich",
        ] {
            let original = case(id)["code"].as_str().unwrap().to_string();
            let decoded = decode_share_code(&original).unwrap();
            assert_eq!(encode_share_code(&decoded).unwrap(), original, "{id}");
        }
    }

    #[test]
    fn 十二场景_v2编码后往返() {
        for scene in crate::scenes::builtin_scenes() {
            let params = &scene["params"];
            let code = encode_share_code(params).unwrap();
            assert!(code.starts_with("HSE2-"));
            assert!(
                json_eq(&decode_share_code(&code).unwrap(), params),
                "场景 {} 往返失败",
                scene["id"]
            );
        }
    }
}
