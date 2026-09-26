#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::{num::NonZeroUsize, ops::Range, sync::Arc, time::Duration};

use rodio::Source;

#[cfg(target_arch = "wasm32")]
use crate::SoundSource;
use crate::{SoundscapeError, decoder};

/// The minimum and maximum sample amplitude in a section of an audio track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaveformPeak {
    /// The lowest sample amplitude in the section.
    pub min: f32,

    /// The highest sample amplitude in the section.
    pub max: f32,
}

impl WaveformPeak {
    const UNAVAILABLE: Self = Self {
        min: f32::INFINITY,
        max: f32::NEG_INFINITY,
    };

    /// Returns the largest absolute amplitude in this section.
    pub fn amplitude(self) -> f32 {
        if self.is_available() {
            self.min.abs().max(self.max.abs())
        } else {
            0.0
        }
    }

    /// Returns whether this peak has been decoded and is available for drawing.
    pub fn is_available(self) -> bool {
        self.min <= self.max
    }
}

/// A multiresolution amplitude overview of an audio track.
///
/// A waveform is decoded once into a compact pyramid of min/max peaks. [`Waveform::slice`]
/// borrows the smallest useful level for a time range and draw budget, so zooming and scrolling
/// do not decode audio or allocate new peak buffers.
#[derive(Clone, Debug)]
pub struct Waveform {
    channels: u16,
    sample_rate: u32,
    frame_count: u64,
    base_frames_per_peak: NonZeroUsize,
    levels: Vec<Arc<[WaveformPeak]>>,
}

impl Waveform {
    /// The number of audio frames represented by each peak at the finest default resolution.
    pub const DEFAULT_FRAMES_PER_PEAK: NonZeroUsize = NonZeroUsize::new(256).unwrap();

    /// Builds a waveform from a finite rodio source using the default resolution.
    pub fn from_source(source: impl Source<Item = f32>) -> Self {
        Self::from_source_with_base_frames(source, Self::DEFAULT_FRAMES_PER_PEAK)
    }

    /// Builds a waveform from a finite rodio source with a custom finest resolution.
    ///
    /// Lower values preserve more detail and consume more memory. Higher values are more compact.
    pub fn from_source_with_base_frames(
        source: impl Source<Item = f32>,
        base_frames_per_peak: NonZeroUsize,
    ) -> Self {
        let channels = source.channels();
        let sample_rate = source.sample_rate();
        let samples_per_peak = base_frames_per_peak
            .get()
            .saturating_mul(usize::from(channels.get()));
        let estimated_peak_count = source
            .total_duration()
            .map(|duration| {
                frame_at_duration(duration, sample_rate.get(), true)
                    .div_ceil(base_frames_per_peak.get() as u64)
                    .min(usize::MAX as u64) as usize
            })
            .unwrap_or_default();
        let mut base_level = Vec::with_capacity(estimated_peak_count);
        let mut peak = WaveformPeak {
            min: f32::INFINITY,
            max: f32::NEG_INFINITY,
        };
        let mut samples_in_peak = 0;
        let mut sample_count = 0_u64;

        for sample in source {
            peak.min = peak.min.min(sample);
            peak.max = peak.max.max(sample);
            samples_in_peak += 1;
            sample_count = sample_count.saturating_add(1);

            if samples_in_peak == samples_per_peak {
                base_level.push(peak);
                peak = WaveformPeak {
                    min: f32::INFINITY,
                    max: f32::NEG_INFINITY,
                };
                samples_in_peak = 0;
            }
        }

        if samples_in_peak > 0 {
            base_level.push(peak);
        }

        let mut levels: Vec<Arc<[WaveformPeak]>> = vec![Arc::from(base_level)];

        while levels.last().is_some_and(|level| level.len() > 1) {
            let coarser_level = levels
                .last()
                .unwrap()
                .chunks(2)
                .map(|peaks| WaveformPeak {
                    min: peaks
                        .iter()
                        .map(|peak| peak.min)
                        .fold(f32::INFINITY, f32::min),
                    max: peaks
                        .iter()
                        .map(|peak| peak.max)
                        .fold(f32::NEG_INFINITY, f32::max),
                })
                .collect::<Vec<_>>();
            levels.push(Arc::from(coarser_level));
        }

        let frame_count = sample_count.div_ceil(u64::from(channels.get()));

        Self {
            channels: channels.get(),
            sample_rate: sample_rate.get(),
            frame_count,
            base_frames_per_peak,
            levels,
        }
    }

