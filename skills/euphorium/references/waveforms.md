# Waveform Integration

Use this reference when an application needs amplitude peaks for an overview, scrubber, zoomed
timeline, or editor.

## Choose Synchronous or Incremental Decoding

Use `Waveform::from_*` when the source is short or decoding occurs away from a latency-sensitive
thread. These constructors decode the complete finite source before returning.

Use `Waveform::builder_from_*` for long tracks and interactive UIs. Keep the `WaveformBuilder` in
application state and call `advance_frames(max_frames)` with a bounded frame budget between UI
updates.

Available constructor families are:

- `from_source` and `builder_from_source` for a public rodio `Source<Item = f32>`.
- `from_static_bytes` and `builder_from_static_bytes` for `include_bytes!` data.
- `from_shared_bytes` and `builder_from_shared_bytes` for runtime bytes.
- Native-only `from_file` and `builder_from_file`.
- WASM-only async `from_url` and `builder_from_url`.

The runtime-byte constructors copy `bytes.as_ref()` in Euphorium. Browser URL waveform methods
fetch and decode the full response before returning a waveform builder.

## Incremental Update Pattern

```rust
use euphorium::{SoundscapeError, WaveformBuilder};
use std::ops::Range;
use std::time::Duration;

fn update_waveform(
    builder: &mut WaveformBuilder,
    visible_range: Range<Duration>,
) -> Result<bool, SoundscapeError> {
    builder.prioritize(visible_range)?;
    Ok(builder.advance_frames(builder.sample_rate() as usize))
}
```

This example decodes at most about one second of audio frames per call. Choose a smaller budget for
tighter UI latency or a larger budget for faster background completion. A frame includes one sample
per channel.

`prioritize(range)` selects missing peaks in the visible range before returning to background
decoding. Out-of-order prioritization requires a source with known duration and working
`Source::try_seek`. Unknown-duration sources continue sequentially. Surface `SoundscapeError::Seek`
instead of repeatedly retrying an unseekable source.

`advance_frames()` returns `true` only when the complete source has been decoded. Use
`is_finished()` to inspect the same condition. `finish()` synchronously decodes all remaining work;
do not call it on an interactive thread merely to obtain drawable peaks.

## Slice for Drawing

Both `Waveform` and `WaveformBuilder` expose:

```rust
let slice = waveform.slice(visible_start..visible_end, horizontal_pixels);

for (index, peak) in slice.peaks().iter().enumerate() {
    if !peak.is_available() {
        continue;
    }

    let Some(track_range) = slice.peak_range(index) else {
        continue;
    };

    draw_peak(track_range, peak.min, peak.max);
}
```

Replace `draw_peak` with the consumer's renderer. `slice(range, max_peaks)` clamps the requested
range to track duration and borrows a multiresolution level sized to the drawing budget. It does not
allocate another peak buffer.

Each `WaveformPeak` combines every channel in its time section; the API does not provide separate
left/right peaks. `min` and `max` are sample amplitudes. `amplitude()` returns the largest absolute
value. Incremental slices can contain unavailable placeholders when decoding occurred out of order,
so always check `is_available()`.

`WaveformSlice::peak_range(index)` returns the absolute source-time range represented by a peak.
Use it for x-positioning; do not assume that peak index zero is track time zero for a zoomed slice.

## Resolution and Memory

The finest default resolution is `Waveform::DEFAULT_FRAMES_PER_PEAK`, currently 256 frames. The
`*_with_base_frames` constructors accept a `NonZeroUsize`:

- Lower values preserve more transient detail and retain more peaks.
- Higher values reduce memory and fine detail.
- Drawing should still request no more peaks than useful horizontal units.

## Verify

- Confirm waveform decoding uses the same asset/format feature as playback.
- Verify the UI remains responsive while a long track decodes.
- Exercise zoomed and scrolled ranges, including track start and end.
- Handle `max_peaks == 0`, empty ranges, unknown duration, unavailable peaks, and seek errors.
- Confirm waveform timestamps align with `Sound::position()` after speed changes; waveform ranges
  remain in source time.
