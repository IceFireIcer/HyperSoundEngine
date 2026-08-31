//! HyperSoundEngine 1-22 级主链；第 22 级通过 hrtf-core 可选启用。
use crate::{
    bass_enhancer::{BassEnhancerSettings, BassEnhancerStage},
    biquad::BiquadStage,
    compressor::{CompressorSettings, CompressorStage},
    convolver::{ConvolverOptions, ConvolverStage},
    deesser::{DeesserSettings, DeesserStage},
    dynamic_eq::{DynamicEqBandParam, DynamicEqParams, DynamicEqStage},
    eq_chain::{EqBandParam, EqChainStage},
    fdn_reverb::{FdnReverbParams, FdnReverbStage},
    fft::Fft,
    limiter::{LimiterSettings, LimiterStage},
    loudness_comp::{LoudnessBandParam, LoudnessCompSettings, LoudnessCompStage},
    lufs_meter::LufsMeter,
    mid_side::MidSideStage,
    mod_effects::{
        ChorusSettings, DelaySettings, FlangerSettings, ModEffectsSettings, ModEffectsStage,
        PhaserSettings, TremoloSettings,
    },
    modulation_matrix::{
        EnvelopeParams, LfoParams, LfoShape, ModSource, ModTarget, ModulationMatrixStage,
        ModulationRoute,
    },
    reverb_simple::{ReverbSimpleParams, ReverbSimpleStage},
    Stage,
};
use hrtf_core::{
    relative_direction_pose, BinauralRenderer, ConvolutionMode, DistanceModel, DistanceParams,
    HrtfGrid, InterpolationMode, ObjectEffects, ObjectInput, RenderProfile, RoomPreset, Vec3,
    WorldListenerPose,
};
use serde_json::{json, Map, Value};
const W: usize = 2048;
const IEQ: [f64; 10] = [
    31.5, 63., 125., 250., 500., 1000., 2000., 4000., 8000., 16000.,
];
const XO: [f64; 4] = [200., 800., 2500., 8000.];
const IDS: [&str; 22] = [
    "loudness-normalization",
    "surround3d",
    "mid-side",
    "pre-eq",
    "deesser",
    "compressor",
    "night-mode",
    "delay",
    "chorus",
    "flanger",
    "phaser",
    "tremolo",
    "reverb",
    "bass-enhancer",
    "loudness-compensation",
    "ieq-post",
    "analysis",
    "dynamic-eq",
    "lufs",
    "mod-master-gain",
    "limiter",
    "spatial",
];
#[derive(Debug, Clone)]
pub struct EngineChainParams {
    value: Value,
}
impl EngineChainParams {
    pub fn from_overrides(fs: f64, overrides: &Value) -> Result<Self, String> {
        if !fs.is_finite() || fs <= 0.0 {
            return Err("invalid sample rate".into());
        }
        let mut value = defaults(fs);
        merge(&mut value, overrides)?;
        let mode = value
            .pointer("/spatial/mode")
            .and_then(Value::as_str)
            .ok_or("/spatial/mode 必须是字符串")?;
        if !["off", "instant", "headLocked", "world", "stage"].contains(&mode) {
            return Err(format!("/spatial/mode 未知枚举值 {mode:?}"));
        }
        Ok(Self { value })
    }
    pub fn as_value(&self) -> &Value {
        &self.value
    }
}
fn merge(dst: &mut Value, src: &Value) -> Result<(), String> {
    let s = src.as_object().ok_or("params.overrides 必须是对象")?;
    let d = dst
        .as_object_mut()
        .ok_or("engine-chain 默认参数必须是对象")?;
    for (k, v) in s {
        if v.is_object() && d.get(k).is_some_and(Value::is_object) {
            let child = d
                .get_mut(k)
                .ok_or_else(|| format!("params.overrides.{k} 合并失败"))?;
            merge(child, v)?
        } else {
            d.insert(k.clone(), v.clone());
        }
    }
    Ok(())
}
fn defaults(fs: f64) -> Value {
    json!({"sampleRate":fs,"eq":{"enabled":true,"mode":"pro","simpleBands":[0,0,0,0,0],"proBands":[{"frequency":31.5,"gain":0,"q":1.1},{"frequency":63,"gain":0,"q":1.1},{"frequency":125,"gain":0,"q":1.1},{"frequency":250,"gain":0,"q":1.1},{"frequency":500,"gain":0,"q":1.1},{"frequency":1000,"gain":0,"q":1.1},{"frequency":2000,"gain":0,"q":1.1},{"frequency":4000,"gain":0,"q":1.1},{"frequency":8000,"gain":0,"q":1.1},{"frequency":16000,"gain":0,"q":1.1}],"bandCount":10,"qCompensation":true},"deesser":{"enabled":false,"centerHz":6000,"q":0.7,"thresholdDb":-30,"ratio":8,"attackMs":1,"releaseMs":80,"splitBand":true,"mix":1,"sidechainEnabled":false},"compressor":{"enabled":false,"thresholdDb":-20,"ratio":4,"kneeDb":6,"attackMs":10,"releaseMs":150,"makeupDb":0,"outputGain":1,"sidechainEnabled":false},"nightMode":{"enabled":false,"amount":0},"bassEnhancer":{"enabled":false,"cutoffHz":90,"q":0.7,"harmonicType":"odd","harmonicGain":0.6,"mix":0.5,"levelDb":0,"lowBoostDb":0},"reverb":{"enabled":false,"mode":"algorithmic","algorithmic":{"type":"hall","roomSize":0.5,"damping":0.5,"wet":0.3,"dry":0.7,"preDelayMs":0,"width":1},"convolution":{"ir":null,"irName":null,"mix":0.3,"preDelayMs":0,"dePeriodize":true}},"surround3d":{"enabled":false,"distance":0.5,"speed":1,"angle":0,"direction":1},"loudnessCompensation":{"enabled":false,"mode":"auto","preset":"flat","bands":[],"volumePercent":80,"maxBoostDb":12,"smoothingSeconds":0.2},"loudnessNormalization":{"enabled":false,"targetLufs":-14,"maxGainDb":9,"minGainDb":-9,"useRealtimeMeter":true,"externalGainDb":0},"limiter":{"enabled":true,"thresholdDb":-1,"lookaheadMs":5,"attackMs":0.5,"releaseMs":150,"truePeak":true},"ieq":{"enabled":false,"strength":0.5,"targetCurve":"flat","timeConstantSec":3},"dynamicEq":{"enabled":false,"strength":0.5,"thresholdDb":-20,"ratio":2,"attackMs":20,"releaseMs":200,"bands":[{"enabled":true,"targetGainDb":0},{"enabled":true,"targetGainDb":0},{"enabled":true,"targetGainDb":0},{"enabled":true,"targetGainDb":0},{"enabled":true,"targetGainDb":0}]},"pitch":{"enabled":false,"voiceBalance":0},"modulation":{"enabled":false,"lfo":{"shape":"sine","rateHz":1,"depth":0.5},"envelope":{"attackMs":10,"releaseMs":200,"amount":0.5},"routes":[]},"modEffects":{"delay":{"enabled":false,"delayMs":250,"feedback":0.3,"mix":0.3},"chorus":{"enabled":false,"rateHz":1,"depthMs":3,"mix":0.4},"flanger":{"enabled":false,"rateHz":0.5,"depthMs":2,"feedback":0.4,"mix":0.5},"phaser":{"enabled":false,"rateHz":0.5,"depth":0.5,"feedback":0.4,"mix":0.5,"stages":4},"tremolo":{"enabled":false,"rateHz":5,"depth":0.5,"mix":1}},"spatial":{"mode":"off","masterGain":0.9,"instant":{"spreadDeg":60,"amount":0.7,"room":"studio","roomAmount":0.15,"multichannelAuto":false},"headLocked":{"layout":"51","speakers":[{"azimuthDeg":0,"elevationDeg":0,"distance":1.5,"gain":1,"size":0},{"azimuthDeg":-30,"elevationDeg":0,"distance":1.5,"gain":1,"size":0},{"azimuthDeg":30,"elevationDeg":0,"distance":1.5,"gain":1,"size":0},{"azimuthDeg":-110,"elevationDeg":0,"distance":1.5,"gain":1,"size":0},{"azimuthDeg":110,"elevationDeg":0,"distance":1.5,"gain":1,"size":0}],"heightLayer":true,"bottomLayer":true,"routes":[]},"world":{"moveSpeed":2,"listener":{"position":{"x":0,"y":1.6,"z":0},"yaw":0,"pitch":0,"roll":0},"sources":[{"id":"vocal","position":{"x":-2,"y":1.6,"z":4},"gain":1,"size":0},{"id":"guitar","position":{"x":-5,"y":1.6,"z":6},"gain":1,"size":0},{"id":"drums","position":{"x":3,"y":1.6,"z":7},"gain":1,"size":0},{"id":"ambience","position":{"x":0,"y":2.5,"z":10},"gain":0.6,"size":0.5}],"playhead":0,"trajectories":[],"occlusion":0},"stage":{"preset":"stage","seat":"middle","roomSize":1,"reverbAmount":0.35,"customSources":[]},"ambience":{"enabled":false,"amount":0.3},"convolution":"partitioned","hrtfInterp":"nearest","distanceModel":"inverse","refDistance":1,"maxDistance":50},"stereoWidth":1})
}
fn o<'a>(v: &'a Value, p: &str) -> Result<&'a Map<String, Value>, String> {
    v.pointer(p)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("缺少对象 {p}"))
}
fn n(o: &Map<String, Value>, k: &str) -> Result<f64, String> {
    let value = o
        .get(k)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("{k} 必须是数字"))?;
    if !value.is_finite() {
        return Err(format!("{k} 必须是有限数字"));
    }
    Ok(value)
}
fn b(o: &Map<String, Value>, k: &str) -> Result<bool, String> {
    o.get(k)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("{k} 必须是布尔"))
}
fn finite_number(o: &Map<String, Value>, k: &str, path: &str) -> Result<f64, String> {
    let value = o
        .get(k)
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("{path} 必须是数字"))?;
    if !value.is_finite() {
        return Err(format!("{path} 必须是有限数字"));
    }
    Ok(value)
}
fn enum_value<'a>(
    o: &'a Map<String, Value>,
    k: &str,
    path: &str,
    allowed: &[&str],
) -> Result<&'a str, String> {
    let value = o
        .get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{path} 必须是字符串"))?;
    if !allowed.contains(&value) {
        return Err(format!("{path} 未知枚举值 {value:?}"));
    }
    Ok(value)
}
fn convolution_ir(v: &Value) -> Result<Option<Vec<f32>>, String> {
    let convolution = o(v, "/reverb/convolution")?;
    let Some(value) = convolution.get("ir") else {
        return Err("缺少 /reverb/convolution/ir".to_string());
    };
    if value.is_null() {
        return Ok(None);
    }
    let array = value
        .as_array()
        .ok_or_else(|| "/reverb/convolution/ir 必须是数组或 null".to_string())?;
    if array.is_empty() {
        return Ok(None);
    }
    let mut ir = Vec::with_capacity(array.len());
    for (index, sample) in array.iter().enumerate() {
        let value = sample
            .as_f64()
            .ok_or_else(|| format!("/reverb/convolution/ir/{index} 必须是数字"))?;
        let sample = value as f32;
        if !value.is_finite() || !sample.is_finite() {
            return Err(format!("/reverb/convolution/ir/{index} 必须是有限 f32"));
        }
        ir.push(sample);
    }
    Ok(Some(ir))
}

const MAX_SPATIAL_OBJECTS: usize = 32;
const SPATIAL_PRIMARY_SLOTS: usize = 64;
const SPATIAL_SLOT_CAPACITY: usize = SPATIAL_PRIMARY_SLOTS * 2;
const SPATIAL_SOFT_CLIP_THRESHOLD: f32 = 0.85;
const AMBIENCE_CHANNELS: usize = 4;
const AMBIENCE_AZIMUTHS: [f64; AMBIENCE_CHANNELS] = [45.0, 135.0, 225.0, 315.0];
const AMBIENCE_DELAY_MS: [f64; AMBIENCE_CHANNELS] = [20.0, 28.0, 36.0, 44.0];

#[derive(Debug, Clone, Copy)]
struct SpatialSpeaker {
    slot: usize,
    channel: usize,
    azimuth_deg: f32,
    elevation_deg: f32,
    distance: f32,
    gain: f32,
    size: f32,
}

struct AmbienceState {
    amount: f64,
    smooth: f64,
    current: [f64; AMBIENCE_CHANNELS],
    lines: [Vec<f32>; AMBIENCE_CHANNELS],
    positions: [usize; AMBIENCE_CHANNELS],
    pan_left: [f64; AMBIENCE_CHANNELS],
    pan_right: [f64; AMBIENCE_CHANNELS],
}

