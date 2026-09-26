# Changelog

## [0.5.0] - 2026-09-26

### Added

- Added the `audio_source!` macro to select native file or byte sources and WebAssembly URL or
  byte sources at compile time.
- Added WebAssembly URL-based waveform APIs: `Waveform::from_url`, `Waveform::builder_from_url`,
  and `WaveformView::set_url`.

### Changed

- Made audio sources target-specific and moved shared browser-byte caching onto `SoundSource`.

### Breaking

- Removed `SoundAsset`, `SoundSource::asset`, and the asset-based waveform APIs. Use
  `audio_source!` for cross-platform sources and the URL-based waveform APIs on WebAssembly.

## [0.4.0] - 2026-09-12

### Added

- Added browser-aware output initialization that defers opening audio outputs until an active user
  gesture is available.
- Added `OutputError::BrowserUserGestureRequired` for browser output attempts outside a user
  gesture.
- Added backend information to the Rayon example UI.

### Changed

- `Sound::play()` and `Soundscape::update()` now retry deferred browser outputs when playback is
  requested.
- Backend and device selection now preserve deferred browser output state until it can be opened
  from a user gesture.
- Prepared WASM audio queues before exposing them to the audio callback to avoid browser-main-thread
  mutex contention.
- Updated browser and API documentation and added coverage for backend selection before a user
  gesture.

## [0.3.0] - 2026-09-11

### Added

- Added deferred and eager output initialization for typed `Soundscape` builders.
- Added asynchronous browser asset replacement that preserves playback state.
- Added thread-safe audio handles and native concurrent-control coverage.
- Added a Rayon and WebAssembly integration example.

### Changed

- Typed `Soundscape::<K>::builder()` now uses a deferred output by default. Call `update()` or
  `ensure_output()` when the output should be opened, or use `builder_eager()` for immediate
  initialization.
- Improved scene output ownership and synchronization for native and browser use.
- Expanded threading, browser playback, and output initialization documentation and examples.

### Fixed

- Fixed the embedded playback example's error import.
- Fixed browser asset loading synchronization for background playback tasks.

## [0.2.1] - 2026-09-11

### Added

- Added `Default` for typed `Soundscape<K>` values, populating every declared sound key with a
  placeholder source.

### Changed

- Simplified the example app's default initialization by deriving its defaults.
- Made `THE UNFORGIVING` the first selected track in the example app.

## [0.2.0] - 2026-09-10

### Added

- Replaced the original player API with `Soundscape`, `Sound`, `SoundGroup`, `Output`, and
  `SoundSource` abstractions.
- Added typed scenes backed by `SoundKey`, including required-key validation and placeholder
  sources.
- Renamed the crate from `rodisnyaa` to `euphorium` and `Nyaa` to `Soundscape`.
- Added native file playback, embedded static-byte playback, shared-byte playback, and browser
  asset loading.
- Added playback controls for seeking, position and duration formatting, variable speed, optional
  pitch preservation, looping, volume, mute state, and post-processing effects.
- Added waveform decoding, incremental waveform builders, waveform statistics, and `WaveformView`.
- Added audio backend and device enumeration, selection, switching, persistent backend preferences,
  and playback preservation across output changes.
- Added the command-line player and configurable MP3, FLAC, MP4, Vorbis, and WAV decoder features.
- Added playback examples, sound-effect examples, control benchmarks, browser coverage, and
  native/WebAssembly CI.

### Changed

- Removed `asio` from the default feature set and added `vorbis` to it.
- Simplified output, sink, seek, and sound-source APIs, including `SoundSource::default()` and
  `SoundSource::is_empty()`.
- Updated the example app with selectable music modes, timeline resume behavior, backend/device
  controls, and browser cache controls.

## [0.1.0] - 2026-09-04

### Added

- Introduced `rodisnyaa`, a Rust audio playback library built on rodio and CPAL for native and
  WebAssembly targets.
- Added playback and pause state tracking, seeking before and during playback, and convenience
  APIs for positions, durations, timestamps, and seek ranges.
- Added native file playback, zero-copy playback from embedded static bytes, shared audio bytes, and
  cross-platform assets with native paths and browser URLs.
- Added shared audio outputs, output backend switching, output device selection, and WebAudio and
  AudioWorklet diagnostics.
- Added looping, volume control, playback speed changes, pitch-preserving speed changes, and
  configurable audio effects.
- Added incremental waveform decoding, waveform visualization, and optional waveform statistics.
- Added a command-line player, decoder feature flags, native and WebAssembly benchmarks, and
  browser/native memory and playback tests.

### Fixed

- Avoided the `rodio::Player::try_seek` deadlock on WebAssembly and supported initialization when no
  audio output is available.
- Fixed browser asset caching, native file seeking, looping source duration reporting, duplicate
  output-device enumeration, and preservation of playback position and volume during replacement.
