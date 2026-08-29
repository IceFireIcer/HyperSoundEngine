//! wav —— 标准（+ 双支线共同约定的变体）RIFF/WAVE 文件编解码（Phase 3 批次三 · 第 2 项）。
//!
//! 行为事实标准：仓库根 `src/io/wav.ts`（`encodeWav` / `decodeWav`）；规格：
//! `specs/io/wav.md`。本模块为其逐字节移植：编码输出与 TS 对相同输入**逐字节一致**
//! （golden 对拍冻结在 `GOLDENS_JSON`，由 node esbuild 打包 TS 源生成）；解码校验
//! 与错误消息逐字对齐（含 node 对畸形输入抛出的原话）。
//!
//! # 与标准 RIFF 的差异（双支线共同契约，改前先读 specs/io/wav.md）
//!
//! TS 实现的全部头部数值字段（chunk size、formatTag、声道数、采样率、byteRate、
//! blockAlign、bitsPerSample、data size）按**大端**写入/读取（`DataView` 的
//! `littleEndian=false`），与 RIFF 标准的小端头字段不同；**样本数据**仍为小端
//! （PCM16 有符号小端 / IEEE-754 float32 小端）。两侧实现一致即契约，解码端
//! 只承诺识别**本模块编码端**产出的文件形态。
//!
//! # 数值语义（与 TS 逐一对齐）
//!
//! - PCM16 量化：先钳制到 [-1, 1]（NaN 不受钳制影响保持 NaN），再
//!   `Math.round(s * 32767)`——**半值向 +∞ 舍入**（0.5→16384、−0.5×32767→−16383；
//!   注意 Rust `f64::round` 是半值远离零，必须走 [`js_round`]），标度是 **32767
//!   而非 32768**（±1 → ±32767）；NaN 经 JS `ToInt32` 落 0（字节 00 00）。
//! - float32：按 f32 位模式原样小端写出/读入（denormal、−0、Infinity 位模式
//!   全程保留；golden 仅覆盖 canonical NaN——引擎不会产生带非规范载荷的 NaN）。
//! - 解码 PCM16：`f64 除以 32767` 后落入 f32（TS 先做 f64 除法再存 Float32Array，
//!   本模块同样先 f64 后收窄，避免 f32 直除的末位偏差）。
//! - 头部整数写入走 `ToUint32` 环绕语义（byteRate = sampleRate×blockAlign 超出
//!   2^32 时按模 2^32 截断），本模块以 u64 乘法 + `as u32` 复刻。
//!
//! # 防注入 / 畸形输入
//!
//! 解码对一切畸形输入返回 `Err`（不静默）：过短文件、坏魔数、缺 fmt/data chunk、
//! 0 声道、不支持位深、formatTag 与位深不匹配、data 长度不按 blockAlign 对齐、
//! fmt 字段越界（对齐 node 的 `DataView` RangeError 原话）。fmt 中的
//! formatTag=0 与"未见 fmt"不可区分，同样报 missing fmt chunk（node 实测）。
//!
//! 确定性：纯函数、无随机/时钟/控制台输出；同输入必得同输出字节/同解码结果。
//! 本模块是文件 I/O 层，允许堆分配，不适用实时回调铁律（无音频设备接触）。

/// 标准 WAV 头部大小：12(RIFF) + 24(fmt) + 8(data header)。
pub const HEADER_SIZE: usize = 44;

const RIFF_MAGIC: u32 = 0x5249_4646; // 'RIFF'
const WAVE_MAGIC: u32 = 0x5741_5645; // 'WAVE'
const FMT_CHUNK: u32 = 0x666d_7420; // 'fmt '
const DATA_CHUNK: u32 = 0x6461_7461; // 'data'

/// 复刻 JS `Math.round` 的半值向 +∞ 舍入（Rust `f64::round` 为半值远离零，
/// `(-0.5).round() == -1` 而 `Math.round(-0.5) == -0`）。
///
/// 本模块量化值域 `[-32767, 32767]` 内 `x + 0.5` 精确可表（f64 尾码余量充足），
/// 故 `Math.round(x) == (x + 0.5).floor()` 逐位成立；`-0.5` 走 `floor(+0) = +0`，
/// 与 JS 的 `-0` 经 `ToInt32` 后字节表达一致（均为 00 00）。NaN 保持 NaN
/// （由调用方按 JS `ToInt32(NaN) == 0` 落 0）。
fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

/// 编码位深（TS `WavEncodeOptions['bitDepth']` 的枚举化：16=PCM / 32=IEEE float）。
/// 以类型收窄替代 TS 的运行时校验（`bitDepth must be 16 or 32` 在 Rust 端不可表达）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WavBitDepth {
    /// 16-bit 有符号 PCM（formatTag=1，TS 默认）。
    Pcm16,
    /// 32-bit IEEE-754 float（formatTag=3）。
    Float32,
}

/// 编码选项（镜像 TS `WavEncodeOptions`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavEncodeOptions {
    /// 位深；缺省 = [`WavBitDepth::Pcm16`]（对齐 TS `opts?.bitDepth ?? 16`）。
    pub bit_depth: WavBitDepth,
}

impl Default for WavEncodeOptions {
    fn default() -> Self {
        Self { bit_depth: WavBitDepth::Pcm16 }
    }
}

/// 解码结果（镜像 TS `WavDecodeResult`；声道为非交叠 f32 向量，可直接进引擎链）。
#[derive(Debug, Clone, PartialEq)]
pub struct WavData {
    /// 采样率 Hz（头部原样透传，解码端不做范围校验——与 TS 一致）。
    pub sample_rate: u32,
    /// 非交叠声道样本（每声道 `frames` 个）。
    pub channels: Vec<Vec<f32>>,
    /// 位深（16 或 32）。
    pub bit_depth: u16,
}

