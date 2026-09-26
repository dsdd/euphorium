# Browser and WASM Integration

## Dependency

The ordinary WebAudio path works on stable Rust. Enable `nightly` only for CPAL's AudioWorklet
path, which also requires `rust-src`, a shared-memory WASM build, and cross-origin isolation.

## Assets And First Playback

```rust
use euphorium::{Soundscape, SoundscapeError};

fn create_audio() -> Result<Soundscape, SoundscapeError> {
    let soundscape = Soundscape::new();
    soundscape.create_sound(
        "music",
        euphorium::audio_source! {
            native: file("assets/music.mp3"),
            wasm: url("/assets/music.mp3"),
        },
    )?;
    Ok(soundscape)
}

fn on_user_gesture(soundscape: &Soundscape) -> Result<(), SoundscapeError> {
    soundscape.find_sound("music").expect("music exists").play()
}
```

Call `Sound::play()` synchronously from the click, pointer, or keyboard callback. It records the
intent and retries the deferred browser output immediately while transient user activation is
available; `soundscape.update()` then completes the browser fetch. If playback was requested before
a gesture, the output stays deferred and a later `play()`, `ensure_output()`, or `update()` during a
gesture retries it. Do not await `Sound::load()` before first playback unless the output was already
opened from a user gesture.

On WASM, the complete response is retained in memory for decoding. Ensure the URL is served, CORS
allows the application origin, the decoder feature matches the file, and large assets fit the
browser memory budget. Clones of the URL-backed `SoundSource` share their browser-byte cache. To
embed a web asset in the `.wasm` instead, use `wasm: bytes(include_bytes!(...))` in
`audio_source!`.

## Update Loop

```rust
fn update_audio(soundscape: &Soundscape) {
    for failure in soundscape.update() {
        log::error!("sound {:?} failed: {}", failure.sound, failure.error);
    }
}
```

`Soundscape::update()` completes all pending loads and records playback events centrally. Request another
UI update while any relevant sound reports `is_loading() == Ok(true)`.

## Threading

`Sound` and `SoundGroup` handles are `Send + Sync`, so worker threads (for example a rayon pool)
may play, pause, seek, and retune sounds while the main thread pumps `update()`. Keep the
`Soundscape` root, `update()`, `ensure_output()`, and backend/device switches on the creating
thread: the root owns the thread-affine `AudioContext`. Euphorium's AudioWorklet setup is compatible
with wasm-bindgen-rayon's generated worker helper.

## AudioWorklet Setup

For the `nightly` feature, configure a nightly toolchain with `rust-src`, WASM atomics/shared memory,
and production-equivalent memory limits. Serve at least these headers:

```text
Cross-Origin-Embedder-Policy: require-corp
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Resource-Policy: same-site
```

All subresources must satisfy cross-origin isolation. The exact linker flags and memory size depend
on the consumer's deployment; use the repository example application's `.cargo/config.toml` as the
tested baseline.

Verify first playback from a real gesture, loading and error states, replay from cache, seek,
completion, failed HTTP responses, and CORS failures.