impl AmbienceState {
    fn new(sample_rate: f64, amount: f64) -> Self {
        let delays =
            AMBIENCE_DELAY_MS.map(|ms| ((ms * sample_rate / 1000.0).round() as usize).max(1));
        Self {
            amount: amount.clamp(0.0, 1.0),
            smooth: (-1.0 / (sample_rate.max(1.0) * 0.02)).exp(),
            current: [0.0; AMBIENCE_CHANNELS],
            lines: std::array::from_fn(|index| vec![0.0; delays[index]]),
            positions: [0; AMBIENCE_CHANNELS],
            pan_left: AMBIENCE_AZIMUTHS.map(|azimuth| {
                let pan = (azimuth.to_radians().sin() + 1.0) * 0.5;
                (1.0 - pan).sqrt()
            }),
            pan_right: AMBIENCE_AZIMUTHS.map(|azimuth| {
                let pan = (azimuth.to_radians().sin() + 1.0) * 0.5;
                pan.sqrt()
            }),
        }
    }

    fn process_add(
        &mut self,
        input_left: &[f32],
        input_right: &[f32],
        output_left: &mut [f32],
        output_right: &mut [f32],
    ) {
        let frames = input_left.len();
        if frames == 0 || self.amount == 0.0 {
            return;
        }
        let mut mid_energy = 0.0;
        let mut side_energy = 0.0;
        for frame in 0..frames {
            let mid = (f64::from(input_left[frame]) + f64::from(input_right[frame])) * 0.5;
            let side = (f64::from(input_left[frame]) - f64::from(input_right[frame])) * 0.5;
            mid_energy += mid * mid;
            side_energy += side * side;
        }
        let w = (2.0 * mid_energy / frames as f64).sqrt();
        let y = (2.0 * side_energy / frames as f64).sqrt();
        let mix = self.amount * 0.5;
        let targets = AMBIENCE_AZIMUTHS
            .map(|azimuth| (w / 2.0 + azimuth.to_radians().sin() * y).clamp(-1.0, 1.0));
        for frame in 0..frames {
            let mut add_left = 0.0;
            let mut add_right = 0.0;
            for channel in 0..AMBIENCE_CHANNELS {
                let position = self.positions[channel];
                let delayed = self.lines[channel][position];
                self.lines[channel][position] = if channel < 2 {
                    input_right[frame]
                } else {
                    input_left[frame]
                };
                self.positions[channel] = (position + 1) % self.lines[channel].len();
                let gain =
                    targets[channel] + self.smooth * (self.current[channel] - targets[channel]);
                self.current[channel] = gain;
                add_left += f64::from(delayed) * gain * self.pan_left[channel];
                add_right += f64::from(delayed) * gain * self.pan_right[channel];
            }
            output_left[frame] += (add_left * mix) as f32;
            output_right[frame] += (add_right * mix) as f32;
        }
    }

    fn reset(&mut self) {
        self.current.fill(0.0);
        self.positions.fill(0);
        for line in &mut self.lines {
            line.fill(0.0);
        }
    }
}

struct SpatialStage {
    renderer: BinauralRenderer,
    speakers: Vec<SpatialSpeaker>,
    amount: f32,
    master_gain: f32,
    input_left: Vec<f32>,
    input_right: Vec<f32>,
    ambience: Option<AmbienceState>,
    dry_delay_left: Vec<f32>,
    dry_delay_right: Vec<f32>,
    dry_delay_position: usize,
    dry_left: Vec<f32>,
    dry_right: Vec<f32>,
}

impl SpatialStage {
    fn new(
        grid: HrtfGrid,
        speakers: Vec<SpatialSpeaker>,
        amount: f32,
        master_gain: f32,
        distance_model: DistanceModel,
        distance_params: DistanceParams,
        ambience_amount: f64,
        sample_rate: f64,
    ) -> Result<Self, String> {
        if speakers.is_empty() || speakers.len() > MAX_SPATIAL_OBJECTS {
            return Err(format!(
                "stage22 对象数必须为 1..={MAX_SPATIAL_OBJECTS}，实际 {}",
                speakers.len()
            ));
        }
        let renderer = BinauralRenderer::new(
            grid,
            RenderProfile::LowLatency,
            distance_model,
            distance_params,
        )
        .map_err(|error| format!("无法构造 HRTF renderer：{error}"))?;
        Ok(Self {
            renderer,
            speakers,
            amount,
            master_gain,
            input_left: Vec::new(),
            input_right: Vec::new(),
            ambience: (ambience_amount > 0.0)
                .then(|| AmbienceState::new(sample_rate, ambience_amount)),
            dry_delay_left: Vec::new(),
            dry_delay_right: Vec::new(),
            dry_delay_position: 0,
            dry_left: Vec::new(),
            dry_right: Vec::new(),
        })
    }

    fn prepare(&mut self, max_frames: usize) {
        self.input_left.resize(max_frames, 0.0);
        self.input_right.resize(max_frames, 0.0);
        self.dry_left.resize(max_frames, 0.0);
        self.dry_right.resize(max_frames, 0.0);
        let latency = self.renderer.latency_samples();
        self.dry_delay_left.resize(latency.max(1), 0.0);
        self.dry_delay_right.resize(latency.max(1), 0.0);
        self.dry_delay_position = 0;
        self.renderer
            .prepare(SPATIAL_SLOT_CAPACITY, max_frames)
            .expect("有效的 stage22 prepare 容量");
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        assert_eq!(left.len(), right.len(), "左右声道块长必须一致");
        assert!(
            left.len() <= self.input_left.len(),
            "stage22 块长超过 prepare 容量"
        );
        let frames = left.len();
        self.input_left[..frames].copy_from_slice(left);
        self.input_right[..frames].copy_from_slice(right);
        let silence = &self.input_left[..0];
        let mut objects = [ObjectInput {
            slot: 0,
            mono: silence,
            azimuth_deg: 0.0,
            elevation_deg: 0.0,
            distance: 1.0,
            gain: 0.0,
        }; SPATIAL_SLOT_CAPACITY];
        let mut effects = [ObjectEffects::default(); SPATIAL_SLOT_CAPACITY];
        let mut object_count = 0;
        for speaker in &self.speakers {
            let mono = if speaker.channel == 0 {
                &self.input_left[..frames]
            } else {
                &self.input_right[..frames]
            };
            objects[object_count] = ObjectInput {
                slot: speaker.slot,
                mono,
                azimuth_deg: speaker.azimuth_deg,
                elevation_deg: speaker.elevation_deg,
                distance: speaker.distance,
                gain: speaker.gain,
            };
            effects[object_count] = ObjectEffects {
                size: speaker.size,
                spread_slot: (speaker.size > 0.0).then_some(speaker.slot + SPATIAL_PRIMARY_SLOTS),
            };
            object_count += 1;
        }
        self.renderer
            .process_with_effects(
                &objects[..object_count],
                &effects[..object_count],
                left,
                right,
                frames,
            )
            .expect("stage22 已在 prepare/参数解析阶段验证");
        if self.amount < 1.0 {
            let dry = 1.0 - self.amount;
            if self.renderer.latency_samples() > 0 {
                for frame in 0..frames {
                    let position = self.dry_delay_position;
                    self.dry_left[frame] = self.dry_delay_left[position];
                    self.dry_right[frame] = self.dry_delay_right[position];
                    self.dry_delay_left[position] = self.input_left[frame];
                    self.dry_delay_right[position] = self.input_right[frame];
                    self.dry_delay_position = (position + 1) % self.dry_delay_left.len();
                }
            } else {
                self.dry_left[..frames].copy_from_slice(&self.input_left[..frames]);
                self.dry_right[..frames].copy_from_slice(&self.input_right[..frames]);
            }
            for frame in 0..frames {
                left[frame] =
                    (self.dry_left[frame] * dry + left[frame] * self.amount) * self.master_gain;
                right[frame] =
                    (self.dry_right[frame] * dry + right[frame] * self.amount) * self.master_gain;
            }
        } else if self.master_gain != 1.0 {
            for frame in 0..frames {
                left[frame] *= self.master_gain;
                right[frame] *= self.master_gain;
            }
        }
        if let Some(ambience) = &mut self.ambience {
            ambience.process_add(
                &self.input_left[..frames],
                &self.input_right[..frames],
                &mut left[..frames],
                &mut right[..frames],
            );
        }
        for frame in 0..frames {
            left[frame] = spatial_soft_clip(left[frame]);
            right[frame] = spatial_soft_clip(right[frame]);
        }
    }

    fn reset(&mut self) {
        self.renderer.reset();
        if let Some(ambience) = &mut self.ambience {
            ambience.reset();
        }
        self.input_left.fill(0.0);
        self.input_right.fill(0.0);
        self.dry_delay_left.fill(0.0);
        self.dry_delay_right.fill(0.0);
        self.dry_delay_position = 0;
        self.dry_left.fill(0.0);
        self.dry_right.fill(0.0);
    }

    fn latency_samples(&self) -> usize {
        self.renderer.latency_samples()
    }

    fn listener_velocity(&self) -> Option<Vec3> {
        self.renderer.listener_velocity()
    }
}

fn spatial_soft_clip(value: f32) -> f32 {
    let magnitude = value.abs();
    if magnitude <= SPATIAL_SOFT_CLIP_THRESHOLD {
        return value;
    }
    let threshold = SPATIAL_SOFT_CLIP_THRESHOLD;
    let clipped =
        threshold + (1.0 - threshold) * ((magnitude - threshold) / (1.0 - threshold)).tanh();
    value.signum() * clipped
}

fn spatial_stage(
    v: &Value,
    fs: f64,
    grid: Option<HrtfGrid>,
    previous: Option<&Value>,
) -> Result<Option<SpatialStage>, String> {
    let spatial = o(v, "/spatial")?;
    let mode = enum_value(
        spatial,
        "mode",
        "/spatial/mode",
        &["off", "instant", "headLocked", "world", "stage"],
    )?;
    if mode == "off" {
        return Ok(None);
    }
    let grid =
        grid.ok_or_else(|| format!("spatial.mode={mode:?} 需要先通过控制 API 注入 HRTF grid"))?;
    if grid.sample_rate() != fs.round() as u32 {
        return Err(format!(
            "HRTF grid 采样率 {}Hz 与引擎采样率 {}Hz 不一致",
            grid.sample_rate(),
            fs
        ));
    }
    let master_gain =
        optional_finite(spatial, "masterGain", 0.9, "/spatial/masterGain")?.clamp(0.5, 1.0) as f32;
    let instant = spatial.get("instant").and_then(Value::as_object);
    let amount = if mode == "instant" {
        instant
            .map(|value| optional_finite(value, "amount", 0.7, "/spatial/instant/amount"))
            .transpose()?
            .unwrap_or(0.7)
            .clamp(0.0, 1.0) as f32
    } else {
        1.0
    };
    let distance_model = match spatial
        .get("distanceModel")
        .and_then(Value::as_str)
        .unwrap_or("inverse")
    {
        "inverse" => DistanceModel::Inverse,
        "linear" => DistanceModel::Linear,
        "exponential" => DistanceModel::Exponential,
        value => return Err(format!("/spatial/distanceModel 未知枚举值 {value:?}")),
    };
    let reference_distance =
        optional_finite(spatial, "refDistance", 1.0, "/spatial/refDistance")?.max(0.1) as f32;
    let maximum_distance = optional_finite(spatial, "maxDistance", 50.0, "/spatial/maxDistance")?
        .max(reference_distance as f64 + 0.1) as f32;
    let ambience_amount = spatial
        .get("ambience")
        .and_then(Value::as_object)
        .filter(|ambience| {
            ambience
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .map(|ambience| optional_finite(ambience, "amount", 0.3, "/spatial/ambience/amount"))
        .transpose()?
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let mut stage = SpatialStage::new(
        grid,
        spatial_speakers(spatial, mode, previous)?,
        amount,
        master_gain,
        distance_model,
        DistanceParams {
            reference_distance,
            maximum_distance,
            rolloff_factor: 1.0,
        },
        ambience_amount,
        fs,
    )?;
    let convolution = spatial
        .get("convolution")
        .and_then(Value::as_str)
        .unwrap_or("time");
    stage
        .renderer
        .set_convolution_mode(match convolution {
            "partitioned" => ConvolutionMode::Partitioned,
            "time" => ConvolutionMode::Time,
            value => return Err(format!("/spatial/convolution 未知枚举值 {value:?}")),
        })
        .map_err(|error| format!("无法设置空间卷积模式：{error}"))?;
    let interpolation = spatial
        .get("hrtfInterp")
        .and_then(Value::as_str)
        .unwrap_or("nearest");
    stage
        .renderer
        .set_interpolation_mode(match interpolation {
            "nearest" => InterpolationMode::Nearest,
            "spherical" => InterpolationMode::Spherical,
            value => return Err(format!("/spatial/hrtfInterp 未知枚举值 {value:?}")),
        })
        .map_err(|error| format!("无法设置 HRTF 插值模式：{error}"))?;
    configure_spatial_room(&mut stage.renderer, spatial, mode)?;
    if mode == "world" {
        let world = required_object(spatial, "world", "/spatial/world")?;
        let occlusion = optional_finite(world, "occlusion", 0.0, "/spatial/world/occlusion")?
            .clamp(0.0, 1.0) as f32;
        stage
            .renderer
            .set_occlusion(occlusion)
            .map_err(|error| format!("无法设置遮挡：{error}"))?;
        stage
            .renderer
            .set_listener_velocity(world_velocity(world, previous)?)
            .map_err(|error| format!("无法设置听者速度：{error}"))?;
    }
    Ok(Some(stage))
}

fn optional_finite(
    object: &Map<String, Value>,
    key: &str,
    default: f64,
    path: &str,
) -> Result<f64, String> {
    match object.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or_else(|| format!("{path} 必须是有限数字")),
    }
}

fn required_object<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<&'a Map<String, Value>, String> {
    object
        .get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{path} 必须是对象"))
}