    /// Starts building a waveform incrementally from a finite rodio source.
    ///
    /// Call [`WaveformBuilder::advance_frames`] with a bounded frame count between UI updates to
    /// avoid decoding a long track all at once.
    pub fn builder_from_source(source: impl Source<Item = f32> + 'static) -> WaveformBuilder {
        WaveformBuilder::from_source(source)
    }

    /// Starts building a waveform incrementally with a custom finest resolution.
    pub fn builder_from_source_with_base_frames(
        source: impl Source<Item = f32> + 'static,
        base_frames_per_peak: NonZeroUsize,
    ) -> WaveformBuilder {
        WaveformBuilder::from_source_with_base_frames(source, base_frames_per_peak)
    }

    /// Decodes shared bytes into a waveform.
    ///
    /// Prefer [`Self::builder_from_shared_bytes`] for long tracks to avoid blocking.
    pub fn from_shared_bytes(bytes: impl AsRef<[u8]>) -> Result<Self, SoundscapeError> {
        let bytes: Arc<[u8]> = Arc::from(bytes.as_ref());
        Ok(Self::from_source(decoder::from_shared_bytes(bytes)?))
    }

    /// Starts decoding shared bytes into a waveform incrementally.
    pub fn builder_from_shared_bytes(
        bytes: impl AsRef<[u8]>,
    ) -> Result<WaveformBuilder, SoundscapeError> {
        let bytes: Arc<[u8]> = Arc::from(bytes.as_ref());
        Ok(Self::builder_from_source(decoder::from_shared_bytes(
            bytes,
        )?))
    }

    /// Decodes static bytes into a waveform without copying the encoded audio onto the heap.
    ///
    /// Prefer [`Self::builder_from_static_bytes`] for long tracks to avoid blocking.
    pub fn from_static_bytes(bytes: &'static [u8]) -> Result<Self, SoundscapeError> {
        Ok(Self::from_source(decoder::from_static_bytes(bytes)?))
    }

    /// Starts decoding static bytes into a waveform incrementally without copying the encoded
    /// audio onto the heap.
    pub fn builder_from_static_bytes(
        bytes: &'static [u8],
    ) -> Result<WaveformBuilder, SoundscapeError> {
        Ok(Self::builder_from_source(decoder::from_static_bytes(
            bytes,
        )?))
    }

    /// Decodes a file into a waveform.
    ///
    /// Prefer [`Self::builder_from_file`] for long tracks to avoid blocking.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, SoundscapeError> {
        let source = decoder::from_file(path)?;
        Ok(Self::from_source(source))
    }

    /// Starts decoding a file into a waveform incrementally.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn builder_from_file(path: impl AsRef<Path>) -> Result<WaveformBuilder, SoundscapeError> {
        let source = decoder::from_file(path)?;
        Ok(Self::builder_from_source(source))
    }

    /// Fetches and decodes a browser URL into a waveform.
    ///
    /// Prefer [`Self::builder_from_url`] for long tracks to avoid blocking.
    #[cfg(target_arch = "wasm32")]
    pub async fn from_url(url: &str) -> Result<Self, SoundscapeError> {
        let source = SoundSource::url(url);
        let bytes = source.load_browser_bytes().await?;
        Ok(Self::from_source(decoder::from_shared_bytes(bytes)?))
    }

    /// Fetches a browser URL and starts decoding it into a waveform incrementally.
    #[cfg(target_arch = "wasm32")]
    pub async fn builder_from_url(url: &str) -> Result<WaveformBuilder, SoundscapeError> {
        let source = SoundSource::url(url);
        let bytes = source.load_browser_bytes().await?;
        Ok(Self::builder_from_source(decoder::from_shared_bytes(
            bytes,
        )?))
    }

    /// Returns the number of channels in the decoded source.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Returns the decoded source sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Returns the waveform duration calculated from its decoded frame count.
    pub fn duration(&self) -> Duration {
        duration_from_frames(self.frame_count, self.sample_rate)
    }

    /// Borrows peaks covering `range`, using no more than `max_peaks` when possible.
    ///
    /// Each returned peak covers all channels. The range is clamped to the track duration.
    pub fn slice(&self, range: Range<Duration>, max_peaks: usize) -> WaveformSlice<'_> {
        let duration = self.duration();
        let start = range.start.min(duration);
        let end = range.end.max(start).min(duration);

        if start == end || max_peaks == 0 {
            return WaveformSlice {
                peaks: &[],
                range: start..end,
                first_frame: 0,
                frames_per_peak: self.base_frames_per_peak.get() as u64,
                sample_rate: self.sample_rate,
                frame_count: self.frame_count,
            };
        }

        let start_frame = frame_at_duration(start, self.sample_rate, false);
        let end_frame = frame_at_duration(end, self.sample_rate, true).min(self.frame_count);
        let mut selected = None;

        for (level_index, level) in self.levels.iter().enumerate() {
            let frames_per_peak = (self.base_frames_per_peak.get() as u64)
                .checked_shl(level_index as u32)
                .unwrap_or(u64::MAX);
            let first_peak = start_frame / frames_per_peak;
            let end_peak = end_frame.div_ceil(frames_per_peak).min(level.len() as u64);

            selected = Some((level, first_peak, end_peak, frames_per_peak));

            if end_peak.saturating_sub(first_peak) <= max_peaks as u64 {
                break;
            }
        }

        let (level, first_peak, end_peak, frames_per_peak) = selected.unwrap();

        WaveformSlice {
            peaks: &level[first_peak as usize..end_peak as usize],
            range: start..end,
            first_frame: first_peak.saturating_mul(frames_per_peak),
            frames_per_peak,
            sample_rate: self.sample_rate,
            frame_count: self.frame_count,
        }
    }
}

