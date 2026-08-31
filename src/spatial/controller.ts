/**
 * controller —— 模式 C 世界漫游：听者 → 声源相对方向计算 + 听者移动/旋转（纯函数）
 *
 * 规划书 §4.3：direction = src − listener；将世界方向乘听者 yaw/pitch/roll 的逆旋转后：
 *   azimuth = atan2(headX, headZ)（度，右正、前方 0、正后 ±180）
 *   elevation = asin(headY / distance)（度，上方为正）
 *   distance = |direction|（米）
 * ListenerState/AudioObject 定义复用于 types.ts（本模块不重复声明）。
 * 移动/旋转（moveListener/rotateListener/listenerForwardVector/applyKeyboardMove）
 * 供 UI 键盘漫游调用：世界系基向量约定 forward=(sin yaw, cos yaw)、
 * right=(cos yaw, −sin yaw)（与 ui/worldControl.ts 一致），位移方向恒相对听者朝向。
 */

import type { ListenerState, Vec3, WorldListenerPose } from './types'

/** 相对方向计算结果（角度单位度，距离单位米） */
export interface RelativeDirection {
  azimuthDeg: number
  elevationDeg: number
  distance: number
}

export interface TimedPosition {
  position: Vec3
  playhead: number
}

/**
 * 相邻参数快照推导听者世界速度。首次（prev=null）、非递增或非有限时间差固定返回零，
 * 从而不依赖系统时钟且在暂停/seek 时保持确定性。
 */
export function computeWorldVelocity(prev: TimedPosition | null, current: TimedPosition): Vec3 {
  if (!prev) return { x: 0, y: 0, z: 0 }
  const dt = current.playhead - prev.playhead
  if (!Number.isFinite(dt) || dt <= 0) return { x: 0, y: 0, z: 0 }
  return {
    x: (current.position.x - prev.position.x) / dt,
    y: (current.position.y - prev.position.y) / dt,
    z: (current.position.z - prev.position.z) / dt,
  }
}

export function wrapAzimuthDeg(angle: number): number {
  return (((angle + 180) % 360) + 360) % 360 - 180
}

/**
 * 计算声源相对听者的方向（世界坐标 → 头相关方位/仰角/距离）。
 * 输入为任意世界坐标（与听者朝向无关的位置量）；输出已扣除听者偏航 yaw，
 * 使 0° 恒为听者正前方。纯函数：无状态、无副作用。
 */
export function computeRelativeDirection(
  listener: WorldListenerPose,
  source: Vec3,
): RelativeDirection {
  if (![
    listener.position.x,
    listener.position.y,
    listener.position.z,
    listener.yaw,
    listener.pitch ?? 0,
    listener.roll ?? 0,
    source.x,
    source.y,
    source.z,
  ].every(Number.isFinite)) {
    throw new Error('computeRelativeDirection: listener and source values must be finite')
  }
  const dx = source.x - listener.position.x
  const dy = source.y - listener.position.y
  const dz = source.z - listener.position.z
  const distance = Math.sqrt(dx * dx + dy * dy + dz * dz)
  if (distance === 0) {
    // 声源与听者重合：方向未定义，约定为正前方（避免 asin 除零 → NaN）
    return { azimuthDeg: 0, elevationDeg: 0, distance: 0 }
  }

  // 世界方向乘听者欧拉姿态的逆旋转：先撤销 yaw，再撤销 pitch、roll。
  // 约定 yaw 右转为正、pitch 抬头为正（固定世界前方因此落到负仰角）、roll 向右倾为正；pitch/roll 缺省 0
  // 保持旧 WorldListenerPose 与冻结 yaw-only case 完全兼容。
  const yaw = -(listener.yaw * Math.PI) / 180
  const pitch = ((listener.pitch ?? 0) * Math.PI) / 180
  const roll = -((listener.roll ?? 0) * Math.PI) / 180
  const cy = Math.cos(yaw)
  const sy = Math.sin(yaw)
  const cp = Math.cos(pitch)
  const sp = Math.sin(pitch)
  const cr = Math.cos(roll)
  const sr = Math.sin(roll)
  const yawX = cy * dx + sy * dz
  const yawZ = -sy * dx + cy * dz
  const pitchY = cp * dy - sp * yawZ
  const pitchZ = sp * dy + cp * yawZ
  const headX = cr * yawX + sr * pitchY
  const headY = -sr * yawX + cr * pitchY
  const azimuthDeg = wrapAzimuthDeg((Math.atan2(headX, pitchZ) * 180) / Math.PI)
  const elevationDeg = (Math.asin(Math.max(-1, Math.min(1, headY / distance))) * 180) / Math.PI
  return { azimuthDeg, elevationDeg, distance }
}

// ==================== 声源轨迹插值（模式 C：声源沿时间轨迹运动） ====================

