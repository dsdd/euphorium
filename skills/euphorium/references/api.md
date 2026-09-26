# Euphorium API Guide

## Scene Model

Euphorium uses one root owner and two lightweight handles:

```rust
use euphorium::{Soundscape, SoundscapeError, SoundGroup, SoundSource};

fn create_audio() -> Result<Soundscape, SoundscapeError> {
    let soundscape = Soundscape::new();
    let music = soundscape.create_group("music")?;
    let combat = music.create_group("combat")?;
    let theme = combat.create_sound("theme", SoundSource::file("assets/theme.mp3"))?;

    music.set_volume(0.8)?;
    theme.set_looping(true)?;
    Ok(soundscape)
}
```

- `Soundscape` owns the output, sounds, groups, and global event stream.
- `Sound` owns one source and playback timeline.
- `SoundGroup` is a recursive mixer bus. It cannot be constructed independently.
- Sound and group handles are cloneable, but become invalid after removal or after their `Soundscape` is
  dropped.
- Names are unique among sibling sounds and groups. Look up descendants with paths such as
  `soundscape.find_sound("music/combat/theme")`.

For application-defined sounds, declare a typed schema and register every source through the
builder:

```rust
euphorium::sound_key! {
    enum AppSound {
        Theme => "music/theme",
        Click => "effects/click",
    }
}

let soundscape = Soundscape::<AppSound>::builder()
    .sound(AppSound::Theme, theme_source)
    .sound(AppSound::Click, click_source)
    .build()?;
let theme = soundscape.sound(AppSound::Theme);
```

`build()` rejects missing keys and duplicate paths. Required sounds cannot be removed, so
`soundscape.sound(key)` returns `Sound` directly. Use `find_sound(path)` for dynamic content.

Use `builder.placeholder(key)` to register one empty sound for later assignment. Use
`builder.placeholders()` to fill every unregistered key with an empty sound. Playing an
empty sound returns `SoundscapeError::NoAudioSource`; assign its resource with `sound.set_source(source)`.

## Sources

Create every sound through the same API:

```rust
let embedded = soundscape.create_sound("click", SoundSource::static_bytes(CLICK_BYTES))?;
let runtime = soundscape.create_sound("voice", SoundSource::shared_bytes(downloaded_bytes))?;
let portable = soundscape.create_sound(
    "music",
    euphorium::audio_source! {
        native: file("assets/music.mp3"),
        wasm: url("/assets/music.mp3"),
    },
)?;
```

Use `native: bytes(include_bytes!(...))` instead of `native: file(path)` to embed native audio.
Likewise, WASM supports `bytes(include_bytes!(...))` as well as `url(...)`; choose bytes explicitly
when you want the audio embedded in the `.wasm`.

Creation validates and loads static bytes, shared bytes, and native paths without playing. Use
`sound.set_source(source)` to replace a source transactionally. Use `sound.load().await` to preload a
browser URL; otherwise `sound.play()` loads it lazily.

## Transport

- `play()` ensures the sound is playing and resumes it when paused.
- `replay()` restarts from zero.
- `pause()` records a sound-local pause.
- `resume()` releases only the sound-local pause; an ancestor group can keep it paused.
- `stop()` retains the source and resets position to zero.
- `try_seek()` and `try_seek_secs()` seek in source time.
- `wait_until_end()` blocks and is intended for command-line or worker contexts.

`play()` records the intent and opens a deferred browser output when called during a user gesture.
The physical output can also open when the owning scene is updated (`Soundscape::update()` retries
it once playback is demanded) or explicitly via `Soundscape::ensure_output()`. Without an update
loop, call `ensure_output()` before `play()`/`wait_until_end()` on a deferred native scene. In a
browser, call `play()` or `ensure_output()` synchronously from the gesture callback.

`Sound` and `SoundGroup` handles are `Send + Sync` and may be driven from worker threads;
keep the `Soundscape` root, `update()`, and output selection on the creating thread.

Use `playback_state()` for `Idle`, `Loading`, `Playing`, `Paused`, `Ended`, or `Failed`. Calling
`playback_state()` or `poll_event()` discovers natural completion for that sound.

## Groups

Group volume is a mixer multiplier. Effective volume is the sound's local volume multiplied by all
ancestor group volumes and the root volume. Muting any ancestor silences the subtree without
changing configured volumes.

Sound effects run before the sound enters its group. Group effects process the combined signal from
that group's sounds and child groups. Nested effects therefore run from the sound outward toward the
root.

`group.pause()` installs a persistent pause gate. `group.resume()` releases only that gate, so a
sound paused directly or by another ancestor remains paused. `stop_all()` and `replay_all()` are
explicit recursive commands; groups have no ambiguous `play()` operation.

Use `sound.set_group(&group)` or `group.set_parent(&parent)` to move nodes. Reparenting preserves
handles and active playback, rejects cycles, and rebuilds the affected routing graph. Removing a
group recursively removes all descendants unless the subtree contains a required typed sound.

## Events

`sound.poll_event()` provides a per-sound stream. `soundscape.poll_event()` provides a global stream where
each `SoundscapeEvent` includes the originating `SoundId`.

Call `soundscape.update()` from an application's update loop. It completes pending browser asset loads,
discovers natural completion for every sound, and returns asynchronous `SoundError` values. Errors
from synchronous operations are returned directly by those operations.

## Output Selection

Use `Output::available_backends()` and `Output::available_devices()` for settings UIs. Refresh them
only after an explicit refresh or device-change signal.

`Soundscape::switch_backend()` and `Soundscape::switch_device()` move the complete scene to a new output while
retaining sources, positions, effects, speed, looping, and pause state. `Soundscape::ensure_output()` opens
a deferred output, especially during a browser user gesture.

## Effects And Pitch

Build effect chains from `SoundEffects::default()`. `Sound::set_effects()` applies the chain before
mixing; `SoundGroup::set_effects()` applies it after mixing that group.

Use `sound.set_speed(value)` for playback speed and `sound.set_preserve_pitch(true)` for WSOLA time
stretching. Keep pitch-preserving speed in `0.25..=8.0`. A WSOLA input must retain a fixed channel
count and sample rate.

Waveform APIs remain independent of scene ownership. See [Waveforms](./waveforms.md).
