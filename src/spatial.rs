use std::{
    f32::consts::PI,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::Duration,
};

use rodio::{ChannelCount, SampleRate, Source, source::SeekError};
use thiserror::Error;

use crate::SoundscapeError;

const HRTF_MAGIC: &[u8; 4] = b"EHR1";
const MAX_HRTF_DIRECTIONS: usize = 512;
const MAX_HRTF_TAPS: usize = 128;
const CONTROL_INTERVAL: usize = 64;

/// A shared head-related impulse response bank for binaural rendering.
///
/// `from_bytes` accepts the Euphorium HRIR bank format: `EHR1`, a little-endian `u32` sample
/// rate, `u16` direction count, `u16` taps per ear, followed by each direction as three `f32`
/// components and then `taps` left-ear and `taps` right-ear `f32` coefficients. Directions are
/// expressed in listener-local coordinates (`+X` right, `+Y` up, `+Z` forward). All values are
/// little-endian. The bank must contain 4–512 directions and 1–128 taps per ear.
///
/// [`Self::builtin`] provides a compact, analytically generated generic-head profile.
#[derive(Clone, Debug)]
pub struct HrtfProfile(Arc<HrtfData>);

#[derive(Clone, Debug)]
struct HrtfData {
    sample_rate: u32,
    taps: usize,
    directions: Vec<HrtfDirection>,
}

#[derive(Clone, Debug)]
struct HrtfDirection {
    direction: [f32; 3],
    left: Vec<f32>,
    right: Vec<f32>,
}

impl PartialEq for HrtfProfile {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || (self.0.sample_rate == other.0.sample_rate
                && self.0.taps == other.0.taps
                && self.0.directions.len() == other.0.directions.len()
                && self
                    .0
                    .directions
                    .iter()
                    .zip(&other.0.directions)
                    .all(|(a, b)| {
                        a.direction == b.direction && a.left == b.left && a.right == b.right
                    }))
    }
}

/// Errors returned when loading an HRIR bank.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum HrtfError {
    /// The bank header, dimensions, or coefficient data is invalid.
    #[error("invalid HRTF profile: {0}")]
    InvalidProfile(&'static str),
}

impl HrtfProfile {
    /// Returns the built-in generic-head HRTF profile.
    pub fn builtin() -> Self {
        static BUILTIN: OnceLock<Arc<HrtfData>> = OnceLock::new();
        Self(
            BUILTIN
                .get_or_init(|| Arc::new(make_builtin_profile()))
                .clone(),
        )
    }

    /// Loads a Euphorium HRIR bank from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, HrtfError> {
        if bytes.get(..4) != Some(HRTF_MAGIC) {
            return Err(HrtfError::InvalidProfile("bad magic; expected EHR1"));
        }
        let sample_rate = read_u32(bytes, 4)?;
        let directions = read_u16(bytes, 8)? as usize;
        let taps = read_u16(bytes, 10)? as usize;
        if sample_rate == 0 {
            return Err(HrtfError::InvalidProfile("sample rate must be non-zero"));
        }
        if !(4..=MAX_HRTF_DIRECTIONS).contains(&directions) {
            return Err(HrtfError::InvalidProfile(
                "direction count must be between 4 and 512",
            ));
        }
        if !(1..=MAX_HRTF_TAPS).contains(&taps) {
            return Err(HrtfError::InvalidProfile(
                "tap count must be between 1 and 128",
            ));
        }

        let direction_bytes = 12usize
            .checked_add(
                taps.checked_mul(8)
                    .ok_or(HrtfError::InvalidProfile("profile dimensions overflow"))?,
            )
            .ok_or(HrtfError::InvalidProfile("profile dimensions overflow"))?;
        let expected_len = 12usize
            .checked_add(
                directions
                    .checked_mul(direction_bytes)
                    .ok_or(HrtfError::InvalidProfile("profile dimensions overflow"))?,
            )
            .ok_or(HrtfError::InvalidProfile("profile dimensions overflow"))?;
        if bytes.len() != expected_len {
            return Err(HrtfError::InvalidProfile(
                "byte length does not match the declared dimensions",
            ));
        }

        let mut cursor = 12;
        let mut entries = Vec::with_capacity(directions);
        for _ in 0..directions {
            let vector = [
                read_f32(bytes, &mut cursor)?,
                read_f32(bytes, &mut cursor)?,
                read_f32(bytes, &mut cursor)?,
            ];
            if !vector.iter().all(|value| value.is_finite()) {
                return Err(HrtfError::InvalidProfile("directions must be finite"));
            }
            let length = length3(vector);
            if length < 1e-6 {
                return Err(HrtfError::InvalidProfile(
                    "directions must have non-zero length",
                ));
            }
            let direction = scale3(vector, 1.0 / length);
            let mut left = Vec::with_capacity(taps);
            let mut right = Vec::with_capacity(taps);
            for _ in 0..taps {
                left.push(read_f32(bytes, &mut cursor)?);
            }
            for _ in 0..taps {
                right.push(read_f32(bytes, &mut cursor)?);
            }
            if !left.iter().chain(&right).all(|value| value.is_finite()) {
                return Err(HrtfError::InvalidProfile("coefficients must be finite"));
            }
            entries.push(HrtfDirection {
                direction,
                left,
                right,
            });
        }