fn parse_vec3(value: &Value, path: &str) -> Result<Vec3, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{path} 必须是对象"))?;
    Ok(Vec3 {
        x: finite_number(object, "x", &format!("{path}/x"))?,
        y: finite_number(object, "y", &format!("{path}/y"))?,
        z: finite_number(object, "z", &format!("{path}/z"))?,
    })
}

fn channel_for_azimuth(azimuth_deg: f32) -> usize {
    usize::from(azimuth_deg > 0.0)
}

fn push_speaker(
    speakers: &mut Vec<SpatialSpeaker>,
    channel: usize,
    azimuth_deg: f64,
    elevation_deg: f64,
    distance: f64,
    gain: f64,
    size: f64,
) -> Result<(), String> {
    let slot = speakers.len();
    push_speaker_with_slot(
        speakers,
        slot,
        channel,
        azimuth_deg,
        elevation_deg,
        distance,
        gain,
        size,
    )
}

#[allow(clippy::too_many_arguments)]
fn push_speaker_with_slot(
    speakers: &mut Vec<SpatialSpeaker>,
    slot: usize,
    channel: usize,
    azimuth_deg: f64,
    elevation_deg: f64,
    distance: f64,
    gain: f64,
    size: f64,
) -> Result<(), String> {
    if speakers.len() == MAX_SPATIAL_OBJECTS || slot >= SPATIAL_PRIMARY_SLOTS {
        return Err(format!("stage22 最多支持 {MAX_SPATIAL_OBJECTS} 个对象"));
    }
    speakers.push(SpatialSpeaker {
        slot,
        channel,
        azimuth_deg: azimuth_deg as f32,
        elevation_deg: elevation_deg as f32,
        distance: distance.max(0.0) as f32,
        gain: gain.clamp(0.0, 2.0) as f32,
        size: size.clamp(0.0, 1.0) as f32,
    });
    Ok(())
}

fn parse_speaker(value: &Value, path: &str) -> Result<(f64, f64, f64, f64, f64), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{path} 必须是对象"))?;
    Ok((
        finite_number(object, "azimuthDeg", &format!("{path}/azimuthDeg"))?,
        finite_number(object, "elevationDeg", &format!("{path}/elevationDeg"))?,
        finite_number(object, "distance", &format!("{path}/distance"))?,
        optional_finite(object, "gain", 1.0, &format!("{path}/gain"))?,
        optional_finite(object, "size", 0.0, &format!("{path}/size"))?,
    ))
}

fn spatial_speakers(
    spatial: &Map<String, Value>,
    mode: &str,
    previous: Option<&Value>,
) -> Result<Vec<SpatialSpeaker>, String> {
    let mut speakers = Vec::new();
    match mode {
        "instant" => {
            let instant = spatial.get("instant").and_then(Value::as_object);
            let spread = instant
                .map(|value| {
                    optional_finite(value, "spreadDeg", 60.0, "/spatial/instant/spreadDeg")
                })
                .transpose()?
                .unwrap_or(60.0)
                .clamp(20.0, 120.0);
            push_speaker(&mut speakers, 0, -spread * 0.5, 0.0, 1.5, 1.0, 0.0)?;
            push_speaker(&mut speakers, 1, spread * 0.5, 0.0, 1.5, 1.0, 0.0)?;
        }
        "headLocked" => {
            let head = required_object(spatial, "headLocked", "/spatial/headLocked")?;
            let values = head
                .get("speakers")
                .and_then(Value::as_array)
                .ok_or("/spatial/headLocked/speakers 必须是数组")?;
            let routes = head.get("routes").and_then(Value::as_array);
            for (index, value) in values.iter().enumerate() {
                let (azimuth, elevation, distance, mut gain, size) =
                    parse_speaker(value, &format!("/spatial/headLocked/speakers/{index}"))?;
                if value.get("muted").and_then(Value::as_bool).unwrap_or(false) {
                    gain = 0.0;
                }
                let default_channel = channel_for_azimuth(azimuth as f32);
                match routes
                    .and_then(|routes| routes.get(index))
                    .and_then(Value::as_str)
                {
                    Some("both") => {
                        push_speaker(
                            &mut speakers,
                            0,
                            azimuth,
                            elevation,
                            distance,
                            gain * 0.5,
                            size,
                        )?;
                        push_speaker(
                            &mut speakers,
                            1,
                            azimuth,
                            elevation,
                            distance,
                            gain * 0.5,
                            size,
                        )?;
                    }
                    Some("l") => {
                        push_speaker(&mut speakers, 0, azimuth, elevation, distance, gain, size)?
                    }
                    Some("r") => {
                        push_speaker(&mut speakers, 1, azimuth, elevation, distance, gain, size)?
                    }
                    None => push_speaker(
                        &mut speakers,
                        default_channel,
                        azimuth,
                        elevation,
                        distance,
                        gain,
                        size,
                    )?,
                    Some(value) => {
                        return Err(format!(
                            "/spatial/headLocked/routes/{index} 未知枚举值 {value:?}"
                        ))
                    }
                }
            }
        }
        "world" => world_speakers(spatial, previous, &mut speakers)?,
        "stage" => stage_speakers(spatial, &mut speakers)?,
        _ => unreachable!("mode validated before speaker projection"),
    }
    Ok(speakers)
}

fn stable_slot_seed(id: &str) -> usize {
    let mut hash = 0x811c_9dc5_u32;
    for unit in id.encode_utf16() {
        hash ^= unit as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash as usize % SPATIAL_PRIMARY_SLOTS
}

fn assign_stable_slots(ids: &[&str]) -> Result<std::collections::HashMap<String, usize>, String> {
    if ids.len() > SPATIAL_PRIMARY_SLOTS {
        return Err("stage22 稳定 slot 历史集合超过容量".to_string());
    }
    let mut sorted = ids.to_vec();
    sorted.sort_unstable();
    let mut occupied = [false; SPATIAL_PRIMARY_SLOTS];
    let mut assigned = std::collections::HashMap::with_capacity(sorted.len());
    for id in sorted {
        let seed = stable_slot_seed(id);
        let slot = (0..SPATIAL_PRIMARY_SLOTS)
            .map(|offset| (seed + offset) % SPATIAL_PRIMARY_SLOTS)
            .find(|slot| !occupied[*slot])
            .ok_or("stage22 稳定 slot 已耗尽")?;
        occupied[slot] = true;
        assigned.insert(id.to_owned(), slot);
    }
    Ok(assigned)
}

fn world_speakers(
    spatial: &Map<String, Value>,
    previous: Option<&Value>,
    speakers: &mut Vec<SpatialSpeaker>,
) -> Result<(), String> {
    let world = required_object(spatial, "world", "/spatial/world")?;
    let listener = required_object(world, "listener", "/spatial/world/listener")?;
    let pose = WorldListenerPose {
        position: parse_vec3(
            listener
                .get("position")
                .ok_or("缺少 /spatial/world/listener/position")?,
            "/spatial/world/listener/position",
        )?,
        yaw_deg: optional_finite(listener, "yaw", 0.0, "/spatial/world/listener/yaw")?,
        pitch_deg: optional_finite(listener, "pitch", 0.0, "/spatial/world/listener/pitch")?,
        roll_deg: optional_finite(listener, "roll", 0.0, "/spatial/world/listener/roll")?,
    };
    let playhead = optional_finite(world, "playhead", 0.0, "/spatial/world/playhead")?;
    let trajectories = world.get("trajectories").and_then(Value::as_array);
    let sources = world
        .get("sources")
        .and_then(Value::as_array)
        .ok_or("/spatial/world/sources 必须是数组")?;
    let mut ids = std::collections::HashSet::new();
    let mut source_ids = Vec::with_capacity(sources.len());
    for (index, value) in sources.iter().enumerate() {
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("/spatial/world/sources/{index}/id 必须是字符串"))?;
        if id.is_empty() || !ids.insert(id) {
            return Err(format!("/spatial/world/sources/{index}/id 必须非空且唯一"));
        }
        source_ids.push(id);
    }
    let mut slot_ids = source_ids.clone();
    if let Some(previous_sources) = previous
        .and_then(|value| value.pointer("/spatial/world/sources"))
        .and_then(Value::as_array)
    {
        for id in previous_sources
            .iter()
            .filter_map(|source| source.get("id").and_then(Value::as_str))
        {
            if !slot_ids.contains(&id) {
                slot_ids.push(id);
            }
        }
    }
    let slots = assign_stable_slots(&slot_ids)?;
    for (index, value) in sources.iter().enumerate() {
        let path = format!("/spatial/world/sources/{index}");
        let source = value
            .as_object()
            .ok_or_else(|| format!("{path} 必须是对象"))?;
        let id = source_ids[index];
        let slot = slots[id];
        let static_position = parse_vec3(
            source
                .get("position")
                .ok_or_else(|| format!("缺少 {path}/position"))?,
            &format!("{path}/position"),
        )?;
        let position = trajectory_position(trajectories, id, playhead)?.unwrap_or(static_position);
        let direction = relative_direction_pose(pose, position);
        let gain = optional_finite(source, "gain", 1.0, &format!("{path}/gain"))?;
        let size = optional_finite(source, "size", 0.0, &format!("{path}/size"))?;
        push_speaker_with_slot(
            speakers,
            slot,
            channel_for_azimuth(direction.azimuth_deg as f32),
            direction.azimuth_deg,
            direction.elevation_deg,
            direction.distance,
            gain,
            size,
        )?;
    }
    Ok(())
}

fn trajectory_position(
    trajectories: Option<&Vec<Value>>,
    source_id: &str,
    playhead: f64,
) -> Result<Option<Vec3>, String> {
    let Some(trajectory) = trajectories.and_then(|items| {
        items
            .iter()
            .find(|item| item.get("sourceId").and_then(Value::as_str) == Some(source_id))
    }) else {
        return Ok(None);
    };
    let keyframes = trajectory
        .get("keyframes")
        .and_then(Value::as_array)
        .ok_or("/spatial/world/trajectories/keyframes 必须是数组")?;
    if keyframes.is_empty() {
        return Ok(Some(Vec3 {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        }));
    }
    let mut before: Option<(f64, Vec3)> = None;
    let mut after: Option<(f64, Vec3)> = None;
    for (index, keyframe) in keyframes.iter().enumerate() {
        let object = keyframe
            .as_object()
            .ok_or_else(|| format!("/spatial/world/trajectories/keyframes/{index} 必须是对象"))?;
        let time = finite_number(
            object,
            "t",
            &format!("/spatial/world/trajectories/keyframes/{index}/t"),
        )?;
        let position = parse_vec3(
            object.get("position").ok_or_else(|| {
                format!("缺少 /spatial/world/trajectories/keyframes/{index}/position")
            })?,
            &format!("/spatial/world/trajectories/keyframes/{index}/position"),
        )?;
        if time <= playhead && before.is_none_or(|current| time > current.0) {
            before = Some((time, position));
        }
        if time >= playhead && after.is_none_or(|current| time < current.0) {
            after = Some((time, position));
        }
    }
    let first = before.or(after).expect("non-empty keyframes");
    let last = after.or(before).expect("non-empty keyframes");
    if first.0 == last.0 {
        return Ok(Some(first.1));
    }
    let factor = (playhead - first.0) / (last.0 - first.0);
    Ok(Some(Vec3 {
        x: first.1.x + (last.1.x - first.1.x) * factor,
        y: first.1.y + (last.1.y - first.1.y) * factor,
        z: first.1.z + (last.1.z - first.1.z) * factor,
    }))
}