/// An incrementally decoded waveform suitable for responsive user interfaces.
///
/// Each call to [`advance_frames`](Self::advance_frames) performs bounded work. Slices can be
/// borrowed at any point and contain the portion decoded so far. [`finish`](Self::finish) decodes
/// any remaining samples and returns an immutable [`Waveform`].
pub struct WaveformBuilder {
    channels: u16,
    sample_rate: u32,
    total_frame_count: Option<u64>,
    loaded_frame_count: u64,
    total_duration: Option<Duration>,
    base_frames_per_peak: NonZeroUsize,
    levels: Vec<Vec<WaveformPeak>>,
    source: Box<dyn Source<Item = f32>>,
    pending_peak: WaveformPeak,
    samples_in_peak: usize,
    current_peak_index: u64,
    active_end_peak: u64,
    finished: bool,
}

impl WaveformBuilder {
    /// Creates an incremental waveform builder using the default resolution.
    pub fn from_source(source: impl Source<Item = f32> + 'static) -> Self {
        Self::from_source_with_base_frames(source, Waveform::DEFAULT_FRAMES_PER_PEAK)
    }

    /// Creates an incremental waveform builder with a custom finest resolution.
    pub fn from_source_with_base_frames(
        source: impl Source<Item = f32> + 'static,
        base_frames_per_peak: NonZeroUsize,
    ) -> Self {
        let channels = source.channels().get();
        let sample_rate = source.sample_rate().get();
        let total_duration = source.total_duration();
        let total_frame_count =
            total_duration.map(|duration| frame_at_duration(duration, sample_rate, true));
        let base_peak_count = total_frame_count
            .map(|frame_count| frame_count.div_ceil(base_frames_per_peak.get() as u64))
            .unwrap_or(u64::MAX);
        let levels = total_frame_count.map_or_else(
            || vec![Vec::new()],
            |_| unavailable_waveform_levels(base_peak_count as usize),
        );

        Self {
            channels,
            sample_rate,
            total_frame_count,
            loaded_frame_count: 0,
            total_duration,
            base_frames_per_peak,
            levels,
            source: Box::new(source),
            pending_peak: WaveformPeak::UNAVAILABLE,
            samples_in_peak: 0,
            current_peak_index: 0,
            active_end_peak: base_peak_count,
            finished: base_peak_count == 0,
        }
    }