/// 编码多通道音频为 WAV 字节流（TS `encodeWav` 的逐字节移植）。
///
/// - `channels`：非交叠声道（声道数 ≥1，各声道等长；0 帧合法 → 仅 44 字节头）；
/// - `sample_rate`：采样率 Hz（u32 入参收窄了 TS 的 number；`0` 即 TS 的
///   `invalid sampleRate` 分支，负数/非有限不可表达）；
/// - 输出布局：44 字节头（大端字段，见模块注释）+ 交叠样本（小端）。
///
/// 错误消息与 TS `encodeWav` 逐字一致。
pub fn encode_wav(channels: &[&[f32]], sample_rate: u32, opts: &WavEncodeOptions) -> Result<Vec<u8>, String> {
    // TS L50: if (!channels || channels.length < 1) throw …
    if channels.is_empty() {
        return Err("encodeWav: at least one channel required".to_string());
    }
    let cc = channels.len();
    // TS L52: frames = channels[0].length（以第 0 声道为基准）
    let frames = channels[0].len();
    // TS L53–55: 其余声道必须等长
    for ch in &channels[1..] {
        if ch.len() != frames {
            return Err("encodeWav: all channels must have equal length".to_string());
        }
    }
    // TS L56: !Number.isFinite(sampleRate) || sampleRate <= 0 → invalid（u32 下仅剩 0）
    if sample_rate == 0 {
        return Err("encodeWav: invalid sampleRate".to_string());
    }
    // TS L57–58: 位深由类型收窄（非法位深在 Rust 端不可表达）
    let (format_tag, bits, bytes_per_sample): (u16, u16, u64) = match opts.bit_depth {
        WavBitDepth::Pcm16 => (1, 16, 2),
        WavBitDepth::Float32 => (3, 32, 4),
    };
    // TS L59–63：blockAlign / dataSize / bufferSize（f64 乘法 → u64 乘法，
    // 头部字段写出走 ToUint32 模 2^32 环绕，与 TS writeU32 的 `>>> 0` 一致）
    let block_align: u64 = cc as u64 * bytes_per_sample;
    let data_size: u64 = frames as u64 * block_align;
    let buffer_size: u64 = HEADER_SIZE as u64 + data_size;

    let mut out = vec![0u8; buffer_size as usize];

    // RIFF header（TS L69–72）
    write_u32_be(&mut out, 0, RIFF_MAGIC);
    write_u32_be(&mut out, 4, ((buffer_size - 8) & 0xffff_ffff) as u32); // chunk size
    write_u32_be(&mut out, 8, WAVE_MAGIC);

    // fmt chunk（TS L74–82）
    write_u32_be(&mut out, 12, FMT_CHUNK);
    write_u32_be(&mut out, 16, 16); // fmt chunk size
    write_u16_be(&mut out, 20, format_tag);
    write_u16_be(&mut out, 22, cc as u16); // TS writeU16 的 & 0xffff 掩码 = 模 2^16 截断
    write_u32_be(&mut out, 24, sample_rate);
    write_u32_be(&mut out, 28, ((sample_rate as u64 * block_align) & 0xffff_ffff) as u32); // byte rate
    write_u16_be(&mut out, 32, (block_align & 0xffff) as u16);
    write_u16_be(&mut out, 34, bits);

    // data chunk header（TS L85–86）
    write_u32_be(&mut out, 36, DATA_CHUNK);
    write_u32_be(&mut out, 40, (data_size & 0xffff_ffff) as u32);

    // 交叠写入样本（TS L89–111）
    let mut off = HEADER_SIZE;
    match opts.bit_depth {
        WavBitDepth::Pcm16 => {
            for i in 0..frames {
                for ch in channels {
                    let s = f64::from(ch[i]);
                    // TS L94–96：钳制到 [-1, 1]（NaN 两比较皆 false → 保持 NaN）
                    let s = if s > 1.0 { 1.0 } else if s < -1.0 { -1.0 } else { s };
                    // TS L97：Math.round(s * 32767)；NaN 经 ToInt32 落 0
                    let v = js_round(s * 32767.0);
                    let v_i16: i16 = if v.is_nan() { 0 } else { v as i16 };
                    // TS L99–100：小端有符号 16-bit（v & 0xff / (v >> 8) & 0xff 的补码语义）
                    out[off..off + 2].copy_from_slice(&v_i16.to_le_bytes());
                    off += 2;
                }
            }
        }
        WavBitDepth::Float32 => {
            for i in 0..frames {
                for ch in channels {
                    // TS L107：view.setFloat32(off, v, true) —— f32 位模式原样小端
                    out[off..off + 4].copy_from_slice(&ch[i].to_le_bytes());
                    off += 4;
                }
            }
        }
    }

    Ok(out)
}