        Ok(Self(Arc::new(HrtfData {
            sample_rate,
            taps,
            directions: entries,
        })))
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, HrtfError> {
    let bytes = bytes
        .get(offset..offset + 2)
        .ok_or(HrtfError::InvalidProfile("truncated header"))?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, HrtfError> {
    let bytes = bytes
        .get(offset..offset + 4)
        .ok_or(HrtfError::InvalidProfile("truncated header"))?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_f32(bytes: &[u8], cursor: &mut usize) -> Result<f32, HrtfError> {
    let value = f32::from_le_bytes(
        bytes
            .get(*cursor..*cursor + 4)
            .ok_or(HrtfError::InvalidProfile("truncated coefficient data"))?
            .try_into()
            .expect("four-byte slice"),
    );
    *cursor += 4;
    Ok(value)
}

fn make_builtin_profile() -> HrtfData {
    const ELEVATIONS: [f32; 7] = [-60.0, -40.0, -20.0, 0.0, 20.0, 40.0, 60.0];
    const AZIMUTH_STEPS: usize = 24;
    const TAPS: usize = 32;
    let sample_rate = 48_000;
    let mut directions = Vec::with_capacity(ELEVATIONS.len() * AZIMUTH_STEPS);

    for elevation_degrees in ELEVATIONS {
        let elevation = elevation_degrees.to_radians();
        for step in 0..AZIMUTH_STEPS {
            let azimuth = (step as f32 * 360.0 / AZIMUTH_STEPS as f32).to_radians();
            let direction = [
                elevation.cos() * azimuth.sin(),
                elevation.sin(),
                elevation.cos() * azimuth.cos(),
            ];
            let itd = 0.00062 * direction[0].abs() * sample_rate as f32;
            let left_delay = if direction[0] > 0.0 { itd } else { 0.0 };
            let right_delay = if direction[0] < 0.0 { itd } else { 0.0 };
            let mut left = vec![0.0; TAPS];
            let mut right = vec![0.0; TAPS];
            synthesize_ear(&mut left, left_delay, direction[0] < 0.0, direction[1]);
            synthesize_ear(&mut right, right_delay, direction[0] > 0.0, direction[1]);
            directions.push(HrtfDirection {
                direction,
                left,
                right,
            });
        }
    }

    HrtfData {
        sample_rate,
        taps: TAPS,
        directions,
    }
}

fn synthesize_ear(taps: &mut [f32], delay: f32, shadowed: bool, elevation: f32) {
    let start = delay.floor() as usize;
    let fraction = delay - start as f32;
    let shadow = if shadowed {
        [0.64, 0.25, 0.08, 0.03]
    } else {
        [1.0, 0.0, 0.0, 0.0]
    };
    for (index, coefficient) in shadow.into_iter().enumerate() {
        let tap = start + index * if shadowed { 2 } else { 1 };
        if tap < taps.len() {
            taps[tap] += coefficient * (1.0 - fraction) * 0.85;
        }
        if fraction > 0.0 && tap + 1 < taps.len() {
            taps[tap + 1] += coefficient * fraction * 0.85;
        }
    }

    // A small elevation-dependent pinna reflection supplies front/back and height cues.
    let reflection = 0.08 * elevation.abs();
    let reflection_tap = start + 9;
    if reflection > 0.0 && reflection_tap < taps.len() {
        taps[reflection_tap] += reflection;
    }
}

/// Selects panning or HRTF convolution for spatial sounds.
#[derive(Clone, Debug, PartialEq)]
pub enum SpatialRenderer {
    /// Equal-power stereo panning without HRTF filtering.
    StereoPanning,
    /// Binaural rendering with the selected HRIR bank.
    Hrtf(HrtfProfile),
}

/// Controls global Doppler pitch shifting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DopplerSettings {
    /// Speed of sound in meters per second.
    pub speed_of_sound_mps: f32,
    /// Scales the physical Doppler shift; zero disables it.
    pub factor: f32,
    /// Lowest allowed Doppler playback ratio.
    pub min_ratio: f32,
    /// Highest allowed Doppler playback ratio.
    pub max_ratio: f32,
}

impl Default for DopplerSettings {
    fn default() -> Self {
        Self {
            speed_of_sound_mps: 343.0,
            factor: 1.0,
            min_ratio: 0.5,
            max_ratio: 2.0,
        }
    }
}

/// Scene-wide spatial rendering settings.
#[derive(Clone, Debug, PartialEq)]
pub struct SpatialAudioSettings {
    /// The stereo renderer used by spatial sounds.
    pub renderer: SpatialRenderer,
    /// Global Doppler model and limits.
    pub doppler: DopplerSettings,
    /// Time constant used to smooth position, gain, filter, and Doppler changes.
    pub smoothing: Duration,
}

impl Default for SpatialAudioSettings {
    fn default() -> Self {
        Self {
            renderer: SpatialRenderer::StereoPanning,
            doppler: DopplerSettings::default(),
            smoothing: Duration::from_millis(20),
        }
    }
}

impl SpatialAudioSettings {
    pub(crate) fn validate(&self) -> Result<(), SoundscapeError> {
        let doppler = self.doppler;
        if !doppler.speed_of_sound_mps.is_finite() || doppler.speed_of_sound_mps <= 0.0 {
            return Err(SoundscapeError::InvalidSpatialSetting(
                "speed of sound must be finite and greater than zero",
            ));
        }
        if !doppler.factor.is_finite() || doppler.factor < 0.0 {
            return Err(SoundscapeError::InvalidSpatialSetting(
                "Doppler factor must be finite and non-negative",
            ));
        }
        if !doppler.min_ratio.is_finite()
            || !doppler.max_ratio.is_finite()
            || doppler.min_ratio <= 0.0
            || doppler.max_ratio < doppler.min_ratio
        {
            return Err(SoundscapeError::InvalidSpatialSetting(
                "Doppler ratio limits must be finite, positive, and ordered",
            ));
        }
        Ok(())
    }
}

/// Position, velocity, and orientation of the listener.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ListenerState {
    /// Listener position in meters.
    pub position: [f32; 3],
    /// Listener velocity in meters per second.
    pub velocity: [f32; 3],
    /// Listener forward direction.
    pub forward: [f32; 3],
    /// Listener up direction.
    pub up: [f32; 3],
}

impl Default for ListenerState {
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            velocity: [0.0; 3],
            forward: [0.0, 0.0, -1.0],
            up: [0.0, 1.0, 0.0],
        }
    }
}

