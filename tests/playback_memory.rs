#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering},
};

use euphorium::{Sound, SoundSource, Soundscape};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

#[cfg(target_arch = "wasm32")]
wasm_bindgen_test_configure!(run_in_browser);

const TEST_AUDIO_BYTES: &[u8] = include_bytes!("../examples/music/THE UNFORGIVING.mp3");
#[cfg(not(target_arch = "wasm32"))]
const TEST_AUDIO_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/music/THE UNFORGIVING.mp3"
);

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

fn add_allocated(bytes: usize) {
    let current = LIVE_BYTES.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK_BYTES.fetch_max(current, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };

        if !pointer.is_null() {
            add_allocated(layout.size());
        }

        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };

        if !pointer.is_null() {
            add_allocated(layout.size());
        }

        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe {
            System.dealloc(pointer, layout);
        }

        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_pointer = unsafe { System.realloc(pointer, layout, new_size) };

        if !new_pointer.is_null() {
            let old_size = layout.size();

            if new_size > old_size {
                add_allocated(new_size - old_size);
            } else {
                LIVE_BYTES.fetch_sub(old_size - new_size, Ordering::Relaxed);
            }
        }

        new_pointer
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

#[derive(Clone, Copy)]
struct MemorySnapshot {
    live_bytes: usize,
    peak_bytes: usize,
}

#[derive(Clone, Copy)]
struct MemoryReport {
    idle: MemorySnapshot,
    playing: MemorySnapshot,
    stopped: MemorySnapshot,
    replayed: MemorySnapshot,
}

enum PlaybackSource {
    StaticBytes,
    Resource(TestResource),
}

impl PlaybackSource {
    fn source(&self) -> SoundSource {
        match self {
            Self::StaticBytes => SoundSource::static_bytes(TEST_AUDIO_BYTES),
            Self::Resource(resource) => resource.source.clone(),
        }
    }

    async fn play(&self, sound: &Sound) {
        if matches!(self, Self::Resource(_)) {
            sound
                .load()
                .await
                .expect("audio resource should be loadable");
        }
        sound.play().expect("audio source should be playable");
    }
}

struct TestResource {
    source: SoundSource,
    #[cfg(target_arch = "wasm32")]
    object_url: String,
}

impl TestResource {
    #[cfg(not(target_arch = "wasm32"))]
    fn new() -> Self {
        Self {
            source: SoundSource::file(TEST_AUDIO_PATH),
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn new() -> Self {
        let array = js_sys::Uint8Array::from(TEST_AUDIO_BYTES);
        let parts = js_sys::Array::new();
        parts.push(&array);
        let blob = web_sys::Blob::new_with_u8_array_sequence(&parts)
            .expect("audio blob should be created");
        let object_url = web_sys::Url::create_object_url_with_blob(&blob)
            .expect("audio object URL should be created");

        Self {
            source: SoundSource::url(&object_url),
            object_url,
        }
    }
}

impl Drop for TestResource {
    fn drop(&mut self) {
        self.source.clear_browser_cache();

        #[cfg(target_arch = "wasm32")]
        web_sys::Url::revoke_object_url(&self.object_url)
            .expect("audio object URL should be revoked");
    }
}

fn memory_snapshot() -> MemorySnapshot {
    MemorySnapshot {
        live_bytes: LIVE_BYTES.load(Ordering::Relaxed),
        peak_bytes: PEAK_BYTES.load(Ordering::Relaxed),
    }
}

#[cfg(not(target_arch = "wasm32"))]
async fn wait_for_audio_settle() {
    std::thread::sleep(Duration::from_millis(250));
}

#[cfg(target_arch = "wasm32")]
async fn wait_for_audio_settle() {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let callback = Closure::once_into_js(move || {
            resolve
                .call0(&JsValue::UNDEFINED)
                .expect("timer promise should resolve");
        });

        web_sys::window()
            .expect("browser window is unavailable")
            .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), 250)
            .expect("browser timer should be scheduled");
    });

    JsFuture::from(promise)
        .await
        .expect("audio settle timer should resolve");
}

async fn measure_playback(source: PlaybackSource) -> MemoryReport {
    let soundscape = Soundscape::new();
    let sound = soundscape
        .create_sound("memory", source.source())
        .expect("the memory-test sound should be created");
    let idle = memory_snapshot();

    source.play(&sound).await;
    wait_for_audio_settle().await;
    let playing = memory_snapshot();

    sound.stop().expect("playback should stop");
    wait_for_audio_settle().await;
    let stopped = memory_snapshot();

    sound.play().expect("audio should replay");
    wait_for_audio_settle().await;
    let replayed = memory_snapshot();

    sound.stop().expect("replayed audio should stop");

    MemoryReport {
        idle,
        playing,
        stopped,
        replayed,
    }
}

