//! Phase 4 底层 WASAPI 诊断工具。
//!
//! 默认路径只枚举设备并校验配置。真实开流必须同时提供 `measure --run` 与
//! `HSE_ALLOW_REAL_AUDIO=1`。测量按固定总帧数和固定脉冲数结束，不以固定时长终止。
//! 此工具直接连接 WASAPI capture/render，不经过 ServiceEngineChain 或服务双环；
//! 完整服务管线由纯内存 readiness 许可测试独立验证。

use std::sync::Arc;
use std::time::{Duration, Instant};

use hse_wasapi::{AccessMode, DeviceInfo, DeviceKind, OpenOptions};
use serde_json::{json, Value};

use crate::backend::BackendFactory;

const ALLOW_ENV: &str = "HSE_ALLOW_REAL_AUDIO";
const USAGE: &str = "hse-real-audio-check - Phase 4 底层 WASAPI 诊断（默认 dry-run，不经过服务 DSP/双环）\n\n用法：\n  hse-real-audio-check inspect [选项]\n  hse-real-audio-check measure [选项] [--run]\n\n选项：\n  --source loopback|capture\n  --input-device <id>       loopback 时为渲染端点；capture 时为捕获端点\n  --output-device <id>      脉冲输出渲染端点\n  --share-mode shared|exclusive\n  --rate <Hz>               默认 48000\n  --block <frames>          默认 128\n  --pulses <count>          默认 12\n  --pulse-interval <frames> 默认 4800\n  --frames <count>          默认 67200\n  --max-latency <frames>    默认 9600\n  --external-loopback-confirmed  明确确认物理/外部回录已接通\n  --pretty                   格式化 JSON\n  --run                      请求真实开流；还必须设置 HSE_ALLOW_REAL_AUDIO=1\n\ninspect 与未授权 measure 都不会打开音频流。";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Inspect,
    Measure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceMode {
    Loopback,
    Capture,
}

impl SourceMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Loopback => "loopback",
            Self::Capture => "capture",
        }
    }
}

#[derive(Debug, Clone)]
struct Config {
    command: Command,
    run: bool,
    source: SourceMode,
    input_id: Option<String>,
    output_id: Option<String>,
    access: AccessMode,
    rate: u32,
    block: u32,
    pulses: u32,
    pulse_interval: u32,
    frames: u64,
    max_latency: u32,
    external_confirmed: bool,
    pretty: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            command: Command::Inspect,
            run: false,
            source: SourceMode::Loopback,
            input_id: None,
            output_id: None,
            access: AccessMode::Shared,
            rate: 48_000,
            block: 128,
            pulses: 12,
            pulse_interval: 4_800,
            frames: 67_200,
            max_latency: 9_600,
            external_confirmed: false,
            pretty: false,
        }
    }
}

fn next_u32(values: &[String], index: &mut usize, name: &str) -> Result<u32, String> {
    *index += 1;
    values
        .get(*index)
        .ok_or_else(|| format!("{name} 缺少取值"))?
        .parse()
        .map_err(|_| format!("{name} 需要 u32 整数"))
}

