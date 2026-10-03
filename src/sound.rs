//! Individual sound playback and control.

#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
#[cfg(target_arch = "wasm32")]
use std::sync::Mutex;
use std::{
    collections::VecDeque,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use dasp_sample::FromSample;
use rodio::{Player, Source};

use crate::{
    PlaybackRangeSource, SoundEffects, SoundSource, SoundscapeError, decoder, format_timestamp,
    scene::SoundGroupId,
    spatial::{
        SpatialAudioSettings, SpatialSceneControls, SpatialSound, SpatialSoundControls,
        SpatialSource,
    },
    wsola::Wsola,
};

/// The current playback lifecycle state of a [`crate::Sound`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaybackState {
    /// No source is currently playing or loading.
    Idle,
    /// An audio asset is being loaded asynchronously.
    Loading,
    /// A source is actively playing.
    Playing,
    /// The current source is paused.
    Paused,
    /// The current source reached the end of playback.
    Ended,
    /// The most recent loading or playback operation failed.
    Failed,
}

/// A playback lifecycle event emitted by [`crate::Sound::poll_event`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaybackEvent {
    /// The sound moved from one lifecycle state to another.
    StateChanged {
        /// The state before the transition.
        previous: PlaybackState,
        /// The state after the transition.
        current: PlaybackState,
    },
}

/// The complete mutable state of one sound in an audio scene.
pub struct SoundNode {
    pub(crate) name: String,
    pub(crate) group: SoundGroupId,
    pub(crate) source: SoundSource,
    pub(crate) source_revision: u64,
    pub(crate) source_loaded: bool,
    pub(crate) wants_playing: bool,
    pub(crate) locally_paused: bool,

    mixer: rodio::mixer::Mixer,
    voice: Player,

    /// The total duration of the current source, if known.
    duration: Option<Duration>,

    /// The offset of the current position in the source.
    position_offset: Duration,

    /// The player position corresponding to [`Self::position_offset`].
    player_position_anchor: Duration,
    speed: f32,
    preserve_pitch: bool,
    volume: f32,
    effects: SoundEffects,
    spatial: Option<SpatialSound>,
    spatial_audio: SpatialAudioSettings,
    spatial_scene: Arc<SpatialSceneControls>,
    spatial_controls: Arc<SpatialSoundControls>,
    spatial_clock: Option<Arc<AtomicU64>>,
    spatial_clock_anchor: u64,

    /// Whether playback repeats at its configured boundary.
    looping: Arc<AtomicBool>,

    /// The configured playback boundary within a source.
    loop_range: Option<Range<Duration>>,

    /// Whether the offset is the complete position while no source is actively playing.
    position_is_held: bool,

    /// The last observed playback lifecycle state.
    playback_state: PlaybackState,

    /// Playback lifecycle transitions waiting to be observed.
    playback_events: VecDeque<PlaybackEvent>,

    /// Playback lifecycle transitions waiting to be published by the scene.
    scene_events: VecDeque<PlaybackEvent>,

    /// Maximum number of playback lifecycle transitions retained for observation.
    event_capacity: usize,

    /// The result of an audio asset being loaded in the browser.
    #[cfg(target_arch = "wasm32")]
    pending_playback: Option<PendingPlayback>,
}

#[cfg(target_arch = "wasm32")]
type PendingPlayback = Arc<Mutex<Option<Result<Arc<[u8]>, SoundscapeError>>>>;

#[derive(Clone, Copy)]
enum SourceMetadata {
    Unloaded,
    Loaded(Option<Duration>),
}

impl SoundNode {
    pub(crate) fn new(
        name: String,
        group: SoundGroupId,
        source: SoundSource,
        mixer: rodio::mixer::Mixer,
        spatial_audio: SpatialAudioSettings,
        spatial_scene: Arc<SpatialSceneControls>,
    ) -> Result<Self, SoundscapeError> {
        let voice = Player::connect_new(&mixer);
        let mut sound = Self {
            name,
            group,
            source,
            source_revision: 0,
            source_loaded: false,
            wants_playing: false,
            locally_paused: false,
            mixer,
            voice,
            duration: None,
            position_offset: Duration::ZERO,
            player_position_anchor: Duration::ZERO,
            speed: 1.0,
            preserve_pitch: false,
            volume: 1.0,
            effects: SoundEffects::default(),
            spatial: None,
            spatial_audio,
            spatial_controls: Arc::new(SpatialSoundControls::new()),
            spatial_scene,
            spatial_clock: None,
            spatial_clock_anchor: 0,
            looping: Arc::new(AtomicBool::new(false)),
            loop_range: None,
            position_is_held: true,
            playback_state: PlaybackState::Idle,
            playback_events: VecDeque::new(),
            scene_events: VecDeque::new(),
            event_capacity: usize::MAX,
            #[cfg(target_arch = "wasm32")]
            pending_playback: None,
        };
        sound.source_loaded = sound.load_source()?;
        Ok(sound)
    }