    /// Prioritizes missing peaks in `range` for the next call to [`Self::advance_frames`].
    ///
    /// If the range is already cached, decoding resumes at the earliest missing background range.
    /// Sources with an unknown duration continue loading sequentially.
    pub fn prioritize(&mut self, range: Range<Duration>) -> Result<(), SoundscapeError> {
        if self.finished {
            return Ok(());
        }

        let Some(total_frame_count) = self.total_frame_count else {
            return Ok(());
        };
        let duration = duration_from_frames(total_frame_count, self.sample_rate);
        let start = range.start.min(duration);
        let end = range.end.max(start).min(duration);
        let base_frames_per_peak = self.base_frames_per_peak.get() as u64;
        let base_peak_count = self.levels[0].len() as u64;
        let first_peak = (frame_at_duration(start, self.sample_rate, false) / base_frames_per_peak)
            .min(base_peak_count);
        let end_peak = frame_at_duration(end, self.sample_rate, true)
            .div_ceil(base_frames_per_peak)
            .min(base_peak_count);
        let decode_run = self
            .missing_run(first_peak, end_peak)
            .or_else(|| self.missing_run(0, base_peak_count));

        let Some((start_peak, end_peak)) = decode_run else {
            self.finished = true;
            return Ok(());
        };

        if self.current_peak_index != start_peak {
            let start_frame = start_peak.saturating_mul(base_frames_per_peak);
            self.source
                .try_seek(duration_from_frames(start_frame, self.sample_rate))
                .map_err(SoundscapeError::Seek)?;
            self.current_peak_index = start_peak;
            self.pending_peak = WaveformPeak::UNAVAILABLE;
            self.samples_in_peak = 0;
        }

        self.active_end_peak = end_peak;

        Ok(())
    }

    /// Decodes at most `max_frames` more audio frames.
    ///
    /// Returns `true` when the complete source has been decoded. A frame contains one sample per
    /// channel.
    pub fn advance_frames(&mut self, max_frames: usize) -> bool {
        if self.finished || max_frames == 0 {
            return self.finished;
        }

        let max_samples = max_frames.saturating_mul(usize::from(self.channels));
        let mut decoded_samples = 0;

        while decoded_samples < max_samples && self.current_peak_index < self.active_end_peak {
            let Some(sample) = self.source.next() else {
                self.finish_at_end_of_source();
                break;
            };

            self.pending_peak.min = self.pending_peak.min.min(sample);
            self.pending_peak.max = self.pending_peak.max.max(sample);
            self.samples_in_peak += 1;
            decoded_samples += 1;

            if self.samples_in_peak == self.samples_in_current_peak() {
                self.store_current_peak();
            }
        }

        self.finished
    }

    /// Returns whether the complete source has been decoded.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Returns the number of channels in the source.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Returns the source sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Returns the duration reported by the source, if known.
    pub fn duration(&self) -> Option<Duration> {
        self.total_duration.or_else(|| {
            self.finished
                .then(|| duration_from_frames(self.loaded_frame_count, self.sample_rate))
        })
    }

    /// Returns the total amount of audio represented by decoded peaks.
    pub fn decoded_duration(&self) -> Duration {
        duration_from_frames(self.loaded_frame_count, self.sample_rate)
    }

    /// Borrows peaks covering `range`, using no more than `max_peaks` when possible.
    ///
    /// While loading out of order, peaks that have not been decoded return `false` from
    /// [`WaveformPeak::is_available`].
    pub fn slice(&self, range: Range<Duration>, max_peaks: usize) -> WaveformSlice<'_> {
        let decoded_duration = self.decoded_duration();
        let duration = self.duration().unwrap_or(decoded_duration);
        let start = range.start.min(duration);
        let end = range.end.max(start).min(duration);

        if start == end || max_peaks == 0 {
            return self.empty_slice(start..end);
        }

        let start_frame = frame_at_duration(start, self.sample_rate, false);
        let frame_count = self.total_frame_count.unwrap_or(self.loaded_frame_count);
        let end_frame = frame_at_duration(end, self.sample_rate, true).min(frame_count);

        if start_frame >= end_frame {
            return self.empty_slice(start..end);
        }

        let mut selected = None;

        for (level_index, level) in self.levels.iter().enumerate() {
            let frames_per_peak = (self.base_frames_per_peak.get() as u64)
                .checked_shl(level_index as u32)
                .unwrap_or(u64::MAX);
            let first_peak = (start_frame / frames_per_peak).min(level.len() as u64);
            let end_peak = end_frame.div_ceil(frames_per_peak).min(level.len() as u64);

            selected = Some((level, first_peak, end_peak, frames_per_peak));

            if end_peak.saturating_sub(first_peak) <= max_peaks as u64 {
                break;
            }
        }

        let Some((level, first_peak, end_peak, frames_per_peak)) = selected else {
            return self.empty_slice(start..end);
        };

