use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        LazyLock,
        atomic::{AtomicUsize, Ordering},
    },
};

use eframe::egui;
use euphorium::{
    AutomaticGainEffect, DistortionEffect, FilterEffect, LimiterEffect, Output, ReverbEffect,
    Sound, SoundEffects, SoundSource, Soundscape, WaveformView, format_timestamp_secs,
    parse_timestamp,
};
use web_time::Duration;

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

fn add_allocated(bytes: usize) {
    let current = LIVE_BYTES.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK_BYTES.fetch_max(current, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };

        if !ptr.is_null() {
            add_allocated(layout.size());
        }

        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };

        if !ptr.is_null() {
            add_allocated(layout.size());
        }

        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe {
            System.dealloc(ptr, layout);
        }

        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };

        if !new_ptr.is_null() {
            let old_size = layout.size();

            if new_size > old_size {
                add_allocated(new_size - old_size);
            } else {
                LIVE_BYTES.fetch_sub(old_size - new_size, Ordering::Relaxed);
            }
        }

        new_ptr
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

#[derive(Clone, Copy)]
struct Song {
    title: &'static str,
    bytes: &'static [u8],
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    native_path: &'static str,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    wasm_url: &'static str,
}

const SONGS: [Song; 3] = [
    Song {
        title: "THE UNFORGIVING",
        bytes: include_bytes!("../../music/THE UNFORGIVING.mp3"),
        native_path: concat!(env!("CARGO_MANIFEST_DIR"), "/../music/THE UNFORGIVING.mp3"),
        wasm_url: "THE UNFORGIVING.mp3",
    },
    Song {
        title: "ATLAS 270 [WHAT NO]",
        bytes: include_bytes!("../../music/ATLAS 270 [WHAT NO].wav"),
        native_path: concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../music/ATLAS 270 [WHAT NO].wav"
        ),
        wasm_url: "ATLAS 270 [WHAT NO].wav",
    },
    Song {
        title: "polar 240 yay",
        bytes: include_bytes!("../../music/polar 240 yay.mp3"),
        native_path: concat!(env!("CARGO_MANIFEST_DIR"), "/../music/polar 240 yay.mp3"),
        wasm_url: "polar 240 yay.mp3",
    },
];
static SOUND_SOURCE_PER_SONG: LazyLock<Vec<SoundSource>> = LazyLock::new(|| {
    SONGS
        .iter()
        .map(|song| {
            euphorium::audio_source! {
                native: file(song.native_path),
                wasm: url(song.wasm_url),
            }
        })
        .collect()
});

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum MusicMode {
    #[default]
    StaticBytes,
    File,
}

impl MusicMode {
    const ALL: &'static [MusicMode] = &[MusicMode::StaticBytes, MusicMode::File];

    fn name(&self) -> &'static str {
        match self {
            MusicMode::StaticBytes => "Static bytes",
            MusicMode::File => "File",
        }
    }
}

euphorium::sound_key! {
    enum AppSound {
        Music => "music",
    }
}

#[derive(Default)]
struct App {
    soundscape: Soundscape<AppSound>,
    waveform: WaveformView,
    music_mode: MusicMode,
    selected_song: usize,
}

impl App {
    fn new(_creation_context: &eframe::CreationContext<'_>) -> Self {
        let mut app = App::default();
        app.select_song(app.selected_song);
        app
    }

    /// Get the [`Sound`] for the music track.
    pub fn music(&self) -> Sound {
        self.soundscape.sound(AppSound::Music)
    }

    /// Get the [`SoundSource`] for the currently selected song based on the current [`MusicMode`].
    pub fn selected_source(&self) -> SoundSource {
        let song = SONGS[self.selected_song];
        match self.music_mode {
            MusicMode::StaticBytes => SoundSource::static_bytes(song.bytes),
            MusicMode::File => SOUND_SOURCE_PER_SONG[self.selected_song].clone(),
        }
    }