    fn connect_voice(&self) -> Player {
        Player::connect_new(&self.mixer)
    }

    fn has_current_source(&self) -> bool {
        self.source_loaded
    }

    pub(crate) fn replace_routing_preserving_playback(&mut self, mixer: rodio::mixer::Mixer) {
        let position = self.position();
        let was_playing = self.is_playing();
        let was_paused = self.is_paused();
        let has_source = self.has_current_source();
        self.mixer = mixer;

        if (was_playing || was_paused) && has_source {
            if self.try_seek(position).is_err() {
                self.voice = self.connect_voice();
                self.voice.set_volume(self.volume());
                self.voice.set_speed(self.player_speed());
                self.position_offset = position;
                self.player_position_anchor = Duration::ZERO;
                self.position_is_held = true;
            }

            return;
        }

        self.voice = self.connect_voice();
        self.voice.set_volume(self.volume());
        self.voice.set_speed(self.player_speed());
    }

    pub(crate) fn load_source(&mut self) -> Result<bool, SoundscapeError> {
        let metadata = Self::source_metadata(&self.source.clone())
            .map_err(|error| self.record_failure(error))?;
        match metadata {
            SourceMetadata::Unloaded => {
                self.reset_loaded_source(None);
                Ok(false)
            }
            SourceMetadata::Loaded(duration) => {
                self.reset_loaded_source(duration);
                Ok(true)
            }
        }
    }

    pub(crate) fn replace_source(&mut self, source: SoundSource) -> Result<(), SoundscapeError> {
        if self.source.same_resource(&source) {
            return Ok(());
        }

        let metadata = Self::source_metadata(&source)?;
        let position = self.position();
        let previous_state = self.state();
        let was_playing = previous_state == PlaybackState::Playing;
        let was_paused = previous_state == PlaybackState::Paused;
        let was_locally_paused = self.locally_paused;
        self.cancel_pending_playback();
        self.source = source;
        self.source_revision = self.source_revision.wrapping_add(1);
        let (source_loaded, duration) = match metadata {
            SourceMetadata::Unloaded => (false, None),
            SourceMetadata::Loaded(duration) => (true, duration),
        };
        let wants_playing = was_playing || was_paused;

        self.source_loaded = source_loaded;
        self.reset_loaded_source(duration);
        let position = duration.map_or(position, |duration| position.min(duration));
        self.position_offset = position;

        if source_loaded && wants_playing {
            self.play_current_source_at(position)?;
            if was_paused {
                self.pause();
            }
        }

        self.wants_playing = wants_playing;
        self.locally_paused = was_locally_paused;

        #[cfg(target_arch = "wasm32")]
        if !source_loaded
            && wants_playing
            && let SoundSource::Url(..) = self.source.clone()
        {
            let source = self.source.clone();
            self.start_url_playback(&source)?;
        }

        Ok(())
    }

    fn source_metadata(source: &SoundSource) -> Result<SourceMetadata, SoundscapeError> {
        let duration = match source {
            SoundSource::Empty => return Ok(SourceMetadata::Unloaded),
            SoundSource::StaticBytes(bytes) => decoder::from_static_bytes(bytes)?.total_duration(),
            SoundSource::SharedBytes(bytes) => {
                decoder::from_shared_bytes(bytes.clone())?.total_duration()
            }
            #[cfg(not(target_arch = "wasm32"))]
            SoundSource::File(path) => decoder::from_file(path)?.total_duration(),
            #[cfg(target_arch = "wasm32")]
            SoundSource::Url(..) => {
                let Some(bytes) = source.cached_browser_bytes() else {
                    return Ok(SourceMetadata::Unloaded);
                };
                decoder::from_shared_bytes(bytes)?.total_duration()
            }
        };
        Ok(SourceMetadata::Loaded(duration))
    }

    /// Resumes playback of a paused player.
    ///
    /// No effect if not paused.
    fn play_source<S>(&mut self, source: S) -> Result<(), SoundscapeError>
    where
        S: Source + Send + 'static,
        f32: FromSample<S::Item>,
    {
        self.play_source_at(source, None)
    }

    fn play_source_at<S>(
        &mut self,
        source: S,
        requested_position: Option<Duration>,
    ) -> Result<(), SoundscapeError>
    where
        S: Source + Send + 'static,
        f32: FromSample<S::Item>,
    {
        self.queue_source_at(source, requested_position, true)
    }

