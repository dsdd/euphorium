use std::{
    collections::HashSet,
    sync::{Arc, Mutex, OnceLock},
};

use rodio::{
    DeviceSinkBuilder, DeviceSinkError, DeviceTrait, MixerDeviceSink,
    cpal::{BufferSize, traits::HostTrait},
};

use crate::{Backend, Device, OutputError};

/// A cloneable handle to an audio output sink owned by a [`crate::Soundscape`] audio scene.
///
/// Pass an output to [`crate::Soundscape::new_with_output`] to select or reuse a device sink.
///
/// ```no_run
/// use euphorium::{Soundscape, Output};
///
/// let output = Output::new();
/// let soundscape = Soundscape::new_with_output(output);
/// ```
#[derive(Clone)]
pub struct Output {
    mixer_device_sink: Arc<Mutex<Option<MixerDeviceSink>>>,
    backend: Option<Backend>,
    device: Option<Device>,
}

impl Default for Output {
    fn default() -> Self {
        Self::new()
    }
}

/// Cached audio backends from the most recent enumeration.
///
/// Populated on the first [`Output::available_backends`] call and refreshed by
/// [`Output::refresh_available_backends`].
static CACHED_AUDIO_BACKENDS: OnceLock<Mutex<Option<Vec<Backend>>>> = OnceLock::new();

/// Cached output devices from the most recent enumeration.
///
/// Populated on the first [`Output::available_devices`] call and refreshed by
/// [`Output::refresh_available_devices`].
static CACHED_OUTPUT_DEVICES: OnceLock<Mutex<Option<Vec<Device>>>> = OnceLock::new();

/// Convenience function to switch every output to one newly opened output device.
///
/// Stops at the first failure, earlier outputs remain switched.
pub fn switch_outputs_to_device<'a>(
    outputs: impl IntoIterator<Item = &'a mut Output>,
    device: &Device,
) -> Result<(), OutputError> {
    for output in outputs {
        output.switch_device(device)?;
    }

    Ok(())
}

/// Convenience function to switch every output to one newly opened backend.
///
/// Stops at the first failure, earlier outputs remain switched.
pub fn switch_outputs_to_backend<'a>(
    outputs: impl IntoIterator<Item = &'a mut Output>,
    backend: Backend,
) -> Result<(), OutputError> {
    for output in outputs {
        output.switch_backend(backend)?;
    }

    Ok(())
}

impl Output {
    #[cfg(target_arch = "wasm32")]
    fn browser_has_active_user_gesture() -> bool {
        let Some(window) = web_sys::window() else {
            return false;
        };
        let Ok(navigator) = js_sys::Reflect::get(
            window.as_ref(),
            &wasm_bindgen::JsValue::from_str("navigator"),
        ) else {
            return true;
        };
        let Ok(user_activation) = js_sys::Reflect::get(
            &navigator,
            &wasm_bindgen::JsValue::from_str("userActivation"),
        ) else {
            return true;
        };

        if user_activation.is_null() || user_activation.is_undefined() {
            return true;
        }

        js_sys::Reflect::get(
            &user_activation,
            &wasm_bindgen::JsValue::from_str("isActive"),
        )
        .ok()
        .and_then(|active| active.as_bool())
        .unwrap_or(true)
    }

    /// Opens the default audio output when the platform permits it.
    ///
    /// Uses the system default. Browser targets defer opening the output until
    /// playback starts so it can happen in response to a user gesture.
    pub fn new() -> Self {
        let backend = Some(rodio::cpal::default_host().id());
        let device = backend.and_then(Self::default_device_for_backend);
        #[cfg(not(target_arch = "wasm32"))]
        let mixer_device_sink: Option<MixerDeviceSink> = Self::open_default_sink();
        #[cfg(target_arch = "wasm32")]
        let mixer_device_sink = None;

        Self {
            mixer_device_sink: Arc::new(Mutex::new(mixer_device_sink)),
            backend,
            device,
        }
    }

    /// Produces a list of hosts that are currently available on the system.
    ///
    /// The first call enumerates the system backends and caches the result.
    /// [`Output::refresh_available_backends`] to re-enumerate after the
    /// platform reports new hardware.
    pub fn available_backends() -> Vec<Backend> {
        let cache = CACHED_AUDIO_BACKENDS.get_or_init(|| Mutex::new(None));

        if let Some(backends) = cache.lock().unwrap().clone() {
            return backends;
        }

        Self::refresh_available_backends()
    }

    /// Re-enumerates the audio backends, updates the cache, and returns the fresh list.
    /// Also invalidates cached output devices.
    pub fn refresh_available_backends() -> Vec<Backend> {
        let backends = rodio::cpal::available_hosts();

        *CACHED_AUDIO_BACKENDS
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = Some(backends.clone());
        *CACHED_OUTPUT_DEVICES
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = None;

        backends
    }

