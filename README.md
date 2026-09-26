# Euphorium

[![Crates.io Version](https://img.shields.io/crates/v/euphorium.svg)](https://crates.io/crates/euphorium)

Painless audio playback for native and web platforms.

A small Rust audio player built on top of [rodio](https://github.com/RustAudio/rodio) and [CPAL](https://github.com/RustAudio/cpal).

## Installation

```sh
cargo add euphorium
```

## Usage

```rust,no_run
use euphorium::{Soundscape, SoundscapeError, SoundSource};

fn main() -> Result<(), SoundscapeError> {
    // One Soundscape owns the output and every sound in the application.
    let soundscape = Soundscape::new();
    let music = soundscape.create_group("music")?;
    let battle = music.create_group("battle")?;
    let theme = battle.create_sound("theme", SoundSource::file("audio.mp3"))?;

    // Group volume multiplies each descendant's local volume.
    music.set_volume(0.8)?;
    theme.set_volume(0.5)?;
    theme.set_speed(1.25)?;

    theme.play()?;
    theme.try_seek_secs(30.0)?;
    theme.wait_until_end()?;
    Ok(())
}

fn play_embedded_audio(audio: &'static [u8]) -> Result<(), SoundscapeError> {
    let soundscape = Soundscape::new();
    let sound = soundscape.create_sound("embedded", SoundSource::static_bytes(audio))?;
    sound.play()?;
    Ok(())
}

fn where_is_my_audio_at(soundscape: &Soundscape) -> Result<(), SoundscapeError> {
    let sound = soundscape.find_sound("music/battle/theme").expect("theme exists");
    sound.pause()?;
    sound.resume()?;
    println!("{} / {}", sound.position_formatted()?, sound.duration_formatted()?);
    Ok(())
}

fn play_my_audio_cross_platform() -> Result<(), SoundscapeError> {
    let soundscape = Soundscape::new();
    let sound = soundscape.create_sound(
        "music",
        euphorium::audio_source! {
            native: file("assets/music.mp3"),
            wasm: url("audio.mp3"),
        },
    )?;
    sound.play()?;

    // Call this from the application's update loop. It completes browser loads
    // and reports asynchronous failures for every sound in one place.
    for failure in soundscape.update() {
        eprintln!("sound {:?} failed: {}", failure.sound, failure.error);
    }
    Ok(())
}

// Define your application's sound keys in one place...
euphorium::sound_key! {
    enum AppSound {
        Music => "music",
        BattleTheme => "music/battle/theme",
    }
}

fn an_epic_scene(music: SoundSource, theme: SoundSource) -> Result<(), SoundscapeError> {
    let soundscape = Soundscape::<AppSound>::builder()
        .sound(AppSound::Music, music)
        .sound(AppSound::BattleTheme, theme)
        .build()?;

    // And now you get `Sound` instead of `Option<Sound>` for every key in `AppSound`.
    soundscape.sound(AppSound::BattleTheme).play()
}
```

> Browsers require audio output to be opened in response to a user gesture.
> Start playback from a click, pointer, or keyboard event when possible.

Please find these [examples](https://github.com/egakareta/euphorium/tree/master/examples) for more guidance.

## Feature flags

| Feature         | Enables              |
| --------------- | -------------------- |
| `cli` (default) | `euphorium <PATH>`   |
| `mp3` (default) | MP3 decoding         |
| `flac`          | FLAC decoding        |
| `mp4`           | MP4 decoding         |
| `vorbis`        | Vorbis decoding      |
| `wav` (default) | WAV decoding         |
| `asio`          | ASIO backend         |
| `jack`          | JACK backend         |
| `nightly`       | AudioWorklet backend |

## Motivation

[rodio](https://github.com/rustaudio/rodio) and [CPAL](https://github.com/RustAudio/cpal) are both great libraries, but they are not very ergonomic to use. Here are some issues that I have encountered while using them:

1. There is no Null backend, so you must forever live with `Option<MixerDeviceSink>`.
2. `rodio::Player::try_seek` deadlocks on WASM and some specific threading setups.
3. rodio seems to prioritize correctness, which is fine, but you end up needing to wrap a lot of code yourself to get predictable behavior e.g. setting time position or playback speed when no audio is playing.
4. rodio does not officially support AudioWorklet while CPAL does.
5. No convenience code in rodio for native and web audio resource handling, causing a ton of `#[cfg(target_arch = "wasm32")]`.
6. Concurrent playback requires callers to own and synchronize a collection of players.
7. No Rayon support.
8. No waveform visualization.

## License

Licensed under either of:

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
