use cpal::FromSample;
use rodio::{
    Source,
    source::{AutomaticGainControlSettings, LimitSettings},
};
use web_time::Duration;

use crate::SoundscapeError;

/// Settings for a rodio low-pass or high-pass filter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterEffect {
    /// The filter cutoff frequency in hertz.
    pub frequency: u32,
    /// The filter resonance or bandwidth.
    pub q: f32,
}

impl Default for FilterEffect {
    fn default() -> Self {
        Self {
            frequency: 1_000,
            q: 0.5,
        }
    }
}

/// Settings for rodio's reverb effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReverbEffect {
    /// The delay before the reflected signal.
    pub delay: Duration,
    /// The amplitude of the reflected signal.
    pub amplitude: f32,
}

impl Default for ReverbEffect {
    fn default() -> Self {
        Self {
            delay: Duration::from_millis(120),
            amplitude: 0.35,
        }
    }
}

/// Settings for rodio's distortion effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DistortionEffect {
    /// The gain applied before clipping.
    pub gain: f32,
    /// The absolute clipping threshold.
    pub threshold: f32,
}

impl Default for DistortionEffect {
    fn default() -> Self {
        Self {
            gain: 2.0,
            threshold: 0.8,
        }
    }
}

/// Settings for rodio's automatic gain control effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AutomaticGainEffect {
    /// The output level the effect tries to maintain.
    pub target_level: f32,
    /// How quickly the effect increases gain.
    pub attack: Duration,
    /// How quickly the effect reduces gain.
    pub release: Duration,
    /// The maximum gain the effect may apply.
    pub maximum_gain: f32,
}

impl Default for AutomaticGainEffect {
    fn default() -> Self {
        Self {
            target_level: 1.0,
            attack: Duration::from_secs(4),
            release: Duration::ZERO,
            maximum_gain: 7.0,
        }
    }
}

/// Settings for rodio's limiter effect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LimiterEffect {
    /// The negative dBFS level where limiting begins.
    pub threshold_db: f32,
    /// The width of the transition into limiting, in decibels.
    pub knee_width_db: f32,
    /// How quickly the limiter responds to peaks.
    pub attack: Duration,
    /// How quickly the limiter recovers after a peak.
    pub release: Duration,
}

impl Default for LimiterEffect {
    fn default() -> Self {
        Self {
            threshold_db: -1.0,
            knee_width_db: 4.0,
            attack: Duration::from_millis(5),
            release: Duration::from_millis(100),
        }
    }
}

/// Post-processing effects applied to newly played audio.
///
/// Optional effects are disabled by default. Effects can be applied before a sound is mixed with
/// [`crate::Sound::set_effects`] or after a group is mixed with [`crate::SoundGroup::set_effects`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SoundEffects {
    /// Linear gain applied before the other effects.
    pub input_gain: f32,
    /// Duration of the fade from silence when a source starts.
    pub fade_in: Duration,
    /// Optional high-pass filter settings.
    pub high_pass: Option<FilterEffect>,
    /// Optional low-pass filter settings.
    pub low_pass: Option<FilterEffect>,
    /// Optional distortion settings.
    pub distortion: Option<DistortionEffect>,
    /// Optional automatic gain control settings.
    pub automatic_gain: Option<AutomaticGainEffect>,
    /// Optional reverb settings.
    pub reverb: Option<ReverbEffect>,
    /// Optional limiter settings, applied last.
    pub limiter: Option<LimiterEffect>,
}

impl Default for SoundEffects {
    fn default() -> Self {
        Self {
            input_gain: 1.0,
            fade_in: Duration::ZERO,
            high_pass: None,
            low_pass: None,
            distortion: None,
            automatic_gain: None,
            reverb: None,
            limiter: None,
        }
    }
}

