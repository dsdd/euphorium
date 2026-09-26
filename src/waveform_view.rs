//! Renderer-agnostic waveform view state.
//!
//! [`WaveformView`] owns an optional [`WaveformBuilder`] plus the zoom/scroll window and scrub
//! flag that an interactive waveform widget needs. It performs the incremental decode pump
//! (`prioritize` + `advance_frames`) and the range math (visible range, seek mapping, playhead
//! and tick fractions) without depending on any UI toolkit. The application wires the result up
//! to its renderer (egui, iced, canvas, ...) and to a [`crate::Sound`] for playhead/seek.

#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::{num::NonZeroUsize, ops::Range, time::Duration};

use rodio::Source;

use crate::{SoundscapeError, Waveform, WaveformBuilder, WaveformSlice};

/// Result of [`WaveformView::poll_visible`] and
/// [`WaveformView::poll_visible_with_budget`].
#[derive(Clone, Debug, PartialEq)]
pub struct WaveformPoll {
    /// Visible time range in seconds after following the playhead.
    pub visible: Range<f64>,
    /// Whether the complete source has been decoded.
    ///
    /// When this is `false` the caller should schedule another update (for example
    /// `egui::Context::request_repaint`) and poll again.
    pub finished: bool,
}

/// Renderer-agnostic owner of waveform decoding and its visible window.
///
/// The view holds the [`WaveformBuilder`] for the current track, the zoom level
/// (`visible_secs`), the scroll position derived from the playhead, and the
/// pause-on-scrub resume flag. All time ranges are in seconds; conversions to
/// [`Duration`] for [`WaveformBuilder`] calls happen inside the view.
pub struct WaveformView {
    builder: Option<WaveformBuilder>,
    visible_secs: f64,
    view_start_secs: f64,
    resume_after_scrub: bool,
}

impl Default for WaveformView {
    /// Creates an empty view with the default zoom level.
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for WaveformView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaveformView")
            .field("is_available", &self.is_available())
            .field("is_finished", &self.is_finished())
            .field("visible_secs", &self.visible_secs)
            .field("view_start_secs", &self.view_start_secs)
            .field("resume_after_scrub", &self.resume_after_scrub)
            .finish()
    }
}

impl WaveformView {
    /// Default visible window in seconds for a new view or a new track.
    pub const DEFAULT_VISIBLE_SECS: f64 = 20.0;

    /// Creates an empty view with the default zoom level.
    pub fn new() -> Self {
        Self {
            builder: None,
            visible_secs: Self::DEFAULT_VISIBLE_SECS,
            view_start_secs: 0.0,
            resume_after_scrub: false,
        }
    }

    /// Replaces the decoded track and resets the zoom/scroll/scrub state.
    pub fn set_builder(&mut self, builder: Option<WaveformBuilder>) {
        self.builder = builder;
        self.reset_view();
    }

    /// Removes the current track and resets the zoom/scroll/scrub state.
    pub fn clear(&mut self) {
        self.set_builder(None);
    }

    /// Removes and returns the current builder, resetting the view state.
    pub fn take_builder(&mut self) -> Option<WaveformBuilder> {
        let builder = self.builder.take();
        self.reset_view();
        builder
    }

    /// Resets zoom, scroll and scrub state while keeping the current track.
    pub fn reset_view(&mut self) {
        self.visible_secs = Self::DEFAULT_VISIBLE_SECS;
        self.view_start_secs = 0.0;
        self.resume_after_scrub = false;
    }

    /// Starts decoding from a rodio source, replacing the current track.
    pub fn set_source(&mut self, source: impl Source<Item = f32> + 'static) {
        self.set_builder(Some(Waveform::builder_from_source(source)));
    }

    /// Starts decoding from a rodio source with a custom finest resolution.
    pub fn set_source_with_base_frames(
        &mut self,
        source: impl Source<Item = f32> + 'static,
        base_frames_per_peak: NonZeroUsize,
    ) {
        self.set_builder(Some(Waveform::builder_from_source_with_base_frames(
            source,
            base_frames_per_peak,
        )));
    }

    /// Starts decoding static bytes, replacing the current track.
    ///
    /// On error the view is cleared and the error is returned.
    pub fn set_static_bytes(&mut self, bytes: &'static [u8]) -> Result<(), SoundscapeError> {
        match Waveform::builder_from_static_bytes(bytes) {
            Ok(builder) => {
                self.set_builder(Some(builder));
                Ok(())
            }
            Err(error) => {
                self.set_builder(None);
                Err(error)
            }
        }
    }