    /// Produces a list of hosts that are currently available on the system.
    ///
    /// The first call enumerates the system backends and caches the result.
    /// [`Output::refresh_available_devices`] to re-enumerate after the
    /// platform reports new hardware.
    pub fn available_devices() -> Vec<Device> {
        let cache = CACHED_OUTPUT_DEVICES.get_or_init(|| Mutex::new(None));

        if let Some(devices) = cache.lock().unwrap().clone() {
            return devices;
        }

        Self::refresh_available_devices()
    }

    /// Re-enumerates the output devices of every current backend, updates the caches, and
    /// returns the fresh list.
    pub fn refresh_available_devices() -> Vec<Device> {
        let backends = rodio::cpal::available_hosts();
        let default_backend = rodio::cpal::default_host().id();
        let mut devices: Vec<Device> = backends
            .iter()
            .flat_map(|backend| Self::available_devices_for_backend(*backend).unwrap_or_default())
            .collect();

        devices.sort_by(|first, second| {
            second
                .is_default
                .cmp(&first.is_default)
                .then_with(|| {
                    (second.backend == default_backend).cmp(&(first.backend == default_backend))
                })
                .then_with(|| first.name().cmp(second.name()))
                .then_with(|| first.backend.name().cmp(second.backend.name()))
        });

        let mut seen = HashSet::new();
        devices.retain(|device| seen.insert(device.name().to_string()));

        devices.sort_by(|first, second| {
            second
                .is_default
                .cmp(&first.is_default)
                .then_with(|| first.name().cmp(second.name()))
        });

        *CACHED_AUDIO_BACKENDS
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = Some(backends);
        *CACHED_OUTPUT_DEVICES
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = Some(devices.clone());

        devices
    }

    /// Returns the canonical UI label for an audio backend.
    pub fn backend_label(backend: Backend) -> String {
        format!("{backend:?}")
    }

    /// Parses a backend label produced by [`Output::backend_label`].
    ///
    /// Matching is case-insensitive and ignores surrounding whitespace. Only backends
    /// reported by [`Output::available_backends`] are recognized.
    pub fn parse_backend_label(label: &str) -> Option<Backend> {
        Self::available_backends()
            .into_iter()
            .find(|backend| Self::backend_label(*backend).eq_ignore_ascii_case(label.trim()))
    }

    /// Returns the output devices currently available from a specific audio backend.
    ///
    /// Unlike [`Output::available_devices`], this always performs a live query
    /// and does not consult the cache.
    pub fn available_devices_for_backend(backend: Backend) -> Result<Vec<Device>, OutputError> {
        let host = rodio::cpal::host_from_id(backend)
            .map_err(|source| OutputError::BackendUnavailable { backend, source })?;
        let default_device = host.default_output_device();
        let default_id = default_device.as_ref().and_then(|device| device.id().ok());
        let default_name = default_device
            .as_ref()
            .and_then(|device| device.description().ok())
            .map(|description| description.name().to_string());
        let mut devices: Vec<Device> = host
            .output_devices()
            .map_err(|source| OutputError::ListDevices { backend, source })?
            .filter_map(|device| {
                let is_default = match (&default_id, device.id().ok()) {
                    (Some(default_id), Some(id)) => &id == default_id,
                    _ => device.description().is_ok_and(|description| {
                        default_name.as_deref() == Some(description.name())
                    }),
                };

                Device::from_cpal_device(backend, &device, is_default)
            })
            .collect();

        if let Some(default_device) = default_device
            && !devices.iter().any(|device| device.is_default)
            && let Some(default) = Device::from_cpal_device(backend, &default_device, true)
        {
            devices.push(default);
        }

        devices.sort_by(|first, second| {
            second
                .is_default
                .cmp(&first.is_default)
                .then_with(|| first.name().cmp(second.name()))
        });

        Ok(devices)
    }

    /// Opens a specific output device such as a speaker, headphone, or virtual device.
    ///
    /// Browser targets defer opening the output until playback starts so it can happen in
    /// response to a user gesture.
    pub fn try_new_with_device(device: &Device) -> Result<Self, OutputError> {
        #[cfg(not(target_arch = "wasm32"))]
        let mixer_device_sink = Some(Self::open_device_sink(device)?);

        #[cfg(target_arch = "wasm32")]
        let mixer_device_sink = {
            rodio::cpal::host_from_id(device.backend).map_err(|source| {
                OutputError::BackendUnavailable {
                    backend: device.backend,
                    source,
                }
            })?;
            None
        };

        Ok(Self {
            mixer_device_sink: Arc::new(Mutex::new(mixer_device_sink)),
            backend: Some(device.backend),
            device: Some(device.clone()),
        })
    }