fn next_string(values: &[String], index: &mut usize, name: &str) -> Result<String, String> {
    *index += 1;
    values
        .get(*index)
        .cloned()
        .ok_or_else(|| format!("{name} 缺少取值"))
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Option<Config>, String> {
    let values: Vec<_> = args.collect();
    if values.iter().any(|v| v == "-h" || v == "--help") {
        return Ok(None);
    }
    let mut cfg = Config::default();
    let mut i = 0;
    if let Some(first) = values.first() {
        match first.as_str() {
            "inspect" => i = 1,
            "measure" => {
                cfg.command = Command::Measure;
                i = 1;
            }
            value if value.starts_with('-') => {}
            other => return Err(format!("未知子命令：{other}")),
        }
    }
    while i < values.len() {
        match values[i].as_str() {
            "--run" => cfg.run = true,
            "--pretty" => cfg.pretty = true,
            "--external-loopback-confirmed" => cfg.external_confirmed = true,
            "--source" => {
                let value = next_string(&values, &mut i, "--source")?;
                cfg.source = match value.as_str() {
                    "loopback" => SourceMode::Loopback,
                    "capture" => SourceMode::Capture,
                    _ => return Err("--source 仅支持 loopback 或 capture".into()),
                };
            }
            "--input-device" => {
                cfg.input_id = Some(next_string(&values, &mut i, "--input-device")?)
            }
            "--output-device" => {
                cfg.output_id = Some(next_string(&values, &mut i, "--output-device")?)
            }
            "--share-mode" => {
                let value = next_string(&values, &mut i, "--share-mode")?;
                cfg.access = match value.as_str() {
                    "shared" => AccessMode::Shared,
                    "exclusive" => AccessMode::Exclusive,
                    _ => return Err("--share-mode 仅支持 shared 或 exclusive".into()),
                };
            }
            "--rate" => cfg.rate = next_u32(&values, &mut i, "--rate")?,
            "--block" => cfg.block = next_u32(&values, &mut i, "--block")?,
            "--pulses" => cfg.pulses = next_u32(&values, &mut i, "--pulses")?,
            "--pulse-interval" => {
                cfg.pulse_interval = next_u32(&values, &mut i, "--pulse-interval")?
            }
            "--frames" => cfg.frames = u64::from(next_u32(&values, &mut i, "--frames")?),
            "--max-latency" => cfg.max_latency = next_u32(&values, &mut i, "--max-latency")?,
            other => return Err(format!("未知参数：{other}")),
        }
        i += 1;
    }
    validate(&cfg)?;
    Ok(Some(cfg))
}

fn validate(cfg: &Config) -> Result<(), String> {
    if !(8_000..=384_000).contains(&cfg.rate) {
        return Err("--rate 必须在 8000..=384000".into());
    }
    if !(16..=8_192).contains(&cfg.block) {
        return Err("--block 必须在 16..=8192".into());
    }
    if cfg.pulses == 0 || cfg.pulse_interval < cfg.block {
        return Err("脉冲数必须非零，且 --pulse-interval 不得小于块长".into());
    }
    let last = u64::from(cfg.block) * 2 + u64::from(cfg.pulses - 1) * u64::from(cfg.pulse_interval);
    if last >= cfg.frames {
        return Err("--frames 不足以容纳固定脉冲计划".into());
    }
    if cfg.source == SourceMode::Loopback && cfg.access == AccessMode::Exclusive {
        return Err("WASAPI loopback 不支持 exclusive".into());
    }
    Ok(())
}

fn device_json(d: &DeviceInfo) -> Value {
    json!({"kind": match d.kind { DeviceKind::Render => "render", DeviceKind::Capture => "capture" },
        "id": d.id, "name": d.name, "isDefault": d.is_default})
}

fn is_vb_cable(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "vb-audio",
        "vb cable",
        "vb-cable",
        "cable input",
        "cable output",
    ]
    .iter()
    .any(|s| n.contains(s))
}

fn find_device<'a>(
    devices: &'a [DeviceInfo],
    id: Option<&str>,
    kind: DeviceKind,
) -> Option<&'a DeviceInfo> {
    match id {
        Some(id) => devices.iter().find(|d| d.kind == kind && d.id == id),
        None => devices.iter().find(|d| d.kind == kind && d.is_default),
    }
}

fn vb_pair(output: &DeviceInfo, input: &DeviceInfo) -> bool {
    output.kind == DeviceKind::Render
        && input.kind == DeviceKind::Capture
        && output.name.to_ascii_lowercase().contains("cable input")
        && input.name.to_ascii_lowercase().contains("cable output")
}

fn topology(
    cfg: &Config,
    input: Option<&DeviceInfo>,
    output: Option<&DeviceInfo>,
) -> (&'static str, bool) {
    match cfg.source {
        SourceMode::Loopback if input.zip(output).is_some_and(|(a, b)| a.id == b.id) => {
            ("wasapi-loopback", true)
        }
        SourceMode::Capture if input.zip(output).is_some_and(|(i, o)| vb_pair(o, i)) => {
            ("vb-cable", true)
        }
        SourceMode::Capture if cfg.external_confirmed => ("external-loopback-confirmed", true),
        _ => ("external-loopback-required", false),
    }
}