/// 解码 WAV 字节流为非交叠 f32 声道（TS `decodeWav` 的逐语义移植）。
///
/// 扫描 chunk（跳过未知 chunk 如 LIST/INFO，chunk 按 2 字节对齐推进），取
/// fmt 与 data；`dataLen` 声明值可大于实际可用字节（按可用帧数截断，与 TS 一致）。
/// 畸形输入返回 `Err`，消息与 node 实测抛错逐字一致（含 DataView 越界的
/// RangeError 原话）。
pub fn decode_wav(bytes: &[u8]) -> Result<WavData, String> {
    // TS L124
    if bytes.len() < HEADER_SIZE {
        return Err("decodeWav: file too short (<44 bytes)".to_string());
    }
    // TS L128–129：RIFF / WAVE 魔数（四字符按大端 u32 比较 = ASCII 原文）
    if read_u32_be(bytes, 0)? != RIFF_MAGIC {
        return Err("decodeWav: bad RIFF magic".to_string());
    }
    if read_u32_be(bytes, 8)? != WAVE_MAGIC {
        return Err("decodeWav: bad WAVE magic".to_string());
    }

    // TS L131–157：扫描 chunk（fmt 与 data；跳过未知 chunk，data 处 break）
    let mut off: usize = 12;
    let mut format_tag: u16 = 0;
    let mut channels: u16 = 0;
    let mut sample_rate: u32 = 0;
    let mut bits_per_sample: u16 = 0;
    let mut data_off: Option<usize> = None;
    let mut data_len: u32 = 0;

    while off + 8 <= bytes.len() {
        let id = read_u32_be(bytes, off)?;
        let size = read_u32_be(bytes, off + 4)?;
        let body = off + 8;
        if id == FMT_CHUNK {
            // TS L145
            if size < 16 {
                return Err("decodeWav: fmt chunk too small".to_string());
            }
            format_tag = read_u16_be(bytes, body)?;
            channels = read_u16_be(bytes, body + 2)?;
            sample_rate = read_u32_be(bytes, body + 4)?;
            bits_per_sample = read_u16_be(bytes, body + 14)?;
        } else if id == DATA_CHUNK {
            data_off = Some(body);
            data_len = size;
            break; // data 之后通常无其他需要解析的 chunk
        }
        // TS L156：chunk 对齐到偶数（size 为 u32，usize 加法在 64 位下无溢出）
        off = body + size as usize + (size % 2) as usize;
    }

    // TS L159–165：校验顺序与 TS 一致（formatTag=0 与"未见 fmt"不可区分）
    if format_tag == 0 {
        return Err("decodeWav: missing fmt chunk".to_string());
    }
    let data_off = match data_off {
        Some(o) => o,
        None => return Err("decodeWav: missing data chunk".to_string()),
    };
    if channels < 1 {
        return Err("decodeWav: channel count must be >= 1".to_string());
    }
    if bits_per_sample != 16 && bits_per_sample != 32 {
        return Err(format!("decodeWav: unsupported bit depth {}", bits_per_sample));
    }
    if format_tag == 1 && bits_per_sample != 16 {
        return Err("decodeWav: PCM format requires 16-bit".to_string());
    }
    if format_tag == 3 && bits_per_sample != 32 {
        return Err("decodeWav: float format requires 32-bit".to_string());
    }
    if format_tag != 1 && format_tag != 3 {
        return Err(format!("decodeWav: unsupported format tag {}", format_tag));
    }

    // TS L167–169：blockAlign 与 data 长度对齐校验
    let bytes_per_sample: usize = (bits_per_sample / 8) as usize;
    let block_align: usize = channels as usize * bytes_per_sample;
    if data_len as usize % block_align != 0 {
        return Err("decodeWav: data length not aligned to block size".to_string());
    }

    // TS L172–173：实际可用帧数（dataLen 可能超过实际 buffer，取较小者）
    let available = (data_len as usize).min(bytes.len() - data_off);
    let frames = available / block_align;

    // TS L175–195：按位深解出非交叠声道
    let mut out: Vec<Vec<f32>> = (0..channels).map(|_| Vec::with_capacity(frames)).collect();
    match bits_per_sample {
        16 => {
            let mut p = data_off;
            for _ in 0..frames {
                for ch in &mut out {
                    let v = i16::from_le_bytes([bytes[p], bytes[p + 1]]);
                    // TS L183：v / 32767（f64 除法后存入 Float32Array 收窄到 f32）
                    ch.push((f64::from(v) / 32767.0) as f32);
                    p += 2;
                }
            }
        }
        _ => {
            let mut p = data_off;
            for _ in 0..frames {
                for ch in &mut out {
                    ch.push(f32::from_le_bytes([bytes[p], bytes[p + 1], bytes[p + 2], bytes[p + 3]]));
                    p += 4;
                }
            }
        }
    }

    Ok(WavData { sample_rate, channels: out, bit_depth: bits_per_sample })
}

// ---------------------------------------------------------------------------
// 大端字段读写（对齐 TS readU32/writeU32/writeU16 的 `littleEndian=false`）
// ---------------------------------------------------------------------------

fn write_u32_be(out: &mut [u8], off: usize, v: u32) {
    out[off..off + 4].copy_from_slice(&v.to_be_bytes());
}

fn write_u16_be(out: &mut [u8], off: usize, v: u16) {
    out[off..off + 2].copy_from_slice(&v.to_be_bytes());
}

fn read_u32_be(bytes: &[u8], off: usize) -> Result<u32, String> {
    if off + 4 > bytes.len() {
        // 对齐 JS DataView 越界读取抛出的 RangeError 原话（node 实测）
        return Err("Offset is outside the bounds of the DataView".to_string());
    }
    Ok(u32::from_be_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]))
}