const STAGE: &[(f64, f64, f64)] = &[
    (0.0, 0.0, 2.5),
    (-30.0, 0.0, 4.0),
    (30.0, 0.0, 4.0),
    (10.0, 0.0, 6.0),
    (-20.0, 0.0, 5.0),
    (-110.0, 0.0, 8.0),
    (110.0, 0.0, 8.0),
];
const CINEMA: &[(f64, f64, f64)] = &[
    (0.0, 0.0, 4.0),
    (-30.0, 0.0, 4.0),
    (30.0, 0.0, 4.0),
    (-100.0, 0.0, 7.0),
    (100.0, 0.0, 7.0),
    (-135.0, 0.0, 7.0),
    (135.0, 0.0, 7.0),
    (-45.0, 45.0, 5.0),
    (45.0, 45.0, 5.0),
    (-135.0, 45.0, 5.0),
    (135.0, 45.0, 5.0),
];
const PIANO: &[(f64, f64, f64)] = &[
    (0.0, 0.0, 2.0),
    (-90.0, 0.0, 9.0),
    (90.0, 0.0, 9.0),
    (180.0, 0.0, 10.0),
];
const NATURE: &[(f64, f64, f64)] = &[
    (0.0, 50.0, 7.0),
    (180.0, 0.0, 15.0),
    (-140.0, 20.0, 8.0),
    (110.0, 0.0, 6.0),
];

fn stage_speakers(
    spatial: &Map<String, Value>,
    speakers: &mut Vec<SpatialSpeaker>,
) -> Result<(), String> {
    let stage = required_object(spatial, "stage", "/spatial/stage")?;
    let preset = stage
        .get("preset")
        .and_then(Value::as_str)
        .unwrap_or("stage");
    let layout = match preset {
        "stage" => STAGE,
        "cinema" => CINEMA,
        "piano" => PIANO,
        "nature" => NATURE,
        value => return Err(format!("/spatial/stage/preset 未知枚举值 {value:?}")),
    };
    let seat_scale = match stage
        .get("seat")
        .and_then(Value::as_str)
        .unwrap_or("middle")
    {
        "front" => 0.8,
        "middle" => 1.0,
        "back" => 1.35,
        value => return Err(format!("/spatial/stage/seat 未知枚举值 {value:?}")),
    };
    let room_scale =
        optional_finite(stage, "roomSize", 1.0, "/spatial/stage/roomSize")?.clamp(0.5, 2.0);
    for &(azimuth, elevation, distance) in layout {
        push_speaker(
            speakers,
            channel_for_azimuth(azimuth as f32),
            azimuth,
            elevation,
            (distance * seat_scale * room_scale).clamp(0.5, 10.0),
            1.0,
            0.0,
        )?;
    }
    if let Some(custom) = stage.get("customSources").and_then(Value::as_array) {
        let listener = WorldListenerPose {
            position: Vec3 {
                x: 0.0,
                y: 1.6,
                z: 0.0,
            },
            yaw_deg: 0.0,
            pitch_deg: 0.0,
            roll_deg: 0.0,
        };
        for (index, value) in custom.iter().enumerate() {
            let object = value
                .as_object()
                .ok_or_else(|| format!("/spatial/stage/customSources/{index} 必须是对象"))?;
            let position = parse_vec3(
                object
                    .get("position")
                    .ok_or_else(|| format!("缺少 /spatial/stage/customSources/{index}/position"))?,
                &format!("/spatial/stage/customSources/{index}/position"),
            )?;
            let direction = relative_direction_pose(listener, position);
            push_speaker(
                speakers,
                channel_for_azimuth(direction.azimuth_deg as f32),
                direction.azimuth_deg,
                direction.elevation_deg,
                direction.distance,
                optional_finite(
                    object,
                    "gain",
                    1.0,
                    &format!("/spatial/stage/customSources/{index}/gain"),
                )?,
                optional_finite(
                    object,
                    "size",
                    0.0,
                    &format!("/spatial/stage/customSources/{index}/size"),
                )?,
            )?;
        }
    }
    Ok(())
}

fn configure_spatial_room(
    renderer: &mut BinauralRenderer,
    spatial: &Map<String, Value>,
    mode: &str,
) -> Result<(), String> {
    let (preset, amount, scale) = if mode == "stage" {
        let stage = required_object(spatial, "stage", "/spatial/stage")?;
        let preset = match stage
            .get("preset")
            .and_then(Value::as_str)
            .unwrap_or("stage")
        {
            "stage" => Some(RoomPreset::Stage),
            "cinema" | "piano" => Some(RoomPreset::Hall),
            "nature" => Some(RoomPreset::Outdoor),
            _ => unreachable!(),
        };
        (
            preset,
            optional_finite(stage, "reverbAmount", 0.35, "/spatial/stage/reverbAmount")?
                .clamp(0.0, 1.0) as f32,
            optional_finite(stage, "roomSize", 1.0, "/spatial/stage/roomSize")?.clamp(0.5, 2.0)
                as f32,
        )
    } else if mode == "instant" {
        let instant = spatial.get("instant").and_then(Value::as_object);
        let room_name = instant
            .and_then(|value| value.get("room"))
            .and_then(Value::as_str)
            .unwrap_or("studio");
        let preset = match room_name {
            "off" => None,
            "studio" => Some(RoomPreset::Studio),
            "hall" => Some(RoomPreset::Hall),
            "stage" => Some(RoomPreset::Stage),
            "church" => Some(RoomPreset::Church),
            "outdoor" => Some(RoomPreset::Outdoor),
            "bathroom" => Some(RoomPreset::Bathroom),
            "corridor" => Some(RoomPreset::Corridor),
            value => return Err(format!("/spatial/instant/room 未知枚举值 {value:?}")),
        };
        let amount = instant
            .map(|value| optional_finite(value, "roomAmount", 0.15, "/spatial/instant/roomAmount"))
            .transpose()?
            .unwrap_or(0.15)
            .clamp(0.0, 1.0) as f32;
        (preset, amount, 1.0)
    } else {
        (None, 0.0, 1.0)
    };
    let params = preset.map(|preset| {
        let mut params = preset.params();
        params.width *= scale;
        params.height *= scale;
        params.depth *= scale;
        params
    });
    renderer
        .set_room(params)
        .map_err(|error| format!("无法设置空间房间：{error}"))?;
    renderer
        .set_room_amount(if preset.is_some() { amount } else { 0.0 })
        .map_err(|error| format!("无法设置空间房间混合：{error}"))?;
    Ok(())
}

fn world_velocity(
    world: &Map<String, Value>,
    previous: Option<&Value>,
) -> Result<Option<Vec3>, String> {
    let Some(previous_world) = previous
        .and_then(|value| value.pointer("/spatial"))
        .filter(|value| value.get("mode").and_then(Value::as_str) == Some("world"))
        .and_then(|value| value.get("world"))
        .and_then(Value::as_object)
    else {
        return Ok(None);
    };
    let playhead = optional_finite(world, "playhead", 0.0, "/spatial/world/playhead")?;
    let previous_playhead = optional_finite(
        previous_world,
        "playhead",
        0.0,
        "/previous/spatial/world/playhead",
    )?;
    let delta = playhead - previous_playhead;
    if delta <= 0.0 {
        return Ok(None);
    }
    let current_listener = required_object(world, "listener", "/spatial/world/listener")?;
    let previous_listener = required_object(
        previous_world,
        "listener",
        "/previous/spatial/world/listener",
    )?;
    let current = parse_vec3(
        current_listener
            .get("position")
            .ok_or("缺少 /spatial/world/listener/position")?,
        "/spatial/world/listener/position",
    )?;
    let old = parse_vec3(
        previous_listener
            .get("position")
            .ok_or("缺少 /previous/spatial/world/listener/position")?,
        "/previous/spatial/world/listener/position",
    )?;
    Ok(Some(Vec3 {
        x: (current.x - old.x) / delta,
        y: (current.y - old.y) / delta,
        z: (current.z - old.z) / delta,
    }))
}

pub struct EngineChainStage {
    fs: f64,
    eq: EqChainStage,
    ms: MidSideStage,
    de: DeesserStage,
    cp: CompressorStage,
    ncp: CompressorStage,
    nsl: BiquadStage,
    nsr: BiquadStage,
    me: ModEffectsStage,
    rv: ReverbSimpleStage,
    fdn: FdnReverbStage,
    conv: Option<ConvolverStage>,
    bass: BassEnhancerStage,
    lc: LoudnessCompStage,
    ieq: EqChainStage,
    dy: DynamicEqStage,
    lm: LimiterStage,
    lufs: LufsMeter,
    mm: ModulationMatrixStage,
    fft: Fft,
    ring: Vec<f32>,
    rp: usize,
    ap: usize,
    re: Vec<f32>,
    im: Vec<f32>,
    mag: Vec<f32>,
    hann: Vec<f32>,
    ig: [f32; 10],
    il: [f32; 10],
    ranges: [(usize, usize); 10],
    targets: [f64; 10],
    ismooth: f64,
    norm: f64,
    sphase: f64,
    mg: f64,
    mw: f64,
    next_frame_count: Option<usize>,
    ieq_bands: [EqBandParam; 10],
    eq_on: bool,
    de_on: bool,
    cp_on: bool,
    de_sidechain: bool,
    cp_sidechain: bool,
    night_on: bool,
    bass_on: bool,
    loudness_comp_on: bool,
    ieq_on: bool,
    dynamic_eq_on: bool,
    limiter_on: bool,
    reverb_kind: u8,
    spatial: Option<SpatialStage>,
    modulation_on: bool,
    pitch_on: bool,
    voice_balance: f64,
    stereo_width: f64,
    norm_on: bool,
    norm_realtime: bool,
    norm_target_lufs: f64,
    norm_min_db: f64,
    norm_max_db: f64,
    norm_external_db: f64,
    surround_on: bool,
    surround_distance: f64,
    surround_speed: f64,
    surround_angle: f64,
    surround_direction: f64,
    ieq_strength: f64,
}
impl EngineChainStage {
    pub fn from_params(fs: f64, p: EngineChainParams) -> Result<Self, String> {
        Self::from_params_with_hrtf_grid(fs, p, None)
    }

    pub fn from_params_with_hrtf_grid(
        fs: f64,
        p: EngineChainParams,
        hrtf_grid: Option<HrtfGrid>,
    ) -> Result<Self, String> {
        Self::from_params_with_hrtf_grid_and_previous(fs, p, hrtf_grid, None)
    }

