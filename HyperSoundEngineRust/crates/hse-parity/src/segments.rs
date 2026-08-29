//! `.f32` 数据文件的解码与平面切分。
//!
//! 布局契约（小端、非交错）：
//! - 流式（moduleKind 缺省/'stream'）四段：
//!   `[输入左 frames][输入右 frames][期望输出左 frames][期望输出右 frames]`；
//! - 计量型（moduleKind='meter'，specs/dsp/lufs-meter.md §三）两段：
//!   `[输入左 frames][输入右 frames]`（无期望输出段，文件总长 = 8 × frames 字节）。

use std::fs;
use std::path::Path;

/// 读取并解码整个 `.f32` 文件为小端 f32 序列。
pub fn decode_file(path: &Path) -> Result<Vec<f32>, String> {
    let bytes = fs::read(path).map_err(|err| format!("读取 {} 失败：{err}", path.display()))?;
    decode_f32_le(&bytes).ok_or_else(|| {
        format!(
            "{} 共 {} 字节，不是 4 的倍数，无法按小端 f32 解码",
            path.display(),
            bytes.len()
        )
    })
}

/// 小端字节流解码为 f32 序列；字节数不是 4 的倍数时返回 `None`。
/// 手写 little-endian 组装（`f32::from_le_bytes`），不引入额外依赖。
pub fn decode_f32_le(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.len() % 4 != 0 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|quad| f32::from_le_bytes([quad[0], quad[1], quad[2], quad[3]]))
            .collect(),
    )
}

/// 四段平面布局的一次切分视图。
pub struct PlanarSegments<'a> {
    pub input_left: &'a [f32],
    pub input_right: &'a [f32],
    pub expected_left: &'a [f32],
    pub expected_right: &'a [f32],
}

/// 按每声道帧数把总长 `4 * frames` 的序列切成四段；长度不符返回 `None`。
pub fn split_planar(data: &[f32], frames: usize) -> Option<PlanarSegments<'_>> {
    let total = frames.checked_mul(4)?;
    if data.len() != total {
        return None;
    }
    Some(PlanarSegments {
        input_left: &data[0..frames],
        input_right: &data[frames..frames * 2],
        expected_left: &data[frames * 2..frames * 3],
        expected_right: &data[frames * 3..frames * 4],
    })
}

/// 计量型两段平面布局的一次切分视图（无期望输出段——音频段不参与比对）。
pub struct MeterSegments<'a> {
    pub input_left: &'a [f32],
    pub input_right: &'a [f32],
}

/// 按每声道帧数把总长 `2 * frames` 的序列切成两段；长度不符返回 `None`。
pub fn split_meter(data: &[f32], frames: usize) -> Option<MeterSegments<'_>> {
    let total = frames.checked_mul(2)?;
    if data.len() != total {
        return None;
    }
    Some(MeterSegments {
        input_left: &data[0..frames],
        input_right: &data[frames..total],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内联组装一段小端 f32 字节流（不依赖外部编码库）。
    fn encode_f32_le(values: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(values.len() * 4);
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn 四段按声明顺序切开() {
        let frames = 3;
        let input_left = [0.125_f32, -1.5, 2.25];
        let input_right = [0.5_f32, -0.75, 3.5];
        let expected_left = [10.0_f32, -20.0, 30.0];
        let expected_right = [-40.0_f32, 50.0, 60.0];

        let mut all = Vec::new();
        all.extend_from_slice(&input_left);
        all.extend_from_slice(&input_right);
        all.extend_from_slice(&expected_left);
        all.extend_from_slice(&expected_right);

        let decoded = decode_f32_le(&encode_f32_le(&all)).expect("合法字节流必须可解码");
        assert_eq!(decoded.len(), 12);

        let segments = split_planar(&decoded, frames).expect("长度吻合必须可切分");
        assert_eq!(segments.input_left, input_left);
        assert_eq!(segments.input_right, input_right);
        assert_eq!(segments.expected_left, expected_left);
        assert_eq!(segments.expected_right, expected_right);
    }

    #[test]
    fn 单帧最小布局成立() {
        let decoded = decode_f32_le(&encode_f32_le(&[1.0_f32, 2.0, 3.0, 4.0])).unwrap();
        let segments = split_planar(&decoded, 1).unwrap();
        assert_eq!(segments.input_left, [1.0]);
        assert_eq!(segments.input_right, [2.0]);
        assert_eq!(segments.expected_left, [3.0]);
        assert_eq!(segments.expected_right, [4.0]);
    }

    #[test]
    fn 总长与帧数不符时拒绝切分() {
        let decoded = [0.0_f32; 7]; // 两帧需要 8 个样本
        assert!(split_planar(&decoded, 2).is_none());
    }

    #[test]
    fn 计量型两段布局按声明顺序切开且长度必须精确() {
        let frames = 3;
        let input_left = [0.125_f32, -1.5, 2.25];
        let input_right = [0.5_f32, -0.75, 3.5];

        let mut all = Vec::new();
        all.extend_from_slice(&input_left);
        all.extend_from_slice(&input_right);

        let decoded = decode_f32_le(&encode_f32_le(&all)).expect("合法字节流必须可解码");
        assert_eq!(decoded.len(), 6);

        let segments = split_meter(&decoded, frames).expect("长度吻合必须可切分");
        assert_eq!(segments.input_left, input_left);
        assert_eq!(segments.input_right, input_right);

        // 四段布局长度（12 个样本）对两段切分（需要 6 个）不符，必须拒绝。
        assert!(split_meter(&[0.0_f32; 12], frames).is_none());
        assert!(split_meter(&decoded, 2).is_none());
    }

    #[test]
    fn 字节数非四倍时拒绝解码() {
        assert!(decode_f32_le(&[0, 0, 0]).is_none());
        assert!(decode_f32_le(&[]).is_some()); // 空流是合法的零样本序列
    }

    #[test]
    fn 特殊位型往返一致() {
        // 用位型而非数值构造，覆盖负零与最小次正规数。
        let bit_patterns: [u32; 3] = [0x8000_0000, 0x0000_0001, 0x7F7F_FFFF];
        let values: Vec<f32> = bit_patterns.iter().map(|bits| f32::from_bits(*bits)).collect();
        let decoded = decode_f32_le(&encode_f32_le(&values)).unwrap();
        assert_eq!(decoded.len(), 3);
        for (got, want) in decoded.iter().zip(values.iter()) {
            assert_eq!(got.to_bits(), want.to_bits());
        }
    }
}
