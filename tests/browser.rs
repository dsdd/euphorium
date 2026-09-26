#![cfg(target_arch = "wasm32")]

use std::time::Duration;

use euphorium::{PlaybackState, SoundSource, Soundscape, Waveform, cpal};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

const WAV_DATA_URL: &str = "data:audio/wav;base64,UklGRiwAAABXQVZFZm10IBAAAAABAAEAQB8AAEAfAAABAAgAZGF0YQgAAACAgICAgICAgA==";

async fn wait_for_browser_task() {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let callback = Closure::once_into_js(move || {
            resolve
                .call0(&JsValue::UNDEFINED)
                .expect("timer promise should resolve");
        });

        web_sys::window()
            .expect("browser window is unavailable")
            .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 0)
            .expect("browser timer should be scheduled");
    });

    JsFuture::from(promise)
        .await
        .expect("browser task timer should resolve");
}

#[wasm_bindgen_test]
fn selecting_a_backend_before_a_user_gesture_keeps_output_deferred() {
    let soundscape = Soundscape::new();

    soundscape
        .switch_backend(cpal::HostId::WebAudio)
        .expect("selecting a browser backend should succeed");

    assert!(!soundscape.has_output());
}

#[wasm_bindgen_test]
async fn duration_from_platform_source_fetches_and_decodes_browser_url() {
    let soundscape = Soundscape::new();
    let sound = soundscape
        .create_sound(
            "browser",
            euphorium::audio_source! {
                native: bytes(include_bytes!("../examples/music/THE UNFORGIVING.mp3")),
                wasm: url(WAV_DATA_URL),
            },
        )
        .expect("the browser sound should be created");
    sound
        .load()
        .await
        .expect("browser audio asset should be fetched and decoded");
    let duration = sound
        .duration()
        .expect("the browser sound should remain valid")
        .expect("WAV duration should be available");

    assert!(duration > Duration::ZERO);

    sound
        .play()
        .expect("cached browser audio asset should be playable");

    assert!(!sound.is_loading().unwrap());
}

#[wasm_bindgen_test]
async fn waveform_from_url_fetches_and_decodes_browser_audio() {
    let waveform = Waveform::from_url(WAV_DATA_URL)
        .await
        .expect("browser audio URL should be decoded into a waveform");

    assert!(waveform.duration() > Duration::ZERO);
}

#[wasm_bindgen_test]
fn platform_source_can_embed_bytes_in_wasm_explicitly() {
    let source = euphorium::audio_source! {
        native: file("unused-native-path"),
        wasm: bytes(include_bytes!("browser.rs")),
    };

    assert_eq!(
        source.len(),
        Some(include_bytes!("browser.rs").len() as u64)
    );
}

#[wasm_bindgen_test]
async fn soundscape_owns_pending_url_playback_and_duration() {
    let soundscape = Soundscape::new();
    let sound = soundscape
        .create_sound("pending", SoundSource::url(WAV_DATA_URL))
        .expect("the pending browser sound should be created");

    sound
        .play()
        .expect("browser audio playback should start loading");
    assert!(sound.is_loading().unwrap());

    loop {
        let failures = soundscape.update();
        assert!(
            failures.is_empty(),
            "browser audio asset should be playable"
        );
        if !sound.is_loading().unwrap() {
            break;
        }

        wait_for_browser_task().await;
    }

    assert!(!sound.is_loading().unwrap());
    assert!(
        sound
            .duration()
            .unwrap()
            .is_some_and(|duration| duration > Duration::ZERO)
    );
}

#[wasm_bindgen_test]
async fn replacing_playing_sound_with_uncached_url_starts_loading() {
    let first = SoundSource::url(WAV_DATA_URL);
    let second = SoundSource::url(format!("{WAV_DATA_URL}#replacement"));
    let soundscape = Soundscape::new();
    let sound = soundscape
        .create_sound("replacement", first)
        .expect("the initial browser sound should be created");

    sound
        .load()
        .await
        .expect("the initial browser asset should be loaded");
    sound
        .set_looping(true)
        .expect("the initial browser sound should support looping");
    sound
        .play()
        .expect("the initial browser sound should start playing");
    assert_eq!(sound.playback_state().unwrap(), PlaybackState::Playing);

    sound
        .set_source(second)
        .expect("the replacement browser asset should be accepted");
    assert!(sound.is_loading().unwrap());

    loop {
        let failures = soundscape.update();
        assert!(failures.is_empty(), "browser asset replacement should load");
        if !sound.is_loading().unwrap() {
            break;
        }

        wait_for_browser_task().await;
    }

    assert!(
        sound
            .duration()
            .unwrap()
            .is_some_and(|duration| duration > Duration::ZERO)
    );
    assert_eq!(sound.playback_state().unwrap(), PlaybackState::Playing);
}

#[wasm_bindgen_test]
async fn group_loads_browser_urls_with_shared_configuration() {
    let first = SoundSource::url(WAV_DATA_URL);
    let second = SoundSource::url(WAV_DATA_URL);
    let soundscape = Soundscape::new();
    let group = soundscape.create_group("browser").unwrap();
    group.set_volume(0.4).unwrap();
    let first = group.create_sound("first", first).unwrap();
    let second = group.create_sound("second", second).unwrap();

    first.load().await.expect("the first asset should load");
    second.load().await.expect("the second asset should load");

    assert_eq!(group.sounds().unwrap().len(), 2);
    assert!(
        first
            .duration()
            .unwrap()
            .is_some_and(|duration| !duration.is_zero())
    );
    assert_eq!(first.playback_state().unwrap(), PlaybackState::Idle);
    assert_eq!(second.playback_state().unwrap(), PlaybackState::Idle);
}