fn pulse_positions(cfg: &Config) -> Vec<u64> {
    let first = u64::from(cfg.block) * 2;
    (0..cfg.pulses)
        .map(|i| first + u64::from(i) * u64::from(cfg.pulse_interval))
        .collect()
}

fn fill_pulses(buf: &mut [f32], start: u64, pulses: &[u64]) {
    buf.fill(0.0);
    let frames = buf.len() / 2;
    for &pulse in pulses {
        if pulse >= start && pulse < start + frames as u64 {
            let i = (pulse - start) as usize * 2;
            buf[i] = 0.8;
            buf[i + 1] = 0.8;
        }
    }
}

fn detect_impulses(samples: &[f32], spacing: usize) -> Vec<u64> {
    let mut result = Vec::new();
    let mut last = None;
    let mut previous_peak = 0.0_f32;
    for (i, frame) in samples.chunks_exact(2).enumerate() {
        let peak = frame[0].abs().max(frame[1].abs());
        let rising_edge = peak >= 0.35 && previous_peak < 0.15;
        if rising_edge && !last.is_some_and(|p| i - p < spacing) {
            result.push(i as u64);
            last = Some(i);
        }
        previous_peak = peak;
    }
    result
}

fn correlate(sent: &[u64], detected: &[u64], max_latency: u64) -> Vec<u64> {
    if sent.is_empty() || detected.len() < sent.len() {
        return Vec::new();
    }
    for start in 0..=detected.len() - sent.len() {
        let Some(base) = detected[start].checked_sub(sent[0]) else {
            continue;
        };
        if base > max_latency {
            continue;
        }
        if sent
            .iter()
            .enumerate()
            .all(|(i, s)| detected[start + i].abs_diff(s + base) <= 2)
        {
            return sent
                .iter()
                .enumerate()
                .map(|(i, s)| detected[start + i] - s)
                .collect();
        }
    }
    Vec::new()
}

fn percentile(sorted: &[u64], pct: usize) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    sorted
        .get((sorted.len() * pct).div_ceil(100).saturating_sub(1))
        .copied()
}

fn acceptance_failures(
    complete: bool,
    p95_frames: Option<u64>,
    latency_limit_frames: u64,
    xruns_total: u64,
) -> Vec<&'static str> {
    let mut failures = Vec::new();
    if !complete {
        failures.push("insufficient-correlation");
    }
    if complete && !p95_frames.is_some_and(|value| value <= latency_limit_frames) {
        failures.push("latency-budget-exceeded");
    }
    if xruns_total != 0 {
        failures.push("xrun-detected");
    }
    failures
}

fn latency_json(mut values: Vec<u64>, rate: u32) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    values.sort_unstable();
    let p50 = percentile(&values, 50).unwrap();
    let p95 = percentile(&values, 95).unwrap();
    let max = *values.last().unwrap();
    let ms = |v| v as f64 * 1_000.0 / f64::from(rate);
    json!({"samples": values.len(), "frames": {"p50": p50, "p95": p95, "max": max},
        "milliseconds": {"p50": ms(p50), "p95": ms(p95), "max": ms(max)}})
}

#[cfg(windows)]
fn process_cpu_ticks() -> Option<u64> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let zero = || FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut c = zero();
    let mut e = zero();
    let mut k = zero();
    let mut u = zero();
    if unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) } == 0 {
        return None;
    }
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    Some(ticks(k) + ticks(u))
}

#[cfg(not(windows))]
fn process_cpu_ticks() -> Option<u64> {
    None
}

fn selected<'a>(
    cfg: &Config,
    devices: &'a [DeviceInfo],
) -> Result<(&'a DeviceInfo, &'a DeviceInfo), String> {
    let input_kind = if cfg.source == SourceMode::Loopback {
        DeviceKind::Render
    } else {
        DeviceKind::Capture
    };
    let input = find_device(devices, cfg.input_id.as_deref(), input_kind)
        .ok_or("配置的输入设备不存在，且对应类别无默认设备")?;
    let output = find_device(devices, cfg.output_id.as_deref(), DeviceKind::Render)
        .ok_or("配置的输出设备不存在，且无默认渲染设备")?;
    Ok((input, output))
}