        WaveformSlice {
            peaks: &level[first_peak as usize..end_peak as usize],
            range: start..end,
            first_frame: first_peak.saturating_mul(frames_per_peak),
            frames_per_peak,
            sample_rate: self.sample_rate,
            frame_count,
        }
    }

    /// Decodes the rest of the source and returns an immutable waveform.
    pub fn finish(mut self) -> Result<Waveform, SoundscapeError> {
        while !self.finished {
            if let Some(duration) = self.total_duration {
                self.prioritize(Duration::ZERO..duration)?;
            }
            self.advance_frames(usize::MAX);
        }

        Ok(Waveform {
            channels: self.channels,
            sample_rate: self.sample_rate,
            frame_count: self.total_frame_count.unwrap_or(self.loaded_frame_count),
            base_frames_per_peak: self.base_frames_per_peak,
            levels: self.levels.into_iter().map(Arc::from).collect(),
        })
    }

    fn empty_slice(&self, range: Range<Duration>) -> WaveformSlice<'_> {
        WaveformSlice {
            peaks: &[],
            range,
            first_frame: 0,
            frames_per_peak: self.base_frames_per_peak.get() as u64,
            sample_rate: self.sample_rate,
            frame_count: self.total_frame_count.unwrap_or(self.loaded_frame_count),
        }
    }

    fn missing_run(&self, start_peak: u64, end_peak: u64) -> Option<(u64, u64)> {
        let first_missing =
            (start_peak..end_peak).find(|&index| !self.levels[0][index as usize].is_available())?;
        let run_end = (first_missing + 1..end_peak)
            .find(|&index| self.levels[0][index as usize].is_available())
            .unwrap_or(end_peak);

        Some((first_missing, run_end))
    }

    fn samples_in_current_peak(&self) -> usize {
        let base_frames_per_peak = self.base_frames_per_peak.get() as u64;
        let frames = self
            .total_frame_count
            .map_or(base_frames_per_peak, |frame_count| {
                let start_frame = self.current_peak_index.saturating_mul(base_frames_per_peak);
                frame_count
                    .saturating_sub(start_frame)
                    .min(base_frames_per_peak)
            });

        (frames as usize).saturating_mul(usize::from(self.channels))
    }

    fn store_current_peak(&mut self) {
        let frames = self.samples_in_peak.div_ceil(usize::from(self.channels)) as u64;
        let peak = if self.pending_peak.is_available() {
            self.pending_peak
        } else {
            WaveformPeak { min: 0.0, max: 0.0 }
        };

        if self.total_frame_count.is_some() {
            let peak_index = self.current_peak_index as usize;

            if !self.levels[0][peak_index].is_available() {
                self.levels[0][peak_index] = peak;
                self.loaded_frame_count = self.loaded_frame_count.saturating_add(frames);
                self.update_parent_levels(peak_index);
            }
        } else {
            self.push_sequential_peak(peak);
            self.loaded_frame_count = self.loaded_frame_count.saturating_add(frames);
        }

        self.current_peak_index = self.current_peak_index.saturating_add(1);
        self.pending_peak = WaveformPeak::UNAVAILABLE;
        self.samples_in_peak = 0;

        if self
            .total_frame_count
            .is_some_and(|frame_count| self.loaded_frame_count >= frame_count)
        {
            self.finished = true;
        }
    }

    fn update_parent_levels(&mut self, mut child_index: usize) {
        for level_index in 1..self.levels.len() {
            let parent_index = child_index / 2;
            let parent_peak = {
                let children = &self.levels[level_index - 1];
                let child_start = parent_index * 2;
                let child_end = (child_start + 2).min(children.len());
                let children = &children[child_start..child_end];

                if children.iter().all(|peak| peak.is_available()) {
                    combine_peaks(children)
                } else {
                    WaveformPeak::UNAVAILABLE
                }
            };

            self.levels[level_index][parent_index] = parent_peak;
            child_index = parent_index;
        }
    }

    fn push_sequential_peak(&mut self, peak: WaveformPeak) {
        self.levels[0].push(peak);
        let mut level_index = 0;

        loop {
            let level = &self.levels[level_index];

            if !level.len().is_multiple_of(2) {
                break;
            }

            let parent_peak = combine_peaks(&level[level.len() - 2..]);
            level_index += 1;

            if self.levels.len() == level_index {
                self.levels.push(Vec::new());
            }

            self.levels[level_index].push(parent_peak);
        }
    }

    fn finish_at_end_of_source(&mut self) {
        if self.samples_in_peak > 0 {
            self.store_current_peak();
        }

        if self.total_frame_count.is_none() {
            self.total_frame_count = Some(self.loaded_frame_count);
            self.total_duration = Some(duration_from_frames(
                self.loaded_frame_count,
                self.sample_rate,
            ));
            let base_level = std::mem::take(&mut self.levels[0]);
            self.levels = vec![base_level];

            while self.levels.last().is_some_and(|level| level.len() > 1) {
                let coarser_level = self
                    .levels
                    .last()
                    .unwrap()
                    .chunks(2)
                    .map(combine_peaks)
                    .collect();
                self.levels.push(coarser_level);
            }
        }

        self.finished = true;
    }
}

