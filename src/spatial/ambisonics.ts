/**
 * ambisonics —— Ambisonics B-format（FOA，一阶实球谐）编解码（HSE 空间音频）
 *
 * 规划书 Phase 4：环境声/混响 send 用。本模块为 Ambisonics Codec 层：
 *   - SH 编码：声源方向 → FOA 信号（encodeSource）；
 *   - SH→虚拟扬声器解码：FOA → 水平面各方向扬声器增益（decodeFoaToSpeakers）；
 *   - 环境提取：立体声输入 → FOA 环境场能量级（stereoToFoa）。
 *
 * 方向约定与 hrtfInterp.ts 一致：az=0 正前、az>0 右、el=0 水平、el>0 上
 * （x=前，y=右，z=上）。ACN 排序（0-3）：W（全向）、X（前后）、Y（左右）、Z（上下）。
 *
 * 归一化约定（SN3D + W 缩放，即常见 DAW/AmbiX 的 B-format 工程约定）：
 *   实球谐基 Y1,±1 = ∓√(3/4π)·sin/cos(az)·cos(el)、Y1,0 = √(3/4π)·sin(el)（见
 *   hrtfInterp.shBasis，含 Condon-Shortley 相因子）。去掉 √(4π/3) 标度后方向分量为
 *   方向余弦 cos(el)·cos(az) / cos(el)·sin(az) / sin(el)（单位增益源方向能量恒为 1）；
 *   全向分量 W 额外乘以 1/√2，使全向与方向通道能量同量级（对正则布局解码能量守恒，
 *   见 decodeFoaToSpeakers 注释）。纯函数、确定性、无依赖（不 import 任何模块）。
 */

/** FOA（First-Order Ambisonics）信号：[W, X, Y, Z]（ACN 0-3） */
export type FoaSignal = [number, number, number, number]

/** 调用方持有的 FOA/解码输出缓冲。 */
export type AmbisonicBuffer = number[] | Float32Array | Float64Array

/**
 * SH 编码：声源方向 (azimuthDeg, elevationDeg) × 增益 gain → FOA 信号。
 *
 * 公式（SN3D 方向余弦 + W 通道 1/√2 标度）：
 *   W = gain·(1/√2)
 *   X = gain·cos(el)·cos(az)
 *   Y = gain·cos(el)·sin(az)
 *   Z = gain·sin(el)
 * 方向余弦归一：X²+Y²+Z² = gain²（单位增益源方向能量 = 1，与方位/仰角无关）。
 */
export function encodeSource(azimuthDeg: number, elevationDeg: number, gain: number): FoaSignal {
  const az = (azimuthDeg * Math.PI) / 180
  const el = (elevationDeg * Math.PI) / 180
  const cosEl = Math.cos(el)
  return [
    gain / Math.SQRT2,
    gain * cosEl * Math.cos(az),
    gain * cosEl * Math.sin(az),
    gain * Math.sin(el),
  ]
}

/**
 * 环境声扬声器布局：标准 4 方向水平等角布局 [45, 135, 225, 315]（FOA 最小解码布局，
 * 等角四面体投影——水平面正方形四角，正侧/侧后各向都有扬声器，环绕感完整）。
 * 全部仰角 0（水平面；无上下通道，环境高度信息由后续完整 3D 解码 wave 补充）。
 */
export const AMBIENCE_SPEAKERS: { azimuthDeg: number; elevationDeg: number }[] = [
  { azimuthDeg: 45, elevationDeg: 0 },
  { azimuthDeg: 135, elevationDeg: 0 },
  { azimuthDeg: 225, elevationDeg: 0 },
  { azimuthDeg: 315, elevationDeg: 0 },
]

/**
 * SH→虚拟扬声器解码：FOA 信号 → azimuths 各方向的扬声器增益（水平面解码矩阵）。
 *
 * 公式（扬声器 el=0，水平面取基函数在 el_k=0 的值：cos(el_k)=1）：
 *   g_k = (1/√2)·W + cos(az_k)·X + sin(az_k)·Y
 * 即解码矩阵 = 编码矩阵的转置（伪逆的解析形式）；Z 通道（上下）在水平面基函数
 * sin(el_k)=0 处无投影，故不参与水平解码（3D 解码需带仰角扬声器，后续 wave）。
 *
 * 能量性质（正则 4 方向布局 45/135/225/315，Σcos=Σsin=0、Σcos²=Σsin²=2）：
 *   Σ_k g_k² = 2·(W²+X²+Y²) —— 水平信号解码能量恰为输入 FOA 能量的 2 倍（恒定因子，
 *   幅值同量级，不会放大/湮灭）。允许负增益（Ambisonics 相位抵消的物理语义），
 *   本波环境上混不使用解码输出（fusion 侧以固定增益馈 4 扬声器），留完整渲染 wave 用。
 */
export function decodeFoaToSpeakers(
  foa: ArrayLike<number>,
  azimuths: readonly number[],
  out: AmbisonicBuffer,
): AmbisonicBuffer {
  if (out.length < azimuths.length) throw new Error('decodeFoaToSpeakers: output buffer is too small')
  for (let i = 0; i < azimuths.length; i++) {
    const az = (azimuths[i] * Math.PI) / 180
    out[i] = foa[0] / Math.SQRT2 + Math.cos(az) * foa[1] + Math.sin(az) * foa[2]
  }
  return out
}

/**
 * 环境提取（音乐播放器场景）：立体声输入 → FOA 环境场能量级（块 RMS）。
 *
 * 简化模型——从立体声近似 FOA 环境场（完整多声道上混后续 wave）：
 *   mid  = (l+r)/2   中置（同相）分量 → 全向
 *   side = (l-r)/2   差分（反相）分量 → 水平左右
 *   W = rms(mid)·√2（全向能量；√2 与 encode 的 1/√2 互为逆标度）
 *   Y = rms(side)·√2（左右差分 ≈ 水平 Y；立体声左右声道即水平 90° 轴采样）
 *   X = 0、Z = 0（无前后/上下信息——M/S 矩阵只能给出左右轴）
 *
 * 有效帧数由 frameCount 指定，允许输入缓冲容量大于本次块长；结果写入调用方缓冲。
 */
export function stereoToFoa(
  l: Float32Array,
  r: Float32Array,
  frameCount: number,
  out: AmbisonicBuffer,
): AmbisonicBuffer {
  if (out.length < 4) throw new Error('stereoToFoa: output buffer is too small')
  const requested = Number.isFinite(frameCount) ? Math.floor(frameCount) : 0
  const n = Math.max(0, Math.min(requested, l.length, r.length))
  if (n <= 0) {
    out[0] = 0
    out[1] = 0
    out[2] = 0
    out[3] = 0
    return out
  }
  let sMid = 0
  let sSide = 0
  for (let i = 0; i < n; i++) {
    const mid = (l[i] + r[i]) * 0.5
    const side = (l[i] - r[i]) * 0.5
    sMid += mid * mid
    sSide += side * side
  }
  out[0] = Math.sqrt((2 * sMid) / n)
  out[1] = 0
  out[2] = Math.sqrt((2 * sSide) / n)
  out[3] = 0
  return out
}
