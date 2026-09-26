use std::sync::{Arc, LazyLock, Mutex};

use eframe::egui;
use euphorium::{Sound, SoundSource, Soundscape, format_timestamp_secs, parse_timestamp};
use rayon::prelude::*;
use web_time::{Duration, Instant};

/// Compile-time proof that euphorium handles can move across rayon threads,
/// including browser worker threads on WASM.
fn assert_euphorium_is_send_sync<T: Send + Sync>() {}

fn assert_euphorium_threading() {
    // Sounds (and groups) are worker-safe on every target. The Soundscape
    // root itself stays on its creating thread on WASM, where it owns the
    // thread-affine audio output.
    assert_euphorium_is_send_sync::<Sound>();
    assert_euphorium_is_send_sync::<SoundSource>();
}

static SOUND_SOURCE: LazyLock<SoundSource> = LazyLock::new(|| {
    euphorium::audio_source! {
        native: file(concat!(env!("CARGO_MANIFEST_DIR"), "/../music/polar 240 yay.mp3")),
        wasm: url("polar 240 yay.mp3"),
    }
});

euphorium::sound_key! {
    enum AppSound {
        Music => "music",
    }
}

#[derive(Default)]
struct PositionPollState {
    pending: bool,
    result: Option<String>,
}

#[derive(Default)]
struct App {
    soundscape: Soundscape<AppSound>,
    rayon_result: String,
    passive_position_result: String,
    position_poll: Arc<Mutex<PositionPollState>>,
    last_position_poll: Option<Instant>,
}

impl App {
    fn new(_creation_context: &eframe::CreationContext<'_>) -> Self {
        let app = App::default();
        let _ = app
            .music()
            .set_source(SOUND_SOURCE.clone());

        #[cfg(all(target_arch = "wasm32", feature = "nightly"))]
        if let Err(error) = app
            .soundscape
            .switch_backend(euphorium::cpal::HostId::AudioWorklet)
        {
            log::error!("could not switch to AudioWorklet backend: {error}");
        }
        app
    }

    /// Get the [`Sound`] for the music track.
    pub fn music(&self) -> Sound {
        self.soundscape.sound(AppSound::Music)
    }

    fn request_positions_from_workers(&self, sound: Sound) {
        let position_poll = Arc::clone(&self.position_poll);

        rayon::spawn(move || {
            let threads = rayon::current_num_threads().max(1);
            let request_count = threads * 4;
            let positions: Vec<f64> = (0..request_count)
                .into_par_iter()
                .map(|_| {
                    sound
                        .clamped_position()
                        .map(|position| position.as_secs_f64())
                        .unwrap_or(f64::NAN)
                })
                .collect();

            let position_secs = positions.first().copied().unwrap_or(f64::NAN);
            let result = format!(
                "{request_count} requests from {threads} Rayon workers, position={position_secs:.2}s"
            );

            let mut position_poll = position_poll.lock().unwrap();
            position_poll.result = Some(result);
            position_poll.pending = false;
        });
    }

    fn update_passive_position_poll(&mut self, sound: Sound) {
        if let Some(result) = self.position_poll.lock().unwrap().result.take() {
            self.passive_position_result = result;
        }

        let due = self
            .last_position_poll
            .map_or(true, |last| last.elapsed() >= Duration::from_secs(1));
        if !due {
            return;
        }

        let mut position_poll = self.position_poll.lock().unwrap();
        if position_poll.pending {
            return;
        }

        position_poll.pending = true;
        self.last_position_poll = Some(Instant::now());
        drop(position_poll);

        self.request_positions_from_workers(sound);
    }
}