impl ListenerState {
    pub(crate) fn normalized(mut self) -> Result<Self, SoundscapeError> {
        if !self
            .position
            .iter()
            .chain(&self.velocity)
            .chain(&self.forward)
            .chain(&self.up)
            .all(|value| value.is_finite())
        {
            return Err(SoundscapeError::InvalidSpatialSetting(
                "listener vectors must contain only finite values",
            ));
        }
        self.forward = normalize3(self.forward).ok_or(SoundscapeError::InvalidSpatialSetting(
            "listener forward must have non-zero length",
        ))?;
        let up_orthogonal = sub3(self.up, scale3(self.forward, dot3(self.up, self.forward)));
        self.up = normalize3(up_orthogonal).ok_or(SoundscapeError::InvalidSpatialSetting(
            "listener forward and up must not be parallel",
        ))?;
        Ok(self)
    }
}

/// Emitter position and velocity in world space.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EmitterState {
    /// Emitter position in meters.
    pub position: [f32; 3],
    /// Emitter velocity in meters per second.
    pub velocity: [f32; 3],
}

impl EmitterState {
    pub(crate) fn validate(self) -> Result<(), SoundscapeError> {
        if self
            .position
            .iter()
            .chain(&self.velocity)
            .all(|value| value.is_finite())
        {
            Ok(())
        } else {
            Err(SoundscapeError::InvalidSpatialSetting(
                "emitter vectors must contain only finite values",
            ))
        }
    }
}

/// Distance-based gain model for a point emitter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DistanceAttenuation {
    /// Do not attenuate gain based on distance.
    None,
    /// Apply `min(1, (reference_distance / distance)^rolloff)`, clamped at `max_distance`.
    Inverse {
        /// Distance below which gain remains at one.
        reference_distance_m: f32,
        /// Distance at which the gain stops decreasing.
        max_distance_m: f32,
        /// Exponent controlling the attenuation curve; zero disables distance attenuation.
        rolloff: f32,
    },
}

impl Default for DistanceAttenuation {
    fn default() -> Self {
        Self::Inverse {
            reference_distance_m: 1.0,
            max_distance_m: 100.0,
            rolloff: 1.0,
        }
    }
}

impl DistanceAttenuation {
    fn validate(self) -> Result<(), SoundscapeError> {
        if let Self::Inverse {
            reference_distance_m,
            max_distance_m,
            rolloff,
        } = self
            && (!reference_distance_m.is_finite()
                || !max_distance_m.is_finite()
                || !rolloff.is_finite()
                || reference_distance_m <= 0.0
                || max_distance_m < reference_distance_m
                || rolloff < 0.0)
        {
            return Err(SoundscapeError::InvalidSpatialSetting(
                "attenuation distances must be positive and ordered, and rolloff non-negative",
            ));
        }
        Ok(())
    }

    fn gain(self, distance: f32) -> f32 {
        match self {
            Self::None => 1.0,
            Self::Inverse {
                reference_distance_m,
                max_distance_m,
                rolloff,
            } => {
                let distance = distance.clamp(reference_distance_m, max_distance_m);
                (reference_distance_m / distance).powf(rolloff)
            }
        }
    }
}

/// Direct-path transmission supplied by an application's acoustic or geometry system.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Occlusion {
    /// Linear direct-path gain in the inclusive range `0.0..=1.0`.
    pub gain: f32,
    /// Optional low-pass cutoff in hertz; `None` means no additional filtering.
    pub low_pass_hz: Option<f32>,
}

impl Default for Occlusion {
    fn default() -> Self {
        Self {
            gain: 1.0,
            low_pass_hz: None,
        }
    }
}

impl Occlusion {
    pub(crate) fn validate(self) -> Result<(), SoundscapeError> {
        if !self.gain.is_finite() || !(0.0..=1.0).contains(&self.gain) {
            return Err(SoundscapeError::InvalidSpatialSetting(
                "occlusion gain must be finite and between zero and one",
            ));
        }
        if self
            .low_pass_hz
            .is_some_and(|frequency| !frequency.is_finite() || frequency <= 0.0)
        {
            return Err(SoundscapeError::InvalidSpatialSetting(
                "occlusion cutoff must be finite and greater than zero",
            ));
        }
        Ok(())
    }
}

/// Selects how a sound's channels are converted to a point emitter.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SpatialInput {
    /// Require mono input. Stereo and multichannel sources return an error when playback starts.
    MonoOnly,
    /// Average all input channels to mono before spatial rendering.
    #[default]
    DownmixToMono,
}

/// Per-sound spatial rendering settings.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SpatialSound {
    /// Emitter transform and velocity.
    pub emitter: EmitterState,
    /// Distance-based gain model.
    pub attenuation: DistanceAttenuation,
    /// Direct-path transmission from the application's acoustic model.
    pub occlusion: Occlusion,
    /// Whether this emitter contributes Doppler pitch shift.
    pub doppler: bool,
    /// How non-mono source material is handled.
    pub input: SpatialInput,
}

impl SpatialSound {
    pub(crate) fn validate(self) -> Result<(), SoundscapeError> {
        self.emitter.validate()?;
        self.attenuation.validate()?;
        self.occlusion.validate()
    }
}

struct AtomicSnapshot<const N: usize> {
    version: AtomicU32,
    values: [AtomicU32; N],
    writer: Mutex<()>,
}

impl<const N: usize> AtomicSnapshot<N> {
    fn new(values: [f32; N]) -> Self {
        Self {
            version: AtomicU32::new(0),
            values: std::array::from_fn(|index| AtomicU32::new(values[index].to_bits())),
            writer: Mutex::new(()),
        }
    }

    fn store(&self, values: [f32; N]) {
        let _writer = self.writer.lock().unwrap();
        self.version.fetch_add(1, Ordering::AcqRel);
        for (slot, value) in self.values.iter().zip(values) {
            slot.store(value.to_bits(), Ordering::Relaxed);
        }
        self.version.fetch_add(1, Ordering::Release);
    }

