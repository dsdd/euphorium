use euphorium::{
    EmitterState, HrtfProfile, ListenerState, SoundSource, Soundscape, SoundscapeError,
    SpatialAudioSettings, SpatialRenderer, SpatialSound,
};

static HIT: &[u8] = include_bytes!("sfx/soft-hitclap.ogg");

fn main() -> Result<(), SoundscapeError> {
    let soundscape = Soundscape::new();
    soundscape.set_spatial_audio(SpatialAudioSettings {
        renderer: SpatialRenderer::Hrtf(HrtfProfile::builtin()),
        ..SpatialAudioSettings::default()
    })?;
    soundscape.set_listener(ListenerState {
        position: [0.0, 1.7, 0.0],
        ..ListenerState::default()
    })?;

    let positions = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [3.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0],
        [-2.0, 0.0, 0.0],
        [-3.0, 0.0, 0.0],
        [0.0, 0.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 2.0],
        [0.0, 0.0, 3.0],
        [0.0, 0.0, 0.0],
        [0.0, 0.0, -1.0],
        [0.0, 0.0, -2.0],
        [0.0, 0.0, -3.0],
    ];
    let drum = soundscape.create_sound("drum", SoundSource::static_bytes(HIT))?;
    drum.set_volume(2.0)?;

    for pos in positions {
        drum.set_spatial(Some(SpatialSound {
            emitter: EmitterState {
                position: pos,
                velocity: [0.0, 0.0, 0.0],
            },
            ..SpatialSound::default()
        }))?;

        drum.play()?;
        drum.wait_until_end()?;
    }

    Ok(())
}