/**
 * 声源轨迹关键帧线性插值（纯函数，供 fusion world 分支按 playhead 取声源位置）：
 *   - t ≤ 首关键帧时刻 → 首位置；t ≥ 末关键帧时刻 → 末位置（越界夹取）；
 *   - 否则取 t 所在相邻两帧 A、B 线性插值：
 *       u = (t − tA) / (tB − tA)，position(t) = A + (B − A) × u（逐分量，u∈[0,1]）；
 *   - 单关键帧 → 恒为该位置；空数组 → 原点 {0,0,0}（防御：避免 NaN 扩散）。
 * 防御：先按 t 升序排序副本——UI 可能按任意顺序追加关键帧，排序保证插值区间正确
 * （轨迹关键帧数量少，O(n log n) 开销可忽略）。
 */
export function computeTrajectoryPosition(
  keyframes: { t: number; position: { x: number; y: number; z: number } }[],
  t: number,
): { x: number; y: number; z: number } {
  if (keyframes.length === 0) return { x: 0, y: 0, z: 0 }
  const sorted = [...keyframes].sort((a, b) => a.t - b.t)
  const first = sorted[0]
  const last = sorted[sorted.length - 1]
  if (t <= first.t) return { ...first.position }
  if (t >= last.t) return { ...last.position }
  for (let i = 0; i < sorted.length - 1; i++) {
    const a = sorted[i]
    const b = sorted[i + 1]
    if (t >= a.t && t <= b.t) {
      const span = b.t - a.t
      const u = span === 0 ? 0 : (t - a.t) / span
      return {
        x: a.position.x + (b.position.x - a.position.x) * u,
        y: a.position.y + (b.position.y - a.position.y) * u,
        z: a.position.z + (b.position.z - a.position.z) * u,
      }
    }
  }
  return { ...last.position } // 防御：t 已在 (first.t, last.t) 内，循环必命中
}

// ==================== 听者移动 / 旋转（模式 C 世界漫游） ====================

/** 世界坐标平移（米）：位置按增量移动，yaw/pitch/roll 不变。返回新对象（不可变）。 */
export function moveListener(l: ListenerState, d: { x: number; y: number; z: number }): ListenerState {
  return {
    position: { x: l.position.x + d.x, y: l.position.y + d.y, z: l.position.z + d.z },
    yaw: l.yaw,
    pitch: l.pitch,
    roll: l.roll,
  }
}

/**
 * 偏航增量旋转（度）：yaw += dYawDeg 后 wrap 到 [-180, 180)，pitch/roll 不变。
 * 返回新对象（不可变）。wrap 公式：((yaw+180) mod 360) − 180 —— 恒等旋转（dYawDeg=0）不改变
 * 既有的合法 yaw；跨 ±180 边界正确折返（如 170°+20° → −170°、355°+10° → 5°）。
 */
export function rotateListener(l: ListenerState, dYawDeg: number): ListenerState {
  const yaw = wrapAzimuthDeg(l.yaw + dYawDeg)
  return { position: { ...l.position }, yaw, pitch: l.pitch, roll: l.roll }
}

/**
 * 水平面前向单位向量（yaw → (x, z)）：
 *   forward = (sin(yaw), cos(yaw))，yaw 弧度化。
 * 与方位角定义互补（azimuth = atan2(dx, dz) − yaw）：
 *   yaw=0 → (0, +1)（+Z 前方）；yaw=90 → (+1, 0)（+X 右前方）。
 */
export function listenerForwardVector(l: ListenerState): { x: number; z: number } {
  const yawRad = (l.yaw * Math.PI) / 180
  return { x: Math.sin(yawRad), z: Math.cos(yawRad) }
}

/**
 * 键盘移动（纯函数）：WASD 按听者朝向相对前后左右、QE 世界 Y 轴升降。
 *   forward = (sin yaw, cos yaw)；right = (cos yaw, −sin yaw)（右手系：右 = 上×前；
 *   yaw=0 时 forward=+Z、right=+X；yaw=90 时 forward=+X、right=−Z）。
 *   W/S 沿 forward 前后，D/A 沿 right 左右（斜向为向量和，不归一化），
 *   Q 升（+Y）/ E 降（−Y）（与 ui/worldControl.ts 的 Q 升 E 降约定一致）。
 * 位移 = 方向 × speed × dt；dt 秒、speed m/s。返回新对象（不可变）。
 */
export function applyKeyboardMove(
  l: ListenerState,
  keys: Record<'w' | 'a' | 's' | 'd' | 'q' | 'e', boolean>,
  speed: number,
  dt: number,
): ListenerState {
  const yawRad = (l.yaw * Math.PI) / 180
  const fwdX = Math.sin(yawRad)
  const fwdZ = Math.cos(yawRad)
  const rightX = Math.cos(yawRad)
  const rightZ = -Math.sin(yawRad)
  const step = speed * dt
  let dx = 0
  let dy = 0
  let dz = 0
  if (keys.w) {
    dx += fwdX * step
    dz += fwdZ * step
  }
  if (keys.s) {
    dx -= fwdX * step
    dz -= fwdZ * step
  }
  if (keys.d) {
    dx += rightX * step
    dz += rightZ * step
  }
  if (keys.a) {
    dx -= rightX * step
    dz -= rightZ * step
  }
  if (keys.q) dy += step // Q：上升（+Y）
  if (keys.e) dy -= step // E：下降（−Y）
  return moveListener(l, { x: dx, y: dy, z: dz })
}