    pub fn from_params_with_hrtf_grid_and_previous(
        fs: f64,
        p: EngineChainParams,
        hrtf_grid: Option<HrtfGrid>,
        previous: Option<&Value>,
    ) -> Result<Self, String> {
        let value = p.as_value().clone();
        let v = &value;
        let spatial = spatial_stage(v, fs, hrtf_grid, previous)?;
        let eo = o(v, "/eq")?;
        let eq_mode = enum_value(eo, "mode", "/eq/mode", &["simple", "pro"])?;
        let mut eq = EqChainStage::new(fs, 20.)?;
        eq.set_bands(&pre_eq(eo, eq_mode)?);
        eq.set_q_compensation(b(eo, "qCompensation")?);
        let cpo = o(v, "/compressor")?;
        let mut cps = cp_settings(cpo)?;
        let cp_sidechain = cps.sidechain_enabled;
        cps.sidechain_enabled = false;
        let deo = o(v, "/deesser")?;
        let mut des = de_settings(deo)?;
        let de_sidechain = des.sidechain_enabled;
        des.sidechain_enabled = false;
        let nm = o(v, "/nightMode")?;
        let k = n(nm, "amount")? / 10.;
        let ncp = CompressorSettings {
            enabled: true,
            threshold_db: cps.threshold_db - 6. * k,
            ratio: (cps.ratio * (1. + 0.5 * k)).max(1.),
            knee_db: cps.knee_db,
            attack_ms: cps.attack_ms,
            release_ms: cps.release_ms,
            makeup_db: cps.makeup_db,
            output_gain: 1.,
            sidechain_enabled: false,
        };
        let rvp = rv_params(o(v, "/reverb/algorithmic")?)?;
        let io = o(v, "/ieq")?;
        let mut ieq = EqChainStage::new(fs, 10.)?;
        ieq.set_bands(
            &(0..10)
                .map(|i| EqBandParam {
                    frequency: IEQ[i],
                    gain: 0.,
                    q: 1.1,
                })
                .collect::<Vec<_>>(),
        );
        let mut ranges = [(0, 0); 10];
        let hz = fs / W as f64;
        for i in 0..10 {
            let lo = if i == 0 {
                20.
            } else {
                (IEQ[i - 1] * IEQ[i]).sqrt()
            };
            let hi = if i == 9 {
                fs / 2.
            } else {
                (IEQ[i] * IEQ[i + 1]).sqrt()
            };
            ranges[i] = (
                (lo / hz).floor() as usize,
                ((hi / hz).ceil() as usize).min(W / 2),
            );
        }
        let mut hann = vec![0.; W];
        for (i, x) in hann.iter_mut().enumerate() {
            *x = (0.5
                * (1.
                    - crate::fft::ts_trig::cos(
                        2. * std::f64::consts::PI * i as f64 / (W - 1) as f64,
                    ))) as f32
        }
        let mo = o(v, "/modulation")?;
        let lfo = o(v, "/modulation/lfo")?;
        let env = o(v, "/modulation/envelope")?;
        let lfo_shape = enum_value(
            lfo,
            "shape",
            "/modulation/lfo/shape",
            &["sine", "triangle", "square", "saw"],
        )?;
        let mm = ModulationMatrixStage::from_params(
            fs,
            routes(
                mo.get("routes")
                    .and_then(Value::as_array)
                    .ok_or("/modulation/routes 必须是数组")?,
            )?,
            LfoParams {
                shape: LfoShape::parse(lfo_shape),
                rate_hz: n(lfo, "rateHz")?,
                depth: n(lfo, "depth")?,
            },
            EnvelopeParams {
                attack_ms: n(env, "attackMs")?,
                release_ms: n(env, "releaseMs")?,
                amount: n(env, "amount")?,
            },
        )?;
        let mut lc =
            LoudnessCompStage::from_settings(fs, lc_settings(o(v, "/loudnessCompensation")?)?)?;
        if b(o(v, "/loudnessCompensation")?, "enabled")? {
            lc.reset()
        }
        let ln = o(v, "/loudnessNormalization")?;
        let surround = o(v, "/surround3d")?;
        let pitch = o(v, "/pitch")?;
        let reverb = o(v, "/reverb")?;
        let reverb_mode = enum_value(
            reverb,
            "mode",
            "/reverb/mode",
            &["convolution", "algorithmic", "fdn", "off"],
        )?;
        let mut conv = None;
        let reverb_kind = if !b(reverb, "enabled")? || reverb_mode == "off" {
            0
        } else if reverb_mode == "fdn" {
            2
        } else if reverb_mode == "convolution" {
            if let Some(ir) = convolution_ir(v)? {
                let convolution = o(v, "/reverb/convolution")?;
                let mut stage = ConvolverStage::new(
                    fs,
                    ConvolverOptions {
                        de_periodize: b(convolution, "dePeriodize")?,
                        ..ConvolverOptions::default()
                    },
                )?;
                stage.load_ir(&ir, convolution.get("irName").and_then(Value::as_str))?;
                stage.set_mix(finite_number(
                    convolution,
                    "mix",
                    "/reverb/convolution/mix",
                )?);
                stage.set_pre_delay_ms(finite_number(
                    convolution,
                    "preDelayMs",
                    "/reverb/convolution/preDelayMs",
                )?);
                conv = Some(stage);
                3
            } else {
                1
            }
        } else {
            1
        };
        Ok(Self {
            fs,
            eq,
            ms: MidSideStage::new(),
            de: DeesserStage::from_settings(fs, des)?,
            cp: CompressorStage::from_settings(fs, cps)?,
            ncp: CompressorStage::from_settings(fs, ncp)?,
            nsl: BiquadStage::new(fs, "highshelf", 6000., 0.707, -1.5 * n(nm, "amount")?)?,
            nsr: BiquadStage::new(fs, "highshelf", 6000., 0.707, -1.5 * n(nm, "amount")?)?,
            me: ModEffectsStage::from_settings(fs, me_settings(o(v, "/modEffects")?)?)?,
            rv: ReverbSimpleStage::from_params(fs, rvp.clone())?,
            fdn: FdnReverbStage::from_params(
                fs,
                FdnReverbParams {
                    room_size: rvp.room_size,
                    damping: rvp.damping,
                    wet: rvp.wet,
                    dry: rvp.dry,
                    pre_delay_ms: rvp.pre_delay_ms,
                    width: rvp.width,
                    reverb_type: rvp.reverb_type,
                    lines: None,
                },
            )?,
            conv,
            bass: BassEnhancerStage::from_settings(fs, bass_settings(o(v, "/bassEnhancer")?)?)?,
            lc,
            ieq,
            dy: DynamicEqStage::from_params(fs, dy_settings(o(v, "/dynamicEq")?)?)?,
            lm: LimiterStage::from_settings(fs, lm_settings(o(v, "/limiter")?)?)?,
            lufs: LufsMeter::new(fs)?,
            mm,
            fft: Fft::new(W)?,
            ring: vec![0.; W],
            rp: 0,
            ap: 0,
            re: vec![0.; W],
            im: vec![0.; W],
            mag: vec![0.; W / 2 + 1],
            hann,
            ig: [0.; 10],
            il: [0.; 10],
            ranges,
            targets: curve(enum_value(
                io,
                "targetCurve",
                "/ieq/targetCurve",
                &["flat", "warm", "bright", "vocal"],
            )?),
            ismooth: 1. - (-(W as f64 / fs) / n(io, "timeConstantSec")?.max(0.1)).exp(),
            norm: 1.,
            sphase: 0.,
            mg: 1.,
            mw: 1.,
            next_frame_count: None,
            ieq_bands: std::array::from_fn(|i| EqBandParam {
                frequency: IEQ[i],
                gain: 0.0,
                q: 1.1,
            }),
            eq_on: b(eo, "enabled")?,
            de_on: b(deo, "enabled")?,
            cp_on: b(cpo, "enabled")?,
            de_sidechain,
            cp_sidechain,
            night_on: b(nm, "enabled")? && n(nm, "amount")? > 0.0,
            bass_on: b(o(v, "/bassEnhancer")?, "enabled")?,
            loudness_comp_on: b(o(v, "/loudnessCompensation")?, "enabled")?,
            ieq_on: b(io, "enabled")?,
            dynamic_eq_on: b(o(v, "/dynamicEq")?, "enabled")?,
            limiter_on: b(o(v, "/limiter")?, "enabled")?,
            reverb_kind,
            spatial,
            modulation_on: b(mo, "enabled")?,
            pitch_on: b(pitch, "enabled")?,
            voice_balance: n(pitch, "voiceBalance")?,
            stereo_width: v["stereoWidth"].as_f64().ok_or("stereoWidth 必须是数字")?,
            norm_on: b(ln, "enabled")?,
            norm_realtime: b(ln, "useRealtimeMeter")?,
            norm_target_lufs: n(ln, "targetLufs")?,
            norm_min_db: n(ln, "minGainDb")?,
            norm_max_db: n(ln, "maxGainDb")?,
            norm_external_db: n(ln, "externalGainDb")?,
            surround_on: b(surround, "enabled")?,
            surround_distance: n(surround, "distance")?,
            surround_speed: n(surround, "speed")?,
            surround_angle: n(surround, "angle")?,
            surround_direction: n(surround, "direction")?,
            ieq_strength: n(io, "strength")?,
        })
    }
    pub fn stage_ids(&self) -> &[&'static str] {
        &IDS
    }
    pub fn norm_gain(&self) -> f64 {
        self.norm
    }
    pub fn ieq_gains(&self) -> [f32; 10] {
        self.ig
    }
    pub fn modulation_targets(&self) -> (f64, f64) {
        (self.mg, self.mw)
    }
    pub fn get_latency_samples(&self) -> usize {
        let limiter = if self.limiter_on {
            self.lm.latency_samples()
        } else {
            0
        };
        let convolution = if self.reverb_kind == 3 {
            self.conv
                .as_ref()
                .map(ConvolverStage::get_latency_samples)
                .unwrap_or(0)
        } else {
            0
        };
        let spatial = self
            .spatial
            .as_ref()
            .map(SpatialStage::latency_samples)
            .unwrap_or(0);
        limiter + convolution + spatial
    }

    pub fn spatial_listener_velocity(&self) -> Option<Vec3> {
        self.spatial
            .as_ref()
            .and_then(SpatialStage::listener_velocity)
    }