impl SoundEffects {
    pub(crate) fn validate(&self) -> Result<(), SoundscapeError> {
        if !self.input_gain.is_finite() || self.input_gain < 0.0 {
            return Err(SoundscapeError::InvalidEffect(
                "input gain must be finite and non-negative",
            ));
        }

        for filter in [self.high_pass, self.low_pass].into_iter().flatten() {
            if filter.frequency == 0 {
                return Err(SoundscapeError::InvalidEffect(
                    "filter frequency must be greater than zero",
                ));
            }
            if !filter.q.is_finite() || filter.q <= 0.0 {
                return Err(SoundscapeError::InvalidEffect(
                    "filter Q must be finite and greater than zero",
                ));
            }
        }

        if let Some(effect) = self.distortion {
            if !effect.gain.is_finite() || effect.gain < 0.0 {
                return Err(SoundscapeError::InvalidEffect(
                    "distortion gain must be finite and non-negative",
                ));
            }
            if !effect.threshold.is_finite() || effect.threshold <= 0.0 {
                return Err(SoundscapeError::InvalidEffect(
                    "distortion threshold must be finite and greater than zero",
                ));
            }
        }

        if let Some(effect) = self.automatic_gain {
            if !effect.target_level.is_finite() || effect.target_level <= 0.0 {
                return Err(SoundscapeError::InvalidEffect(
                    "automatic gain target must be finite and greater than zero",
                ));
            }
            if !effect.maximum_gain.is_finite() || effect.maximum_gain <= 0.0 {
                return Err(SoundscapeError::InvalidEffect(
                    "automatic maximum gain must be finite and greater than zero",
                ));
            }
        }

        if let Some(effect) = self.reverb
            && (!effect.amplitude.is_finite() || effect.amplitude < 0.0)
        {
            return Err(SoundscapeError::InvalidEffect(
                "reverb amplitude must be finite and non-negative",
            ));
        }

        if let Some(effect) = self.limiter {
            if !effect.threshold_db.is_finite() || effect.threshold_db >= 0.0 {
                return Err(SoundscapeError::InvalidEffect(
                    "limiter threshold must be finite and below zero dBFS",
                ));
            }
            if !effect.knee_width_db.is_finite() || effect.knee_width_db < 0.0 {
                return Err(SoundscapeError::InvalidEffect(
                    "limiter knee width must be finite and non-negative",
                ));
            }
        }

        Ok(())
    }

    pub(crate) fn apply<S>(&self, source: S) -> Box<dyn Source<Item = f32> + Send>
    where
        S: Source + Send + 'static,
        f32: FromSample<S::Item>,
    {
        let mut source: Box<dyn Source<Item = f32> + Send> = Box::new(source);

        if self.input_gain != 1.0 {
            source = Box::new(source.amplify(self.input_gain));
        }
        if let Some(effect) = self.high_pass {
            source = Box::new(source.high_pass_with_q(effect.frequency, effect.q));
        }
        if let Some(effect) = self.low_pass {
            source = Box::new(source.low_pass_with_q(effect.frequency, effect.q));
        }
        if let Some(effect) = self.distortion {
            source = Box::new(source.distortion(effect.gain, effect.threshold));
        }
        if let Some(effect) = self.automatic_gain {
            source = Box::new(source.automatic_gain_control(AutomaticGainControlSettings {
                target_level: effect.target_level,
                attack_time: effect.attack,
                release_time: effect.release,
                absolute_max_gain: effect.maximum_gain,
                ..AutomaticGainControlSettings::default()
            }));
        }
        if let Some(effect) = self.reverb {
            source = Box::new(source.buffered().reverb(effect.delay, effect.amplitude));
        }
        if let Some(effect) = self.limiter {
            source = Box::new(source.limit(LimitSettings {
                threshold: effect.threshold_db,
                knee_width: effect.knee_width_db,
                attack: effect.attack,
                release: effect.release,
            }));
        }
        if !self.fade_in.is_zero() {
            source = Box::new(source.fade_in(self.fade_in));
        }

        source
    }
}