    /// Stops current song and loads the new one. Also changes the waveform to match the new song.
    pub fn select_song(&mut self, selected_song: usize) {
        let song = SONGS[selected_song];

        if let Err(error) = self.music().stop() {
            log::error!("could not stop audio: {error}");
        }
        self.selected_song = selected_song;

        if let Err(error) = self.music().set_source(self.selected_source()) {
            log::error!("could not load audio duration: {error}");
        }

        if let Err(error) = self.waveform.set_static_bytes(song.bytes) {
            log::error!("could not start waveform decoding: {error}");
        }
    }

    fn show_effects(&mut self, ui: &mut egui::Ui) {
        let sound = self.music();
        let mut effects = sound.effects().unwrap_or_default();
        let old_effects = effects;

        egui::CollapsingHeader::new("Post processing")
            .default_open(false)
            .show(ui, |ui| {
                egui::Grid::new("post_processing_grid")
                    .num_columns(2)
                    .spacing(egui::vec2(16.0, 8.0))
                    .show(ui, |ui| {
                        ui.label("Input gain");
                        ui.add(
                            egui::Slider::new(&mut effects.input_gain, 0.0..=4.0)
                                .suffix("x")
                                .logarithmic(true),
                        );
                        ui.end_row();

                        ui.label("Fade in");
                        let mut fade_in_secs = effects.fade_in.as_secs_f64();
                        if ui
                            .add(egui::Slider::new(&mut fade_in_secs, 0.0..=5.0).suffix(" s"))
                            .changed()
                        {
                            effects.fade_in = Duration::from_secs_f64(fade_in_secs);
                        }
                        ui.end_row();

                        let mut high_pass_enabled = effects.high_pass.is_some();
                        if ui.checkbox(&mut high_pass_enabled, "High-pass").changed() {
                            effects.high_pass = high_pass_enabled.then_some(FilterEffect {
                                frequency: 120,
                                ..FilterEffect::default()
                            });
                        }
                        if let Some(effect) = effects.high_pass.as_mut() {
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::Slider::new(&mut effect.frequency, 20..=5_000)
                                        .suffix(" Hz")
                                        .logarithmic(true),
                                );
                                ui.add(egui::Slider::new(&mut effect.q, 0.1..=2.0).prefix("Q "));
                            });
                        } else {
                            ui.label("Off");
                        }
                        ui.end_row();

                        let mut low_pass_enabled = effects.low_pass.is_some();
                        if ui.checkbox(&mut low_pass_enabled, "Low-pass").changed() {
                            effects.low_pass = low_pass_enabled.then_some(FilterEffect {
                                frequency: 8_000,
                                ..FilterEffect::default()
                            });
                        }
                        if let Some(effect) = effects.low_pass.as_mut() {
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::Slider::new(&mut effect.frequency, 100..=20_000)
                                        .suffix(" Hz")
                                        .logarithmic(true),
                                );
                                ui.add(egui::Slider::new(&mut effect.q, 0.1..=2.0).prefix("Q "));
                            });
                        } else {
                            ui.label("Off");
                        }
                        ui.end_row();

                        let mut distortion_enabled = effects.distortion.is_some();
                        if ui.checkbox(&mut distortion_enabled, "Distortion").changed() {
                            effects.distortion =
                                distortion_enabled.then_some(DistortionEffect::default());
                        }
                        if let Some(effect) = effects.distortion.as_mut() {
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::Slider::new(&mut effect.gain, 1.0..=20.0)
                                        .prefix("Gain ")
                                        .logarithmic(true),
                                );
                                ui.add(
                                    egui::Slider::new(&mut effect.threshold, 0.05..=1.0)
                                        .prefix("Clip "),
                                );
                            });
                        } else {
                            ui.label("Off");
                        }
                        ui.end_row();

                        let mut automatic_gain_enabled = effects.automatic_gain.is_some();
                        if ui
                            .checkbox(&mut automatic_gain_enabled, "Automatic gain")
                            .changed()
                        {
                            effects.automatic_gain =
                                automatic_gain_enabled.then_some(AutomaticGainEffect::default());
                        }
                        if let Some(effect) = effects.automatic_gain.as_mut() {
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::Slider::new(&mut effect.target_level, 0.1..=2.0)
                                            .prefix("Target "),
                                    );
                                    ui.add(
                                        egui::Slider::new(&mut effect.maximum_gain, 1.0..=10.0)
                                            .prefix("Max "),
                                    );
                                });

                                let mut attack_secs = effect.attack.as_secs_f64();
                                let mut release_secs = effect.release.as_secs_f64();
                                ui.horizontal(|ui| {
                                    if ui
                                        .add(
                                            egui::Slider::new(&mut attack_secs, 0.0..=10.0)
                                                .prefix("Attack ")
                                                .suffix(" s"),
                                        )
                                        .changed()
                                    {
                                        effect.attack = Duration::from_secs_f64(attack_secs);
                                    }
                                    if ui
                                        .add(
                                            egui::Slider::new(&mut release_secs, 0.0..=10.0)
                                                .prefix("Release ")
                                                .suffix(" s"),
                                        )
                                        .changed()
                                    {
                                        effect.release = Duration::from_secs_f64(release_secs);
                                    }
                                });
                            });
                        } else {
                            ui.label("Off");
                        }
                        ui.end_row();

                        let mut reverb_enabled = effects.reverb.is_some();
                        if ui.checkbox(&mut reverb_enabled, "Reverb").changed() {
                            effects.reverb = reverb_enabled.then_some(ReverbEffect::default());
                        }
                        if let Some(effect) = effects.reverb.as_mut() {
                            let mut delay_ms = effect.delay.as_secs_f64() * 1_000.0;
                            ui.horizontal(|ui| {
                                if ui
                                    .add(
                                        egui::Slider::new(&mut delay_ms, 20.0..=500.0)
                                            .prefix("Delay ")
                                            .suffix(" ms")
                                            .logarithmic(true),
                                    )
                                    .changed()
                                {
                                    effect.delay = Duration::from_secs_f64(delay_ms / 1_000.0);
                                }
                                ui.add(
                                    egui::Slider::new(&mut effect.amplitude, 0.0..=1.0)
                                        .prefix("Mix "),
                                );
                            });
                        } else {
                            ui.label("Off");
                        }
                        ui.end_row();

                        let mut limiter_enabled = effects.limiter.is_some();
                        if ui.checkbox(&mut limiter_enabled, "Limiter").changed() {
                            effects.limiter = limiter_enabled.then_some(LimiterEffect::default());
                        }
                        if let Some(effect) = effects.limiter.as_mut() {
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::Slider::new(&mut effect.threshold_db, -20.0..=-0.1)
                                            .prefix("Threshold ")
                                            .suffix(" dB"),
                                    );
                                    ui.add(
                                        egui::Slider::new(&mut effect.knee_width_db, 0.0..=12.0)
                                            .prefix("Knee ")
                                            .suffix(" dB"),
                                    );
                                });

                                let mut attack_ms = effect.attack.as_secs_f64() * 1_000.0;
                                let mut release_ms = effect.release.as_secs_f64() * 1_000.0;
                                ui.horizontal(|ui| {
                                    if ui
                                        .add(
                                            egui::Slider::new(&mut attack_ms, 0.1..=50.0)
                                                .prefix("Attack ")
                                                .suffix(" ms")
                                                .logarithmic(true),
                                        )
                                        .changed()
                                    {
                                        effect.attack =
                                            Duration::from_secs_f64(attack_ms / 1_000.0);
                                    }
                                    if ui
                                        .add(
                                            egui::Slider::new(&mut release_ms, 10.0..=500.0)
                                                .prefix("Release ")
                                                .suffix(" ms")
                                                .logarithmic(true),
                                        )
                                        .changed()
                                    {
                                        effect.release =
                                            Duration::from_secs_f64(release_ms / 1_000.0);
                                    }
                                });
                            });
                        } else {
                            ui.label("Off");
                        }
                        ui.end_row();
                    });

                ui.horizontal(|ui| {
                    if ui.button("Reset").clicked()
                        && let Err(error) = sound.set_effects(SoundEffects::default())
                    {
                        log::error!("could not reset audio effects: {error}");
                    }
                });
            });
        if old_effects != effects
            && let Err(error) = sound.set_effects(effects)
        {
            log::error!("could not apply audio effects: {error}");
        }
    }

    fn show_waveform(&mut self, ui: &mut egui::Ui) {
        let sound = self.music();
        if !self.waveform.is_available() {
            ui.label("Waveform unavailable");
            return;
        }
        let duration_secs = self.waveform.duration_secs();
        let minimum_visible_secs = self.waveform.minimum_visible_secs();

        let playhead_secs = sound.position().unwrap_or_default().as_secs_f64();
        let desired_size = egui::vec2(ui.available_width().min(760.0), 156.0);

        ui.horizontal(|ui| {
            let offset = ((ui.available_width() - desired_size.x) / 2.0).max(0.0);
            ui.add_space(offset);

            if ui.button("-").on_hover_text("Zoom out").clicked() {
                self.waveform.zoom_out();
            }

            ui.add(
                egui::Slider::new(
                    self.waveform.visible_secs_mut(),
                    minimum_visible_secs..=duration_secs,
                )
                .suffix("s")
                .logarithmic(true),
            );

            if ui.button("+").on_hover_text("Zoom in").clicked() {
                self.waveform.zoom_in();
            }
        });

        let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::click_and_drag());

        if response.hovered() {
            let (zoom_delta, scroll_delta) = ui.input(|input| {
                (
                    input.zoom_delta() as f64,
                    input.smooth_scroll_delta().y as f64,
                )
            });
            let zoom_delta = if zoom_delta != 1.0 {
                zoom_delta
            } else {
                (scroll_delta * 0.01).exp()
            };

            self.waveform.zoom_by(zoom_delta);
        }

        let visible_range = match self.waveform.poll_visible(playhead_secs) {
            Ok(poll) => {
                if !poll.finished {
                    ui.ctx().request_repaint();
                }
                poll.visible
            }
            Err(error) => {
                log::error!("could not prioritize waveform decoding: {error}");
                self.waveform.visible_range(playhead_secs)
            }
        };
        let visible_duration = visible_range.end - visible_range.start;

        if (response.clicked() || response.dragged())
            && let Some(pointer) = response.interact_pointer_pos()
        {
            let pointer_fraction = ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let seek_secs = WaveformView::seek_secs(&visible_range, f64::from(pointer_fraction));

            if let Err(error) = sound.try_seek_secs(seek_secs) {
                log::error!("could not seek audio: {error}");
            }
        }

        if response.drag_started() {
            self.waveform
                .begin_scrub(sound.is_playing().unwrap_or(false));
            if let Err(error) = sound.pause() {
                log::error!("could not pause audio: {error}");
            }
        }

        if response.drag_stopped()
            && self.waveform.end_scrub()
            && let Err(error) = sound.resume()
        {
            log::error!("could not resume audio: {error}");
        }

        let painter = ui.painter_at(rect);
        let background = egui::Color32::from_rgb(20, 24, 29);
        let grid = egui::Color32::from_rgb(53, 61, 68);
        let future = egui::Color32::from_rgb(81, 133, 141);
        let played = egui::Color32::from_rgb(238, 151, 73);
        let playhead = egui::Color32::from_rgb(250, 232, 191);
        let text = egui::Color32::from_rgb(184, 193, 198);
        let wave_rect = rect.shrink2(egui::vec2(0.0, 20.0));
        let center_y = wave_rect.center().y;

        painter.rect_filled(rect, 6.0, background);
        painter.line_segment(
            [
                egui::pos2(rect.left(), center_y),
                egui::pos2(rect.right(), center_y),
            ],
            egui::Stroke::new(1.0, grid),
        );

        for tick in 0..=4 {
            let fraction = tick as f32 / 4.0;
            let x = egui::lerp(rect.x_range(), fraction);
            let tick_secs = WaveformView::tick_secs(&visible_range, tick, 5);

            painter.line_segment(
                [
                    egui::pos2(x, wave_rect.top()),
                    egui::pos2(x, wave_rect.bottom()),
                ],
                egui::Stroke::new(1.0, grid),
            );
            painter.text(
                egui::pos2(x, rect.bottom() - 4.0),
                egui::Align2::CENTER_BOTTOM,
                format_timestamp_secs(tick_secs),
                egui::FontId::monospace(10.0),
                text,
            );
        }

        if visible_duration > 0.0 {
            let waveform_slice = self.waveform.slice(
                visible_range.clone(),
                (rect.width() / 2.0).max(1.0) as usize,
            );
            let amplitude_height = wave_rect.height() * 0.46;

            if let Some(waveform_slice) = waveform_slice {
                for (index, peak) in waveform_slice.peaks().iter().enumerate() {
                    if !peak.is_available() {
                        continue;
                    }

                    let Some(peak_range) = waveform_slice.peak_range(index) else {
                        continue;
                    };
                    let peak_secs =
                        (peak_range.start.as_secs_f64() + peak_range.end.as_secs_f64()) * 0.5;
                    let fraction = ((peak_secs - visible_range.start) / visible_duration) as f32;
                    let x = egui::lerp(rect.x_range(), fraction);
                    let top = center_y - peak.max.clamp(-1.0, 1.0) * amplitude_height;
                    let bottom = center_y - peak.min.clamp(-1.0, 1.0) * amplitude_height;
                    let color = if peak_secs <= playhead_secs {
                        played
                    } else {
                        future
                    };

                    if rect.x_range().contains(x) {
                        painter.line_segment(
                            [egui::pos2(x, top), egui::pos2(x, bottom.max(top + 1.0))],
                            egui::Stroke::new(1.5, color),
                        );
                    }
                }
            }

            let playhead_fraction = WaveformView::playhead_fraction(playhead_secs, &visible_range);
            let playhead_x = egui::lerp(rect.x_range(), playhead_fraction);
            painter.line_segment(
                [
                    egui::pos2(playhead_x, rect.top()),
                    egui::pos2(playhead_x, rect.bottom()),
                ],
                egui::Stroke::new(2.0, playhead),
            );
        }
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

        // repaint to update fps counter
        ui.ctx().request_repaint();

        egui::Panel::top("top_panel").show(ui, |ui| {
            let fps = ui.ctx().input(|input| 1.0 / f64::from(input.unstable_dt));

            let live = LIVE_BYTES.load(Ordering::Relaxed);
            let peak = PEAK_BYTES.load(Ordering::Relaxed);

            ui.horizontal(|ui| {
                ui.label(format!("i love euphorium | fps: {fps:.0}"));

                // Consume all remaining space except what the right label needs.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!(
                        "memory: {:.2}/{:.2} MiB",
                        live as f64 / 1024.0 / 1024.0,
                        peak as f64 / 1024.0 / 1024.0
                    ));
                });
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    let mut selected_song = self.selected_song;
                    let mut selected_backend = self.soundscape.backend();
                    let mut selected_device = self.soundscape.device();

                    ui.add_enabled_ui(!sound.is_loading().unwrap_or(false), |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Song:");
                            egui::ComboBox::from_id_salt("song_selector")
                                .selected_text(SONGS[selected_song].title)
                                .width(240.0)
                                .show_ui(ui, |ui| {
                                    for (index, song) in SONGS.iter().enumerate() {
                                        ui.selectable_value(&mut selected_song, index, song.title);
                                    }
                                });

                            #[cfg(target_arch = "wasm32")]
                            {
                                if ui
                                    .add(egui::Button::new("Clear cache"))
                                    .on_hover_text(
                                        "Forget downloaded audio so files are fetched again",
                                    )
                                    .clicked()
                                {
                                    for source in SOUND_SOURCE_PER_SONG.iter() {
                                        source.clear_browser_cache();
                                    }
                                }
                            }
                        });

                        ui.horizontal(|ui| {
                            ui.label("Audio backend:");
                            egui::ComboBox::from_id_salt("backend_selector")
                                .selected_text(
                                    selected_backend
                                        .map(|backend| backend.name())
                                        .unwrap_or("Custom output"),
                                )
                                .width(240.0)
                                .show_ui(ui, |ui: &mut egui::Ui| {
                                    for &backend in &Output::available_backends() {
                                        ui.selectable_value(
                                            &mut selected_backend,
                                            Some(backend),
                                            backend.name(),
                                        );
                                    }
                                });
                        });

                        ui.horizontal(|ui| {
                            ui.label("Output device:");
                            let selected_text = selected_device
                                .as_ref()
                                .map(|device| device.to_string())
                                .unwrap_or_else(|| "Custom output".to_string());
                            egui::ComboBox::from_id_salt("device_selector")
                                .selected_text(selected_text)
                                .width(240.0)
                                .show_ui(ui, |ui| {
                                    let devices = Output::available_devices();
                                    if devices.is_empty() {
                                        ui.label("No output devices found");
                                    }

                                    for device in &devices {
                                        ui.selectable_value(
                                            &mut selected_device,
                                            Some(device.clone()),
                                            device.to_string(),
                                        );
                                    }
                                });

                            if ui
                                .button("Refresh")
                                .on_hover_text("Re-enumerate output devices")
                                .clicked()
                            {
                                Output::refresh_available_backends();
                                Output::refresh_available_devices();
                            }
                        });
                    });

                    if selected_song != self.selected_song {
                        self.select_song(selected_song);
                    }

                    if selected_backend != self.soundscape.backend() {
                        if let Some(backend) = selected_backend
                            && let Err(error) = self.soundscape.switch_backend(backend)
                        {
                            log::error!("could not switch audio backend: {error}");
                        }
                    } else if selected_device != self.soundscape.device()
                        && let Some(device) = selected_device
                        && let Err(error) = self.soundscape.switch_device(&device)
                    {
                        log::error!("could not switch audio device: {error}");
                    }

                    self.show_waveform(ui);

                    let tab_width = 90.0;
                    let tab_height = ui.spacing().interact_size.y;
                    let gap = ui.spacing().item_spacing.x;
                    let tabs_width = tab_width * MusicMode::ALL.len() as f32
                        + gap * (MusicMode::ALL.len().saturating_sub(1)) as f32;

                    ui.horizontal(|ui| {
                        let offset = ((ui.available_width() - tabs_width) / 2.0).max(0.0);
                        ui.add_space(offset);

                        for mode in MusicMode::ALL {
                            if ui
                                .add_sized(
                                    [tab_width, tab_height],
                                    egui::Button::selectable(self.music_mode == *mode, mode.name()),
                                )
                                .clicked()
                            {
                                self.music_mode = *mode;

                                if let Err(error) = self.music().set_source(self.selected_source())
                                {
                                    log::error!("could not change audio mode: {error}");
                                }
                            }
                        }
                    });

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
                                egui::Slider::new(
                                    &mut position_secs,
                                    sound.seek_range().unwrap_or(0.0..=1.0),
                                )
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

                            let button_rect =
                                egui::Rect::from_center_size(row_rect.center(), button_size);

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

                            let button =
                                button_ui
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

                                let response = columns[1].add(
                                    egui::Slider::new(&mut volume, 0.0..=100.0).show_value(false),
                                );

                                if response.changed()
                                    && let Err(error) = sound.set_volume(volume / 100.0)
                                {
                                    log::error!("could not change volume: {error}");
                                }
                            });

                            ui.add_space(8.0);

                            ui.horizontal(|ui| {
                                let mut preserve_pitch = sound.preserves_pitch().unwrap_or(false);
                                if ui
                                    .checkbox(&mut preserve_pitch, "Preserve pitch")
                                    .on_hover_text("Use WSOLA for tempo changes")
                                    .changed()
                                    && let Err(error) = sound.set_preserve_pitch(preserve_pitch)
                                {
                                    log::error!("could not change pitch preservation: {error}");
                                }

                                let mut looping = sound.is_looping().unwrap_or(false);
                                if ui
                                    .checkbox(&mut looping, "Loop")
                                    .on_hover_text("Repeat playback")
                                    .changed()
                                    && let Err(error) = sound.set_looping(looping)
                                {
                                    log::error!("could not change looping: {error}");
                                }
                            });
                        },
                    );

                    ui.add_space(16.0);
                    self.show_effects(ui);
                });
            });
        });
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