fn base_report(cfg: &Config, devices: &[DeviceInfo], allowed: bool) -> Result<Value, String> {
    let (input, output) = selected(cfg, devices)?;
    let (topology, constructible) = topology(cfg, Some(input), Some(output));
    let vb: Vec<_> = devices
        .iter()
        .filter(|d| is_vb_cable(&d.name))
        .map(device_json)
        .collect();
    Ok(json!({
        "schemaVersion": 2, "tool": "hse-real-audio-check",
        "diagnosticScope": "low-level-wasapi",
        "path": ["wasapi-capture", "wasapi-render"],
        "excludedPath": ["ServiceEngineChain", "input-ring", "output-ring", "service-worker-threads"],
        "measurements": {
            "lowLevelWasapi": {"status": "not-run", "latency": Value::Null,
                "xruns": {"capture": Value::Null, "render": Value::Null, "total": Value::Null},
                "performance": {"cpuPercent": Value::Null, "framesPerSecond": Value::Null, "realtimeFactor": Value::Null}},
            "servicePipeline": {"status": "not-measured", "kind": "pure-memory-automatic-gate",
                "command": "cargo test -p hse-service --test pipeline_fake 完整管线由readiness许可推进_经过service_chain与双环 --locked"}
        },
        "dryRun": cfg.command != Command::Measure || !cfg.run || !allowed,
        "gate": {"runFlag": cfg.run, "environment": ALLOW_ENV, "environmentAllowed": allowed},
        "status": "dry-run-ready", "topology": topology, "topologyConstructible": constructible,
        "vbCable": {"detected": !vb.is_empty(), "devices": vb},
        "config": {"source": cfg.source.as_str(),
            "shareMode": if cfg.access == AccessMode::Shared { "shared" } else { "exclusive" },
            "inputDeviceId": input.id, "inputDeviceName": input.name,
            "outputDeviceId": output.id, "outputDeviceName": output.name,
            "sampleRate": cfg.rate, "blockSizeFrames": cfg.block, "totalFrames": cfg.frames,
            "pulseCount": cfg.pulses, "pulseIntervalFrames": cfg.pulse_interval,
            "maxLatencyFrames": cfg.max_latency},
        "devices": devices.iter().map(device_json).collect::<Vec<_>>(),
        "latency": Value::Null,
        "xruns": {"capture": Value::Null, "render": Value::Null, "total": Value::Null},
        "performance": {"cpuPercent": Value::Null, "framesPerSecond": Value::Null, "realtimeFactor": Value::Null}
    }))
}