    fn queue_source_at<S>(
        &mut self,
        mut source: S,
        requested_position: Option<Duration>,
        autoplay: bool,
    ) -> Result<(), SoundscapeError>
    where
        S: Source + Send + 'static,
        f32: FromSample<S::Item>,
    {
        let duration = source.total_duration();
        let (range_start, range_end) = self
            .playback_bounds(duration)
            .map_err(|error| self.record_failure(error))?;
        let position = if let Some(position) = requested_position {
            position
        } else if self.is_empty() {
            match source.total_duration() {
                Some(duration) => self.position_offset.min(duration),
                None => self.position_offset,
            }
        } else {
            Duration::ZERO
        };
        let position = range_end.map_or(position.max(range_start), |end| {
            position.clamp(range_start, end)
        });

        if !position.is_zero() {
            source
                .try_seek(position)
                .map_err(SoundscapeError::Seek)
                .map_err(|error| self.record_failure(error))?;
        }

        let source = PlaybackRangeSource::new(
            source,
            range_start,
            range_end,
            position,
            self.looping.clone(),
        );
        let source = self.process_source(source)?;
        self.position_offset = position;
        self.player_position_anchor = Duration::ZERO;
        self.position_is_held = true;
        self.duration = duration;

        let (new_player, queue) = Player::new();
        let volume = self.volume();

        new_player.set_volume(volume);
        new_player.set_speed(self.player_speed());
        if !autoplay {
            new_player.pause();
        }
        new_player.append(source);
        if autoplay {
            new_player.play();
        }

        // Finish preparing the queue before the renderer can consume it. Appending
        // to an already connected queue can contend on its mutex in an AudioWorklet.
        self.mixer.add(queue);
        self.voice = new_player;
        self.position_offset = position;
        self.player_position_anchor = Duration::ZERO;
        self.position_is_held = false;
        if autoplay {
            self.set_playback_state(PlaybackState::Playing);
        }

        Ok(())
    }

    fn play_current_source_at(&mut self, position: Duration) -> Result<(), SoundscapeError> {
        match self.source.clone() {
            SoundSource::Empty => Err(self.record_failure(SoundscapeError::NoAudioSource)),
            SoundSource::StaticBytes(bytes) => {
                let source = decoder::from_static_bytes(bytes)
                    .map_err(|error| self.record_failure(error))?;
                self.play_source_at(source, Some(position))
            }
            SoundSource::SharedBytes(bytes) => {
                let source = decoder::from_shared_bytes(bytes)
                    .map_err(|error| self.record_failure(error))?;
                self.play_source_at(source, Some(position))
            }
            #[cfg(not(target_arch = "wasm32"))]
            SoundSource::File(path) => {
                let source =
                    decoder::from_file(path).map_err(|error| self.record_failure(error))?;
                self.play_source_at(source, Some(position))
            }
            #[cfg(target_arch = "wasm32")]
            SoundSource::Url(..) => {
                let source = decoder::from_shared_bytes(
                    self.source
                        .cached_browser_bytes()
                        .ok_or_else(|| self.record_failure(SoundscapeError::NoAudioSource))?,
                )
                .map_err(|error| self.record_failure(error))?;
                self.play_source_at(source, Some(position))
            }
        }
    }

    fn playback_bounds(
        &self,
        duration: Option<Duration>,
    ) -> Result<(Duration, Option<Duration>), SoundscapeError> {
        let Some(range) = self.loop_range.as_ref() else {
            return Ok((Duration::ZERO, duration));
        };
        let end = duration.map_or(range.end, |duration| range.end.min(duration));

        if range.start >= end {
            return Err(SoundscapeError::InvalidPlaybackRange);
        }

        Ok((range.start, Some(end)))
    }

    fn process_source<S>(
        &mut self,
        source: S,
    ) -> Result<Box<dyn Source<Item = f32> + Send>, SoundscapeError>
    where
        S: Source + Send + 'static,
        f32: FromSample<S::Item>,
    {
        let source = self.effects.apply(source);

        let source: Box<dyn Source<Item = f32> + Send> = if self.preserve_pitch {
            Box::new(Wsola::new(source, self.speed))
        } else {
            source
        };

        if let Some(spatial) = self.spatial {
            let clock = Arc::new(AtomicU64::new(0));
            let processed = SpatialSource::new(
                source,
                spatial,
                &self.spatial_audio,
                self.spatial_scene.clone(),
                self.spatial_controls.clone(),
                clock.clone(),
            )?;
            self.spatial_clock = Some(clock);
            self.spatial_clock_anchor = 0;
            Ok(Box::new(processed))
        } else {
            self.spatial_clock = None;
            self.spatial_clock_anchor = 0;
            Ok(source)
        }
    }