    fn load(&self) -> [f32; N] {
        loop {
            let before = self.version.load(Ordering::Acquire);
            if before & 1 != 0 {
                continue;
            }
            let values = std::array::from_fn(|index| {
                f32::from_bits(self.values[index].load(Ordering::Relaxed))
            });
            if self.version.load(Ordering::Acquire) == before {
                return values;
            }
        }
    }

    fn try_load(&self) -> Option<[f32; N]> {
        for _ in 0..3 {
            let before = self.version.load(Ordering::Acquire);
            if before & 1 != 0 {
                continue;
            }
            let values = std::array::from_fn(|index| {
                f32::from_bits(self.values[index].load(Ordering::Relaxed))
            });
            if self.version.load(Ordering::Acquire) == before {
                return Some(values);
            }
        }
        None
    }
}

pub(crate) struct SpatialSceneControls {
    listener: AtomicSnapshot<12>,
    settings: AtomicSnapshot<5>,
}

impl SpatialSceneControls {
    pub(crate) fn new(listener: ListenerState, settings: &SpatialAudioSettings) -> Self {
        Self {
            listener: AtomicSnapshot::new(listener_values(listener)),
            settings: AtomicSnapshot::new(settings_values(settings)),
        }
    }

    pub(crate) fn set_listener(&self, listener: ListenerState) {
        self.listener.store(listener_values(listener));
    }

    pub(crate) fn set_settings(&self, settings: &SpatialAudioSettings) {
        self.settings.store(settings_values(settings));
    }
}

fn listener_values(listener: ListenerState) -> [f32; 12] {
    [
        listener.position[0],
        listener.position[1],
        listener.position[2],
        listener.velocity[0],
        listener.velocity[1],
        listener.velocity[2],
        listener.forward[0],
        listener.forward[1],
        listener.forward[2],
        listener.up[0],
        listener.up[1],
        listener.up[2],
    ]
}

fn settings_values(settings: &SpatialAudioSettings) -> [f32; 5] {
    [
        settings.doppler.speed_of_sound_mps,
        settings.doppler.factor,
        settings.doppler.min_ratio,
        settings.doppler.max_ratio,
        settings.smoothing.as_secs_f32(),
    ]
}

pub(crate) struct SpatialSoundControls {
    values: AtomicSnapshot<9>,
}