fn assert_does_not_allocate_asset(report: MemoryReport, source: &str) {
    let playing_live_delta = report
        .playing
        .live_bytes
        .saturating_sub(report.idle.live_bytes);
    let playing_peak_delta = report
        .playing
        .peak_bytes
        .saturating_sub(report.idle.peak_bytes);

    assert!(
        playing_live_delta < TEST_AUDIO_BYTES.len() / 2,
        "{source} playback allocated {playing_live_delta} live bytes (peak delta {playing_peak_delta}); it should not retain the whole encoded asset"
    );
    assert!(
        playing_peak_delta < TEST_AUDIO_BYTES.len() / 2,
        "{source} playback allocated a {playing_peak_delta}-byte peak delta; it should not allocate the whole encoded asset"
    );
}

fn assert_replay_reuses_asset(report: MemoryReport, source: &str) {
    let replay_peak_growth = report
        .replayed
        .peak_bytes
        .saturating_sub(report.playing.peak_bytes);

    assert!(
        replay_peak_growth < TEST_AUDIO_BYTES.len() / 2,
        "replaying {source} grew peak memory by {replay_peak_growth} bytes; it should reuse the encoded asset"
    );
}

#[cfg(target_arch = "wasm32")]
fn assert_resource_memory(report: MemoryReport) {
    let playing_live_delta = report
        .playing
        .live_bytes
        .saturating_sub(report.idle.live_bytes);
    let playing_peak_delta = report
        .playing
        .peak_bytes
        .saturating_sub(report.idle.peak_bytes);

    assert!(
        playing_live_delta >= TEST_AUDIO_BYTES.len() / 2,
        "browser resource playback retained only {playing_live_delta} live bytes; it should retain the fetched encoded asset"
    );
    assert!(
        playing_peak_delta >= TEST_AUDIO_BYTES.len() / 2,
        "browser resource playback allocated only a {playing_peak_delta}-byte peak delta; it should allocate the fetched encoded asset"
    );
    assert!(
        playing_peak_delta < TEST_AUDIO_BYTES.len() * 3 / 2,
        "browser resource playback allocated a {playing_peak_delta}-byte peak delta; fetching should allocate the encoded asset only once"
    );
}

#[cfg(not(target_arch = "wasm32"))]
fn assert_resource_memory(report: MemoryReport) {
    assert_does_not_allocate_asset(report, "native resource");
}

fn format_mib(bytes: usize) -> String {
    format!("{:.2}", bytes as f64 / 1024.0 / 1024.0)
}

fn log_message(message: &str) {
    #[cfg(not(target_arch = "wasm32"))]
    println!("{message}");

    #[cfg(target_arch = "wasm32")]
    web_sys::console::log_1(&JsValue::from_str(message));
}

fn print_memory_snapshot(label: &str, snapshot: MemorySnapshot, idle: MemorySnapshot) {
    let live_delta = snapshot.live_bytes as isize - idle.live_bytes as isize;
    let peak_delta = snapshot.peak_bytes as isize - idle.peak_bytes as isize;

    log_message(&format!(
        "{label:>8}: live={} MiB ({:+.2} MiB), peak={} MiB ({:+.2} MiB)",
        format_mib(snapshot.live_bytes),
        live_delta as f64 / 1024.0 / 1024.0,
        format_mib(snapshot.peak_bytes),
        peak_delta as f64 / 1024.0 / 1024.0,
    ));
}

fn print_memory_report(label: &str, report: MemoryReport) {
    log_message(&format!("{label} audio memory report:"));
    print_memory_snapshot("idle", report.idle, report.idle);
    print_memory_snapshot("playing", report.playing, report.idle);
    print_memory_snapshot("stopped", report.stopped, report.idle);
    print_memory_snapshot("replayed", report.replayed, report.idle);
}

async fn reports_memory_for_static_bytes_and_resource_audio() {
    let static_bytes = measure_playback(PlaybackSource::StaticBytes).await;
    let resource = measure_playback(PlaybackSource::Resource(TestResource::new())).await;

    assert_does_not_allocate_asset(static_bytes, "static bytes");
    assert_replay_reuses_asset(static_bytes, "static bytes");
    assert_resource_memory(resource);
    assert_replay_reuses_asset(resource, "resource");

    print_memory_report("static bytes", static_bytes);
    print_memory_report("resource", resource);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn native_reports_memory_for_static_bytes_and_resource_audio() {
    pollster::block_on(reports_memory_for_static_bytes_and_resource_audio());
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test]
async fn browser_reports_memory_for_static_bytes_and_resource_audio() {
    reports_memory_for_static_bytes_and_resource_audio().await;
}