fn measure(
    cfg: &Config,
    factory: &dyn BackendFactory,
    devices: &[DeviceInfo],
    mut report: Value,
) -> Result<Value, String> {
    let (input, output) = selected(cfg, devices)?;
    if !topology(cfg, Some(input), Some(output)).1 {
        report["status"] = json!("external-loopback-required");
        return Ok(report);
    }
    let input_opts = OpenOptions {
        device_id: Some(input.id.clone()),
        sample_rate: cfg.rate,
        block_size_frames: cfg.block,
        access_mode: cfg.access,
    };
    let output_opts = OpenOptions {
        device_id: Some(output.id.clone()),
        ..input_opts.clone()
    };
    let opener = if cfg.source == SourceMode::Loopback {
        factory.loopback_opener(&input_opts)
    } else {
        factory.capture_opener(&input_opts)
    };
    let mut capture = opener.open().map_err(|e| e.to_string())?;
    let mut render = factory
        .render_opener(&output_opts)
        .open()
        .map_err(|e| e.to_string())?;
    let cap_fmt = capture.start().map_err(|e| e.to_string())?;
    let ren_fmt = match render.start() {
        Ok(format) => format,
        Err(error) => {
            let _ = capture.stop();
            return Err(error.to_string());
        }
    };
    if cap_fmt.channels != 2
        || ren_fmt.channels != 2
        || cap_fmt.sample_rate != cfg.rate
        || ren_fmt.sample_rate != cfg.rate
    {
        let _ = render.stop();
        let _ = capture.stop();
        return Err(format!(
            "协商格式不匹配：capture={cap_fmt:?}, render={ren_fmt:?}"
        ));
    }

    let pulses = pulse_positions(cfg);
    let mut tx = vec![0.0; cfg.block as usize * 2];
    let mut rx = vec![0.0; cfg.block as usize * 2];
    let budget = cfg.frames + u64::from(cfg.max_latency);
    let max_waits = budget.div_ceil(u64::from(cfg.block)) * 4;
    let wait = Duration::from_millis(
        (u64::from(cfg.block) * 1_000)
            .div_ceil(u64::from(cfg.rate))
            .clamp(1, 25),
    );
    let mut captured = Vec::with_capacity(budget as usize * 2);
    let cpu0 = process_cpu_ticks();
    let wall0 = Instant::now();
    let mut rendered = 0_u64;
    let mut waits = 0_u64;
    while rendered < cfg.frames {
        let n = (cfg.frames - rendered).min(u64::from(cfg.block)) as usize;
        fill_pulses(&mut tx[..n * 2], rendered, &pulses);
        render.push(&tx[..n * 2]).map_err(|e| e.to_string())?;
        rendered += n as u64;
        if capture.wait_ready(wait).map_err(|e| e.to_string())? {
            let got = capture.pull(&mut rx).map_err(|e| e.to_string())?;
            captured.extend_from_slice(&rx[..got * 2]);
        }
        waits += 1;
    }
    while captured.len() / 2 < budget as usize && waits < max_waits {
        if capture.wait_ready(wait).map_err(|e| e.to_string())? {
            let got = capture.pull(&mut rx).map_err(|e| e.to_string())?;
            captured.extend_from_slice(&rx[..got * 2]);
            let found = detect_impulses(&captured, cfg.pulse_interval as usize / 2);
            if correlate(&pulses, &found, u64::from(cfg.max_latency)).len() == pulses.len() {
                break;
            }
        }
        waits += 1;
    }
    let wall = wall0.elapsed();
    let cpu1 = process_cpu_ticks();
    let cap_xruns = capture.xruns();
    let ren_xruns = render.xruns();
    render.stop().map_err(|e| e.to_string())?;
    capture.stop().map_err(|e| e.to_string())?;
    let detected = detect_impulses(&captured, cfg.pulse_interval as usize / 2);
    let latencies = correlate(&pulses, &detected, u64::from(cfg.max_latency));
    let complete = latencies.len() == pulses.len();
    let p95_frames = percentile(&latencies, 95);
    let latency_limit_ms = if cfg.access == AccessMode::Shared {
        30_u64
    } else {
        10_u64
    };
    let latency_limit_frames = u64::from(cfg.rate) * latency_limit_ms / 1_000;
    let xruns_total = cap_xruns + ren_xruns;
    let failures = acceptance_failures(complete, p95_frames, latency_limit_frames, xruns_total);
    let passed = failures.is_empty();
    let seconds = wall.as_secs_f64().max(f64::EPSILON);
    let fps = rendered as f64 / seconds;
    let cpu = cpu0
        .zip(cpu1)
        .map(|(a, b)| (b.saturating_sub(a) as f64 / 10_000_000.0) / seconds * 100.0);
    report["status"] = json!(if passed { "pass" } else { "fail" });
    report["acceptance"] = json!({
        "latencyLimitMs": latency_limit_ms,
        "latencyLimitFrames": latency_limit_frames,
        "requiresZeroXruns": true,
        "failures": failures,
    });
    report["negotiated"] = json!({"capture": {"sampleRate": cap_fmt.sample_rate, "channels": cap_fmt.channels},
        "render": {"sampleRate": ren_fmt.sample_rate, "channels": ren_fmt.channels}});
    report["observed"] = json!({"renderedFrames": rendered, "capturedFrames": captured.len() / 2,
        "pulsesSent": pulses.len(), "pulsesDetected": detected.len(), "pulsesCorrelated": latencies.len(), "waitAttempts": waits});
    let latency = latency_json(latencies, cfg.rate);
    let xruns = json!({"capture": cap_xruns, "render": ren_xruns, "total": xruns_total});
    let performance = json!({"cpuPercent": cpu, "cpuScope": "low-level-wasapi-diagnostic-process-total",
        "framesPerSecond": fps, "realtimeFactor": fps / f64::from(cfg.rate), "wallMilliseconds": seconds * 1_000.0});
    report["latency"] = latency.clone();
    report["xruns"] = xruns.clone();
    report["performance"] = performance.clone();
    report["measurements"]["lowLevelWasapi"] = json!({
        "status": if passed { "pass" } else { "fail" },
        "latency": latency,
        "xruns": xruns,
        "performance": performance,
    });
    Ok(report)
}

