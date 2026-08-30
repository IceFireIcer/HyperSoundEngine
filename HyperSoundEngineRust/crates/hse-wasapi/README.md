# hse-wasapi

Windows WASAPI 低层后端，提供事件驱动共享模式的三类流：

- `open_render(&OpenOptions)`：渲染端点输出，`device_id=None` 选择默认渲染端点；
- `open_loopback(&OpenOptions)`：渲染端点 loopback 捕获，`device_id=None` 选择默认渲染端点；
- `open_capture(&OpenOptions)`：捕获端点直接捕获，`device_id=None` 选择默认捕获端点。

三类入口均协商交错立体声 f32。`open_loopback` 与 `open_capture` 复用相同的无分配读取实现，但严格选择不同类别的端点；显式 `device_id` 类别不匹配时返回 `BackendError::DeviceNotFound`。