    /// Opens the default output device provided by a specific audio backend.
    ///
    /// Browser targets defer opening the output until playback starts so it can happen in
    /// response to a user gesture.
    pub fn try_new_with_backend(backend: Backend) -> Result<Self, OutputError> {
        #[cfg(not(target_arch = "wasm32"))]
        let mixer_device_sink = Some(Self::open_backend_sink(backend)?);

        #[cfg(target_arch = "wasm32")]
        let mixer_device_sink = {
            rodio::cpal::host_from_id(backend)
                .map_err(|source| OutputError::BackendUnavailable { backend, source })?;
            None
        };

        Ok(Self {
            mixer_device_sink: Arc::new(Mutex::new(mixer_device_sink)),
            backend: Some(backend),
            device: Self::default_device_for_backend(backend),
        })
    }

    /// Creates an output for a backend preference without failing.
    pub(crate) fn new_with_preferred_backend(preferred: Option<Backend>) -> Self {
        let Some(backend) = preferred else {
            return Self::new();
        };

        match Self::try_new_with_backend(backend) {
            Ok(output) => output,
            Err(_) => Self::new_deferred(Some(backend)),
        }
    }

    /// Creates a deferred output for a backend preference without opening any device.
    pub fn new_deferred(preferred: Option<Backend>) -> Self {
        let backend = preferred.unwrap_or_else(|| rodio::cpal::default_host().id());

        Self {
            mixer_device_sink: Arc::new(Mutex::new(None)),
            backend: Some(backend),
            device: Self::default_device_for_backend(backend),
        }
    }

    /// Switches this output to a specific audio backend.
    ///
    /// Does nothing when already on that backend. Otherwise replaces this output
    /// with the selected backend; browser targets keep it deferred when called
    /// outside a user gesture. Outputs cloned before the switch keep using the
    /// previous sink.
    pub fn switch_backend(&mut self, backend: Backend) -> Result<(), OutputError> {
        if self.backend == Some(backend) {
            return Ok(());
        }

        let new = Self::try_new_with_backend(backend)?;
        let _ = new.retry_sink(); // try_new_with_backend defers opening the sink on wasm
        *self = new;
        Ok(())
    }

    /// Switches this output to a specific output device.
    ///
    /// Does nothing when already on that device. Otherwise replaces this output
    /// with the selected device; browser targets keep it deferred when called
    /// outside a user gesture. Outputs cloned before the switch keep using the
    /// previous sink. Switching devices also switches the output's backend.
    pub fn switch_device(&mut self, device: &Device) -> Result<(), OutputError> {
        if self.device.as_ref() == Some(device) {
            return Ok(());
        }

        let new = Self::try_new_with_device(device)?;
        let _ = new.retry_sink(); // try_new_with_backend defers opening the sink on wasm
        *self = new;
        Ok(())
    }

    /// Returns the selected audio backend, or `None` for an output created from a sink.
    pub fn backend(&self) -> Option<Backend> {
        self.backend
    }

    /// Returns the selected output device, if one was chosen or resolved.
    ///
    /// Returns `None` for an output created with [`Output::from_sink`] or when the
    /// platform did not report a device description.
    pub fn device(&self) -> Option<Device> {
        self.device.clone()
    }

    /// Returns a UI-friendly name for this output.
    ///
    /// Returns the backend label with the device name in parentheses when both are known
    /// (for example `"Alsa (Built-in Audio)"`), the backend label alone when no device was
    /// resolved, and `"Custom"` for an output created with [`Output::from_sink`].
    pub fn display_name(&self) -> String {
        let Some(backend) = self.backend else {
            return "Custom".to_string();
        };
        let label = Self::backend_label(backend);

        self.device.as_ref().map_or_else(
            || label.clone(),
            |device| {
                if device.name().is_empty() {
                    label.clone()
                } else {
                    format!("{label} ({})", device.name())
                }
            },
        )
    }