    pub fn set_next_frame_count(&mut self, n: usize) {
        self.next_frame_count = Some(n)
    }
    pub fn process_with_sidechain(
        &mut self,
        left: &mut [f32],
        right: &mut [f32],
        side_l: &[f32],
        side_r: &[f32],
    ) {
        assert_eq!(left.len(), right.len(), "左右声道块长必须一致");
        assert!(
            side_l.len() >= left.len() && side_r.len() >= left.len(),
            "sidechain 块长不足"
        );
        self.process_inner(left, right, Some((side_l, side_r)));
    }
    fn analysis(&mut self, l: &[f32], r: &[f32]) {
        for i in 0..l.len() {
            self.ring[self.rp] = (0.5 * (f64::from(l[i]) + f64::from(r[i]))) as f32;
            self.rp = (self.rp + 1) % W
        }
        self.ap += l.len();
        while self.ap >= W {
            self.ap -= W;
            self.analyze()
        }
    }
    fn analyze(&mut self) {
        for i in 0..W {
            let x = self.ring[(self.rp + i) % W];
            self.re[i] = (f64::from(x) * f64::from(self.hann[i])) as f32;
            self.im[i] = 0.
        }
        self.fft
            .transform(&mut self.re, &mut self.im, false)
            .unwrap();
        for k in 0..self.mag.len() {
            let x = f64::from(self.re[k]);
            let y = f64::from(self.im[k]);
            self.mag[k] = (x * x + y * y).sqrt() as f32
        }
        if !self.ieq_on {
            return;
        }
        let mut avg = 0.;
        for i in 0..10 {
            let (lo, hi) = self.ranges[i];
            let mut ss = 0.;
            for k in lo..=hi {
                let x = f64::from(self.mag[k]);
                ss += x * x
            }
            let rms = (ss / (hi - lo + 1) as f64).sqrt();
            self.il[i] = (20. * rms.max(1e-4).log10()) as f32;
            avg += f64::from(self.il[i])
        }
        avg /= 10.;
        let strength = self.ieq_strength;
        for i in 0..10 {
            let rel = f64::from(self.il[i]) - avg;
            let want = strength * (self.targets[i] - rel);
            let g = (f64::from(self.ig[i]) + self.ismooth * (want - f64::from(self.ig[i])))
                .clamp(-12., 12.);
            self.ig[i] = g as f32;
            self.ieq_bands[i].gain = g;
        }
        self.ieq.set_bands(&self.ieq_bands)
    }
    fn process_inner(&mut self, l: &mut [f32], r: &mut [f32], sidechain: Option<(&[f32], &[f32])>) {
        let active_n = self.next_frame_count.take().unwrap_or(l.len()).min(l.len());
        let mod_on = self.modulation_on;
        if mod_on {
            let t = self.mm.process_block(&l[..active_n], &r[..active_n]);
            self.mg = t.master_gain;
            self.mw = t.stereo_width
        } else {
            self.mg = 1.;
            self.mw = 1.
        }
        if self.norm_on {
            let rt = self.norm_realtime;
            let db = if rt {
                let i = self.lufs.get_integrated_lufs();
                let m = if i.is_finite() {
                    i
                } else {
                    self.lufs.get_momentary_lufs()
                };
                if m.is_finite() {
                    (self.norm_target_lufs - m).clamp(self.norm_min_db, self.norm_max_db)
                } else {
                    0.
                }
            } else {
                self.norm_external_db
                    .clamp(self.norm_min_db, self.norm_max_db)
            };
            let a = 1. - (-(active_n as f64 / self.fs) / if rt { 3. } else { 0.08 }).exp();
            self.norm += a * (10f64.powf(db / 20.) - self.norm);
            gain(&mut l[..active_n], &mut r[..active_n], self.norm)
        }
        if self.surround_on {
            self.sphase += 2.
                * std::f64::consts::PI
                * self.surround_speed
                * (active_n as f64 / self.fs)
                * 0.125;
            let th = self.surround_angle * std::f64::consts::PI / 180.
                + self.surround_direction * self.sphase;
            let (c, s) = (th.cos(), th.sin());
            let z = 0.5 + 0.5 * self.surround_distance;
            for i in 0..active_n {
                let (x, y) = (f64::from(l[i]), f64::from(r[i]));
                l[i] = ((x * c - y * s) * z) as f32;
                r[i] = ((x * s + y * c) * z) as f32
            }
        }
        self.ms.set_params(
            if mod_on { self.mw } else { self.stereo_width },
            if self.pitch_on {
                self.voice_balance
            } else {
                0.
            },
        );
        self.ms.process(l, r);
        if self.eq_on {
            self.eq.process(l, r)
        }
        if self.de_on {
            if self.de_sidechain {
                if let Some((side_l, side_r)) = sidechain {
                    self.de.process_with_sidechain(l, r, side_l, side_r)
                } else {
                    self.de.process(l, r)
                }
            } else {
                self.de.process(l, r)
            }
        }
        if self.cp_on {
            if self.cp_sidechain {
                if let Some((side_l, side_r)) = sidechain {
                    self.cp.process_with_sidechain(l, r, side_l, side_r)
                } else {
                    self.cp.process(l, r)
                }
            } else {
                self.cp.process(l, r)
            }
        }
        if self.night_on {
            self.ncp.process(l, r);
            self.nsl.process_mono(l);
            self.nsr.process_mono(r)
        }
        self.me.process(l, r);
        match self.reverb_kind {
            3 => self
                .conv
                .as_mut()
                .expect("卷积模式必须已加载 IR")
                .process(l, r),
            2 => self.fdn.process(l, r),
            1 => self.rv.process(l, r),
            _ => {}
        }
        if self.bass_on {
            self.bass.process(l, r)
        }
        if self.loudness_comp_on {
            self.lc.process(l, r)
        }
        if self.ieq_on {
            self.ieq.process(l, r)
        }
        self.analysis(&l[..active_n], &r[..active_n]);
        if self.dynamic_eq_on {
            self.dy.process(l, r)
        }
        self.lufs.process_stereo(l, r);
        if mod_on {
            gain(&mut l[..active_n], &mut r[..active_n], self.mg)
        }
        if self.limiter_on {
            self.lm.process(l, r)
        }
        if let Some(spatial) = self.spatial.as_mut() {
            spatial.process(&mut l[..active_n], &mut r[..active_n])
        }
    }
}
impl Stage for EngineChainStage {
    fn prepare(&mut self, x: usize) {
        self.eq.prepare(x);
        self.de.prepare(x);
        self.cp.prepare(x);
        self.ncp.prepare(x);
        self.me.prepare(x);
        self.rv.prepare(x);
        self.fdn.prepare(x);
        if let Some(conv) = self.conv.as_mut() {
            conv.prepare(x)
        }
        self.bass.prepare(x);
        self.lc.prepare(x);
        self.ieq.prepare(x);
        self.dy.prepare(x);
        self.lm.prepare(x);
        if let Some(spatial) = self.spatial.as_mut() {
            spatial.prepare(x)
        }
    }
    fn process(&mut self, l: &mut [f32], r: &mut [f32]) {
        self.process_inner(l, r, None)
    }
    fn reset(&mut self) {
        self.eq.reset();
        self.ms.reset();
        self.de.reset();
        self.cp.reset();
        self.ncp.reset();
        self.nsl.reset();
        self.nsr.reset();
        self.me.reset();
        self.rv.reset();
        self.fdn.reset();
        if let Some(conv) = self.conv.as_mut() {
            conv.reset()
        }
        self.bass.reset();
        self.lc.reset();
        self.ieq.reset();
        self.dy.reset();
        self.lm.reset();
        if let Some(spatial) = self.spatial.as_mut() {
            spatial.reset()
        }
        self.lufs.reset();
        self.mm.reset();
        self.ring.fill(0.);
        self.rp = 0;
        self.ap = 0;
        self.ig = [0.; 10];
        self.il = [0.; 10];
        self.norm = 1.;
        self.sphase = 0.;
        self.mg = 1.;
        self.mw = 1.;
        self.next_frame_count = None
    }
}
fn gain(l: &mut [f32], r: &mut [f32], g: f64) {
    for i in 0..l.len() {
        l[i] = (f64::from(l[i]) * g) as f32;
        r[i] = (f64::from(r[i]) * g) as f32
    }
}
fn pre_eq(x: &Map<String, Value>, mode: &str) -> Result<Vec<EqBandParam>, String> {
    if !b(x, "enabled")? {
        return Ok(vec![]);
    }
    if mode == "simple" {
        let a = x["simpleBands"].as_array().ok_or("simpleBands")?;
        Ok([80., 250., 1000., 4000., 12000.]
            .iter()
            .enumerate()
            .map(|(i, &f)| EqBandParam {
                frequency: f,
                gain: a.get(i).and_then(Value::as_f64).unwrap_or(0.),
                q: 1.1,
            })
            .collect())
    } else {
        let a = x["proBands"].as_array().ok_or("proBands")?;
        (0..(n(x, "bandCount")? as usize).min(a.len()))
            .map(|i| {
                let q = a[i].as_object().ok_or("proBand")?;
                Ok(EqBandParam {
                    frequency: n(q, "frequency")?,
                    gain: n(q, "gain")?,
                    q: n(q, "q")?,
                })
            })
            .collect()
    }
}
fn cp_settings(x: &Map<String, Value>) -> Result<CompressorSettings, String> {
    Ok(CompressorSettings {
        enabled: b(x, "enabled")?,
        threshold_db: n(x, "thresholdDb")?,
        ratio: n(x, "ratio")?,
        knee_db: n(x, "kneeDb")?,
        attack_ms: n(x, "attackMs")?,
        release_ms: n(x, "releaseMs")?,
        makeup_db: n(x, "makeupDb")?,
        output_gain: n(x, "outputGain")?,
        sidechain_enabled: x
            .get("sidechainEnabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}
fn de_settings(x: &Map<String, Value>) -> Result<DeesserSettings, String> {
    Ok(DeesserSettings {
        enabled: b(x, "enabled")?,
        center_hz: n(x, "centerHz")?,
        q: n(x, "q")?,
        threshold_db: n(x, "thresholdDb")?,
        ratio: n(x, "ratio")?,
        attack_ms: n(x, "attackMs")?,
        release_ms: n(x, "releaseMs")?,
        split_band: b(x, "splitBand")?,
        mix: n(x, "mix")?,
        sidechain_enabled: b(x, "sidechainEnabled")?,
    })
}
fn bass_settings(x: &Map<String, Value>) -> Result<BassEnhancerSettings, String> {
    Ok(BassEnhancerSettings {
        enabled: b(x, "enabled")?,
        cutoff_hz: n(x, "cutoffHz")?,
        q: n(x, "q")?,
        harmonic_type: enum_value(
            x,
            "harmonicType",
            "/bassEnhancer/harmonicType",
            &["odd", "even", "atan", "soft"],
        )?
        .to_owned(),
        harmonic_gain: n(x, "harmonicGain")?,
        mix: n(x, "mix")?,
        level_db: n(x, "levelDb")?,
        low_boost_db: x.get("lowBoostDb").and_then(Value::as_f64),
    })
}
fn lm_settings(x: &Map<String, Value>) -> Result<LimiterSettings, String> {
    Ok(LimiterSettings {
        enabled: b(x, "enabled")?,
        threshold_db: n(x, "thresholdDb")?,
        lookahead_ms: n(x, "lookaheadMs")?,
        attack_ms: n(x, "attackMs")?,
        release_ms: n(x, "releaseMs")?,
        true_peak: b(x, "truePeak")?,
    })
}
fn rv_params(x: &Map<String, Value>) -> Result<ReverbSimpleParams, String> {
    Ok(ReverbSimpleParams {
        room_size: n(x, "roomSize")?,
        damping: n(x, "damping")?,
        wet: n(x, "wet")?,
        dry: n(x, "dry")?,
        pre_delay_ms: n(x, "preDelayMs")?,
        width: n(x, "width")?,
        reverb_type: enum_value(
            x,
            "type",
            "/reverb/algorithmic/type",
            &["hall", "room", "plate", "spring", "stage"],
        )?
        .to_owned(),
    })
}
fn lc_settings(x: &Map<String, Value>) -> Result<LoudnessCompSettings, String> {
    let bands = x["bands"]
        .as_array()
        .ok_or("bands")?
        .iter()
        .map(|v| {
            let q = v.as_object().ok_or("band")?;
            Ok(LoudnessBandParam {
                frequency: n(q, "frequency")?,
                gain: n(q, "gain")?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(LoudnessCompSettings {
        volume_percent: n(x, "volumePercent")?,
        max_boost_db: n(x, "maxBoostDb")?,
        preset: enum_value(
            x,
            "preset",
            "/loudnessCompensation/preset",
            &["flat", "bass", "vocal", "warm", "bright", "night"],
        )?
        .to_owned(),
        bands,
        mode: enum_value(
            x,
            "mode",
            "/loudnessCompensation/mode",
            &["auto", "preset", "custom"],
        )?
        .to_owned(),
        smoothing_seconds: n(x, "smoothingSeconds")?,
    })
}
fn dy_settings(x: &Map<String, Value>) -> Result<DynamicEqParams, String> {
    let bands = x["bands"]
        .as_array()
        .ok_or("bands")?
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let q = v.as_object().ok_or("band")?;
            Ok(DynamicEqBandParam {
                enabled: b(q, "enabled")?,
                frequency: XO.get(i).copied().unwrap_or(0.),
                target_gain_db: q.get("targetGainDb").and_then(Value::as_f64),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(DynamicEqParams {
        enabled: Some(b(x, "enabled")?),
        strength: Some(n(x, "strength")?),
        threshold_db: Some(n(x, "thresholdDb")?),
        ratio: Some(n(x, "ratio")?),
        knee_db: None,
        attack_ms: Some(n(x, "attackMs")?),
        release_ms: Some(n(x, "releaseMs")?),
        block_size: None,
        bands: Some(bands),
    })
}
fn object_field<'a>(
    x: &'a Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<&'a Map<String, Value>, String> {
    x.get(key)
        .and_then(Value::as_object)
        .ok_or_else(|| format!("{path} 必须是对象"))
}
fn me_settings(x: &Map<String, Value>) -> Result<ModEffectsSettings, String> {
    let d = object_field(x, "delay", "/modEffects/delay")?;
    let c = object_field(x, "chorus", "/modEffects/chorus")?;
    let f = object_field(x, "flanger", "/modEffects/flanger")?;
    let p = object_field(x, "phaser", "/modEffects/phaser")?;
    let t = object_field(x, "tremolo", "/modEffects/tremolo")?;
    Ok(ModEffectsSettings {
        delay: DelaySettings {
            enabled: b(d, "enabled")?,
            delay_ms: n(d, "delayMs")?,
            feedback: n(d, "feedback")?,
            mix: n(d, "mix")?,
        },
        chorus: ChorusSettings {
            enabled: b(c, "enabled")?,
            rate_hz: n(c, "rateHz")?,
            depth_ms: n(c, "depthMs")?,
            mix: n(c, "mix")?,
        },
        flanger: FlangerSettings {
            enabled: b(f, "enabled")?,
            rate_hz: n(f, "rateHz")?,
            depth_ms: n(f, "depthMs")?,
            feedback: n(f, "feedback")?,
            mix: n(f, "mix")?,
        },
        phaser: PhaserSettings {
            enabled: b(p, "enabled")?,
            rate_hz: n(p, "rateHz")?,
            depth: n(p, "depth")?,
            feedback: n(p, "feedback")?,
            mix: n(p, "mix")?,
            stages: n(p, "stages")?,
        },
        tremolo: TremoloSettings {
            enabled: b(t, "enabled")?,
            rate_hz: n(t, "rateHz")?,
            depth: n(t, "depth")?,
            mix: n(t, "mix")?,
        },
    })
}
fn routes(a: &[Value]) -> Result<Vec<ModulationRoute>, String> {
    a.iter()
        .enumerate()
        .map(|(index, v)| {
            let path = format!("/modulation/routes/{index}");
            let q = v.as_object().ok_or_else(|| format!("{path} 必须是对象"))?;
            let source = enum_value(q, "source", &format!("{path}/source"), &["lfo", "envelope"])?;
            let target = enum_value(
                q,
                "target",
                &format!("{path}/target"),
                &["masterGain", "stereoWidth"],
            )?;
            Ok(ModulationRoute {
                source: ModSource::parse(source),
                target: ModTarget::parse(target),
                amount: finite_number(q, "amount", &format!("{path}/amount"))?,
                offset: match q.get("offset") {
                    None | Some(Value::Null) => 0.0,
                    _ => finite_number(q, "offset", &format!("{path}/offset"))?,
                },
            })
        })
        .collect()
}
fn curve(x: &str) -> [f64; 10] {
    match x {
        "warm" => [4., 3.5, 2.5, 1.5, 0.5, 0., -0.5, -1.5, -2.5, -3.5],
        "bright" => [-3.5, -2.5, -1.5, -0.5, 0., 0.5, 1.5, 2.5, 3.5, 4.],
        "vocal" => [-1.5, -1., 0., 1., 2., 2.5, 2., 1., 0., -0.5],
        _ => [0.; 10],
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use hrtf_core::HrtfGrid;

    fn asymmetric_grid() -> HrtfGrid {
        HrtfGrid::new(
            48_000,
            vec![-30.0, 30.0],
            vec![0.0],
            3,
            vec![1.0, 0.5, 0.0, 0.25, 0.0, 0.0],
            vec![0.25, 0.0, 0.0, 1.0, 0.5, 0.0],
        )
        .unwrap()
    }

    fn spatial(overrides: Value) -> EngineChainStage {
        let params = EngineChainParams::from_overrides(48_000.0, &overrides).unwrap();
        EngineChainStage::from_params_with_hrtf_grid(48_000.0, params, Some(asymmetric_grid()))
            .unwrap()
    }

    fn bypass() -> EngineChainStage {
        EngineChainStage::from_params(
            48000.,
            EngineChainParams::from_overrides(
                48000.,
                &json!({"eq":{"enabled":false},"limiter":{"enabled":false}}),
            )
            .unwrap(),
        )
        .unwrap()
    }
    #[test]
    fn 顺序() {
        assert_eq!(bypass().stage_ids(), IDS)
    }
    #[test]
    fn 旁路与reset() {
        let mut e = bypass();
        let il = vec![0.25, -0.5, 0.75];
        let ir = vec![-0.125, 0.5, -0.75];
        let (mut l, mut r) = (il.clone(), ir.clone());
        e.process(&mut l, &mut r);
        assert_eq!((&l, &r), (&il, &ir));
        e.reset();
        let (mut x, mut y) = (il, ir);
        e.process(&mut x, &mut y);
        assert_eq!((l, r), (x, y))
    }
    #[test]
    fn 空间模式解析且缺少网格明确拒绝() {
        for mode in ["instant", "headLocked", "world", "stage"] {
            let params =
                EngineChainParams::from_overrides(48_000.0, &json!({"spatial":{"mode":mode}}))
                    .unwrap();
            let error = EngineChainStage::from_params(48_000.0, params)
                .err()
                .expect("非 off 空间模式缺少网格时必须失败");
            assert!(error.contains("HRTF grid"), "实际错误：{error}");
        }
    }

    #[test]
    fn stage22_delta产生左右不对称双耳输出() {
        let mut engine = spatial(json!({
            "eq":{"enabled":false}, "limiter":{"enabled":false},
            "spatial":{"mode":"instant","masterGain":1.0,"convolution":"time",
                "instant":{"spreadDeg":60.0,"amount":1.0}}
        }));
        engine.prepare(8);
        let mut left = [1.0, 0.0, 0.0, 0.0];
        let mut right = [0.0; 4];
        engine.process(&mut left, &mut right);
        assert_ne!(left, right);
        assert!(left.iter().chain(&right).any(|sample| *sample != 0.0));
        assert_eq!(engine.get_latency_samples(), 0);
    }

    #[test]
    fn stage22短块连续且reset复现() {
        let overrides = json!({
            "eq":{"enabled":false}, "limiter":{"enabled":false},
            "spatial":{"mode":"headLocked","masterGain":1.0,"convolution":"time",
                "headLocked":{"speakers":[
                    {"azimuthDeg":-30.0,"elevationDeg":0.0,"distance":1.0,"gain":1.0},
                    {"azimuthDeg":30.0,"elevationDeg":0.0,"distance":1.0,"gain":1.0}
                ]}}
        });
        let input_left = [1.0, 0.25, -0.5, 0.0, 0.75];
        let input_right = [0.0, -0.25, 0.5, 0.25, 0.0];

        let mut whole = spatial(overrides.clone());
        whole.prepare(5);
        let (mut whole_left, mut whole_right) = (input_left, input_right);
        whole.process(&mut whole_left, &mut whole_right);

        let mut split = spatial(overrides);
        split.prepare(3);
        let (mut split_left, mut split_right) = (input_left, input_right);
        split.process(&mut split_left[..2], &mut split_right[..2]);
        split.process(&mut split_left[2..], &mut split_right[2..]);
        assert_eq!((split_left, split_right), (whole_left, whole_right));

        split.reset();
        let (mut reset_left, mut reset_right) = (input_left, input_right);
        split.process(&mut reset_left[..2], &mut reset_right[..2]);
        split.process(&mut reset_left[2..], &mut reset_right[2..]);
        assert_eq!((reset_left, reset_right), (whole_left, whole_right));
    }

    #[test]
    fn 非instant模式不受instant_amount影响() {
        let cases = [
            json!({"mode":"headLocked","headLocked":{"speakers":[
                {"azimuthDeg":-30.0,"elevationDeg":0.0,"distance":1.0,"gain":1.0},
                {"azimuthDeg":30.0,"elevationDeg":0.0,"distance":1.0,"gain":1.0}
            ]}}),
            json!({"mode":"world","world":{"listener":{"position":{"x":0,"y":1.6,"z":0}},
                "sources":[{"id":"source","position":{"x":1,"y":1.6,"z":2},"gain":1.0,"size":0.0}],
                "playhead":0,"trajectories":[],"occlusion":0}}),
            json!({"mode":"stage","stage":{"preset":"stage","seat":"middle","roomSize":1.0,
                "reverbAmount":0.0,"customSources":[]}}),
        ];
        for spatial_case in cases {
            let run = |amount: f64| {
                let mut spatial_params = spatial_case.clone();
                spatial_params["masterGain"] = json!(1.0);
                spatial_params["convolution"] = json!("time");
                spatial_params["instant"] = json!({"amount":amount,"room":"off","roomAmount":0.0});
                let mut engine = spatial(json!({
                    "eq":{"enabled":false}, "limiter":{"enabled":false}, "spatial":spatial_params
                }));
                engine.prepare(8);
                let mut left = [1.0, 0.25, -0.5, 0.0, 0.75, -0.25, 0.1, 0.0];
                let mut right = [0.0, -0.25, 0.5, 0.25, 0.0, 0.5, -0.1, 0.0];
                engine.process(&mut left, &mut right);
                (left, right)
            };
            assert_eq!(run(0.0), run(1.0), "mode={:?}", spatial_case["mode"]);
        }
    }

    #[test]
    fn stage22高增益多对象与room输出有界() {
        let mut speakers = Vec::new();
        for index in 0..16 {
            speakers.push(json!({
                "azimuthDeg":if index % 2 == 0 { -30.0 } else { 30.0 },
                "elevationDeg":0.0,"distance":1.0,"gain":2.0,"size":0.0
            }));
        }
        let cases = [
            json!({"mode":"headLocked","masterGain":1.0,"convolution":"time",
                "instant":{"amount":0.0},"headLocked":{"speakers":speakers}}),
            json!({"mode":"stage","masterGain":1.0,"convolution":"time",
                "instant":{"amount":0.0},"stage":{"preset":"stage","seat":"front",
                    "roomSize":2.0,"reverbAmount":1.0,"customSources":[]}}),
        ];
        for spatial_params in cases {
            let mut engine = spatial(json!({
                "eq":{"enabled":false}, "limiter":{"enabled":false}, "spatial":spatial_params
            }));
            engine.prepare(128);
            let mut left = [8.0; 128];
            let mut right = [8.0; 128];
            engine.process(&mut left, &mut right);
            assert!(left
                .iter()
                .chain(&right)
                .all(|sample| sample.is_finite() && sample.abs() <= 1.0));
        }
    }

    #[test]
    fn world投影完整姿态轨迹与稳定slot() {
        let current = json!({"spatial":{"mode":"world","world":{
            "listener":{"position":{"x":1,"y":1.6,"z":0},"yaw":20,"pitch":10,"roll":-5},
            "sources":[
                {"id":"beta","position":{"x":4,"y":2,"z":8},"gain":0.8,"size":0.5},
                {"id":"alpha","position":{"x":-2,"y":1.6,"z":4},"gain":1,"size":0}
            ],
            "playhead":2,
            "trajectories":[{"sourceId":"beta","keyframes":[
                {"t":0,"position":{"x":0,"y":1.6,"z":4}},
                {"t":4,"position":{"x":8,"y":1.6,"z":4}}
            ]}],
            "occlusion":0.4
        }}});
        let previous = json!({"spatial":{"mode":"world","world":{
            "listener":{"position":{"x":0,"y":1.6,"z":0},"yaw":0,"pitch":0,"roll":0},
            "sources":[
                {"id":"alpha","position":{"x":-2,"y":1.6,"z":4},"gain":1,"size":0},
                {"id":"beta","position":{"x":4,"y":2,"z":8},"gain":0.8,"size":0.5}
            ],"playhead":1,"trajectories":[],"occlusion":0
        }}});
        let params = EngineChainParams::from_overrides(48_000.0, &current).unwrap();
        let value = params.as_value();
        let projected = spatial_speakers(
            value["spatial"].as_object().unwrap(),
            "world",
            Some(&previous),
        )
        .unwrap();
        assert_eq!(projected.len(), 2);
        let beta = projected
            .iter()
            .find(|speaker| speaker.size == 0.5)
            .unwrap();
        let expected = relative_direction_pose(
            WorldListenerPose {
                position: Vec3 {
                    x: 1.0,
                    y: 1.6,
                    z: 0.0,
                },
                yaw_deg: 20.0,
                pitch_deg: 10.0,
                roll_deg: -5.0,
            },
            Vec3 {
                x: 4.0,
                y: 1.6,
                z: 4.0,
            },
        );
        assert_eq!(beta.azimuth_deg, expected.azimuth_deg as f32);
        assert_eq!(beta.elevation_deg, expected.elevation_deg as f32);
        assert_eq!(beta.distance, expected.distance as f32);

        let reordered = json!({"spatial":{"mode":"world","world":{
            "listener":{"position":{"x":1,"y":1.6,"z":0},"yaw":20,"pitch":10,"roll":-5},
            "sources":[
                {"id":"alpha","position":{"x":-2,"y":1.6,"z":4},"gain":1,"size":0},
                {"id":"beta","position":{"x":4,"y":2,"z":8},"gain":0.8,"size":0.5}
            ],"playhead":2,"trajectories":[],"occlusion":0.4
        }}});
        let reordered = EngineChainParams::from_overrides(48_000.0, &reordered).unwrap();
        let reordered = spatial_speakers(
            reordered.as_value()["spatial"].as_object().unwrap(),
            "world",
            Some(&previous),
        )
        .unwrap();
        for speaker in &projected {
            let matching = reordered
                .iter()
                .find(|candidate| candidate.size == speaker.size)
                .unwrap();
            assert_eq!(matching.slot, speaker.slot);
        }
        let velocity = world_velocity(
            value["spatial"]["world"].as_object().unwrap(),
            Some(&previous),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            velocity,
            Vec3 {
                x: 1.0,
                y: 0.0,
                z: 0.0
            }
        );
    }

    #[test]
    fn stable_slot历史并集可覆盖两组各32对象() {
        let ids: Vec<String> = (0..SPATIAL_PRIMARY_SLOTS)
            .map(|index| format!("source-{index}"))
            .collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let slots = assign_stable_slots(&refs).unwrap();
        assert_eq!(slots.len(), SPATIAL_PRIMARY_SLOTS);
        let unique: std::collections::HashSet<_> = slots.values().copied().collect();
        assert_eq!(unique.len(), SPATIAL_PRIMARY_SLOTS);
    }

    #[test]
    fn stage投影preset座位房间与自定义声源() {
        let params = EngineChainParams::from_overrides(
            48_000.0,
            &json!({"spatial":{"mode":"stage","stage":{
                "preset":"cinema","seat":"back","roomSize":2,"reverbAmount":0.6,
                "customSources":[{"id":"custom","position":{"x":2,"y":1.6,"z":2},"gain":0.4,"size":0.25}]
            }}}),
        )
        .unwrap();
        let spatial = params.as_value()["spatial"].as_object().unwrap();
        let speakers = spatial_speakers(spatial, "stage", None).unwrap();
        assert_eq!(speakers.len(), CINEMA.len() + 1);
        assert_eq!(speakers[0].distance, 10.0);
        assert_eq!(speakers.last().unwrap().size, 0.25);

        let mut renderer = BinauralRenderer::new(
            asymmetric_grid(),
            RenderProfile::LowLatency,
            DistanceModel::Inverse,
            DistanceParams::default(),
        )
        .unwrap();
        configure_spatial_room(&mut renderer, spatial, "stage").unwrap();
        assert_eq!(renderer.room_amount(), 0.6);
        let room = renderer.room_params().unwrap();
        assert_eq!(room.width, RoomPreset::Hall.params().width * 2.0);
    }

    #[test]
    fn canonical默认空间卷积为partitioned并上报延迟() {
        let mut engine = spatial(json!({
            "limiter":{"enabled":false},
            "spatial":{"mode":"instant"}
        }));
        engine.prepare(128);
        assert_eq!(engine.get_latency_samples(), 64);
    }

    #[test]
    fn lufs启动() {
        let p=EngineChainParams::from_overrides(48000.,&json!({"eq":{"enabled":false},"limiter":{"enabled":false},"loudnessNormalization":{"enabled":true}})).unwrap();
        let mut e = EngineChainStage::from_params(48000., p).unwrap();
        let mut l = vec![0.1; 128];
        let mut r = l.clone();
        e.process(&mut l, &mut r);
        assert_eq!(e.norm_gain(), 1.)
    }
    #[test]
    fn 双目标() {
        let p=EngineChainParams::from_overrides(48000.,&json!({"eq":{"enabled":false},"limiter":{"enabled":false},"modulation":{"enabled":true,"lfo":{"shape":"triangle","rateHz":3,"depth":0.8},"envelope":{"attackMs":3,"releaseMs":90,"amount":0.9},"routes":[{"source":"lfo","target":"masterGain","amount":0.35},{"source":"envelope","target":"stereoWidth","amount":0.9}]}})).unwrap();
        let mut e = EngineChainStage::from_params(48000., p).unwrap();
        let mut l = vec![0.5; 256];
        let mut r = vec![-0.25; 256];
        e.process(&mut l, &mut r);
        assert_ne!(e.modulation_targets(), (1., 1.))
    }

    #[test]
    fn night_mode_派生压缩与双_highshelf() {
        let overrides = json!({
            "eq":{"enabled":false}, "limiter":{"enabled":false},
            "compressor":{"enabled":false,"thresholdDb":-24,"ratio":5,"kneeDb":6,
                "attackMs":4,"releaseMs":120,"makeupDb":0,"outputGain":1},
            "nightMode":{"enabled":true,"amount":8}
        });
        let params = EngineChainParams::from_overrides(48_000.0, &overrides).unwrap();
        let mut engine = EngineChainStage::from_params(48_000.0, params).unwrap();
        let input_l: Vec<f32> = (0..256)
            .map(|i| ((i as f64 * 0.17).sin() * 0.7) as f32)
            .collect();
        let input_r: Vec<f32> = (0..256)
            .map(|i| ((i as f64 * 0.11).cos() * 0.5) as f32)
            .collect();
        let (mut got_l, mut got_r) = (input_l.clone(), input_r.clone());
        engine.process(&mut got_l, &mut got_r);

        let mut compressor = CompressorStage::from_settings(
            48_000.0,
            CompressorSettings {
                enabled: true,
                threshold_db: -28.8,
                ratio: 7.0,
                knee_db: 6.0,
                attack_ms: 4.0,
                release_ms: 120.0,
                makeup_db: 0.0,
                output_gain: 1.0,
                sidechain_enabled: false,
            },
        )
        .unwrap();
        let mut shelf_l = BiquadStage::new(48_000.0, "highshelf", 6000.0, 0.707, -12.0).unwrap();
        let mut shelf_r = BiquadStage::new(48_000.0, "highshelf", 6000.0, 0.707, -12.0).unwrap();
        let (mut want_l, mut want_r) = (input_l, input_r);
        compressor.process(&mut want_l, &mut want_r);
        shelf_l.process_mono(&mut want_l);
        shelf_r.process_mono(&mut want_r);
        assert_eq!(got_l, want_l);
        assert_eq!(got_r, want_r);
    }

    #[test]
    fn sidechain_仅在显式提供且启用时生效_night永不使用() {
        let common = json!({
            "eq":{"enabled":false}, "limiter":{"enabled":false},
            "compressor":{"enabled":true,"thresholdDb":-24,"ratio":8,"kneeDb":0,
                "attackMs":0.05,"releaseMs":80,"makeupDb":0,"outputGain":1,
                "sidechainEnabled":true}
        });
        let input_l: Vec<f32> = (0..1024)
            .map(|i| ((i as f64 * 0.17).sin() * 0.4) as f32)
            .collect();
        let input_r: Vec<f32> = (0..1024)
            .map(|i| ((i as f64 * 0.11).cos() * 0.3) as f32)
            .collect();
        let side_l = vec![1.0_f32; 1024];
        let side_r = vec![-1.0_f32; 1024];

        let mut no_external = EngineChainStage::from_params(
            48_000.0,
            EngineChainParams::from_overrides(48_000.0, &common).unwrap(),
        )
        .unwrap();
        let (mut internal_l, mut internal_r) = (input_l.clone(), input_r.clone());
        no_external.process(&mut internal_l, &mut internal_r);

        let disabled = json!({
            "eq":{"enabled":false}, "limiter":{"enabled":false},
            "compressor":{"enabled":true,"thresholdDb":-24,"ratio":8,"kneeDb":0,
                "attackMs":0.05,"releaseMs":80,"makeupDb":0,"outputGain":1,
                "sidechainEnabled":false}
        });
        let mut disabled_external = EngineChainStage::from_params(
            48_000.0,
            EngineChainParams::from_overrides(48_000.0, &disabled).unwrap(),
        )
        .unwrap();
        let (mut disabled_l, mut disabled_r) = (input_l.clone(), input_r.clone());
        disabled_external.process_with_sidechain(
            &mut disabled_l,
            &mut disabled_r,
            &side_l,
            &side_r,
        );
        assert_eq!(
            (internal_l.clone(), internal_r.clone()),
            (disabled_l, disabled_r)
        );

        let mut enabled_external = EngineChainStage::from_params(
            48_000.0,
            EngineChainParams::from_overrides(48_000.0, &common).unwrap(),
        )
        .unwrap();
        let (mut external_l, mut external_r) = (input_l.clone(), input_r.clone());
        enabled_external.process_with_sidechain(&mut external_l, &mut external_r, &side_l, &side_r);
        assert_ne!(
            internal_l, external_l,
            "启用且显式提供时必须使用外部 sidechain"
        );

        let night = json!({
            "eq":{"enabled":false}, "limiter":{"enabled":false},
            "compressor":{"enabled":false,"thresholdDb":-24,"ratio":8,"kneeDb":0,
                "attackMs":0.05,"releaseMs":80,"makeupDb":0,"outputGain":1,
                "sidechainEnabled":true},
            "nightMode":{"enabled":true,"amount":8}
        });
        let make_night = || {
            EngineChainStage::from_params(
                48_000.0,
                EngineChainParams::from_overrides(48_000.0, &night).unwrap(),
            )
            .unwrap()
        };
        let (mut night_internal_l, mut night_internal_r) = (input_l.clone(), input_r.clone());
        make_night().process(&mut night_internal_l, &mut night_internal_r);
        let (mut night_external_l, mut night_external_r) = (input_l, input_r);
        make_night().process_with_sidechain(
            &mut night_external_l,
            &mut night_external_r,
            &side_l,
            &side_r,
        );
        assert_eq!(
            (night_internal_l, night_internal_r),
            (night_external_l, night_external_r)
        );
    }

    #[test]
    fn deesser_sidechain_三形态() {
        let base = json!({
            "eq":{"enabled":false}, "limiter":{"enabled":false},
            "deesser":{"enabled":true,"centerHz":7500,"q":0.7,"thresholdDb":-30,
                "ratio":8,"attackMs":0.05,"releaseMs":80,"splitBand":false,"mix":1,
                "sidechainEnabled":true}
        });
        let input_l: Vec<f32> = (0..2048)
            .map(|i| ((i as f64 * 0.03).sin() * 0.4) as f32)
            .collect();
        let input_r = input_l.clone();
        let side: Vec<f32> = (0..2048)
            .map(|i| (2.0 * std::f64::consts::PI * 7500.0 * i as f64 / 48_000.0).sin() as f32)
            .collect();
        let make = |overrides: &Value| {
            EngineChainStage::from_params(
                48_000.0,
                EngineChainParams::from_overrides(48_000.0, overrides).unwrap(),
            )
            .unwrap()
        };

        let mut internal = make(&base);
        let (mut internal_l, mut internal_r) = (input_l.clone(), input_r.clone());
        internal.process(&mut internal_l, &mut internal_r);

        let disabled = json!({
            "eq":{"enabled":false}, "limiter":{"enabled":false},
            "deesser":{"enabled":true,"centerHz":7500,"q":0.7,"thresholdDb":-30,
                "ratio":8,"attackMs":0.05,"releaseMs":80,"splitBand":false,"mix":1,
                "sidechainEnabled":false}
        });
        let mut ignored = make(&disabled);
        let (mut ignored_l, mut ignored_r) = (input_l.clone(), input_r.clone());
        ignored.process_with_sidechain(&mut ignored_l, &mut ignored_r, &side, &side);
        assert_eq!((internal_l.clone(), internal_r), (ignored_l, ignored_r));

        let mut external = make(&base);
        let (mut external_l, mut external_r) = (input_l, input_r);
        external.process_with_sidechain(&mut external_l, &mut external_r, &side, &side);
        assert_ne!(internal_l, external_l);
    }

    #[test]
    fn convolution_空ir回退算法_有效ir进入卷积() {
        let algorithmic = json!({
            "eq":{"enabled":false}, "limiter":{"enabled":false},
            "reverb":{"enabled":true,"mode":"algorithmic"}
        });
        let empty = json!({
            "eq":{"enabled":false}, "limiter":{"enabled":false},
            "reverb":{"enabled":true,"mode":"convolution","convolution":{"ir":[]}}
        });
        let input: Vec<f32> = (0..1024)
            .map(|i| ((i as f64 * 0.13).sin() * 0.5) as f32)
            .collect();
        let run = |overrides: &Value| {
            let mut engine = EngineChainStage::from_params(
                48_000.0,
                EngineChainParams::from_overrides(48_000.0, overrides).unwrap(),
            )
            .unwrap();
            let (mut l, mut r) = (input.clone(), input.clone());
            engine.process(&mut l, &mut r);
            (l, r)
        };
        assert_eq!(run(&algorithmic), run(&empty));

        let convolution = json!({
            "eq":{"enabled":false}, "limiter":{"enabled":false},
            "reverb":{"enabled":true,"mode":"convolution","convolution":{
                "ir":[1.0],"irName":"delta","mix":1.0,"preDelayMs":0,"dePeriodize":false
            }}
        });
        let (l, r) = run(&convolution);
        assert!(l[..512].iter().all(|sample| sample.to_bits() == 0));
        assert!(r[..512].iter().all(|sample| sample.to_bits() == 0));
        assert_ne!(l, run(&algorithmic).0);
    }

    #[test]
    fn 非法配置返回带路径错误() {
        let cases = [
            (json!({"modEffects":{"delay":1}}), "/modEffects/delay"),
            (json!({"reverb":{"mode":"bogus"}}), "/reverb/mode"),
            (
                json!({"bassEnhancer":{"harmonicType":"bogus"}}),
                "/bassEnhancer/harmonicType",
            ),
            (
                json!({"reverb":{"algorithmic":{"type":"bogus"}}}),
                "/reverb/algorithmic/type",
            ),
            (
                json!({"modulation":{"lfo":{"shape":"bogus"}}}),
                "/modulation/lfo/shape",
            ),
            (
                json!({"loudnessCompensation":{"mode":"bogus"}}),
                "/loudnessCompensation/mode",
            ),
            (json!({"ieq":{"targetCurve":"bogus"}}), "/ieq/targetCurve"),
            (
                json!({"reverb":{"enabled":true,"mode":"convolution","convolution":{"ir":"bad"}}}),
                "/reverb/convolution/ir",
            ),
            (
                json!({"reverb":{"enabled":true,"mode":"convolution","convolution":{"ir":[0.0]}}}),
                "invalid impulse response",
            ),
            (
                json!({"reverb":{"enabled":true,"mode":"convolution","convolution":{"ir":[1.0e100]}}}),
                "/reverb/convolution/ir/0",
            ),
        ];
        for (overrides, path) in cases {
            let params = EngineChainParams::from_overrides(48_000.0, &overrides).unwrap();
            let err = EngineChainStage::from_params(48_000.0, params)
                .err()
                .expect("非法配置必须返回 Err");
            assert!(err.contains(path), "错误应包含 {path}，实际 {err}");
        }
        assert!(EngineChainParams::from_overrides(f64::INFINITY, &json!({}))
            .unwrap_err()
            .contains("sample rate"));
    }

    #[test]
    fn ieq_首个分析窗后更新() {
        let params = EngineChainParams::from_overrides(
            48_000.0,
            &json!({
                "eq":{"enabled":false}, "limiter":{"enabled":false},
                "ieq":{"enabled":true,"strength":0.8,"targetCurve":"vocal","timeConstantSec":0.2}
            }),
        )
        .unwrap();
        let mut engine = EngineChainStage::from_params(48_000.0, params).unwrap();
        let mut left: Vec<f32> = (0..W)
            .map(|i| {
                ((2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 48_000.0).sin() * 0.4) as f32
            })
            .collect();
        let mut right = left.clone();
        engine.process(&mut left, &mut right);
        assert!(engine.ieq_gains().iter().any(|gain| *gain != 0.0));
    }
}