impl SpatialSoundControls {
    pub(crate) fn new() -> Self {
        Self {
            values: AtomicSnapshot::new([0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
        }
    }

    pub(crate) fn set(&self, spatial: Option<SpatialSound>) {
        let Some(spatial) = spatial else {
            let mut values = self.values.load();
            values[8] = 0.0;
            self.values.store(values);
            return;
        };
        self.values.store([
            spatial.emitter.position[0],
            spatial.emitter.position[1],
            spatial.emitter.position[2],
            spatial.emitter.velocity[0],
            spatial.emitter.velocity[1],
            spatial.emitter.velocity[2],
            spatial.occlusion.gain,
            spatial.occlusion.low_pass_hz.unwrap_or(0.0),
            1.0,
        ]);
    }
}

/// Converts mono or multichannel input to stereo using live scene and emitter state.
pub(crate) struct SpatialSource {
    input: Box<dyn Source<Item = f32> + Send>,
    channels: usize,
    scene: Arc<SpatialSceneControls>,
    controls: Arc<SpatialSoundControls>,
    spatial: SpatialSound,
    hrtf: Option<Arc<HrtfData>>,
    left_filters: Vec<f32>,
    right_filters: Vec<f32>,
    target_left_filters: Vec<f32>,
    target_right_filters: Vec<f32>,
    history: Vec<f32>,
    history_index: usize,
    current_input: Option<f32>,
    next_input: Option<f32>,
    phase: f32,
    current_doppler: f32,
    target_doppler: f32,
    current_gain: f32,
    target_gain: f32,
    current_occlusion_gain: f32,
    target_occlusion_gain: f32,
    current_cutoff: f32,
    target_cutoff: f32,
    current_pan: f32,
    target_pan: f32,
    filter_state: f32,
    current_frame: [f32; 2],
    output_channel: usize,
    frames_until_control: usize,
    listener_snapshot: [f32; 12],
    scene_snapshot: [f32; 5],
    emitter_snapshot: [f32; 9],
    sample_rate: u32,
    smoothing_seconds: f32,
    clock: Arc<AtomicU64>,
}

impl SpatialSource {
    pub(crate) fn new<S>(
        input: S,
        spatial: SpatialSound,
        settings: &SpatialAudioSettings,
        scene: Arc<SpatialSceneControls>,
        controls: Arc<SpatialSoundControls>,
        clock: Arc<AtomicU64>,
    ) -> Result<Self, SoundscapeError>
    where
        S: Source<Item = f32> + Send + 'static,
    {
        let channels = input.channels().get() as usize;
        if spatial.input == SpatialInput::MonoOnly && channels != 1 {
            return Err(SoundscapeError::SpatialInputNotMono);
        }
        let sample_rate = input.sample_rate().get();
        let hrtf = match &settings.renderer {
            SpatialRenderer::StereoPanning => None,
            SpatialRenderer::Hrtf(profile) => {
                Some(Arc::new(resample_profile(&profile.0, sample_rate)))
            }
        };
        let taps = hrtf.as_ref().map_or(1, |profile| profile.taps);
        let listener_snapshot = scene.listener.load();
        let scene_snapshot = scene.settings.load();
        let emitter_snapshot = controls.values.load();
        let mut input: Box<dyn Source<Item = f32> + Send> = Box::new(input);
        let current_input = read_mono_frame(&mut *input, channels);
        let next_input = read_mono_frame(&mut *input, channels);
        let hrtf_vectors = initial_filters(hrtf.as_deref(), &scene, &controls);
        let mut result = Self {
            input,
            channels,
            scene,
            controls,
            spatial,
            hrtf,
            left_filters: hrtf_vectors.0.clone(),
            right_filters: hrtf_vectors.1.clone(),
            target_left_filters: hrtf_vectors.0,
            target_right_filters: hrtf_vectors.1,
            history: vec![0.0; taps],
            history_index: 0,
            current_input,
            next_input,
            phase: 0.0,
            current_doppler: 1.0,
            target_doppler: 1.0,
            current_gain: 1.0,
            target_gain: 1.0,
            current_occlusion_gain: 1.0,
            target_occlusion_gain: 1.0,
            current_cutoff: sample_rate as f32 * 0.5,
            target_cutoff: sample_rate as f32 * 0.5,
            current_pan: 0.0,
            target_pan: 0.0,
            filter_state: 0.0,
            current_frame: [0.0; 2],
            output_channel: 0,
            frames_until_control: 0,
            listener_snapshot,
            scene_snapshot,
            emitter_snapshot,
            sample_rate,
            smoothing_seconds: settings.smoothing.as_secs_f32(),
            clock,
        };
        result.update_targets();
        result.left_filters.clone_from(&result.target_left_filters);
        result
            .right_filters
            .clone_from(&result.target_right_filters);
        result.current_doppler = result.target_doppler;
        result.current_gain = result.target_gain;
        result.current_occlusion_gain = result.target_occlusion_gain;
        result.current_cutoff = result.target_cutoff;
        result.smoothing_seconds = settings.smoothing.as_secs_f32();
        result.clock.store(0, Ordering::Relaxed);
        Ok(result)
    }

    fn update_targets(&mut self) {
        if let Some(listener) = self.scene.listener.try_load() {
            self.listener_snapshot = listener;
        }
        if let Some(settings) = self.scene.settings.try_load() {
            self.scene_snapshot = settings;
        }
        if let Some(emitter) = self.controls.values.try_load() {
            self.emitter_snapshot = emitter;
        }
        let listener = self.listener_snapshot;
        let scene_settings = self.scene_snapshot;
        let emitter = self.emitter_snapshot;
        let listener_position = [listener[0], listener[1], listener[2]];
        let listener_velocity = [listener[3], listener[4], listener[5]];
        let forward =
            normalize3([listener[6], listener[7], listener[8]]).unwrap_or([0.0, 0.0, 1.0]);
        let up = normalize3([listener[9], listener[10], listener[11]]).unwrap_or([0.0, 1.0, 0.0]);
        let right = normalize3(cross3(forward, up)).unwrap_or([1.0, 0.0, 0.0]);
        let emitter_position = [emitter[0], emitter[1], emitter[2]];
        let emitter_velocity = [emitter[3], emitter[4], emitter[5]];
        let relative = sub3(emitter_position, listener_position);
        let distance = length3(relative);
        let direction = normalize3(relative).unwrap_or(scale3(forward, -1.0));
        let local_direction = [
            dot3(direction, right),
            dot3(direction, up),
            dot3(direction, scale3(forward, -1.0)),
        ];
        let local_direction = normalize3(local_direction).unwrap_or([0.0, 0.0, 1.0]);

        self.smoothing_seconds = scene_settings[4].max(0.0);
        let enabled = emitter[8] > 0.5;
        self.target_gain = if enabled {
            self.spatial.attenuation.gain(distance)
        } else {
            1.0
        };
        self.target_occlusion_gain = if enabled {
            emitter[6].clamp(0.0, 1.0)
        } else {
            1.0
        };
        let nyquist = self.sample_rate as f32 * 0.5;
        self.target_cutoff = if enabled && emitter[7] > 0.0 {
            emitter[7].clamp(20.0f32.min(nyquist), 20.0f32.max(nyquist))
        } else {
            nyquist
        };
        self.target_pan = if enabled {
            local_direction[0].clamp(-1.0, 1.0)
        } else {
            0.0
        };

        let doppler_factor = if enabled && self.spatial.doppler {
            scene_settings[1].max(0.0)
        } else {
            0.0
        };
        let sound_speed = scene_settings[0].max(1.0);
        let numerator = (sound_speed + dot3(listener_velocity, direction)).max(sound_speed * 0.1);
        let denominator = (sound_speed + dot3(emitter_velocity, direction)).max(sound_speed * 0.1);
        self.target_doppler = if doppler_factor > 0.0 {
            let ratio = 1.0 + doppler_factor * (numerator / denominator - 1.0);
            ratio.clamp(
                scene_settings[2].max(0.01),
                scene_settings[3].max(scene_settings[2]),
            )
        } else {
            1.0
        };

        if enabled {
            if let Some(profile) = &self.hrtf {
                interpolate_hrtf(
                    profile,
                    local_direction,
                    &mut self.target_left_filters,
                    &mut self.target_right_filters,
                );
            }
        } else {
            self.target_left_filters.fill(0.0);
            self.target_right_filters.fill(0.0);
            if !self.target_left_filters.is_empty() {
                self.target_left_filters[0] = 1.0;
                self.target_right_filters[0] = 1.0;
            }
        }
    }

    fn next_mono(&mut self) -> Option<f32> {
        let current = self.current_input?;
        let next = self.next_input.unwrap_or(current);
        let sample = current + (next - current) * self.phase;
        self.phase += self.current_doppler;
        while self.phase >= 1.0 {
            self.phase -= 1.0;
            self.current_input = self.next_input;
            self.next_input = self
                .current_input
                .and_then(|_| read_mono_frame(&mut *self.input, self.channels));
            if self.current_input.is_none() {
                self.phase = 0.0;
                break;
            }
        }
        Some(sample)
    }

    fn render_frame(&mut self) -> Option<[f32; 2]> {
        if self.frames_until_control == 0 {
            self.update_targets();
            self.frames_until_control = CONTROL_INTERVAL;
        }
        self.frames_until_control -= 1;
        let smoothing_frames = (self.smoothing_seconds * self.sample_rate as f32).max(1.0);
        let smoothing = (1.0 / smoothing_frames).min(1.0);
        self.current_doppler += (self.target_doppler - self.current_doppler) * smoothing;
        self.current_gain += (self.target_gain - self.current_gain) * smoothing;
        self.current_occlusion_gain +=
            (self.target_occlusion_gain - self.current_occlusion_gain) * smoothing;
        self.current_cutoff += (self.target_cutoff - self.current_cutoff) * smoothing;
        self.current_pan += (self.target_pan - self.current_pan) * smoothing;
        for (current, target) in self.left_filters.iter_mut().zip(&self.target_left_filters) {
            *current += (*target - *current) * smoothing;
        }
        for (current, target) in self
            .right_filters
            .iter_mut()
            .zip(&self.target_right_filters)
        {
            *current += (*target - *current) * smoothing;
        }

        let sample = self.next_mono()?;
        let filter_alpha = (-2.0 * PI * self.current_cutoff / self.sample_rate as f32).exp();
        self.filter_state = (1.0 - filter_alpha) * sample + filter_alpha * self.filter_state;
        let mono = self.filter_state * self.current_occlusion_gain;
        self.history[self.history_index] = mono;
        self.history_index = (self.history_index + 1) % self.history.len();

        let mut output = if self.hrtf.is_some() {
            let mut ears = [0.0; 2];
            for tap in 0..self.history.len() {
                let history_index =
                    (self.history_index + self.history.len() - 1 - tap) % self.history.len();
                let sample = self.history[history_index];
                ears[0] += sample * self.left_filters[tap];
                ears[1] += sample * self.right_filters[tap];
            }
            ears
        } else {
            let angle = (self.current_pan + 1.0) * PI * 0.25;
            [mono * angle.cos(), mono * angle.sin()]
        };
        output[0] *= self.current_gain;
        output[1] *= self.current_gain;

        let progress_ns = (self.current_doppler as f64 * 1_000_000_000.0 / self.sample_rate as f64)
            .round() as u64;
        self.clock.fetch_add(progress_ns, Ordering::Relaxed);
        Some(output)
    }
}

fn read_mono_frame(source: &mut dyn Source<Item = f32>, channels: usize) -> Option<f32> {
    let mut sum = 0.0;
    for _ in 0..channels {
        sum += source.next()?;
    }
    Some(sum / channels as f32)
}

fn initial_filters(
    profile: Option<&HrtfData>,
    scene: &SpatialSceneControls,
    controls: &SpatialSoundControls,
) -> (Vec<f32>, Vec<f32>) {
    let Some(profile) = profile else {
        return (vec![0.0], vec![0.0]);
    };
    let mut left = vec![0.0; profile.taps];
    let mut right = vec![0.0; profile.taps];
    let listener = scene.listener.load();
    let emitter = controls.values.load();
    let forward = normalize3([listener[6], listener[7], listener[8]]).unwrap_or([0.0, 0.0, -1.0]);
    let up = normalize3([listener[9], listener[10], listener[11]]).unwrap_or([0.0, 1.0, 0.0]);
    let right_axis = normalize3(cross3(forward, up)).unwrap_or([1.0, 0.0, 0.0]);
    let relative = sub3(
        [emitter[0], emitter[1], emitter[2]],
        [listener[0], listener[1], listener[2]],
    );
    let direction = normalize3(relative).unwrap_or(scale3(forward, -1.0));
    let local = normalize3([
        dot3(direction, right_axis),
        dot3(direction, up),
        dot3(direction, scale3(forward, -1.0)),
    ])
    .unwrap_or([0.0, 0.0, 1.0]);
    interpolate_hrtf(profile, local, &mut left, &mut right);
    (left, right)
}

fn interpolate_hrtf(profile: &HrtfData, direction: [f32; 3], left: &mut [f32], right: &mut [f32]) {
    left.fill(0.0);
    right.fill(0.0);
    let mut nearest = [(f32::NEG_INFINITY, 0usize); 3];
    for (index, entry) in profile.directions.iter().enumerate() {
        let similarity = dot3(direction, entry.direction);
        if similarity > nearest[0].0 {
            nearest[2] = nearest[1];
            nearest[1] = nearest[0];
            nearest[0] = (similarity, index);
        } else if similarity > nearest[1].0 {
            nearest[2] = nearest[1];
            nearest[1] = (similarity, index);
        } else if similarity > nearest[2].0 {
            nearest[2] = (similarity, index);
        }
    }
    let exact = nearest[0].0 > 0.99999;
    let weights = if exact {
        [1.0, 0.0, 0.0]
    } else {
        let mut weights = [0.0; 3];
        let mut total = 0.0;
        for (weight, (similarity, _)) in weights.iter_mut().zip(nearest) {
            *weight = (1.0 - similarity.clamp(-1.0, 1.0))
                .max(0.001)
                .recip()
                .powi(2);
            total += *weight;
        }
        weights.map(|weight| weight / total)
    };
    for (weight, (_, index)) in weights.into_iter().zip(nearest) {
        if weight == 0.0 {
            continue;
        }
        let entry = &profile.directions[index];
        for tap in 0..profile.taps {
            left[tap] += weight * entry.left[tap];
            right[tap] += weight * entry.right[tap];
        }
    }
}

fn resample_profile(profile: &HrtfData, sample_rate: u32) -> HrtfData {
    if profile.sample_rate == sample_rate {
        return profile.clone();
    }
    let taps = ((profile.taps as f64 * sample_rate as f64 / profile.sample_rate as f64).round()
        as usize)
        .clamp(1, MAX_HRTF_TAPS);
    let directions = profile
        .directions
        .iter()
        .map(|entry| HrtfDirection {
            direction: entry.direction,
            left: resample_impulse(&entry.left, taps, profile.sample_rate, sample_rate),
            right: resample_impulse(&entry.right, taps, profile.sample_rate, sample_rate),
        })
        .collect();
    HrtfData {
        sample_rate,
        taps,
        directions,
    }
}

fn resample_impulse(input: &[f32], taps: usize, input_rate: u32, output_rate: u32) -> Vec<f32> {
    (0..taps)
        .map(|index| {
            let source_position = index as f64 * input_rate as f64 / output_rate as f64;
            let start = source_position.floor() as usize;
            let fraction = (source_position - start as f64) as f32;
            let first = input.get(start).copied().unwrap_or(0.0);
            let second = input.get(start + 1).copied().unwrap_or(0.0);
            first + (second - first) * fraction
        })
        .collect()
}

impl Iterator for SpatialSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.output_channel == 0 {
            self.current_frame = self.render_frame()?;
            self.output_channel = 1;
            Some(self.current_frame[0])
        } else {
            self.output_channel = 0;
            Some(self.current_frame[1])
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, None)
    }
}