/// Drive euphorium handles from rayon worker threads plus CPU-heavy parallel
/// work, proving audio keeps playing while the pool is busy.
///
/// The same `Sound` handle is queried concurrently from the pool
/// (`Sound: Send + Sync` on every target, including browser workers), while
/// the scene root and its output stay on the creating thread.
///
/// Returns a one-line human-readable summary for the UI.
fn run_rayon_euphorium_test(sound: Sound) -> String {
    assert_euphorium_threading();

    let threads = rayon::current_num_threads().max(1);
    let started = web_time::Instant::now();

    // 1. Touch the same `Sound` handle concurrently from many rayon threads.
    let probes: Vec<(bool, f32)> = (0..threads * 4)
        .into_par_iter()
        .map(|_| {
            (
                sound.is_playing().unwrap_or(false),
                sound.speed().unwrap_or(f32::NAN),
            )
        })
        .collect();

    // 2. Split two euphorium queries across exactly two pool threads,
    //    including a fallible one whose `Result` crosses back to this thread.
    let (volume, position_secs) = rayon::join(
        || sound.set_volume(sound.local_volume().unwrap_or(1.0)),
        || {
            sound
                .clamped_position()
                .map(|position| position.as_secs_f64())
                .unwrap_or(f64::NAN)
        },
    );
    let volume = volume
        .map(|()| sound.local_volume().unwrap_or(f32::NAN))
        .unwrap_or(f32::NAN);

    // 3. Parallel CPU load while the audio mixer thread runs independently.
    const PAR_ITEMS: u64 = 10_000_000;
    let sum: u64 = (0..PAR_ITEMS).into_par_iter().sum();
    let expected: u64 = PAR_ITEMS * PAR_ITEMS.wrapping_sub(1) / 2;
    let elapsed = started.elapsed();

    if sum == expected {
        format!(
            "OK: {threads} threads, {} parallel sound probes \
             (playing={}, speed={:.2}, vol={volume:.2}, pos={position_secs:.2}s), \
             parallel sum 0..{PAR_ITEMS} = {sum} in {elapsed:.2?}",
            probes.len(),
            probes.first().map(|(playing, _)| playing).unwrap_or(&false),
            probes.first().map(|(_, speed)| *speed).unwrap_or(f32::NAN),
        )
    } else {
        format!("MISMATCH: parallel sum = {sum}, expected {expected}")
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let sound = self.music();
        for failure in self.soundscape.update() {
            log::error!(
                "could not update sound {:?}: {}",
                failure.sound,
                failure.error
            );
        }

        self.update_passive_position_poll(sound.clone());

        // update timeline
        ui.ctx().request_repaint();

        let control_width = ui.available_width().min(360.0);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 0.0),
            egui::Layout::top_down(egui::Align::Center),
            |ui| {
                ui.set_width(control_width);

                ui.spacing_mut().slider_width = control_width;

                let mut position_secs: f64 =
                    sound.clamped_position().unwrap_or_default().as_secs_f64();
                let response = ui.add(
                    egui::Slider::new(&mut position_secs, sound.seek_range().unwrap_or(0.0..=1.0))
                        .show_value(false),
                );
                let timeline_resume_id = egui::Id::new("timeline_resume_after_scrub");

                if response.drag_started() {
                    let resume = sound.is_playing().unwrap_or(false);

                    ui.ctx().data_mut(|data| {
                        data.insert_temp(timeline_resume_id, resume);
                    });

                    if let Err(error) = sound.pause() {
                        log::error!("could not pause audio: {error}");
                    }
                }

                if response.changed()
                    && let Err(error) = sound.try_seek_secs(position_secs)
                {
                    log::error!("could not seek audio: {error}");
                }

                if response.drag_stopped() {
                    let resume = ui.ctx().data_mut(|data| {
                        data.remove_temp::<bool>(timeline_resume_id)
                            .unwrap_or(false)
                    });

                    if resume && let Err(error) = sound.resume() {
                        log::error!("could not resume audio: {error}");
                    }
                }

                let is_loading = sound.is_loading().unwrap_or(false);
                let button_size = ui.spacing().interact_size;

                let (row_rect, _) = ui.allocate_exact_size(
                    egui::vec2(control_width, button_size.y),
                    egui::Sense::hover(),
                );

                let button_rect = egui::Rect::from_center_size(row_rect.center(), button_size);

                let left_rect = egui::Rect::from_min_max(
                    row_rect.min,
                    egui::pos2(button_rect.left(), row_rect.bottom()),
                );

                let mut left_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .id_salt("position_control")
                        .max_rect(left_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );

                let response = left_ui.add(
                    egui::DragValue::new(&mut position_secs)
                        .range(sound.seek_range().unwrap_or(0.0..=1.0))
                        .custom_formatter(|position, _| format_timestamp_secs(position))
                        .custom_parser(parse_timestamp),
                );

                if response.changed()
                    && let Err(error) = sound.try_seek_secs(position_secs)
                {
                    log::error!("could not seek audio: {error}");
                }

                let mut button_builder = egui::UiBuilder::new()
                    .id_salt("play_button")
                    .max_rect(button_rect)
                    .layout(egui::Layout::centered_and_justified(
                        egui::Direction::TopDown,
                    ));

                if is_loading {
                    button_builder = button_builder.disabled();
                }

                let mut button_ui = ui.new_child(button_builder);

                let button = button_ui
                    .add(egui::Button::new(""))
                    .on_hover_text(if is_loading {
                        "Loading"
                    } else if sound.is_playing().unwrap_or(false) {
                        "Pause"
                    } else {
                        "Play"
                    });

                let center = button.rect.center();
                let icon_color = button_ui.style().interact(&button).fg_stroke.color;

                if is_loading {
                    button_ui.painter().text(
                        center,
                        egui::Align2::CENTER_CENTER,
                        "...",
                        egui::TextStyle::Button.resolve(button_ui.style()),
                        icon_color,
                    );
                } else if sound.is_playing().unwrap_or(false) {
                    let size = 8.0;
                    button_ui.painter().rect_filled(
                        egui::Rect::from_center_size(center, egui::vec2(size, size)),
                        0.0,
                        icon_color,
                    );
                } else {
                    let half_h = 6.0;
                    let half_w = 5.0;

                    button_ui.painter().add(egui::Shape::convex_polygon(
                        vec![
                            egui::pos2(center.x - half_w, center.y - half_h),
                            egui::pos2(center.x - half_w, center.y + half_h),
                            egui::pos2(center.x + half_w, center.y),
                        ],
                        icon_color,
                        egui::Stroke::NONE,
                    ));
                }

                if button.clicked() {
                    if sound.is_playing().unwrap_or(false) {
                        if let Err(error) = self.music().pause() {
                            log::error!("could not pause audio: {error}");
                        }
                    } else {
                        if let Err(error) = self.music().play() {
                            log::error!("could not play audio: {error}");
                        }
                    }
                }

                ui.painter().text(
                    egui::pos2(row_rect.right(), row_rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    sound.duration_formatted().unwrap_or_else(|_| "0:00".into()),
                    egui::TextStyle::Body.resolve(ui.style()),
                    ui.visuals().text_color(),
                );

                ui.add_space(30.0);

                ui.columns(2, |columns| {
                    let mut speed = sound.speed().unwrap_or(1.0);
                    let speed_width = columns[0].available_width();
                    columns[0].label(format!("Speed: {speed:.2}x"));
                    columns[0].spacing_mut().slider_width = speed_width;

                    let response = columns[0].add(
                        egui::Slider::new(&mut speed, 0.5..=2.0)
                            .show_value(false)
                            .logarithmic(true),
                    );

                    if response.changed()
                        && let Err(error) = sound.set_speed(speed)
                    {
                        log::error!("could not change playback speed: {error}");
                    }

                    let mut volume = sound.local_volume().unwrap_or(1.0) * 100.0;
                    let volume_width = columns[1].available_width();
                    columns[1].label(format!("Volume: {volume:.0}%"));
                    columns[1].spacing_mut().slider_width = volume_width;

                    let response = columns[1]
                        .add(egui::Slider::new(&mut volume, 0.0..=100.0).show_value(false));

                    if response.changed()
                        && let Err(error) = sound.set_volume(volume / 100.0)
                    {
                        log::error!("could not change volume: {error}");
                    }
                });

                ui.add_space(10.0);
                ui.separator();
                ui.label(format!(
                    "Backend: {}",
                    self.soundscape
                        .backend()
                        .map(|b| b.to_string())
                        .unwrap_or_else(|| "Unknown".to_string())
                ));
                ui.label(format!("Threads: {}", rayon::current_num_threads()));

                if !self.passive_position_result.is_empty() {
                    ui.label(&self.passive_position_result);
                }

                if ui.button("run stress test").clicked() {
                    self.rayon_result = run_rayon_euphorium_test(sound.clone());
                    log::info!("rayon test: {}", self.rayon_result);
                }

                if !self.rayon_result.is_empty() {
                    ui.label(&self.rayon_result);
                }
            },
        );
    }
}