    /// Starts decoding shared bytes, replacing the current track.
    ///
    /// On error the view is cleared and the error is returned.
    pub fn set_shared_bytes(&mut self, bytes: impl AsRef<[u8]>) -> Result<(), SoundscapeError> {
        match Waveform::builder_from_shared_bytes(bytes) {
            Ok(builder) => {
                self.set_builder(Some(builder));
                Ok(())
            }
            Err(error) => {
                self.set_builder(None);
                Err(error)
            }
        }
    }

    /// Starts decoding a file, replacing the current track.
    ///
    /// On error the view is cleared and the error is returned.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_file(&mut self, path: impl AsRef<Path>) -> Result<(), SoundscapeError> {
        match Waveform::builder_from_file(path) {
            Ok(builder) => {
                self.set_builder(Some(builder));
                Ok(())
            }
            Err(error) => {
                self.set_builder(None);
                Err(error)
            }
        }
    }

    /// Fetches and starts decoding a browser URL, replacing the current track.
    ///
    /// On error the view is cleared and the error is returned.
    #[cfg(target_arch = "wasm32")]
    pub async fn set_url(&mut self, url: &str) -> Result<(), SoundscapeError> {
        match Waveform::builder_from_url(url).await {
            Ok(builder) => {
                self.set_builder(Some(builder));
                Ok(())
            }
            Err(error) => {
                self.set_builder(None);
                Err(error)
            }
        }
    }

    /// Returns the current builder, if a track is loaded.
    pub fn builder(&self) -> Option<&WaveformBuilder> {
        self.builder.as_ref()
    }

    /// Returns the current builder mutably, if a track is loaded.
    pub fn builder_mut(&mut self) -> Option<&mut WaveformBuilder> {
        self.builder.as_mut()
    }

    /// Returns whether a track is loaded.
    pub fn is_available(&self) -> bool {
        self.builder.is_some()
    }

    /// Returns whether the complete source has been decoded.
    ///
    /// An empty view reports `true` so callers stop scheduling decode work.
    pub fn is_finished(&self) -> bool {
        self.builder
            .as_ref()
            .is_none_or(WaveformBuilder::is_finished)
    }

    /// Returns the source sample rate, if a track is loaded.
    pub fn sample_rate(&self) -> Option<u32> {
        self.builder.as_ref().map(WaveformBuilder::sample_rate)
    }

    /// Returns the track duration in seconds.
    ///
    /// Uses the source duration when known and falls back to the decoded
    /// duration while loading. Returns `0.0` when no track is loaded.
    pub fn duration_secs(&self) -> f64 {
        self.builder.as_ref().map_or(0.0, |builder| {
            builder
                .duration()
                .unwrap_or_else(|| builder.decoded_duration())
                .as_secs_f64()
        })
    }

    /// Returns the smallest useful visible window for the current track.
    pub fn minimum_visible_secs(&self) -> f64 {
        self.duration_secs().min(0.25)
    }

    /// Returns the current visible window size in seconds.
    pub fn visible_secs(&self) -> f64 {
        self.visible_secs
    }

    /// Returns the visible window mutably for direct binding to a slider.
    ///
    /// The value is clamped to the track duration on the next
    /// [`visible_range`](Self::visible_range) or [`poll_visible`](Self::poll_visible) call.
    pub fn visible_secs_mut(&mut self) -> &mut f64 {
        &mut self.visible_secs
    }

    /// Sets the desired visible window size in seconds.
    ///
    /// Non-finite values are ignored. Clamping to the track duration happens on
    /// the next [`visible_range`](Self::visible_range) or
    /// [`poll_visible`](Self::poll_visible) call.
    pub fn set_visible_secs(&mut self, visible_secs: f64) {
        if visible_secs.is_finite() {
            self.visible_secs = visible_secs;
        }
    }

    /// Zooms by a delta where values above `1.0` zoom in.
    ///
    /// Non-finite or non-positive deltas are ignored. This matches pinch/scroll
    /// zoom deltas: the visible window is divided by `delta`.
    pub fn zoom_by(&mut self, delta: f64) {
        if delta.is_finite() && delta > 0.0 {
            self.visible_secs /= delta;
        }
    }

    /// Halves the visible window (zoom in).
    pub fn zoom_in(&mut self) {
        self.zoom_by(2.0);
    }

    /// Doubles the visible window (zoom out).
    pub fn zoom_out(&mut self) {
        self.zoom_by(0.5);
    }

    /// Returns the visible time range in seconds, following the playhead.
    ///
    /// The window stays centered on `playhead_secs` where possible and is
    /// clamped to the track duration. Returns `0.0..0.0` when no track is
    /// loaded.
    pub fn visible_range(&mut self, playhead_secs: f64) -> Range<f64> {
        let duration_secs = self.duration_secs();
        self.visible_range_inner(duration_secs, playhead_secs)
    }

    /// Returns the visible time range as [`Duration`]s for decoding and slicing.
    pub fn waveform_range(&mut self, playhead_secs: f64) -> Range<Duration> {
        let visible = self.visible_range(playhead_secs);
        Self::durations_from_secs(&visible)
    }

    /// Prioritizes the playhead-visible range and decodes about one second of audio.
    ///
    /// This is the per-frame pump previously embedded in the example app: it
    /// computes the follow-playhead range, calls
    /// [`WaveformBuilder::prioritize`] for it, then
    /// [`WaveformBuilder::advance_frames`] with a budget of one second of
    /// frames (`sample_rate`). When the returned [`WaveformPoll::finished`] is
    /// `false`, schedule another update and poll again.
    pub fn poll_visible(&mut self, playhead_secs: f64) -> Result<WaveformPoll, SoundscapeError> {
        let budget = self.sample_rate().map_or(0, |rate| rate as usize);
        self.poll_visible_with_budget(playhead_secs, budget)
    }

    /// Prioritizes the playhead-visible range and decodes at most `max_frames`.
    ///
    /// A frame contains one sample per channel. See [`poll_visible`](Self::poll_visible).
    pub fn poll_visible_with_budget(
        &mut self,
        playhead_secs: f64,
        max_frames: usize,
    ) -> Result<WaveformPoll, SoundscapeError> {
        if self.builder.is_none() {
            return Ok(WaveformPoll {
                visible: 0.0..0.0,
                finished: true,
            });
        }

        let duration_secs = self.duration_secs();
        let visible = self.visible_range_inner(duration_secs, playhead_secs);
        let range = Self::durations_from_secs(&visible);

        let Some(builder) = self.builder.as_mut() else {
            return Ok(WaveformPoll {
                visible: 0.0..0.0,
                finished: true,
            });
        };

        builder.prioritize(range)?;
        if !builder.is_finished() {
            builder.advance_frames(max_frames);
        }

        Ok(WaveformPoll {
            visible,
            finished: builder.is_finished(),
        })
    }

    /// Borrows peaks covering `visible`, using no more than `max_peaks` when possible.
    ///
    /// Returns `None` when no track is loaded. The range is clamped to the
    /// track duration; pass the range from [`visible_range`](Self::visible_range)
    /// or [`WaveformPoll::visible`].
    pub fn slice(&self, visible: Range<f64>, max_peaks: usize) -> Option<WaveformSlice<'_>> {
        let builder = self.builder.as_ref()?;
        let duration_secs = self.duration_secs();
        let start_secs = sanitize_secs(visible.start, duration_secs);
        let end_secs = sanitize_secs(visible.end, duration_secs);
        let (start, end) = if end_secs >= start_secs {
            (start_secs, end_secs)
        } else {
            (end_secs, start_secs)
        };

        Some(builder.slice(
            Duration::from_secs_f64(start)..Duration::from_secs_f64(end),
            max_peaks,
        ))
    }

    /// Maps a `0.0..=1.0` pointer fraction inside `visible` to track seconds.
    ///
    /// Out-of-range or non-finite fractions are clamped to the visible range.
    pub fn seek_secs(visible: &Range<f64>, fraction: f64) -> f64 {
        if !visible.start.is_finite() || !visible.end.is_finite() {
            return 0.0;
        }
        let fraction = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        visible.start + (visible.end - visible.start).max(0.0) * fraction
    }

    /// Maps a track time to a `0.0..=1.0` fraction inside `visible` for drawing.
    ///
    /// Returns `0.0` for empty or invalid ranges.
    pub fn playhead_fraction(playhead_secs: f64, visible: &Range<f64>) -> f32 {
        if !playhead_secs.is_finite() || !visible.start.is_finite() || !visible.end.is_finite() {
            return 0.0;
        }
        let len = visible.end - visible.start;
        if !len.is_finite() || len <= 0.0 {
            return 0.0;
        }
        ((playhead_secs - visible.start) / len).clamp(0.0, 1.0) as f32
    }

    /// Returns the track time of tick `index` out of `tick_count` evenly spaced ticks.
    ///
    /// Returns the visible start when `tick_count <= 1`. Indices beyond the
    /// last tick clamp to the visible end.
    pub fn tick_secs(visible: &Range<f64>, index: usize, tick_count: usize) -> f64 {
        if tick_count <= 1 {
            return if visible.start.is_finite() {
                visible.start.max(0.0)
            } else {
                0.0
            };
        }
        let last = tick_count - 1;
        let fraction = index.min(last) as f64 / last as f64;
        Self::seek_secs(visible, fraction)
    }

    /// Records whether playback was active when a scrub gesture started.
    pub fn begin_scrub(&mut self, was_playing: bool) {
        self.resume_after_scrub = was_playing;
    }

    /// Clears the scrub flag, returning whether the caller should resume playback.
    pub fn end_scrub(&mut self) -> bool {
        let resume = self.resume_after_scrub;
        self.resume_after_scrub = false;
        resume
    }

    /// Returns the pending scrub-resume flag without clearing it.
    pub fn resume_after_scrub(&self) -> bool {
        self.resume_after_scrub
    }

    fn visible_range_inner(&mut self, duration_secs: f64, playhead_secs: f64) -> Range<f64> {
        if !duration_secs.is_finite() || duration_secs <= 0.0 {
            return 0.0..0.0;
        }
        if !self.visible_secs.is_finite() {
            self.visible_secs = Self::DEFAULT_VISIBLE_SECS;
        }

        let minimum_visible_secs = duration_secs.min(0.25);
        self.visible_secs = self.visible_secs.clamp(minimum_visible_secs, duration_secs);
        let visible_secs = self.visible_secs;
        let max_start = (duration_secs - visible_secs).max(0.0);
        let playhead_secs = if playhead_secs.is_finite() {
            playhead_secs
        } else {
            0.0
        };

        self.view_start_secs = (playhead_secs - visible_secs * 0.5).clamp(0.0, max_start);

        self.view_start_secs..self.view_start_secs + visible_secs
    }

    fn durations_from_secs(visible: &Range<f64>) -> Range<Duration> {
        let start = if visible.start.is_finite() {
            visible.start.max(0.0)
        } else {
            0.0
        };
        let end = if visible.end.is_finite() {
            visible.end.max(start)
        } else {
            start
        };

        Duration::from_secs_f64(start)..Duration::from_secs_f64(end)
    }
}

