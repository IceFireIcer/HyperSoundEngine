# 规格：world-listener —— 世界坐标到头坐标方向

> **规格属性**：双支线共享的空间控制域契约。TS 事实实现为
> `src/spatial/controller.ts`，Rust 实现为 `hrtf-core::world`。本切片不包含 HRIR、卷积、房间、
> ambience、多声道或多普勒。

## 一、坐标与单位

- 右手世界坐标：`+X` 向右、`+Y` 向上、`+Z` 向前。
- 位置和距离单位为米，角度单位为度。
- listener `yaw=0` 朝 `+Z`；正 yaw 向 `+X` 右转。
- 输入只包含 listener 的 `position + yaw` 与 source position。公开 `ListenerState` 中的 pitch/roll 保留兼容，但不属于本规格输入。

## 二、计算规则

给定：

```text
dx = source.x - listener.position.x
dy = source.y - listener.position.y
dz = source.z - listener.position.z
distance = sqrt(dx² + dy² + dz²)
```

- `distance == 0` 时固定输出 `{ azimuthDeg: 0, elevationDeg: 0, distance: 0 }`。
- `worldAzimuth = atan2(dx, dz) × 180 / π`。
- `azimuthDeg = wrap(worldAzimuth - listener.yaw)`。
- `wrap(a) = ((a + 180) mod 360 + 360) mod 360 - 180`，结果区间为 `[-180, 180)`；正后方规范为 `-180`。
- `elevationDeg = asin(clamp(dy / distance, -1, 1)) × 180 / π`。
- 两支线均使用 IEEE-754 binary64 计算，不主动量化为 f32。
- 输入必须是有限数；非有限输入在调用边界拒绝，不进入冻结夹具。

## 三、GWT 条款

### GWT-WORLD-01：六个轴向方向
- **给定**：listener 位于原点且 yaw=0，source 位于前、后、左、右、上、下任一轴向。
- **当**：计算相对方向。
- **则**：方位、仰角和距离与 `world-listener.v1.json` 对应 case 在字段级绝对容差内一致。

### GWT-WORLD-02：平移不变性
- **给定**：listener/source 同时平移相同世界向量。
- **当**：计算相对方向。
- **则**：结果与平移前一致。

### GWT-WORLD-03：yaw 旋转与规范化
- **给定**：正/负 yaw、跨越 ±180 或超出一整圈的 yaw。
- **当**：从世界方位扣除 yaw。
- **则**：结果按 §二规范化到 `[-180,180)`；等价的 `yaw ± 360k` 结果一致。

### GWT-WORLD-04：重合点
- **给定**：source 与 listener.position 完全重合。
- **当**：计算相对方向。
- **则**：三个输出均为有限数且固定为 0。

## 四、冻结夹具

- Schema：`specs/schema/world-listener.schema.json`。
- 数据：`specs/spatial/vectors/world-listener.v1.json`。
- 字段级绝对容差：角度 `1e-9` 度，距离 `1e-9` 米。
- 夹具为结构化 JSON，不使用 DSP `.f32` 四段格式，不计入 72 组音频向量。
- 既有夹具不得静默覆盖；导出/校验入口为 `node scripts/export-spatial-vectors.mjs`。

## 五、门禁

- TS：`npx vitest run test/spatial-spec-vectors.test.ts src/spatial/test/controller.test.ts`。
- Rust：`cargo test --manifest-path HyperSoundEngineRust/Cargo.toml -p hrtf-core --locked -j 1`。
- 综合：`cargo run --manifest-path HyperSoundEngineRust/Cargo.toml -q -p hse-parity` 必须分别报告 DSP audio 72/72 与 Spatial world-listener 12/12。

## 六、范围外

pitch/roll、多普勒速度、距离增益、声道路由、HRIR、HRTF 插值、卷积、房间、ambience、多声道、WASM 与服务 stage 22 均不在本切片。Rust `EngineChainStage` 继续只接受 `spatial.mode='off'`。
