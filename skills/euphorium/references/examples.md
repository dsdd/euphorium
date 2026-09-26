# Consumer Examples

## Application State

Store one root owner:

```rust
use euphorium::{Soundscape, SoundscapeError, SoundSource};

pub struct AppState {
    pub soundscape: Soundscape,
}

impl AppState {
    pub fn new() -> Result<Self, SoundscapeError> {
        let soundscape = Soundscape::new();
        let music = soundscape.create_group("music")?;
        let effects = soundscape.create_group("effects")?;

        music.create_sound("theme", SoundSource::file("assets/theme.mp3"))?;
        effects.create_sound(
            "notification",
            SoundSource::static_bytes(include_bytes!("../assets/notification.wav")),
        )?;

        Ok(Self { soundscape })
    }

    pub fn notify(&self) -> Result<(), SoundscapeError> {
        self.soundscape
            .find_sound("effects/notification")
            .expect("notification exists")
            .replay()
    }
}
```

## Recursive Mixing

```rust
let music = soundscape.create_group("music")?;
let combat = music.create_group("combat")?;
let boss = combat.create_sound("boss", SoundSource::file("assets/boss.mp3"))?;

soundscape.set_volume(0.9)?;
music.set_volume(0.7)?;
combat.set_volume(0.8)?;
boss.set_volume(0.5)?;
boss.play()?;
```

The effective volume is `0.9 * 0.7 * 0.8 * 0.5`. Changing any group volume updates active
descendants without overwriting their local volume.

## Cross-Platform Playback

```rust
let soundscape = euphorium::Soundscape::new();
let music = soundscape.create_sound(
    "music",
    euphorium::audio_source! {
        native: file("assets/music.mp3"),
        wasm: url("/assets/music.mp3"),
    },
)?;

// Call synchronously from a browser gesture.
music.play()?;

// Call from the application's update loop.
for failure in soundscape.update() {
    eprintln!("sound {:?} failed: {}", failure.sound, failure.error);
}
```

## Effects And Pitch Preservation

```rust
use euphorium::{SoundEffects, FilterEffect, LimiterEffect};
use std::time::Duration;

voice.set_effects(SoundEffects {
    high_pass: Some(FilterEffect {
        frequency: 80,
        q: 0.7,
    }),
    limiter: Some(LimiterEffect {
        threshold_db: -1.0,
        knee_width_db: 4.0,
        attack: Duration::from_millis(5),
        release: Duration::from_millis(100),
    }),
    ..SoundEffects::default()
})?;
voice.set_preserve_pitch(true)?;
voice.set_speed(1.25)?;
```

Apply effects to a `SoundGroup` instead when the combined subtree should be processed as one bus.

## Output Device Selection

```rust
fn select_device(soundscape: &euphorium::Soundscape, device: &euphorium::Device) {
    if let Err(error) = soundscape.switch_device(device) {
        eprintln!("could not switch output: {error}");
    }
}
```

The complete scene moves together, so consumers do not need to maintain or synchronize multiple
players.