    fn player_speed(&self) -> f32 {
        if self.preserve_pitch { 1.0 } else { self.speed }
    }

    fn reset_loaded_source(&mut self, duration: Option<Duration>) {
        self.voice.stop();

        self.position_offset = Duration::ZERO;
        self.player_position_anchor = Duration::ZERO;
        self.position_is_held = true;
        self.duration = duration;
        self.set_playback_state(PlaybackState::Idle);
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn load_resolved_bytes(&mut self, bytes: Arc<[u8]>) -> Result<(), SoundscapeError> {
        let duration = decoder::from_shared_bytes(bytes)
            .map_err(|error| self.record_failure(error))?
            .total_duration();

        self.reset_loaded_source(duration);
        Ok(())
    }

    /// Plays an audio asset using its native path.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn play_file(&mut self, path: impl AsRef<Path>) -> Result<(), SoundscapeError> {
        let path = path.as_ref().to_path_buf();
        let source = decoder::from_file(&path).map_err(|error| self.record_failure(error))?;

        self.play_source(source)?;
        Ok(())
    }

    /// Starts loading and playing a browser URL in the background.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn start_url_playback(
        &mut self,
        source: &SoundSource,
    ) -> Result<(), SoundscapeError> {
        if self.pending_playback.is_some() {
            return Ok(());
        }

        if let Some(bytes) = source.cached_browser_bytes() {
            let decoded =
                decoder::from_shared_bytes(bytes).map_err(|error| self.record_failure(error))?;
            self.play_source(decoded)?;
            return Ok(());
        }

        let pending_playback = PendingPlayback::default();
        let pending_result = pending_playback.clone();
        let source = source.clone();

        wasm_bindgen_futures::spawn_local(async move {
            *pending_result.lock().unwrap() = Some(source.load_browser_bytes().await);
        });

        self.pending_playback = Some(pending_playback);
        self.set_playback_state(PlaybackState::Loading);
        Ok(())
    }

    /// Completes a browser asset playback once its fetch has finished.
    ///
    /// Returns `None` while no playback is pending or the current fetch is still in progress.
    pub fn poll_pending_playback(&mut self) -> Option<Result<(), SoundscapeError>> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            None
        }

        #[cfg(target_arch = "wasm32")]
        {
            let result = self.pending_playback.as_ref()?.lock().unwrap().take()?;
            self.pending_playback = None;

            let result = result.and_then(|bytes| {
                let source = decoder::from_shared_bytes(bytes.clone())?;
                self.play_source(source)?;
                Ok(())
            });

            if result.is_err() {
                self.set_playback_state(PlaybackState::Failed);
            }

            Some(result)
        }
    }

    pub(crate) fn cancel_pending_playback(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            self.pending_playback = None;
        }
    }

    /// Returns the current playback lifecycle state.
    pub fn state(&mut self) -> PlaybackState {
        self.refresh_playback_state();
        self.playback_state
    }

    /// Returns the next pending playback lifecycle event.
    ///
    /// Natural playback completion is detected when this method or [`Self::state`] is called.
    pub fn poll_event(&mut self) -> Option<PlaybackEvent> {
        self.refresh_playback_state();
        self.playback_events.pop_front()
    }

    pub(crate) fn poll_scene_event(&mut self) -> Option<PlaybackEvent> {
        self.refresh_playback_state();
        self.scene_events.pop_front()
    }

    pub(crate) fn set_event_capacity(&mut self, capacity: usize) {
        self.event_capacity = capacity;
        while self.playback_events.len() > capacity {
            self.playback_events.pop_front();
        }
    }

    fn refresh_playback_state(&mut self) {
        let state = self.playback_state;

        if matches!(state, PlaybackState::Playing | PlaybackState::Paused) && self.voice.empty() {
            self.set_playback_state(PlaybackState::Ended);
        }
    }

    fn set_playback_state(&mut self, state: PlaybackState) {
        if self.playback_state == state {
            return;
        }

        let previous = self.playback_state;
        self.playback_state = state;

        let event = PlaybackEvent::StateChanged {
            previous,
            current: state,
        };
        self.scene_events.push_back(event);

        if self.event_capacity > 0 {
            if self.playback_events.len() >= self.event_capacity {
                self.playback_events.pop_front();
            }
            self.playback_events.push_back(event);
        }
    }

    fn record_failure(&mut self, error: SoundscapeError) -> SoundscapeError {
        self.set_playback_state(PlaybackState::Failed);
        error
    }

    /// Returns whether a browser audio asset is currently loading.
    pub fn is_loading(&mut self) -> bool {
        self.state() == PlaybackState::Loading
    }

    /// Convenience version of [`Self::try_seek`] that accepts the position in seconds.
    pub fn try_seek_secs(&mut self, position_secs: f64) -> Result<(), SoundscapeError> {
        if position_secs.is_nan() || position_secs.is_infinite() || position_secs < 0.0 {
            return Err(self.record_failure(SoundscapeError::InvalidSeekPosition));
        }

        self.try_seek(Duration::from_secs_f64(position_secs))
    }

    /// Attempts to seek to a given position in the current source.
    ///
    /// This blocks between 0 and ~5 milliseconds.
    ///
    /// As long as the duration of the source is known, seek is guaranteed to saturate
    /// at the end of the source. For example given a source that reports a total duration
    /// of 42 seconds calling `try_seek()` with 60 seconds as argument will seek to
    /// 42 seconds.
    pub fn try_seek_without_decoding(&mut self, position: Duration) -> Result<(), SoundscapeError> {
        if self.set_position_if_empty(position) {
            return Ok(());
        }

        // `Player` applies `speed` through its internal `Speed` filter (or through
        // `Wsola` when pitch is preserved). In both cases player time runs at
        // `source_time / speed`, and `Speed::try_seek`/`Wsola::try_seek` multiply
        // the requested player position by `speed` to obtain the source position.
        // Passing the source position directly therefore overshoots by `speed`,
        // which seeks past the end (and immediately ends playback) when
        // `speed > 1` and the target is near the end.
        let player_position = if self.speed.is_finite() && self.speed > 0.0 {
            position.div_f32(self.speed)
        } else {
            position
        };
        self.voice
            .try_seek(player_position)
            .map_err(SoundscapeError::Seek)
            .map_err(|error| self.record_failure(error))?;
        self.position_offset = position;
        self.player_position_anchor = player_position;
        self.position_is_held = false;
        self.spatial_clock_anchor = 0;

        Ok(())
    }

    /// Functionally equivalent to `try_seek_without_decoding`, but avoids the deadlock that occurs
    /// when calling [`Player::try_seek`] on the same thread as the audio callback.
    pub fn try_seek(&mut self, position: Duration) -> Result<(), SoundscapeError> {
        if self.set_position_if_empty(position) {
            return Ok(());
        }

        let position = match self.duration() {
            Some(duration) => position.min(duration),
            None => position,
        };

        match self.source.clone() {
            SoundSource::Empty => Err(SoundscapeError::NoAudioSource),
            SoundSource::StaticBytes(bytes) => {
                self.seek_with_decode_source(decoder::from_static_bytes(bytes)?, position)
            }
            SoundSource::SharedBytes(bytes) => {
                self.seek_with_decode_source(decoder::from_shared_bytes(bytes)?, position)
            }
            #[cfg(not(target_arch = "wasm32"))]
            SoundSource::File(path) => {
                self.seek_with_decode_source(decoder::from_file(path)?, position)
            }
            #[cfg(target_arch = "wasm32")]
            SoundSource::Url(..) => {
                let source = decoder::from_shared_bytes(
                    self.source
                        .cached_browser_bytes()
                        .ok_or(SoundscapeError::NoAudioSource)?,
                )?;
                self.seek_with_decode_source(source, position)
            }
        }
    }

    fn set_position_if_empty(&mut self, position: Duration) -> bool {
        if !self.is_empty() {
            return false;
        }

        let position = match self.duration() {
            Some(duration) => position.min(duration),
            None => position,
        };
        let volume = self.volume();
        let was_paused = self.is_paused();

        let new_player = self.connect_voice();
        new_player.set_volume(volume);
        new_player.set_speed(self.player_speed());

        if was_paused {
            new_player.pause();
        }

        self.voice = new_player;

        self.position_offset = position;
        self.player_position_anchor = Duration::ZERO;
        self.position_is_held = true;

        if self.playback_state == PlaybackState::Ended {
            self.set_playback_state(PlaybackState::Idle);
        }

        true
    }

    fn seek_with_decode_source<S>(
        &mut self,
        mut source: S,
        position: Duration,
    ) -> Result<(), SoundscapeError>
    where
        S: Source + Send + 'static,
        f32: FromSample<S::Item>,
    {
        let was_paused = self.voice.is_paused();

        // This decoder is NOT owned by the audio callback yet,
        // therefore this seek executes directly on this thread.

        let (range_start, range_end) = self
            .playback_bounds(source.total_duration())
            .map_err(|error| self.record_failure(error))?;
        let position = range_end.map_or(position.max(range_start), |end| {
            position.clamp(range_start, end)
        });
        source
            .try_seek(position)
            .map_err(SoundscapeError::Seek)
            .map_err(|error| self.record_failure(error))?;
        let source = PlaybackRangeSource::new(
            source,
            range_start,
            range_end,
            position,
            self.looping.clone(),
        );
        let source = self.process_source(source)?;

        let volume = self.volume();

        // Prepare the queue before exposing it to the audio callback. On WASM,
        // contending for the queue mutex from the browser main thread would
        // attempt a forbidden `Atomics.wait`.
        let (new_player, output) = Player::new();

        new_player.set_volume(volume);
        new_player.set_speed(self.player_speed());

        if was_paused {
            new_player.pause();
        }

        new_player.append(source);

        if !was_paused {
            new_player.play();
        }

        self.mixer.add(output);

        // Dropping the old Player only marks its source as stopped.
        // It does not synchronously wait for WebAudio.
        self.voice = new_player;

        self.position_offset = position;
        self.player_position_anchor = Duration::ZERO;
        self.position_is_held = false;

        Ok(())
    }

    /// Returns the current rodio post-processing settings.
    pub fn effects(&self) -> SoundEffects {
        self.effects
    }

    pub(crate) fn spatial(&self) -> Option<SpatialSound> {
        self.spatial
    }

    pub(crate) fn set_spatial(&mut self, spatial: Option<SpatialSound>) {
        self.spatial_controls.set(spatial);
        self.spatial = spatial;
    }

    pub(crate) fn set_spatial_audio(&mut self, settings: SpatialAudioSettings) {
        self.spatial_audio = settings;
    }

    pub(crate) fn set_emitter(
        &mut self,
        emitter: crate::EmitterState,
    ) -> Result<(), SoundscapeError> {
        emitter.validate()?;
        let spatial = self
            .spatial
            .as_mut()
            .ok_or(SoundscapeError::SoundNotSpatialized)?;
        spatial.emitter = emitter;
        self.spatial_controls.set(Some(*spatial));
        Ok(())
    }

    pub(crate) fn set_occlusion(
        &mut self,
        occlusion: crate::Occlusion,
    ) -> Result<(), SoundscapeError> {
        occlusion.validate()?;
        let spatial = self
            .spatial
            .as_mut()
            .ok_or(SoundscapeError::SoundNotSpatialized)?;
        spatial.occlusion = occlusion;
        self.spatial_controls.set(Some(*spatial));
        Ok(())
    }

    /// Replaces the rodio post-processing chain.
    ///
    /// If a source is playing, its decoder is rebuilt at the current position and keeps its
    /// paused state. Loaded or stopped sources use the settings the next time playback starts.
    pub fn set_effects(&mut self, effects: SoundEffects) -> Result<(), SoundscapeError> {
        effects.validate()?;

        if effects == self.effects {
            return Ok(());
        }

        if self.is_empty() {
            self.effects = effects;
            return Ok(());
        }

        let position = self.position();
        let previous_effects = std::mem::replace(&mut self.effects, effects);
        let result = self.try_seek(position);

        if result.is_err() {
            self.effects = previous_effects;
        }

        result
    }

    /// Returns the position of the sound that's being played.
    ///
    /// This takes into account any speedup or delay applied.
    ///
    /// Example: if you apply a speedup of *2* to an mp3 decoder source and
    /// [`get_pos()`](Player::get_pos) returns *5s* then the position in the mp3
    /// recording is *10s* from its start.
    pub fn position(&self) -> Duration {
        let player_position = if self.position_is_held {
            Duration::ZERO
        } else {
            self.voice.get_pos()
        };

        self.position_at_player_time(player_position)
    }

    fn position_at_player_time(&self, player_position: Duration) -> Duration {
        if let Some(clock) = &self.spatial_clock {
            if self.position_is_held {
                return self.position_offset;
            }
            let elapsed = Duration::from_nanos(
                clock
                    .load(Ordering::Relaxed)
                    .saturating_sub(self.spatial_clock_anchor),
            );
            let elapsed = if self.preserve_pitch {
                elapsed.mul_f32(self.speed)
            } else {
                elapsed
            };
            let position = self.position_offset.saturating_add(elapsed);
            if self.is_looping()
                && let Ok((start, Some(end))) = self.playback_bounds(self.duration())
                && position >= end
            {
                let range_duration = end - start;
                let range_nanos = range_duration.as_nanos();
                if range_nanos > 0 {
                    let position_nanos = position.saturating_sub(start).as_nanos() % range_nanos;
                    return start
                        + Duration::new(
                            (position_nanos / 1_000_000_000) as u64,
                            (position_nanos % 1_000_000_000) as u32,
                        );
                }
            }
            return self
                .duration()
                .map_or(position, |duration| position.min(duration));
        }

        let elapsed = player_position.saturating_sub(self.player_position_anchor);
        let mut position = self
            .position_offset
            .saturating_add(elapsed.mul_f32(self.speed));

        if self.is_looping()
            && let Ok((start, Some(end))) = self.playback_bounds(self.duration())
            && position >= end
        {
            let range_duration = end - start;
            let range_nanos = range_duration.as_nanos();

            if range_nanos > 0 {
                let position_nanos = position.saturating_sub(start).as_nanos() % range_nanos;
                position = start
                    + Duration::new(
                        (position_nanos / 1_000_000_000) as u64,
                        (position_nanos % 1_000_000_000) as u32,
                    );
            }
        }

        match self.duration() {
            Some(duration) => position.min(duration),
            None => position,
        }
    }

    /// Returns the position of the sound that's being played in the format `H:MM:SS` or `M:SS`.
    pub fn position_formatted(&self) -> String {
        format_timestamp(self.position())
    }

    /// Returns the total duration of the audio source.
    pub fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// Returns the total duration of the audio source in the format `H:MM:SS` or `M:SS`.
    ///
    /// Falls back to `0:00` if the duration is unknown.
    pub fn duration_formatted(&self) -> String {
        self.duration()
            .map(format_timestamp)
            .unwrap_or_else(|| "0:00".into())
    }

    /// [`Self::position`] clamped to the range `[0, duration]`.
    ///
    /// This is useful for user input.
    pub fn clamped_position(&self) -> Duration {
        self.position()
            .min(self.duration().unwrap_or_default())
            .max(Duration::ZERO)
    }

    /// Attempts to produce a valid range suitable for a seek slider, even if the duration is unknown.
    ///
    /// Will return `0.0..=1.0` if the duration is unknown, otherwise returns `0.0..=duration`.
    pub fn seek_range(&self) -> std::ops::RangeInclusive<f64> {
        0.0..=self.duration().unwrap_or_default().as_secs_f64().max(1.0)
    }

    /// Pauses playback of this player.
    ///
    /// No effect if already paused.
    ///
    /// A paused sink can be resumed with `play()`.
    pub fn pause(&mut self) {
        self.voice.pause();

        if self.state() == PlaybackState::Playing {
            self.set_playback_state(PlaybackState::Paused);
        }
    }

    /// Resumes playback of a paused player.
    ///
    /// No effect if not paused.
    pub fn resume(&mut self) {
        self.voice.play();

        if self.state() == PlaybackState::Paused {
            self.set_playback_state(PlaybackState::Playing);
        }
    }

    /// Stops the sink by emptying the queue and resets its position.
    pub fn stop(&mut self) {
        self.voice.stop();

        self.position_offset = Duration::ZERO;
        self.player_position_anchor = Duration::ZERO;
        self.position_is_held = true;
        self.set_playback_state(PlaybackState::Idle);
    }

    /// Seeks to `position` and starts playback.
    ///
    /// If a source is actively playing or paused, this seeks in place and resumes
    /// playing. Otherwise, if a source was loaded, stopped with
    /// stopped, or ended, this rebuilds playback from the
    /// retained source at `position`.
    ///
    /// The requested position is clamped to the known duration and the configured
    /// playback bounds (see [`Self::set_loop_range`]).
    ///
    /// Returns [`SoundscapeError::NoAudioSource`] when no source is available.
    pub fn try_play_at(&mut self, position: Duration) -> Result<(), SoundscapeError> {
        if !self.has_current_source() {
            return Err(self.record_failure(SoundscapeError::NoAudioSource));
        }

        let duration = self.duration();
        let (range_start, range_end) = self
            .playback_bounds(duration)
            .map_err(|error| self.record_failure(error))?;
        let position = range_end.map_or(position.max(range_start), |end| {
            position.clamp(range_start, end)
        });

        let playback_state = self.state();
        if playback_state == PlaybackState::Paused && position == self.position() {
            self.resume();
            return Ok(());
        }

        if playback_state == PlaybackState::Playing || playback_state == PlaybackState::Paused {
            self.try_seek(position)?;
            self.resume();
            return Ok(());
        }

        self.play_current_source_at(position)
    }

    /// Changes the volume of the sound.
    ///
    /// The value `1.0` is the "normal" volume (unfiltered input). Any value other than `1.0` will
    /// multiply each sample by this value.
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume;

        self.voice.set_volume(volume);
    }

    /// Gets the volume of the sound.
    ///
    /// The value `1.0` is the "normal" volume (unfiltered input). Any value other than 1.0 will
    /// multiply each sample by this value.
    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Changes playback speed and reports errors from rebuilding a pitch-preserving source.
    pub fn try_set_speed(&mut self, speed: f32) -> Result<(), SoundscapeError> {
        if self.preserve_pitch && self.speed != speed && !self.is_empty() {
            let position = self.position();
            let previous_speed = std::mem::replace(&mut self.speed, speed);
            let result = self.try_seek(position);

            if result.is_err() {
                self.speed = previous_speed;
            }

            return result;
        }

        if !self.position_is_held {
            let player_position = self.voice.get_pos();
            self.position_offset = self.position_at_player_time(player_position);
            self.player_position_anchor = player_position;
            if let Some(clock) = &self.spatial_clock {
                self.spatial_clock_anchor = clock.load(Ordering::Relaxed);
            }
        }

        self.speed = speed;

        self.voice.set_speed(self.player_speed());

        Ok(())
    }

    /// Gets the playback speed of the sound.
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// Enables or disables pitch preservation for playback speed changes.
    ///
    /// Active playback is rebuilt at its current source position so the new mode takes effect
    /// immediately while preserving whether playback is paused.
    pub fn set_preserve_pitch(&mut self, preserve_pitch: bool) -> Result<(), SoundscapeError> {
        if preserve_pitch == self.preserve_pitch {
            return Ok(());
        }

        if self.is_empty() {
            self.preserve_pitch = preserve_pitch;

            self.voice.set_speed(self.player_speed());

            return Ok(());
        }

        let position = self.position();
        self.preserve_pitch = preserve_pitch;
        let result = self.try_seek(position);

        if result.is_err() {
            self.preserve_pitch = !preserve_pitch;
        }

        result
    }

    /// Returns whether playback speed changes preserve the source pitch.
    pub fn preserves_pitch(&self) -> bool {
        self.preserve_pitch
    }

    /// Enables or disables repeating at the configured playback boundary.
    ///
    /// Without a loop range, the complete source repeats. Changes apply to active playback at
    /// its next boundary as well as to future sources.
    pub fn set_looping(&self, looping: bool) {
        self.looping.store(looping, Ordering::Relaxed);
    }

    /// Returns whether playback repeats at its configured boundary.
    pub fn is_looping(&self) -> bool {
        self.looping.load(Ordering::Relaxed)
    }

    /// Sets the source-time range used as the playback and looping boundary.
    ///
    /// The range applies to active and future playback. Its end is clamped to the source duration.
    pub fn set_loop_range(&mut self, range: Range<Duration>) -> Result<(), SoundscapeError> {
        if range.start >= range.end {
            return Err(SoundscapeError::InvalidPlaybackRange);
        }

        let state = self.state();
        let position = self.position().clamp(range.start, range.end);
        let previous_range = self.loop_range.replace(range);

        if matches!(state, PlaybackState::Playing | PlaybackState::Paused)
            && let Err(error) = self.try_seek(position)
        {
            self.loop_range = previous_range;
            return Err(error);
        }

        Ok(())
    }

    /// Returns the configured playback and looping range.
    pub fn loop_range(&self) -> Option<Range<Duration>> {
        self.loop_range.clone()
    }

    /// Removes the configured playback and looping range.
    pub fn clear_loop_range(&mut self) {
        self.loop_range = None;
    }

    /// Gets if a sink is playing
    ///
    /// Equivalent to `!is_paused() && !is_empty()`.
    ///
    /// Players can be paused and resumed using `pause()` and `play()`. This returns `true` if the
    /// sink is playing.
    pub fn is_playing(&mut self) -> bool {
        self.state() == PlaybackState::Playing
    }

    /// Gets if a sink is paused
    ///
    /// Players can be paused and resumed using `pause()` and `play()`. This returns `true` if the
    /// sink is paused.
    pub fn is_paused(&mut self) -> bool {
        self.state() == PlaybackState::Paused
    }

    /// Returns true if this sink has no more sounds to play.
    pub fn is_empty(&mut self) -> bool {
        !matches!(self.state(), PlaybackState::Playing | PlaybackState::Paused)
    }

    /// Sleeps the current thread until the sound ends.
    pub fn wait_until_end(&self) {
        self.voice.sleep_until_end();
    }
}