fn sanitize_secs(secs: f64, duration_secs: f64) -> f64 {
    if secs.is_nan() {
        return 0.0;
    }
    if !secs.is_finite() {
        return if secs.is_sign_positive() {
            duration_secs.max(0.0)
        } else {
            0.0
        };
    }
    secs.clamp(0.0, duration_secs.max(0.0))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use rodio::buffer::SamplesBuffer;

    use super::WaveformView;

    fn test_view() -> WaveformView {
        let samples = vec![-0.25, 0.5, -1.25, 0.75, -0.5, 0.25, -0.75, 1.5];
        let source = SamplesBuffer::new(
            std::num::NonZeroU16::new(1).unwrap(),
            std::num::NonZeroU32::new(8).unwrap(),
            samples,
        );
        let mut view = WaveformView::new();
        view.set_source(source);
        view
    }

    #[test]
    fn visible_range_follows_the_playhead_and_clamps_to_duration() {
        let mut view = test_view();
        assert_eq!(view.duration_secs(), 1.0);

        let visible = view.visible_range(0.5);
        assert_eq!(visible, 0.0..1.0);

        view.set_visible_secs(0.5);
        let visible = view.visible_range(0.5);
        assert_eq!(visible, 0.25..0.75);

        let start = view.visible_range(0.0);
        assert_eq!(start, 0.0..0.5);
        let end = view.visible_range(10.0);
        assert_eq!(end, 0.5..1.0);
    }

    #[test]
    fn zoom_and_seek_mapping_round_trip() {
        let mut view = test_view();
        view.set_visible_secs(0.5);
        view.zoom_in();
        assert_eq!(view.visible_secs(), 0.25);
        view.zoom_out();
        assert_eq!(view.visible_secs(), 0.5);

        let visible = view.visible_range(0.5);
        assert_eq!(WaveformView::seek_secs(&visible, 0.0), visible.start);
        assert_eq!(WaveformView::seek_secs(&visible, 1.0), visible.end);
        assert_eq!(WaveformView::seek_secs(&visible, 0.5), 0.5);
        assert_eq!(WaveformView::tick_secs(&visible, 0, 5), visible.start);
        assert_eq!(WaveformView::tick_secs(&visible, 4, 5), visible.end);
        assert_eq!(WaveformView::playhead_fraction(0.5, &visible), 0.5);
    }

    #[test]
    fn poll_decodes_to_completion_and_scrub_flag_round_trips() {
        let mut view = test_view();
        view.begin_scrub(true);
        assert!(view.resume_after_scrub());

        let mut finished = false;
        for _ in 0..16 {
            let poll = view
                .poll_visible(0.0)
                .expect("buffer source should prioritize");
            finished = poll.finished;
            if finished {
                break;
            }
        }
        assert!(finished);
        assert!(view.is_finished());
        assert!(view.end_scrub());
        assert!(!view.resume_after_scrub());
    }
}