fn unavailable_waveform_levels(mut peak_count: usize) -> Vec<Vec<WaveformPeak>> {
    let mut levels = Vec::new();

    loop {
        levels.push(vec![WaveformPeak::UNAVAILABLE; peak_count]);

        if peak_count <= 1 {
            break;
        }

        peak_count = peak_count.div_ceil(2);
    }

    levels
}

fn combine_peaks(peaks: &[WaveformPeak]) -> WaveformPeak {
    WaveformPeak {
        min: peaks
            .iter()
            .map(|peak| peak.min)
            .fold(f32::INFINITY, f32::min),
        max: peaks
            .iter()
            .map(|peak| peak.max)
            .fold(f32::NEG_INFINITY, f32::max),
    }
}

/// A borrowed, density-limited view into a [`Waveform`].
#[derive(Clone, Debug)]
pub struct WaveformSlice<'a> {
    peaks: &'a [WaveformPeak],
    range: Range<Duration>,
    first_frame: u64,
    frames_per_peak: u64,
    sample_rate: u32,
    frame_count: u64,
}

impl<'a> WaveformSlice<'a> {
    /// Returns the peaks selected for this slice.
    pub fn peaks(&self) -> &'a [WaveformPeak] {
        self.peaks
    }

    /// Returns the requested range after clamping it to the waveform duration.
    pub fn range(&self) -> Range<Duration> {
        self.range.clone()
    }

    /// Returns the absolute track range represented by one peak.
    pub fn peak_range(&self, index: usize) -> Option<Range<Duration>> {
        (index < self.peaks.len()).then(|| {
            let start_frame = self
                .first_frame
                .saturating_add((index as u64).saturating_mul(self.frames_per_peak));
            let end_frame = start_frame
                .saturating_add(self.frames_per_peak)
                .min(self.frame_count);

            duration_from_frames(start_frame, self.sample_rate)
                ..duration_from_frames(end_frame, self.sample_rate)
        })
    }

    /// Returns whether this slice contains no peaks.
    pub fn is_empty(&self) -> bool {
        self.peaks.is_empty()
    }

    /// Returns the number of peaks in this slice.
    pub fn len(&self) -> usize {
        self.peaks.len()
    }
}

/// A summary of a streaming waveform decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaveformDecodeSummary {
    /// The decoded source sample rate.
    pub sample_rate: u32,

    /// The total number of finest-resolution peaks emitted through `on_chunk`.
    pub peak_count: usize,
}

/// Decodes shared audio bytes into finest-resolution peak amplitudes in bounded chunks.
///
/// Each call to `on_chunk` receives the peak offset of the chunk, the chunk amplitudes computed
/// with [`WaveformPeak::amplitude`], and the source sample rate. Offsets are contiguous: the
/// first chunk starts at peak `0` and each subsequent offset advances by the previous chunk
/// length. The callback runs synchronously on the calling thread.
///
/// `window_size` is the number of audio frames per peak at the finest resolution and is clamped
/// to at least `1`. `chunk_peak_count` bounds how many frames are decoded between callbacks and
/// is clamped to `1..=4096`.
pub fn decode_audio_to_waveform_streaming<F>(
    bytes: &[u8],
    window_size: usize,
    chunk_peak_count: usize,
    mut on_chunk: F,
) -> Result<WaveformDecodeSummary, SoundscapeError>
where
    F: FnMut(usize, Vec<f32>, u32),
{
    let source = decoder::from_shared_bytes(Arc::from(bytes))?;
    let window_size = NonZeroUsize::new(window_size.max(1)).expect("clamped to nonzero");
    let mut builder = Waveform::builder_from_source_with_base_frames(source, window_size);
    let sample_rate = builder.sample_rate();
    let frames_per_chunk = window_size
        .get()
        .saturating_mul(chunk_peak_count.clamp(1, 4096));
    let mut emitted_peak_count = 0usize;

    loop {
        let finished = builder.advance_frames(frames_per_chunk);
        let slice = builder.slice(Duration::ZERO..builder.decoded_duration(), usize::MAX);
        let chunk: Vec<f32> = slice
            .peaks()
            .iter()
            .skip(emitted_peak_count)
            .take_while(|peak| peak.is_available())
            .map(|peak| peak.amplitude())
            .collect();
        if !chunk.is_empty() {
            let chunk_len = chunk.len();
            on_chunk(emitted_peak_count, chunk, sample_rate);
            emitted_peak_count += chunk_len;
        }
        if finished {
            break;
        }
    }

    Ok(WaveformDecodeSummary {
        sample_rate,
        peak_count: emitted_peak_count,
    })
}