fn read_u16_be(bytes: &[u8], off: usize) -> Result<u16, String> {
    if off + 2 > bytes.len() {
        return Err("Offset is outside the bounds of the DataView".to_string());
    }
    Ok(u16::from_be_bytes([bytes[off], bytes[off + 1]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// golden 套件：由 node（esbuild 打包 TS 支线 src/io/wav.ts）生成并冻结。
    /// 结构：{ enc: [...], dec: [...] }；
    /// - enc case：{ name, channels: [[f32 位 hex]...], sampleRate, bitDepth: 16|32|null,
    ///   kind: "ok"|"err", want: 整文件 hex | 抛错消息 }；
    /// - dec case：{ name, input: 整文件 hex, kind: "ok"|"err",
    ///   want: { sampleRate, bitDepth, channels: [[f32 位 hex]...] } | 抛错消息 }。
    /// f32 一律以 8 位十六进制位模式传递，杜绝十进制解析歧义。
    const GOLDENS_JSON: &str = r##"{"enc":[{"name":"enc_2ch_pcm16_sine","channels":[["00000000","3ec5e757","3f175d76","3f0496c6","3e4dd115","be57856e","bf05dfc1","bf16e79c","bec1ecd4","3c254991","3ec9d387","3f17c85b","3f034432","3e440dd5","be612a2f","bf071f0b"],["3ecccccd","3ec3a725","3ea9075b","3e7e9c88","3e146bf7","3ce7caaa","bdba1fbc","be4ec8f3","be9704ab","beb9276b","becac01e","beca3c61","beb7a7f8","be94abc2","be48cf93","bdacaf15"]],"sampleRate":48000,"bitDepth":16,"kind":"ok","want":"524946460000006457415645666d742000000010000100020000bb800002ee00000400106461746100000040000033337931e930ae4b422a4b42d31fba198d1210e59f0311bd5ef48db427e685cf3fda4b01b7d1743250cde44b71cda24116d28218d5dadbe3e6e671bc35f5"},{"name":"enc_2ch_f32_sine","channels":[["00000000","3ec5e757","3f175d76","3f0496c6","3e4dd115","be57856e","bf05dfc1","bf16e79c","bec1ecd4","3c254991","3ec9d387","3f17c85b","3f034432","3e440dd5","be612a2f","bf071f0b"],["3ecccccd","3ec3a725","3ea9075b","3e7e9c88","3e146bf7","3ce7caaa","bdba1fbc","be4ec8f3","be9704ab","beb9276b","becac01e","beca3c61","beb7a7f8","be94abc2","be48cf93","bdacaf15"]],"sampleRate":48000,"bitDepth":32,"kind":"ok","want":"52494646000000a457415645666d742000000010000300020000bb800005dc0000080020646174610000008000000000cdcccc3e57e7c53e25a7c33e765d173f5b07a93ec696043f889c7e3e15d14d3ef76b143e6e8557beaacae73cc1df05bfbc1fbabd9ce716bff3c84ebed4ecc1beab0497be9149253c6b27b9be87d3c93e1ec0cabe5bc8173f613ccabe3244033ff8a7b7bed50d443ec2ab94be2f2a61be93cf48be0b1f07bf15afacbd"},{"name":"enc_1ch_pcm16_default","channels":[["00000000","3f000000","bf000000","3e800000","be800000","00000000","3f800000","bf800000"]],"sampleRate":44100,"bitDepth":null,"kind":"ok","want":"524946460000003457415645666d742000000010000100010000ac44000158880002001064617461000000100000004001c0002000e00000ff7f0180"},{"name":"enc_1ch_pcm16_edge","channels":[["40000000","c0000000","3fc00000","3f7ffffe","bf800001","3f800000","bf800000","37800100","b7800100","b7000100","7fc00000","7f800000","ff800000","3f7ffff7","bf7ffff7"]],"sampleRate":8000,"bitDepth":16,"kind":"ok","want":"524946460000004257415645666d7420000000100001000100001f4000003e8000020010646174610000001eff7f0180ff7fff7f0180ff7f01800000000000000000ff7f0180ff7f0180"},{"name":"enc_odd_2ch_pcm16","channels":[["3dcccccd","bf666666","3f333333"],["3ea8f5c3","3f0ccccd","bf733333"]],"sampleRate":22050,"bitDepth":16,"kind":"ok","want":"524946460000003057415645666d74200000001000010002000056220001588800040010646174610000000ccd0c3d2ace8c664699596786"},{"name":"enc_odd_2ch_f32","channels":[["3dcccccd","bf666666","3f333333"],["3ea8f5c3","3f0ccccd","bf733333"]],"sampleRate":22050,"bitDepth":32,"kind":"ok","want":"524946460000003c57415645666d74200000001000030002000056220002b110000800206461746100000018cdcccc3dc3f5a83e666666bfcdcc0c3f3333333f333373bf"},{"name":"enc_6ch_f32_51","channels":[["3dcccccd","3dcccccd","3dcccccd","3dcccccd"],["3e4ccccd","3e4ccccd","3e4ccccd","3e4ccccd"],["3e99999a","3e99999a","3e99999a","3e99999a"],["3ecccccd","3ecccccd","3ecccccd","3ecccccd"],["3f000000","3f000000","3f000000","3f000000"],["3f19999a","3f19999a","3f19999a","3f19999a"]],"sampleRate":48000,"bitDepth":32,"kind":"ok","want":"524946460000008457415645666d742000000010000300060000bb8000119400001800206461746100000060cdcccc3dcdcc4c3e9a99993ecdcccc3e0000003f9a99193fcdcccc3dcdcc4c3e9a99993ecdcccc3e0000003f9a99193fcdcccc3dcdcc4c3e9a99993ecdcccc3e0000003f9a99193fcdcccc3dcdcc4c3e9a99993ecdcccc3e0000003f9a99193f"},{"name":"enc_zero_frames_1ch","channels":[[]],"sampleRate":48000,"bitDepth":null,"kind":"ok","want":"524946460000002457415645666d742000000010000100010000bb8000017700000200106461746100000000"},{"name":"enc_zero_frames_2ch_f32","channels":[[],[]],"sampleRate":48000,"bitDepth":32,"kind":"ok","want":"524946460000002457415645666d742000000010000300020000bb800005dc00000800206461746100000000"},{"name":"enc_f32_special","channels":[["7fc00000","00000001","80000001","80000000","7f7fc99e","00800000","ff7fffff"]],"sampleRate":48000,"bitDepth":32,"kind":"ok","want":"524946460000004057415645666d742000000010000300010000bb800002ee0000040020646174610000001c0000c07f0100000001000080000000809ec97f7f00008000ffff7fff"},{"name":"enc_err_empty_channels","channels":[],"sampleRate":48000,"bitDepth":16,"kind":"err","want":"encodeWav: at least one channel required"},{"name":"enc_err_unequal_lengths","channels":[["3f800000","40000000","40400000"],["3f800000","40000000"]],"sampleRate":48000,"bitDepth":16,"kind":"err","want":"encodeWav: all channels must have equal length"},{"name":"enc_err_sample_rate_zero","channels":[["3f000000"]],"sampleRate":0,"bitDepth":16,"kind":"err","want":"encodeWav: invalid sampleRate"}],"dec":[{"name":"err_too_short","input":"0000000000000000000000000000000000000000","kind":"err","want":"decodeWav: file too short (<44 bytes)"},{"name":"err_too_short_43","input":"524946460000002657415645666d742000000010000100010000bb80000000000000001064617461000000","kind":"err","want":"decodeWav: file too short (<44 bytes)"},{"name":"err_bad_riff_zero","input":"0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000","kind":"err","want":"decodeWav: bad RIFF magic"},{"name":"err_bad_riff_xfer","input":"584645520000002857415645666d742000000010000100010000bb800000000000000010646174610000000400000000","kind":"err","want":"decodeWav: bad RIFF magic"},{"name":"err_bad_wave","input":"524946460000002858585858666d742000000010000100010000bb800000000000000010646174610000000400000000","kind":"err","want":"decodeWav: bad WAVE magic"},{"name":"err_missing_fmt","input":"524946460000002857415645646174610000000400000000000000000000000000000000000000000000000000000000","kind":"err","want":"decodeWav: missing fmt chunk"},{"name":"err_missing_data","input":"524946460000002857415645666d742000000010000100010000bb800000000000000010000000000000000000000000","kind":"err","want":"decodeWav: missing data chunk"},{"name":"err_zero_channels","input":"524946460000002457415645666d742000000010000100000000bb8000000000000000106461746100000000","kind":"err","want":"decodeWav: channel count must be >= 1"},{"name":"err_bit_depth_24","input":"524946460000002a57415645666d742000000010000100010000bb8000000000000000186461746100000006000000000000","kind":"err","want":"decodeWav: unsupported bit depth 24"},{"name":"err_pcm_32bit","input":"524946460000002c57415645666d742000000010000100010000bb80000000000000002064617461000000080000000000000000","kind":"err","want":"decodeWav: PCM format requires 16-bit"},{"name":"err_float_16bit","input":"524946460000002857415645666d742000000010000300010000bb800000000000000010646174610000000400000000","kind":"err","want":"decodeWav: float format requires 32-bit"},{"name":"err_tag_2","input":"524946460000002857415645666d742000000010000200010000bb800000000000000010646174610000000400000000","kind":"err","want":"decodeWav: unsupported format tag 2"},{"name":"err_tag_0","input":"524946460000002857415645666d742000000010000000010000bb800000000000000010646174610000000400000000","kind":"err","want":"decodeWav: missing fmt chunk"},{"name":"err_data_not_aligned","input":"524946460000002a57415645666d742000000010000100020000bb8000000000000000106461746100000006000000000000","kind":"err","want":"decodeWav: data length not aligned to block size"},{"name":"err_fmt_too_small","input":"524946460000002457415645666d74200000000c000100010000bb8000000000646174610000000400000000","kind":"err","want":"decodeWav: fmt chunk too small"},{"name":"err_truncated_fmt_body_at_bits","input":"5249464600000030574156454a554e4b0000000c000000000000000000000000666d742000000010000100020000bb8000000000","kind":"err","want":"Offset is outside the bounds of the DataView"},{"name":"err_truncated_fmt_body_at_rate","input":"5249464600000030574156454a554e4b0000000c000000000000000000000000666d74200000001000010002","kind":"err","want":"Offset is outside the bounds of the DataView"},{"name":"ok_data_declared_larger","input":"524946460000002c57415645666d742000000010000100010000bb80000000000000001064617461000000200000ff3f00c00180","kind":"ok","want":{"sampleRate":48000,"bitDepth":16,"channels":[["00000000","3efffe00","bf000100","bf800000"]]}},{"name":"ok_data_declared_smaller","input":"524946460000006457415645666d74200000001000030002000177000000000000000020646174610000000800000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000","kind":"ok","want":{"sampleRate":96000,"bitDepth":32,"channels":[["00000000"],["00000000"]]}},{"name":"ok_list_chunk_skip","input":"524946460000004857415645666d742000000010000100020000bb8000000000000000104c49535400000007494e464f616263004a554e4b0000000378797a006461746100000008ff7f018000000100","kind":"ok","want":{"sampleRate":48000,"bitDepth":16,"channels":[["3f800000","00000000"],["bf800000","38000100"]]}},{"name":"ok_fmt_size_18","input":"524946460000002a57415645666d742000000012000100010000ac44000000000000001000026461746100000004004000c0","kind":"ok","want":{"sampleRate":44100,"bitDepth":16,"channels":[["3f000100","bf000100"]]}},{"name":"ok_two_fmt_chunks","input":"524946460000004457415645666d742000000010000100010000bb800000000000000010666d7420000000100003000200017700000000000000002064617461000000080000000000000000","kind":"ok","want":{"sampleRate":96000,"bitDepth":32,"channels":[["00000000"],["00000000"]]}},{"name":"ok_roundtrip_from_encode_f32","input":"52494646000000a457415645666d742000000010000300020000bb800005dc0000080020646174610000008000000000cdcccc3e57e7c53e25a7c33e765d173f5b07a93ec696043f889c7e3e15d14d3ef76b143e6e8557beaacae73cc1df05bfbc1fbabd9ce716bff3c84ebed4ecc1beab0497be9149253c6b27b9be87d3c93e1ec0cabe5bc8173f613ccabe3244033ff8a7b7bed50d443ec2ab94be2f2a61be93cf48be0b1f07bf15afacbd","kind":"ok","want":{"sampleRate":48000,"bitDepth":32,"channels":[["00000000","3ec5e757","3f175d76","3f0496c6","3e4dd115","be57856e","bf05dfc1","bf16e79c","bec1ecd4","3c254991","3ec9d387","3f17c85b","3f034432","3e440dd5","be612a2f","bf071f0b"],["3ecccccd","3ec3a725","3ea9075b","3e7e9c88","3e146bf7","3ce7caaa","bdba1fbc","be4ec8f3","be9704ab","beb9276b","becac01e","beca3c61","beb7a7f8","be94abc2","be48cf93","bdacaf15"]]}},{"name":"ok_roundtrip_from_encode_pcm16","input":"524946460000003457415645666d742000000010000100010000ac44000158880002001064617461000000100000004001c0002000e00000ff7f0180","kind":"ok","want":{"sampleRate":44100,"bitDepth":16,"channels":[["00000000","3f000100","befffe00","3e800100","be800100","00000000","3f800000","bf800000"]]}}]}"##;

    fn goldens() -> Value {
        serde_json::from_str(GOLDENS_JSON).expect("golden JSON 必须可解析")
    }

    fn hex_to_bytes(hex: &str) -> Vec<u8> {
        assert_eq!(hex.len() % 2, 0, "hex 长度必须为偶数");
        (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex 字符合法"))
            .collect()
    }

    fn hex_to_f32_bits(hex: &str) -> f32 {
        f32::from_bits(u32::from_str_radix(hex, 16).expect("f32 位 hex 合法"))
    }

    fn f32_bits(v: f32) -> u32 {
        v.to_bits()
    }

    fn bytes_to_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }

    fn build_channels(case: &Value) -> Vec<Vec<f32>> {
        case["channels"]
            .as_array()
            .expect("channels 数组")
            .iter()
            .map(|c| c.as_array().unwrap().iter().map(|h| hex_to_f32_bits(h.as_str().unwrap())).collect())
            .collect()
    }

    fn opts_of(case: &Value) -> WavEncodeOptions {
        match &case["bitDepth"] {
            Value::Number(n) if n.as_u64() == Some(32) => WavEncodeOptions { bit_depth: WavBitDepth::Float32 },
            Value::Number(n) if n.as_u64() == Some(16) => WavEncodeOptions { bit_depth: WavBitDepth::Pcm16 },
            Value::Null => WavEncodeOptions::default(), // TS 不传 opts → 默认 16
            other => panic!("非法 bitDepth golden 字段: {:?}", other),
        }
    }

    // ------------------------- golden 对拍：编码（逐字节） -------------------------

    #[test]
    fn golden_encode_与node输出逐字节一致() {
        for case in goldens()["enc"].as_array().expect("enc 数组") {
            let name = case["name"].as_str().unwrap();
            let chans = build_channels(case);
            let refs: Vec<&[f32]> = chans.iter().map(|v| v.as_slice()).collect();
            let sr = case["sampleRate"].as_u64().expect("sampleRate") as u32;
            let result = encode_wav(&refs, sr, &opts_of(case));
            match case["kind"].as_str().unwrap() {
                "ok" => {
                    let want = hex_to_bytes(case["want"].as_str().unwrap());
                    assert_eq!(result.as_ref(), Ok(&want), "case {} 编码字节不一致", name);
                }
                "err" => {
                    assert_eq!(result.unwrap_err(), case["want"].as_str().unwrap(), "case {} 错误消息不一致", name);
                }
                other => panic!("非法 kind: {}", other),
            }
        }
    }

    // ------------------------- golden 对拍：解码 -------------------------

    #[test]
    fn golden_decode_ok_与node结果一致() {
        for case in goldens()["dec"].as_array().expect("dec 数组") {
            if case["kind"].as_str().unwrap() != "ok" {
                continue;
            }
            let name = case["name"].as_str().unwrap();
            let input = hex_to_bytes(case["input"].as_str().unwrap());
            let got = decode_wav(&input).unwrap_or_else(|e| panic!("case {} 应解码成功，实际 Err({})", name, e));
            let want = &case["want"];
            assert_eq!(got.sample_rate, want["sampleRate"].as_u64().unwrap() as u32, "case {} 采样率不一致", name);
            assert_eq!(got.bit_depth, want["bitDepth"].as_u64().unwrap() as u16, "case {} 位深不一致", name);
            let want_chs = want["channels"].as_array().unwrap();
            assert_eq!(got.channels.len(), want_chs.len(), "case {} 声道数不一致", name);
            for (c, (got_ch, want_ch)) in got.channels.iter().zip(want_chs).enumerate() {
                let want_samples: Vec<f32> =
                    want_ch.as_array().unwrap().iter().map(|h| hex_to_f32_bits(h.as_str().unwrap())).collect();
                assert_eq!(got_ch.len(), want_samples.len(), "case {} 声道 {} 帧数不一致", name, c);
                for (i, (g, w)) in got_ch.iter().zip(want_samples.iter()).enumerate() {
                    // 位模式精确比较（NaN 亦按位；golden 输入仅含 canonical NaN）
                    assert_eq!(f32_bits(*g), f32_bits(*w), "case {} 声道 {} 样本 {} 位模式不一致", name, c, i);
                }
            }
        }
    }

    #[test]
    fn golden_decode_err_错误消息与node逐字一致() {
        for case in goldens()["dec"].as_array().expect("dec 数组") {
            if case["kind"].as_str().unwrap() != "err" {
                continue;
            }
            let name = case["name"].as_str().unwrap();
            let input = hex_to_bytes(case["input"].as_str().unwrap());
            let msg = decode_wav(&input).err().unwrap_or_else(|| panic!("case {} 应抛错，实际成功", name));
            assert_eq!(msg, case["want"].as_str().unwrap(), "case {} 错误消息不一致", name);
        }
    }

    // ------------------------- 行为对拍（镜像 test/wav.test.ts） -------------------------

    #[test]
    fn float32_往返_单声道电平与长度一致() {
        // 镜像 TS 测试 1：单声道 8 样本 f32 往返逐位一致
        let mono: Vec<f32> = vec![0.0, 0.5, -0.5, 0.25, -0.25, 0.0, 1.0, -1.0];
        let buf = encode_wav(&[&mono], 48000, &WavEncodeOptions { bit_depth: WavBitDepth::Float32 }).unwrap();
        let res = decode_wav(&buf).unwrap();
        assert_eq!(res.bit_depth, 32);
        assert_eq!(res.sample_rate, 48000);
        assert_eq!(res.channels.len(), 1);
        assert_eq!(res.channels[0].len(), 8);
        for (g, w) in res.channels[0].iter().zip(mono.iter()) {
            assert_eq!(f32_bits(*g), f32_bits(*w));
        }
    }

    #[test]
    fn float32_特殊位模式往返保留() {
        // denormal / -0 / Infinity 位模式全程保留（canonical NaN 按位一致）
        let samples = [0.0_f32, 1e-45, -1e-45, -0.0, f32::INFINITY, f32::NEG_INFINITY, f32::MIN, f32::MAX];
        let buf = encode_wav(&[&samples], 48000, &WavEncodeOptions { bit_depth: WavBitDepth::Float32 }).unwrap();
        let res = decode_wav(&buf).unwrap();
        for (g, w) in res.channels[0].iter().zip(samples.iter()) {
            assert_eq!(f32_bits(*g), f32_bits(*w));
        }
    }

    #[test]
    fn pcm16_往返_量化误差在容忍范围内() {
        // 镜像 TS 测试 2：256 点正弦 ×0.8，量化步长 ~3.05e-5，半步误差 ≈ 1.5e-5
        let fs = 44100_u32;
        let ch: Vec<f32> = (0..256).map(|i| (f64::from(i) * 0.1).sin() * 0.8).map(|x| x as f32).collect();
        let buf = encode_wav(&[&ch], fs, &WavEncodeOptions::default()).unwrap();
        let res = decode_wav(&buf).unwrap();
        assert_eq!(res.bit_depth, 16);
        assert_eq!(res.channels[0].len(), 256);
        for (g, w) in res.channels[0].iter().zip(ch.iter()) {
            assert!((g - w).abs() < 1.7e-5, "|{} - {}| 超出量化容忍", g, w);
        }
    }

    #[test]
    fn js_round_半值向正无穷与NaN传播() {
        // Math.round(16383.5) = 16384、Math.round(-16383.5) = -16383（向 +∞）；
        // Rust f64::round(-16383.5) = -16384 会得到不同字节，故必须走 js_round
        assert_eq!(js_round(16383.5), 16384.0);
        assert_eq!(js_round(-16383.5), -16383.0);
        assert_eq!(js_round(0.5), 1.0);
        assert_eq!(js_round(-0.5), 0.0); // JS 得 -0，经 ToInt32 后字节同为 00 00
        assert!(js_round(f64::NAN).is_nan());
    }

    #[test]
    fn pcm16_钳制超量程样本并按32767标度量化() {
        // 镜像 TS 测试 3 + 标度语义：±1 → ±32767（非 ±32768）；0.25 → round(8191.75) = 8192
        let ch: Vec<f32> = [2.0_f32, -2.0, 1.5, 0.25, -0.25, 1.0, -1.0].into();
        let buf = encode_wav(&[&ch], 48000, &WavEncodeOptions::default()).unwrap();
        // 量化字节（小端 i16）：32767, -32767, 32767, 8192, -8192, 32767, -32767
        assert_eq!(&buf[44..46], &[0xff, 0x7f]); // 2.0 → clamp 1 → 32767
        assert_eq!(&buf[46..48], &[0x01, 0x80]); // -2.0 → -32767（非 -32768 = 00 80）
        assert_eq!(&buf[48..50], &[0xff, 0x7f]);
        assert_eq!(&buf[50..52], &[0x00, 0x20]); // round(0.25 × 32767) = round(8191.75) = 8192
        assert_eq!(&buf[52..54], &[0x00, 0xe0]); // -8192
        let res = decode_wav(&buf).unwrap();
        let expect: [f32; 7] = [
            1.0,
            -1.0,
            1.0,
            (8192.0_f64 / 32767.0) as f32,
            (-8192.0_f64 / 32767.0) as f32,
            1.0,
            -1.0,
        ];
        for (g, w) in res.channels[0].iter().zip(expect.iter()) {
            assert!((g - w).abs() < 1e-6, "{} != {}", g, w);
        }
    }

    #[test]
    fn pcm16_NaN样本量化为0() {
        // TS：NaN 两钳制分支皆不命中 → Math.round(NaN)=NaN → ToInt32(NaN)=0 → 00 00
        let ch: Vec<f32> = vec![f32::NAN, f32::INFINITY, f32::NEG_INFINITY];
        let buf = encode_wav(&[&ch], 48000, &WavEncodeOptions::default()).unwrap();
        assert_eq!(&buf[44..50], &[0x00, 0x00, 0xff, 0x7f, 0x01, 0x80]);
    }

    #[test]
    fn 多通道_5_1六声道_交叠写与非交叠读() {
        // 镜像 TS 测试 4：6 通道 × 128 帧
        let n = 128;
        let channels: Vec<Vec<f32>> = (0..6).map(|c| vec![(c as f32 + 1.0) * 0.1; n]).collect();
        let refs: Vec<&[f32]> = channels.iter().map(|v| v.as_slice()).collect();
        let buf = encode_wav(&refs, 48000, &WavEncodeOptions { bit_depth: WavBitDepth::Float32 }).unwrap();
        let res = decode_wav(&buf).unwrap();
        assert_eq!(res.channels.len(), 6);
        for (c, ch) in res.channels.iter().enumerate() {
            assert_eq!(ch.len(), n);
            assert_eq!(f32_bits(ch[0]), f32_bits((c as f32 + 1.0) * 0.1));
        }
        // data 区按帧交叠：第 0 帧 = [ch0[0], ch1[0], …]
        let mut first_frame = [0u8; 24];
        first_frame.copy_from_slice(&buf[44..68]);
        for c in 0..6 {
            let v = f32::from_le_bytes(first_frame[c * 4..c * 4 + 4].try_into().unwrap());
            assert_eq!(f32_bits(v), f32_bits((c as f32 + 1.0) * 0.1));
        }
    }

    #[test]
    fn 默认位深为16位pcm() {
        // 镜像 TS 测试 9：不传 opts（Default）→ fmt 中 formatTag=1、bits=16
        let ch = vec![0.5_f32; 8];
        let buf = encode_wav(&[&ch], 48000, &WavEncodeOptions::default()).unwrap();
        assert_eq!(&buf[20..24], &[0x00, 0x01, 0x00, 0x01]); // formatTag=1, channels=1（大端）
        assert_eq!(&buf[34..36], &[0x00, 0x10]); // bitsPerSample=16（大端）
        let res = decode_wav(&buf).unwrap();
        assert_eq!(res.bit_depth, 16);
    }

    #[test]
    fn 头部字段为大端且data样本为小端() {
        // 双支线共同契约：头字段大端（44100 → 00 00 AC 44）、样本小端
        let ch = vec![0.25_f32];
        let buf = encode_wav(&[&ch], 44100, &WavEncodeOptions::default()).unwrap();
        assert_eq!(&buf[0..4], b"RIFF");
        assert_eq!(&buf[4..8], &38_u32.to_be_bytes()); // bufferSize(46) - 8
        assert_eq!(&buf[8..12], b"WAVE");
        assert_eq!(&buf[12..16], b"fmt ");
        assert_eq!(&buf[24..28], &44100_u32.to_be_bytes());
        assert_eq!(&buf[28..32], &88200_u32.to_be_bytes()); // byteRate = 44100 × 2
        assert_eq!(&buf[36..40], b"data");
        assert_eq!(&buf[40..44], &2_u32.to_be_bytes());
        assert_eq!(&buf[44..46], &8192_i16.to_le_bytes()); // round(0.25 × 32767) = 8192
        assert_eq!(buf.len(), 46);
    }

    #[test]
    fn 零帧音频产出仅头部的44字节且可解码() {
        let buf = encode_wav(&[&[] as &[f32]], 48000, &WavEncodeOptions::default()).unwrap();
        assert_eq!(buf.len(), 44);
        let res = decode_wav(&buf).unwrap();
        assert_eq!(res.channels.len(), 1);
        assert!(res.channels[0].is_empty());
        assert_eq!(res.sample_rate, 48000);
    }

    #[test]
    fn encode_参数校验错误消息与ts逐字一致() {
        let ch = vec![0.5_f32; 2];
        let ch2 = vec![0.5_f32; 3];
        assert_eq!(
            encode_wav(&[], 48000, &WavEncodeOptions::default()).unwrap_err(),
            "encodeWav: at least one channel required"
        );
        assert_eq!(
            encode_wav(&[&ch, &ch2], 48000, &WavEncodeOptions::default()).unwrap_err(),
            "encodeWav: all channels must have equal length"
        );
        assert_eq!(encode_wav(&[&ch], 0, &WavEncodeOptions::default()).unwrap_err(), "encodeWav: invalid sampleRate");
    }

    #[test]
    fn decode_容忍data声明长度与实际字节不符() {
        // 2 帧文件（data 区 4 字节）声明 dataLen=16（8 帧）→
        // available = min(16, 48-44) = 4 → 按 2 帧解码（不抛错，与 TS 一致）
        let ch = vec![0.5_f32; 2];
        let mut buf = encode_wav(&[&ch], 48000, &WavEncodeOptions::default()).unwrap();
        write_u32_be(&mut buf, 40, 16); // data size 字段（offset 40，大端）改为 16
        let res = decode_wav(&buf).unwrap();
        assert_eq!(res.channels[0].len(), 2);
        // dataLen 声明 1 帧而实际 2 帧 → 只取 1 帧（data 之后的尾部字节被忽略）
        let mut buf2 = encode_wav(&[&ch], 48000, &WavEncodeOptions::default()).unwrap();
        write_u32_be(&mut buf2, 40, 2);
        let res2 = decode_wav(&buf2).unwrap();
        assert_eq!(res2.channels[0].len(), 1);
        // PCM16 解码值 = 16384/32767（f64 除法后收窄），非精确 0.5
        let half_q = (16384.0_f64 / 32767.0) as f32;
        assert_eq!(f32_bits(res2.channels[0][0]), f32_bits(half_q));
    }

    #[test]
    fn decode_跳过未知chunk含奇数长度对齐() {
        // LIST（7 字节，奇数 → 补 1 对齐）与 JUNK 位于 fmt 与 data 之间，
        // 逐个跳过后仍能定位 data（若不按 (size % 2) 对齐会错位解析）
        let ch = vec![0.5_f32; 2];
        let base = encode_wav(&[&ch], 48000, &WavEncodeOptions::default()).unwrap();
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&0_u32.to_be_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(&base[12..36]); // fmt chunk（24 字节）
        for (id, body) in [(b"LIST" as &[u8], b"INFOabc" as &[u8]), (b"JUNK", b"xyz")] {
            out.extend_from_slice(id);
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            out.extend_from_slice(body);
            if body.len() % 2 == 1 {
                out.push(0); // 奇数长度 chunk 对齐补位
            }
        }
        out.extend_from_slice(b"data");
        out.extend_from_slice(&4_u32.to_be_bytes());
        out.extend_from_slice(&base[44..]); // 2 帧 PCM16
        let n = (out.len() - 8) as u32;
        out[4..8].copy_from_slice(&n.to_be_bytes());

        let res = decode_wav(&out).unwrap();
        assert_eq!(res.channels[0].len(), 2);
        // PCM16 解码值 = 16384/32767（f64 除法后收窄），非精确 0.5
        let half_q = (16384.0_f64 / 32767.0) as f32;
        assert_eq!(f32_bits(res.channels[0][0]), f32_bits(half_q));
    }

    #[test]
    fn decode_formatTag为0等同缺失fmt() {
        // golden（err_tag_0）实测 node：fmt 中 formatTag=0 与"未见 fmt"不可区分
        let ch = vec![0.0_f32; 2];
        let mut bad = encode_wav(&[&ch], 48000, &WavEncodeOptions::default()).unwrap();
        bad[20] = 0; // formatTag 高字节（大端 0x0001 → 0x0000）
        bad[21] = 0;
        assert_eq!(decode_wav(&bad).unwrap_err(), "decodeWav: missing fmt chunk");
    }

    #[test]
    fn pcm16解码走f64除法后收窄f32() {
        // TS：v / 32767 在 f64 完成后存入 Float32Array 收窄；位模式必须逐位等于
        // 该路径（f32 直除可能在末位产生不同位模式）
        for v in [1_i16, -1, 12345, -12345, 32767, -32767, 8192, -8192] {
            let buf = encode_wav(&[&[(v as f64 / 32767.0) as f32]], 48000, &WavEncodeOptions::default()).unwrap();
            let res = decode_wav(&buf).unwrap();
            let expected = (f64::from(v) / 32767.0) as f32;
            assert_eq!(f32_bits(res.channels[0][0]), f32_bits(expected), "v = {}", v);
        }
    }
}