impl Source for SpatialSource {
    fn current_span_len(&self) -> Option<usize> {
        // SpatialSource always emits stereo at the sample rate captured when it was built.
        // `output_channel` is only the position within the current stereo frame, not an audio
        // span boundary. Reporting 2 then 1 here makes downstream resamplers treat every frame
        // as a separate span and can severely attenuate or corrupt the rendered audio.
        None
    }

    fn channels(&self) -> ChannelCount {
        ChannelCount::new(2).expect("two is non-zero")
    }

    fn sample_rate(&self) -> SampleRate {
        SampleRate::new(self.sample_rate).expect("captured sample rate is non-zero")
    }

    fn total_duration(&self) -> Option<Duration> {
        self.input.total_duration()
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        self.input.try_seek(position)?;
        self.current_input = read_mono_frame(&mut *self.input, self.channels);
        self.next_input = read_mono_frame(&mut *self.input, self.channels);
        self.phase = 0.0;
        self.history.fill(0.0);
        self.history_index = 0;
        self.filter_state = 0.0;
        self.output_channel = 0;
        self.clock.store(0, Ordering::Relaxed);
        Ok(())
    }
}

fn dot3(left: [f32; 3], right: [f32; 3]) -> f32 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn length3(value: [f32; 3]) -> f32 {
    ((value[0] as f64).powi(2) + (value[1] as f64).powi(2) + (value[2] as f64).powi(2))
        .sqrt()
        .min(f32::MAX as f64) as f32
}

