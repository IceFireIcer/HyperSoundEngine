# hse-wasapi

Windows WASAPI 低层后端，提供事件驱动 shared/exclusive 模式的三类流：

- `open_render(&OpenOptions)`：渲染端点输出，`device_id=None` 选择默认渲染端点；
- `open_loopback(&OpenOptions)`：渲染端点 loopback 捕获，`device_id=None` 选择默认渲染端点；
- `open_capture(&OpenOptions)`：捕获端点直接捕获，`device_id=None` 选择默认捕获端点。

三类入口均使用交错立体声 f32。`OpenOptions.access_mode` 缺省语义为 `AccessMode::Shared`：保留近似格式和 `AUTOCONVERTPCM` 兜底。`AccessMode::Exclusive` 仅用于普通 capture/render，使用 `EventsExclusive`，要求目标采样率的立体声 f32 格式由设备原生支持且禁用自动转换；`block_size_frames` 换算为 100ns 目标周期，再由 wasapi 0.24 按设备最小周期和 128-byte 对齐要求调整。loopback+exclusive 明确返回 `BackendError::Format`，不会回退 shared。

`open_loopback` 与 `open_capture` 复用相同的无分配读取实现，但严格选择不同类别的端点；显式 `device_id` 类别不匹配时返回 `BackendError::DeviceNotFound`。
