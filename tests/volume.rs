use euphorium::{Output, SoundSource, Soundscape, SoundscapeError};

fn close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() <= f32::EPSILON * expected.abs().max(1.0),
        "expected {expected}, got {actual}"
    );
}

#[test]
fn effective_volume_tracks_sound_group_and_master_multipliers() {
    let soundscape = Soundscape::new_with_output(Output::new_deferred(None));
    let parent = soundscape.create_group("parent").unwrap();
    let child = parent.create_group("child").unwrap();
    let sound = child.create_sound("tone", SoundSource::Empty).unwrap();

    sound.set_volume(0.8).unwrap();
    parent.set_volume(0.5).unwrap();
    child.set_volume(0.25).unwrap();
    soundscape.set_volume(0.5).unwrap();

    close(sound.local_volume().unwrap(), 0.8);
    close(sound.effective_volume().unwrap(), 0.05);
    close(child.effective_volume().unwrap(), 0.0625);

    child.set_volume(0.1).unwrap();
    close(sound.effective_volume().unwrap(), 0.02);
}

#[test]
fn muting_group_silences_effective_volume_without_changing_configured_levels() {
    let soundscape = Soundscape::new_with_output(Output::new_deferred(None));
    let group = soundscape.create_group("music").unwrap();
    let sound = group.create_sound("theme", SoundSource::Empty).unwrap();
    group.set_volume(0.4).unwrap();
    sound.set_volume(0.75).unwrap();

    close(sound.effective_volume().unwrap(), 0.3);
    group.set_muted(true).unwrap();
    close(sound.effective_volume().unwrap(), 0.0);
    close(sound.local_volume().unwrap(), 0.75);
    close(group.local_volume().unwrap(), 0.4);

    group.set_muted(false).unwrap();
    close(sound.effective_volume().unwrap(), 0.3);
}

#[test]
fn volume_rejects_negative_and_non_finite_values_without_changing_output_level() {
    let soundscape = Soundscape::new_with_output(Output::new_deferred(None));
    let group = soundscape.create_group("effects").unwrap();
    let sound = group.create_sound("click", SoundSource::Empty).unwrap();

    sound.set_volume(0.6).unwrap();
    assert!(matches!(
        sound.set_volume(-0.1),
        Err(SoundscapeError::InvalidVolume)
    ));
    assert!(matches!(
        group.set_volume(f32::NAN),
        Err(SoundscapeError::InvalidVolume)
    ));
    assert!(matches!(
        soundscape.set_volume(f32::INFINITY),
        Err(SoundscapeError::InvalidVolume)
    ));

    close(sound.local_volume().unwrap(), 0.6);
    close(sound.effective_volume().unwrap(), 0.6);
    close(group.local_volume().unwrap(), 1.0);
    close(soundscape.volume(), 1.0);
}