fn normalize3(value: [f32; 3]) -> Option<[f32; 3]> {
    if !value.iter().all(|component| component.is_finite()) {
        return None;
    }
    let scale = value
        .iter()
        .map(|component| component.abs())
        .fold(0.0, f32::max);
    if scale <= 1e-6 {
        return None;
    }
    let scaled = scale3(value, 1.0 / scale);
    let length = dot3(scaled, scaled).sqrt();
    Some(scale3(scaled, 1.0 / length))
}

fn scale3(value: [f32; 3], scalar: f32) -> [f32; 3] {
    [value[0] * scalar, value[1] * scalar, value[2] * scalar]
}

fn sub3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn cross3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use rodio::{ChannelCount, SampleRate, buffer::SamplesBuffer};

    use super::*;

    fn mono_source(samples: Vec<f32>, rate: u32) -> SamplesBuffer {
        SamplesBuffer::new(
            ChannelCount::new(1).unwrap(),
            SampleRate::new(rate).unwrap(),
            samples,
        )
    }

    fn spatial_source(
        source: SamplesBuffer,
        spatial: SpatialSound,
        settings: &SpatialAudioSettings,
    ) -> SpatialSource {
        let listener = ListenerState::default();
        let controls = Arc::new(SpatialSoundControls::new());
        controls.set(Some(spatial));
        SpatialSource::new(
            source,
            spatial,
            settings,
            Arc::new(SpatialSceneControls::new(listener, settings)),
            controls,
            Arc::new(AtomicU64::new(0)),
        )
        .unwrap()
    }

    #[test]
    fn hrtf_profile_binary_parser_rejects_malformed_data() {
        assert!(matches!(
            HrtfProfile::from_bytes(b"EHR1"),
            Err(HrtfError::InvalidProfile(_))
        ));
        assert!(HrtfProfile::from_bytes(&[]).is_err());
    }

    #[test]
    fn hrtf_profile_binary_parser_accepts_a_valid_bank() {
        let mut bytes = b"EHR1".to_vec();
        bytes.extend_from_slice(&48_000u32.to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        let directions: [[f32; 3]; 4] = [
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
        ];
        for direction in directions {
            for component in direction {
                bytes.extend_from_slice(&component.to_le_bytes());
            }
            bytes.extend_from_slice(&0.5f32.to_le_bytes());
            bytes.extend_from_slice(&0.5f32.to_le_bytes());
        }
        assert!(HrtfProfile::from_bytes(&bytes).is_ok());
    }

    #[test]
    fn hrtf_binaural_output_differs_for_left_and_right_emitters() {
        let settings = SpatialAudioSettings {
            renderer: SpatialRenderer::Hrtf(HrtfProfile::builtin()),
            smoothing: Duration::ZERO,
            ..SpatialAudioSettings::default()
        };
        let mut left = spatial_source(
            mono_source(vec![1.0; 256], 48_000),
            SpatialSound {
                emitter: EmitterState {
                    position: [-1.0, 0.0, -1.0],
                    ..EmitterState::default()
                },
                attenuation: DistanceAttenuation::None,
                ..SpatialSound::default()
            },
            &settings,
        );
        let mut right = spatial_source(
            mono_source(vec![1.0; 256], 48_000),
            SpatialSound {
                emitter: EmitterState {
                    position: [1.0, 0.0, -1.0],
                    ..EmitterState::default()
                },
                attenuation: DistanceAttenuation::None,
                ..SpatialSound::default()
            },
            &settings,
        );
        let left_frame = [left.next().unwrap(), left.next().unwrap()];
        let right_frame = [right.next().unwrap(), right.next().unwrap()];
        assert!(left_frame[0] > left_frame[1]);
        assert!(right_frame[1] > right_frame[0]);
    }

    #[test]
    fn panning_and_occlusion_are_applied_to_spatial_output() {
        let settings = SpatialAudioSettings {
            smoothing: Duration::ZERO,
            ..SpatialAudioSettings::default()
        };
        let mut left = spatial_source(
            mono_source(vec![1.0; 16], 48_000),
            SpatialSound {
                emitter: EmitterState {
                    position: [-1.0, 0.0, -1.0],
                    ..EmitterState::default()
                },
                attenuation: DistanceAttenuation::None,
                ..SpatialSound::default()
            },
            &settings,
        );
        let left_frame = [left.next().unwrap(), left.next().unwrap()];
        assert!(left_frame[0] > left_frame[1]);

        let mut occluded = spatial_source(
            mono_source(vec![1.0; 16], 48_000),
            SpatialSound {
                attenuation: DistanceAttenuation::None,
                occlusion: Occlusion {
                    gain: 0.0,
                    low_pass_hz: None,
                },
                ..SpatialSound::default()
            },
            &settings,
        );
        assert_eq!(occluded.next(), Some(0.0));
        assert_eq!(occluded.next(), Some(0.0));
    }

    #[test]
    fn approaching_emitter_advances_the_spatial_source_clock_faster() {
        let settings = SpatialAudioSettings {
            smoothing: Duration::ZERO,
            ..SpatialAudioSettings::default()
        };
        let scene = Arc::new(SpatialSceneControls::new(
            ListenerState::default(),
            &settings,
        ));
        let controls = Arc::new(SpatialSoundControls::new());
        controls.set(Some(SpatialSound {
            emitter: EmitterState {
                position: [0.0, 0.0, -10.0],
                velocity: [0.0, 0.0, 34.3],
            },
            attenuation: DistanceAttenuation::None,
            doppler: true,
            ..SpatialSound::default()
        }));
        let clock = Arc::new(AtomicU64::new(0));
        let mut source = SpatialSource::new(
            mono_source(vec![1.0; 64], 1_000),
            SpatialSound {
                emitter: EmitterState {
                    position: [0.0, 0.0, -10.0],
                    velocity: [0.0, 0.0, 34.3],
                },
                attenuation: DistanceAttenuation::None,
                doppler: true,
                ..SpatialSound::default()
            },
            &settings,
            scene,
            controls,
            clock.clone(),
        )
        .unwrap();

        for _ in 0..40 {
            source.next().unwrap();
        }
        let progress = clock.load(Ordering::Relaxed);
        assert!(
            (21_000_000..24_000_000).contains(&progress),
            "20 ms of rendered audio should advance the source by about 22.2 ms, got {progress} ns"
        );
    }

    #[test]
    fn spatial_downmix_emits_stereo_and_mono_only_rejects_stereo() {
        let settings = SpatialAudioSettings::default();
        let stereo = SamplesBuffer::new(
            ChannelCount::new(2).unwrap(),
            SampleRate::new(48_000).unwrap(),
            vec![1.0, -1.0, 1.0, -1.0],
        );
        assert!(matches!(
            SpatialSource::new(
                stereo.clone(),
                SpatialSound {
                    input: SpatialInput::MonoOnly,
                    ..SpatialSound::default()
                },
                &settings,
                Arc::new(SpatialSceneControls::new(
                    ListenerState::default(),
                    &settings
                )),
                Arc::new(SpatialSoundControls::new()),
                Arc::new(AtomicU64::new(0)),
            ),
            Err(SoundscapeError::SpatialInputNotMono)
        ));
        let mut source = spatial_source(
            stereo,
            SpatialSound {
                input: SpatialInput::DownmixToMono,
                attenuation: DistanceAttenuation::None,
                ..SpatialSound::default()
            },
            &settings,
        );
        assert_eq!(source.next(), Some(0.0));
        assert_eq!(source.next(), Some(0.0));
    }

    #[test]
    fn panning_and_device_rate_conversion_preserve_a_tone() {
        use rodio::source::UniformSourceIterator;

        let input_rate = 44_100;
        let output_rate = 48_000;
        let input_frames = input_rate / 10;
        let frequency = 440.0f32;
        let samples: Vec<f32> = (0..input_frames)
            .map(|frame| (2.0 * PI * frequency * frame as f32 / input_rate as f32).sin() * 0.5)
            .collect();
        let source = SamplesBuffer::new(
            ChannelCount::new(1).unwrap(),
            SampleRate::new(input_rate).unwrap(),
            samples,
        );
        let spatial = spatial_source(
            source,
            SpatialSound {
                attenuation: DistanceAttenuation::None,
                ..SpatialSound::default()
            },
            &SpatialAudioSettings::default(),
        );
        let rendered: Vec<f32> = UniformSourceIterator::new(
            spatial,
            ChannelCount::new(2).unwrap(),
            SampleRate::new(output_rate).unwrap(),
        )
        .collect();

        let frames = rendered.chunks_exact(2).skip(64).collect::<Vec<_>>();
        assert!(frames.len() > 4_000, "expected a full rendered tone");

        let (mut sine_projection, mut cosine_projection) = (0.0f64, 0.0f64);
        for (index, frame) in frames.iter().enumerate() {
            let phase = 2.0 * std::f64::consts::PI * frequency as f64 * (index + 64) as f64
                / output_rate as f64;
            sine_projection += frame[0] as f64 * phase.sin();
            cosine_projection += frame[0] as f64 * phase.cos();
        }
        let amplitude = 2.0 * sine_projection.hypot(cosine_projection) / frames.len() as f64;
        assert!(
            (0.30..0.40).contains(&amplitude),
            "expected a clean, equal-power panned tone near 0.354 amplitude, got {amplitude}"
        );
    }
}