fn execute(cfg: &Config, factory: &dyn BackendFactory, allowed: bool) -> Result<Value, String> {
    let devices = factory.list_devices().map_err(|e| e.to_string())?;
    let mut report = base_report(cfg, &devices, allowed)?;
    if cfg.command != Command::Measure || !cfg.run || !allowed {
        if cfg.command == Command::Measure && cfg.run && !allowed {
            report["status"] = json!("real-audio-gate-required");
        }
        return Ok(report);
    }
    measure(cfg, factory, &devices, report)
}

pub fn run_cli(args: impl Iterator<Item = String>, factory: Arc<dyn BackendFactory>) -> i32 {
    let cfg = match parse_args(args) {
        Ok(Some(cfg)) => cfg,
        Ok(None) => {
            println!("{USAGE}");
            return 0;
        }
        Err(e) => {
            eprintln!("参数错误：{e}\n\n{USAGE}");
            return 2;
        }
    };
    let allowed = std::env::var(ALLOW_ENV).as_deref() == Ok("1");
    match execute(&cfg, factory.as_ref(), allowed) {
        Ok(report) => {
            let text = if cfg.pretty {
                serde_json::to_string_pretty(&report)
            } else {
                serde_json::to_string(&report)
            }
            .unwrap();
            println!("{text}");
            if matches!(report["status"].as_str(), Some("pass" | "dry-run-ready")) {
                0
            } else {
                3
            }
        }
        Err(error) => {
            println!(
                "{}",
                json!({"schemaVersion": 2, "tool": "hse-real-audio-check", "diagnosticScope": "low-level-wasapi", "status": "error", "error": error})
            );
            3
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_backend::FakeFactory;

    fn args(v: &[&str]) -> impl Iterator<Item = String> {
        v.iter()
            .map(|s| (*s).to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn 默认dry_run且门控失败不开流() {
        let cfg = parse_args(args(&["measure", "--run"])).unwrap().unwrap();
        let factory = FakeFactory::working(Duration::ZERO, Duration::ZERO);
        let report = execute(&cfg, factory.as_ref(), false).unwrap();
        assert_eq!(report["status"], "real-audio-gate-required");
        assert!(factory.opened_loopback.lock().unwrap().is_empty());
        assert!(factory.opened_render.lock().unwrap().is_empty());
    }

    #[test]
    fn 解析拒绝非法边界和loopback独占() {
        assert!(parse_args(args(&["measure", "--pulses", "0"])).is_err());
        assert!(parse_args(args(&["measure", "--share-mode", "exclusive"])).is_err());
        assert!(parse_args(args(&["measure", "--frames", "128"])).is_err());
    }

    #[test]
    fn vb_cable检测和拓扑使用友好名() {
        let out = DeviceInfo {
            kind: DeviceKind::Render,
            id: "r".into(),
            name: "CABLE Input (VB-Audio Virtual Cable)".into(),
            is_default: false,
        };
        let input = DeviceInfo {
            kind: DeviceKind::Capture,
            id: "c".into(),
            name: "CABLE Output (VB-Audio Virtual Cable)".into(),
            is_default: false,
        };
        let cfg = Config {
            source: SourceMode::Capture,
            ..Config::default()
        };
        assert!(is_vb_cable(&out.name));
        assert!(vb_pair(&out, &input));
        assert_eq!(topology(&cfg, Some(&input), Some(&out)), ("vb-cable", true));
    }

    #[test]
    fn 脉冲相关和百分位为纯函数() {
        let cfg = Config {
            block: 16,
            pulses: 3,
            pulse_interval: 32,
            frames: 128,
            ..Config::default()
        };
        let sent = pulse_positions(&cfg);
        assert_eq!(sent, [32, 64, 96]);
        let mut samples = vec![0.0_f32; 140 * 2];
        for i in [39usize, 71, 103] {
            samples[i * 2] = 0.8;
            samples[i * 2 + 1] = 0.8;
        }
        let detected = detect_impulses(&samples, 16);
        assert_eq!(correlate(&sent, &detected, 20), [7, 7, 7]);
        assert_eq!(percentile(&[1, 2, 3, 100], 50), Some(2));
        assert_eq!(percentile(&[1, 2, 3, 100], 95), Some(100));
    }

    #[test]
    fn 外部闭环未确认时明确报告且不开流() {
        let cfg = parse_args(args(&[
            "measure",
            "--run",
            "--source",
            "capture",
            "--input-device",
            "cable-output",
            "--output-device",
            "render-headphone",
        ]))
        .unwrap()
        .unwrap();
        let factory = FakeFactory::working(Duration::ZERO, Duration::ZERO);
        let report = execute(&cfg, factory.as_ref(), true).unwrap();
        assert_eq!(report["status"], "external-loopback-required");
        assert!(report["latency"].is_null());
        assert!(factory.opened_capture.lock().unwrap().is_empty());
    }

    #[test]
    fn 验收判定拒绝超延迟_非零xrun与相关不完整() {
        assert!(acceptance_failures(true, Some(480), 1_440, 0).is_empty());
        assert_eq!(
            acceptance_failures(true, Some(1_441), 1_440, 0),
            ["latency-budget-exceeded"]
        );
        assert_eq!(
            acceptance_failures(true, Some(480), 1_440, 2),
            ["xrun-detected"]
        );
        assert_eq!(
            acceptance_failures(false, None, 1_440, 0),
            ["insufficient-correlation"]
        );
    }

    #[test]
    fn fake驱动按固定帧终止并产出性能与xrun字段() {
        let cfg = parse_args(args(&[
            "measure",
            "--run",
            "--input-device",
            "render-default",
            "--output-device",
            "render-default",
            "--block",
            "16",
            "--pulses",
            "3",
            "--pulse-interval",
            "32",
            "--frames",
            "128",
            "--max-latency",
            "64",
        ]))
        .unwrap()
        .unwrap();
        let factory = FakeFactory::working(Duration::ZERO, Duration::ZERO);
        let report = execute(&cfg, factory.as_ref(), true).unwrap();
        assert!(matches!(report["status"].as_str(), Some("pass" | "fail")));
        assert_eq!(report["schemaVersion"], 2);
        assert_eq!(report["diagnosticScope"], "low-level-wasapi");
        assert_eq!(
            report["measurements"]["servicePipeline"]["status"],
            "not-measured"
        );
        assert_eq!(report["acceptance"]["failures"].is_array(), true);
        assert_eq!(report["acceptance"]["latencyLimitMs"], 30);
        assert_eq!(report["observed"]["renderedFrames"], 128);
        assert_eq!(report["xruns"]["total"], 0);
        assert_eq!(
            report["measurements"]["lowLevelWasapi"]["xruns"]["total"],
            0
        );
        assert_eq!(
            report["performance"]["cpuScope"],
            "low-level-wasapi-diagnostic-process-total"
        );
        assert!(report["performance"]["framesPerSecond"].as_f64().unwrap() > 0.0);
        assert_eq!(factory.opened_loopback.lock().unwrap().len(), 1);
        assert_eq!(factory.opened_render.lock().unwrap().len(), 1);
    }
}