fn duration_from_frames(frames: u64, sample_rate: u32) -> Duration {
    let sample_rate = u64::from(sample_rate);
    let seconds = frames / sample_rate;
    let nanoseconds = frames % sample_rate * 1_000_000_000 / sample_rate;

    Duration::new(seconds, nanoseconds as u32)
}

fn frame_at_duration(duration: Duration, sample_rate: u32, round_up: bool) -> u64 {
    let sample_rate = u64::from(sample_rate);
    let whole_frames = duration.as_secs().saturating_mul(sample_rate);
    let fractional_frames = u64::from(duration.subsec_nanos()).saturating_mul(sample_rate);
    let fractional_frames = if round_up {
        fractional_frames.div_ceil(1_000_000_000)
    } else {
        fractional_frames / 1_000_000_000
    };

    whole_frames.saturating_add(fractional_frames)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::num::NonZeroUsize;

    use rodio::buffer::SamplesBuffer;
    use web_time::Duration;

    use crate::{Waveform, WaveformPeak};

    #[test]
    fn waveform_slices_preserve_extrema_with_a_bounded_peak_count() {
        let samples = vec![-0.25, 0.5, -1.25, 0.75, -0.5, 0.25, -0.75, 1.5];
        let source = SamplesBuffer::new(
            std::num::NonZeroU16::new(1).unwrap(),
            std::num::NonZeroU32::new(8).unwrap(),
            samples,
        );
        let waveform =
            Waveform::from_source_with_base_frames(source, NonZeroUsize::new(2).unwrap());

        let detailed = waveform.slice(Duration::ZERO..waveform.duration(), 4);
        assert_eq!(detailed.len(), 4);
        assert_eq!(
            detailed.peaks(),
            &[
                WaveformPeak {
                    min: -0.25,
                    max: 0.5,
                },
                WaveformPeak {
                    min: -1.25,
                    max: 0.75,
                },
                WaveformPeak {
                    min: -0.5,
                    max: 0.25,
                },
                WaveformPeak {
                    min: -0.75,
                    max: 1.5,
                },
            ]
        );

        let overview = waveform.slice(Duration::ZERO..waveform.duration(), 1);
        assert_eq!(
            overview.peaks(),
            &[WaveformPeak {
                min: -1.25,
                max: 1.5,
            }]
        );
        assert_eq!(
            overview.peak_range(0),
            Some(Duration::ZERO..waveform.duration())
        );

        let middle = waveform.slice(Duration::from_millis(250)..Duration::from_millis(750), 2);
        assert_eq!(
            middle.peaks(),
            &[
                WaveformPeak {
                    min: -1.25,
                    max: 0.75,
                },
                WaveformPeak {
                    min: -0.5,
                    max: 0.25,
                },
            ]
        );
        assert_eq!(
            middle.peak_range(0),
            Some(Duration::from_millis(250)..Duration::from_millis(500))
        );
    }

    #[test]
    fn waveform_builder_exposes_slices_while_decoding() {
        let samples = vec![-0.25, 0.5, -1.25, 0.75, -0.5, 0.25, -0.75, 1.5];
        let source = SamplesBuffer::new(
            std::num::NonZeroU16::new(1).unwrap(),
            std::num::NonZeroU32::new(8).unwrap(),
            samples,
        );
        let mut builder =
            Waveform::builder_from_source_with_base_frames(source, NonZeroUsize::new(2).unwrap());

        assert!(!builder.advance_frames(4));
        assert_eq!(builder.decoded_duration(), Duration::from_millis(500));
        assert_eq!(
            builder
                .slice(Duration::ZERO..builder.decoded_duration(), 4)
                .peaks(),
            &[
                WaveformPeak {
                    min: -0.25,
                    max: 0.5,
                },
                WaveformPeak {
                    min: -1.25,
                    max: 0.75,
                },
            ]
        );

        assert!(builder.advance_frames(5));
        let waveform = builder.finish().expect("waveform should finish decoding");
        assert_eq!(
            waveform
                .slice(Duration::ZERO..waveform.duration(), 1)
                .peaks(),
            &[WaveformPeak {
                min: -1.25,
                max: 1.5,
            }]
        );
    }

    #[test]
    fn waveform_builder_prioritizes_a_distant_requested_range() {
        let samples = vec![-0.25, 0.5, -1.25, 0.75, -0.5, 0.25, -0.75, 1.5];
        let source = SamplesBuffer::new(
            std::num::NonZeroU16::new(1).unwrap(),
            std::num::NonZeroU32::new(8).unwrap(),
            samples,
        );
        let mut builder =
            Waveform::builder_from_source_with_base_frames(source, NonZeroUsize::new(2).unwrap());

        builder
            .prioritize(Duration::from_millis(750)..Duration::from_secs(1))
            .expect("buffer source should seek to the requested range");
        assert!(!builder.advance_frames(2));

        let distant = builder.slice(Duration::from_millis(750)..Duration::from_secs(1), 1);
        assert_eq!(
            distant.peaks(),
            &[WaveformPeak {
                min: -0.75,
                max: 1.5,
            }]
        );
        assert!(
            builder
                .slice(Duration::ZERO..Duration::from_millis(250), 1)
                .peaks()
                .iter()
                .all(|peak| !peak.is_available()),
            "background peaks should remain pending until the requested window is decoded"
        );

        let waveform = builder.finish().expect("waveform should finish decoding");
        assert_eq!(
            waveform
                .slice(Duration::ZERO..waveform.duration(), 1)
                .peaks(),
            &[WaveformPeak {
                min: -1.25,
                max: 1.5,
            }]
        );
    }

    #[test]
    fn streaming_decode_emits_contiguous_amplitude_chunks() {
        let bytes = mono_pcm16_wav_bytes(8000, &alternating_samples(16, -0.5, 0.5));
        let mut chunks = Vec::new();
        let summary = crate::decode_audio_to_waveform_streaming(
            &bytes,
            4,
            2,
            |offset, amplitudes, sample_rate| {
                assert_eq!(sample_rate, 8000);
                chunks.push((offset, amplitudes));
            },
        )
        .expect("valid wav bytes should decode");

        assert_eq!(summary.sample_rate, 8000);
        assert_eq!(summary.peak_count, 4);

        let mut reassembled = Vec::new();
        for (index, (offset, amplitudes)) in chunks.iter().enumerate() {
            assert_eq!(
                *offset,
                reassembled.len(),
                "chunk {index} should continue the peak stream"
            );
            assert!(
                amplitudes.len() <= 2,
                "chunk {index} should respect the chunk bound"
            );
            reassembled.extend_from_slice(amplitudes);
        }
        assert_eq!(reassembled.len(), summary.peak_count);
        assert!(
            reassembled
                .iter()
                .all(|amplitude| (amplitude - 0.5).abs() < 0.002),
            "unexpected amplitudes: {reassembled:?}"
        );

        assert!(
            crate::decode_audio_to_waveform_streaming(b"not audio", 4, 2, |_, _, _| {
                panic!("invalid audio should not emit chunks")
            })
            .is_err(),
            "invalid audio should report a decode error"
        );
    }

    fn alternating_samples(frames: usize, even: f32, odd: f32) -> Vec<f32> {
        (0..frames)
            .map(|index| if index % 2 == 0 { even } else { odd })
            .collect()
    }

    fn mono_pcm16_wav_bytes(sample_rate: u32, samples: &[f32]) -> Vec<u8> {
        let data_len = samples.len() * 2;
        let mut bytes = Vec::with_capacity(44 + data_len);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(data_len as u32).to_le_bytes());
        for sample in samples {
            let quantized = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            bytes.extend_from_slice(&quantized.to_le_bytes());
        }
        bytes
    }
}
