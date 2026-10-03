use thiserror::Error;

use crate::{Backend, Device};

/// The error type for Euphorium operations.
#[derive(Debug, Error)]
pub enum SoundscapeError {
    /// The error type for I/O operations of the Read, Write, Seek, and associated traits.
    #[error("failed to open audio file: {0}")]
    File(#[source] std::io::Error),

    /// Errors that can occur when creating a decoder.
    #[error("failed to decode audio file: {0}")]
    Decode(#[source] rodio::decoder::DecoderError),

    /// Occurs when `try_seek` fails because the underlying decoder has an error or does not support seeking.
    #[error("failed to seek audio: {0}")]
    Seek(#[source] rodio::source::SeekError),

    /// An audio effect setting is outside its supported range.
    #[error("invalid audio effect setting: {0}")]
    InvalidEffect(&'static str),

    /// The audio output could not be initialized.
    #[error(transparent)]
    Output(#[from] OutputError),

    /// The playback range is empty or starts beyond the end of the source.
    #[error("the playback range must contain audio")]
    InvalidPlaybackRange,

    /// The seek position is NaN, infinite, or negative.
    #[error("the seek position must be a finite non-negative number")]
    InvalidSeekPosition,

    /// A sound or group name is empty, contains `/`, or has surrounding whitespace.
    #[error("sound and group names must be non-empty path components")]
    InvalidName,

    /// A sound or group with this name already exists under the same parent.
    #[error("a sound or group named {0:?} already exists under this parent")]
    DuplicateName(String),

    /// A typed scene was built without registering one of its declared sounds.
    #[error("required sound {0:?} was not registered")]
    MissingRequiredSound(&'static str),

    /// Two typed sound keys declare the same path.
    #[error("multiple required sound keys use path {0:?}")]
    DuplicateRequiredSoundPath(&'static str),

    /// A sound was registered with a key absent from [`crate::SoundKey::ALL`].
    #[error("sound key for path {0:?} is absent from SoundKey::ALL")]
    UnknownRequiredSound(&'static str),

    /// Required sounds are permanent members of their typed scene.
    #[error("a required sound cannot be removed")]
    RequiredSound,

    /// A sound handle no longer refers to a live sound.
    #[error("the sound handle is no longer valid")]
    InvalidSoundHandle,

    /// A pending load completed after the sound was assigned a different source.
    #[error("the sound source changed while it was loading")]
    SoundSourceChanged,

    /// A sound-group handle no longer refers to a live group.
    #[error("the sound-group handle is no longer valid")]
    InvalidSoundGroupHandle,

    /// Two handles from different audio roots were used together.
    #[error("sounds and groups must belong to the same scene")]
    DifferentSoundscape,

    /// Reparenting a group would make it one of its own ancestors.
    #[error("a sound group cannot be parented beneath itself")]
    SoundGroupCycle,

    /// The implicit root sound group cannot be moved or removed.
    #[error("the root sound group cannot be moved or removed")]
    RootSoundGroup,

    /// A volume was negative, NaN, or infinite.
    #[error("volume must be a finite non-negative number")]
    InvalidVolume,

    /// A playback speed was non-positive, NaN, or infinite.
    #[error("playback speed must be a finite positive number")]
    InvalidSpeed,

    /// A spatial audio parameter is outside its supported range.
    #[error("invalid spatial audio setting: {0}")]
    InvalidSpatialSetting(&'static str),

    /// A spatial sound was configured as mono-only but its source has multiple channels.
    #[error(
        "spatial playback requires mono input; use SpatialInput::DownmixToMono for multichannel sources"
    )]
    SpatialInputNotMono,

    /// A spatial control was used on a sound without spatial settings.
    #[error("the sound is not spatialized")]
    SoundNotSpatialized,

    /// A playback operation required a previously loaded or played source.
    #[error("no audio source is available")]
    NoAudioSource,

    /// Errors that might occur when loading an audio asset in a browser.
    #[cfg(target_arch = "wasm32")]
    #[error("failed to load browser audio asset: {0}")]
    BrowserAsset(String),
}

/// An error that can occur while opening an audio backend or device.
#[derive(Debug, Error)]
pub enum OutputError {
    /// Browser policy requires the output to be opened from a user gesture.
    #[cfg(target_arch = "wasm32")]
    #[error("browser audio output must be opened from a user gesture")]
    BrowserUserGestureRequired,

    /// The requested backend is not available on this system.
    #[error("audio backend {backend} is unavailable: {source}")]
    BackendUnavailable {
        /// The backend that was requested.
        backend: Backend,
        /// The error returned by CPAL while initializing the backend.
        #[source]
        source: rodio::cpal::Error,
    },

    /// The requested backend has no output device.
    #[error("audio backend {0} has no output device")]
    NoOutputDevice(Backend),

    /// The output devices of the requested backend could not be listed.
    #[error("failed to list output devices of audio backend {backend}: {source}")]
    ListDevices {
        /// The backend whose devices could not be listed.
        backend: Backend,
        /// The error returned by CPAL while listing devices.
        #[source]
        source: rodio::cpal::Error,
    },

    /// The requested output device could not be found.
    ///
    /// The device may have been unplugged or disabled after enumeration.
    #[error("audio output device \"{device}\" was not found")]
    DeviceNotFound {
        /// The device that could not be found.
        device: Box<Device>,
    },

    /// The requested backend's output stream could not be opened.
    #[error("failed to open audio backend {backend}: {source}")]
    OpenStream {
        /// The backend that was requested.
        backend: Backend,
        /// The error returned by rodio while opening the output stream.
        #[source]
        source: rodio::stream::DeviceSinkError,
    },

    /// The requested output device's stream could not be opened.
    #[error("failed to open audio output device \"{device}\": {source}")]
    OpenDeviceStream {
        /// The device that was requested.
        device: Box<Device>,
        /// The error returned by rodio while opening the output stream.
        #[source]
        source: rodio::stream::DeviceSinkError,
    },
}