// entrypoint

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    eframe::run_native(
        "this is an app",
        eframe::NativeOptions::default(),
        Box::new(|creation_context| Ok(Box::new(App::new(creation_context)))),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {
    use wasm_bindgen::JsCast as _;
    wasm_bindgen_futures::spawn_local(async {
        eframe::WebLogger::init(log::LevelFilter::Debug).ok();

        // Initialize the rayon worker pool before any `par_iter` use.
        // Requires cross-origin isolation (see Trunk.toml headers) and the
        // shared-memory wasm build (see .cargo/config.toml).
        let concurrency = web_sys::window()
            .map(|window| window.navigator().hardware_concurrency() as usize)
            .unwrap_or(4)
            .clamp(1, 16);
        if let Err(error) =
            wasm_bindgen_futures::JsFuture::from(wasm_bindgen_rayon::init_thread_pool(concurrency))
                .await
        {
            log::error!("could not init rayon thread pool: {error:?}");
        } else {
            log::info!("rayon thread pool initialized with {concurrency} threads");
        }

        let document = web_sys::window()
            .expect("browser window is unavailable")
            .document()
            .expect("browser document is unavailable");
        let canvas = document
            .get_element_by_id("the_canvas_id")
            .expect("the_canvas_id canvas is missing")
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .expect("the_canvas_id is not a canvas");

        eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|creation_context| Ok(Box::new(App::new(creation_context)))),
            )
            .await
            .expect("failed to start eframe");
    });
}
