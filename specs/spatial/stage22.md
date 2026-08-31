# 规格：stage22 —— Rust EngineChain 空间参数投影

> **规格属性**：TS `SpatialSettings` 到 Rust `hse-core::EngineChainStage` 第 22 级的共享行为契约。
> 既有 72 组 engine-chain 音频向量继续固定 `spatial.mode='off'`，其输入与期望值不得修改。

## 一、模式与输出边界

- `off`：不构造 renderer、不触碰左右声道，旧 72 组音频向量保持逐位一致。
- `instant`：左右输入分别映射到 `±spreadDeg/2`，距离 1.5m；仅此模式使用 `instant.amount` 作为干湿比。
- `headLocked`：消费 `speakers/routes/muted`，主渲染固定纯湿，不受 `instant.amount` 与 `instant.roomAmount` 影响；`both` 拆为同位置左右各半增益对象。
- `world`：消费 listener position/yaw/pitch/roll、sources、trajectories、playhead、occlusion，主渲染固定纯湿，不受 `instant.amount` 与 `instant.roomAmount` 影响。
- `stage`：消费 preset/seat/roomSize/reverbAmount/customSources；布局和房间映射与 TS `scenes.ts` 一致，主渲染固定纯湿，`reverbAmount` 只控制 stage 房间湿量，不受 `instant.amount` 影响。
- 输入和物理输出均保持立体声，不在本契约增加物理 multichannel 输出。

## 二、world 投影

1. 匹配 `sourceId` 的 trajectory 优先于 source 静态位置；关键帧按时间选择相邻点线性插值，越界钳到端点，空关键帧返回原点。
2. 插值后位置经 world-listener 完整欧拉逆旋转得到方位、仰角和距离。
3. 首次构造、上一模式非 world、非递增或非有限 playhead 差值时 listener velocity 为零；否则 `velocity=(currentPosition-previousPosition)/(currentPlayhead-previousPlayhead)`，不得读取时钟。
4. source id 必须非空且唯一。活动对象最多 32 个；slot 由 UTF-16 FNV-1a 候选值和按 id 排序的确定性线性探测得到。服务热更新构造新链时以当前/上一快照 id 并集分配，使对象重排和相邻快照中的增删不改变 identity 到 slot 的映射；该语义不承诺跨链迁移卷积历史。

## 三、renderer 效果顺序

每个逻辑对象按以下顺序处理：

1. 距离增益与空气吸收；
2. world 全局遮挡：增益 `1-0.8*occlusion`，低通截止 `max(12000*(1-occlusion),1)` Hz；
3. listener velocity 对声源方向投影后计算 Doppler rate，钳到 `[0.5,2]`，以每 slot 小数延迟线重采样；
4. `size>0` 时按 `azimuth±size*30°` 两方向各半增益渲染，并对右耳施加 `size*6` 样本小数延迟；`size=0` 保持旧 renderer 算术路径；
5. HRTF 卷积、房间处理、`amount` 干湿混合和 `masterGain`；
6. ambience 开启时，按 TS FOA 环境提取与四条固定延迟线叠加到最终双耳输出；
7. 干湿混合、`masterGain`、房间与 ambience 全部叠加后，逐样本应用 TS 同式最终峰值保护：`|x|<=0.85` 保持线性，否则输出 `sign(x)*(0.85+0.15*tanh((|x|-0.85)/0.15))`，保证有限输入的最终绝对峰值不超过 1。

所有状态按稳定 slot 保存。`reset` 清空卷积、空气、遮挡、Doppler、size 去相关、房间和 ambience 状态；任意不超过 `prepare(maxBlockSize)` 的短块须连续工作，稳态处理不得分配。

## 四、stage 投影

- preset：`stage` / `cinema` / `piano` / `nature` 使用 TS `STAGE_SCENES` 相同方位、仰角、基准距离和房间映射。
- seat 距离倍率：front `0.8`、middle `1.0`、back `1.35`。
- `roomSize` 钳到 `[0.5,2]`，同时缩放对象距离和房间几何；对象距离最终钳到 `[0.5,10]`m。
- `reverbAmount` 钳到 `[0,1]`；customSources 相对固定 listener `(0,1.6,0)`、零朝向投影。

## 五、门禁

- TS：`npx vitest run test/spatial-spec-vectors.test.ts src/spatial/test/controller.test.ts`。
- Rust renderer：`cargo test --manifest-path HyperSoundEngineRust/Cargo.toml -p hrtf-core`。
- Rust core/service/wasm：各 crate 的 stage22、参数和构造测试，以及 `hse-core/tests/realtime_alloc.rs`。
- 综合：`cargo run --manifest-path HyperSoundEngineRust/Cargo.toml -q -p hse-parity`；音频仍为 72/72，空间为 world-listener 14/14 + renderer/ABI 14/14。