    /// Creates a shared output from an existing rodio device sink.
    pub fn from_sink(mixer_device_sink: MixerDeviceSink) -> Self {
        Self {
            mixer_device_sink: Arc::new(Mutex::new(Some(mixer_device_sink))),
            backend: None,
            device: None,
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn open_default_sink() -> Option<MixerDeviceSink> {
        let device = cpal::default_host().default_output_device()?;
        match Self::sink_builder(device).and_then(|builder| builder.open_sink_or_fallback()) {
            Ok(mut sink) => {
                sink.log_on_drop(false);
                Some(sink)
            }
            Err(_) => None,
        }
    }

    /// use this over [`DeviceSinkBuilder::from_device()`]
    fn sink_builder(device: cpal::Device) -> Result<DeviceSinkBuilder, DeviceSinkError> {
        // Rodio's DeviceSinkBuilder::open_default_sink() fixed 50 ms period can leave
        // plugin streams waiting forever to start.
        Ok(DeviceSinkBuilder::from_device(device)?.with_buffer_size(BufferSize::Default))
    }

    fn open_backend_sink(backend: Backend) -> Result<MixerDeviceSink, OutputError> {
        let host = rodio::cpal::host_from_id(backend)
            .map_err(|source| OutputError::BackendUnavailable { backend, source })?;
        let device = host
            .default_output_device()
            .or_else(|| host.output_devices().ok()?.next())
            .ok_or(OutputError::NoOutputDevice(backend))?;
        let mut sink = Self::sink_builder(device)
            .and_then(|builder| builder.open_sink_or_fallback())
            .map_err(|source| OutputError::OpenStream { backend, source })?;

        sink.log_on_drop(false);
        Ok(sink)
    }

    fn default_device_for_backend(backend: Backend) -> Option<Device> {
        let host = rodio::cpal::host_from_id(backend).ok()?;
        let device = host.default_output_device()?;

        Device::from_cpal_device(backend, &device, true)
    }

    fn find_backend_device(device: &Device) -> Result<rodio::cpal::Device, OutputError> {
        let host = rodio::cpal::host_from_id(device.backend).map_err(|source| {
            OutputError::BackendUnavailable {
                backend: device.backend,
                source,
            }
        })?;

        if let Some(id) = device.id.as_ref()
            && let Some(device) = host.device_by_id(id)
        {
            return Ok(device);
        }

        host.output_devices()
            .map_err(|source| OutputError::ListDevices {
                backend: device.backend,
                source,
            })?
            .find(|candidate| device.matches_cpal_device(candidate))
            .ok_or_else(|| OutputError::DeviceNotFound {
                device: Box::new(device.clone()),
            })
    }

    fn open_device_sink(device: &Device) -> Result<MixerDeviceSink, OutputError> {
        let backend_device = Self::find_backend_device(device)?;
        let mut sink = Self::sink_builder(backend_device)
            .and_then(|builder| builder.open_sink_or_fallback())
            .map_err(|source| OutputError::OpenDeviceStream {
                device: Box::new(device.clone()),
                source,
            })?;

        sink.log_on_drop(false);
        Ok(sink)
    }

    /// Retries opening this output's selected backend or device.
    ///
    /// Does nothing when the sink already exists or when the output was created with
    /// [`Output::from_sink`] without a backend or device.
    ///
    /// Browser targets return `OutputError::BrowserUserGestureRequired` outside an active user
    /// gesture and leave the output deferred for a later retry.
    pub fn retry_sink(&self) -> Result<(), OutputError> {
        #[cfg(all(target_arch = "wasm32", feature = "nightly"))]
        crate::patch::ensure_audioworklet_text_polyfill();

        let mut mixer_device_sink = self.mixer_device_sink.lock().unwrap();

        if mixer_device_sink.is_none() {
            #[cfg(target_arch = "wasm32")]
            if !Self::browser_has_active_user_gesture() {
                return Err(OutputError::BrowserUserGestureRequired);
            }

            let sink = match (self.device.clone(), self.backend) {
                (Some(device), _) => Self::open_device_sink(&device)?,
                (None, Some(backend)) => Self::open_backend_sink(backend)?,
                (None, None) => return Ok(()),
            };

            *mixer_device_sink = Some(sink);
        }

        Ok(())
    }

    /// Returns whether this output currently has an initialized device sink.
    ///
    /// Browser outputs return `false` until playback initializes WebAudio in response to a
    /// user gesture. Deferred outputs created with
    /// [`Output::new_deferred`] also return `false` until [`Output::retry_sink`] succeeds.
    pub fn has_output(&self) -> bool {
        self.mixer_device_sink.lock().unwrap().is_some()
    }

    pub(crate) fn mixer(&self) -> Option<rodio::mixer::Mixer> {
        self.mixer_device_sink
            .lock()
            .unwrap()
            .as_ref()
            .map(|mixer_device_sink| mixer_device_sink.mixer().clone())
    }

    /// Controls whether dropping the underlying device sink logs a message.
    pub fn log_on_drop(&self, log: bool) {
        if let Some(mixer_device_sink) = self.mixer_device_sink.lock().unwrap().as_mut() {
            mixer_device_sink.log_on_drop(log);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Output, switch_outputs_to_device};

    #[test]
    fn switching_many_outputs_to_the_current_device_is_a_noop() {
        let Some(device) = Output::new_deferred(None).device() else {
            eprintln!("Skipping batch assertions: no output device reported");
            return;
        };

        let mut third = Output::new_deferred(None);
        let mut fourth = Output::new_deferred(None);
        switch_outputs_to_device([&mut third, &mut fourth], &device)
            .expect("switching every output to its current device should succeed");
    }
}
