#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
#[cfg(target_arch = "wasm32")]
use std::sync::Mutex;
use std::{
    fmt::{Debug, Formatter},
    sync::Arc,
};

#[cfg(target_arch = "wasm32")]
use js_sys::Uint8Array;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::JsFuture;
#[cfg(target_arch = "wasm32")]
use web_sys::Response;

#[cfg(target_arch = "wasm32")]
use crate::SoundscapeError;

#[cfg(target_arch = "wasm32")]
fn browser_url_error(error: impl std::fmt::Debug) -> SoundscapeError {
    SoundscapeError::BrowserAsset(format!("{error:?}"))
}

/// Fetches a web resource from the browser and returns its bytes.
#[cfg(target_arch = "wasm32")]
pub async fn fetch_browser_url(url: &str) -> Result<Arc<[u8]>, SoundscapeError> {
    let window = web_sys::window().ok_or_else(|| {
        SoundscapeError::BrowserAsset("browser window is unavailable".to_string())
    })?;
    let response = JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(browser_url_error)?
        .dyn_into::<Response>()
        .map_err(browser_url_error)?;

    if !response.ok() {
        return Err(SoundscapeError::BrowserAsset(format!(
            "request returned HTTP status {} {}",
            response.status(),
            response.status_text()
        )));
    }

    let buffer = JsFuture::from(response.array_buffer().map_err(browser_url_error)?)
        .await
        .map_err(browser_url_error)?;
    let array = Uint8Array::new(&buffer);
    let mut bytes = Arc::<[u8]>::new_uninit_slice(array.length() as usize);
    array.copy_to_uninit(Arc::get_mut(&mut bytes).unwrap());

    Ok(unsafe { bytes.assume_init() })
}

/// An audio source assignment retained by a [`crate::Sound`].
#[derive(Clone, Default)]
pub enum SoundSource {
    /// No audio resource has been assigned yet.
    #[default]
    Empty,
    /// Encoded bytes with a static lifetime, such as bytes produced by `include_bytes!`.
    StaticBytes(&'static [u8]),
    /// Encoded bytes in shared heap storage.
    SharedBytes(Arc<[u8]>),
    /// A native filesystem path.
    #[cfg(not(target_arch = "wasm32"))]
    File(PathBuf),
    /// A browser URL fetched and cached when loaded.
    #[cfg(target_arch = "wasm32")]
    #[doc(hidden)]
    Url(String, Arc<Mutex<Option<Arc<[u8]>>>>),
}

impl Debug for SoundSource {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("Empty"),
            Self::StaticBytes(bytes) => formatter
                .debug_tuple("StaticBytes")
                .field(&bytes.len())
                .finish(),
            Self::SharedBytes(bytes) => formatter
                .debug_tuple("SharedBytes")
                .field(&bytes.len())
                .finish(),
            #[cfg(not(target_arch = "wasm32"))]
            Self::File(path) => formatter.debug_tuple("File").field(path).finish(),
            #[cfg(target_arch = "wasm32")]
            Self::Url(url, _) => formatter.debug_tuple("Url").field(url).finish(),
        }
    }
}

impl PartialEq for SoundSource {
    fn eq(&self, other: &Self) -> bool {
        self.same_resource(other)
    }
}

impl SoundSource {
    pub(crate) fn same_resource(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Empty, Self::Empty) => true,
            (Self::StaticBytes(left), Self::StaticBytes(right)) => std::ptr::eq(*left, *right),
            (Self::SharedBytes(left), Self::SharedBytes(right)) => Arc::ptr_eq(left, right),
            #[cfg(not(target_arch = "wasm32"))]
            (Self::File(left), Self::File(right)) => left == right,
            #[cfg(target_arch = "wasm32")]
            (Self::Url(left, _), Self::Url(right, _)) => left == right,
            _ => false,
        }
    }

    /// Creates an empty source that can be replaced later with [`crate::Sound::set_source`].
    pub fn empty() -> Self {
        Self::Empty
    }

    /// Creates a source from encoded bytes with a static lifetime.
    pub fn static_bytes(bytes: &'static [u8]) -> Self {
        Self::StaticBytes(bytes)
    }

    /// Copies encoded bytes into shared storage.
    pub fn shared_bytes(bytes: impl AsRef<[u8]>) -> Self {
        Self::SharedBytes(Arc::from(bytes.as_ref()))
    }

    /// Creates a source from an existing shared byte allocation.
    pub fn shared_arc_bytes(bytes: Arc<[u8]>) -> Self {
        Self::SharedBytes(bytes)
    }

    /// Creates a source from a native filesystem path.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self::File(path.into())
    }

    /// Creates a source fetched from a browser URL.
    #[cfg(target_arch = "wasm32")]
    pub fn url(url: impl Into<String>) -> Self {
        Self::Url(url.into(), Arc::new(Mutex::new(None)))
    }

    /// Clears bytes cached after loading this URL in a browser.
    ///
    /// Clones of this source share the same cache. Active sounds may retain their own decoder
    /// references until playback is stopped and released.
    pub fn clear_browser_cache(&self) {
        #[cfg(target_arch = "wasm32")]
        if let Self::Url(_, bytes) = self {
            bytes.lock().unwrap().take();
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn cached_browser_bytes(&self) -> Option<Arc<[u8]>> {
        match self {
            Self::Url(_, bytes) => bytes.lock().unwrap().clone(),
            _ => None,
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) async fn load_browser_bytes(&self) -> Result<Arc<[u8]>, SoundscapeError> {
        let Self::Url(url, cached_bytes) = self else {
            return Err(SoundscapeError::NoAudioSource);
        };
        if let Some(bytes) = cached_bytes.lock().unwrap().clone() {
            return Ok(bytes);
        }

        let bytes = fetch_browser_url(url).await?;
        let mut cached_bytes = cached_bytes.lock().unwrap();
        if let Some(existing) = cached_bytes.as_ref() {
            return Ok(existing.clone());
        }
        *cached_bytes = Some(bytes.clone());
        Ok(bytes)
    }

    /// Returns the byte length of this source.
    pub fn len(&self) -> Option<u64> {
        match self {
            Self::Empty => Some(0),
            Self::StaticBytes(bytes) => Some(bytes.len() as u64),
            Self::SharedBytes(bytes) => Some(bytes.len() as u64),
            #[cfg(not(target_arch = "wasm32"))]
            Self::File(path) => std::fs::metadata(path).ok().map(|metadata| metadata.len()),
            #[cfg(target_arch = "wasm32")]
            Self::Url(_, bytes) => bytes
                .lock()
                .unwrap()
                .as_ref()
                .map(|bytes| bytes.len() as u64),
        }
    }

    /// Returns `true` if this is [`SoundSource::Empty`].
    ///
    /// Typically happens when a [`crate::Sound`] is created with a placeholder source,
    /// and the actual audio resource has not been assigned yet.
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }
}

impl From<&'static [u8]> for SoundSource {
    fn from(bytes: &'static [u8]) -> Self {
        Self::static_bytes(bytes)
    }
}

impl From<Arc<[u8]>> for SoundSource {
    fn from(bytes: Arc<[u8]>) -> Self {
        Self::shared_arc_bytes(bytes)
    }
}

impl From<Vec<u8>> for SoundSource {
    fn from(bytes: Vec<u8>) -> Self {
        Self::SharedBytes(bytes.into())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl From<PathBuf> for SoundSource {
    fn from(path: PathBuf) -> Self {
        Self::File(path)
    }
}

/// Selects a native and WebAssembly source at compile time.
///
/// Choose explicitly on both targets: native supports `bytes(expression)` or `file(path)`, while
/// WASM supports `bytes(expression)` or `url(expression)`. The non-selected expression is
/// cfg-gated out, so native-only files and WASM-only embedded bytes need not exist on other targets.
///
/// ```rust,no_run
/// # use euphorium::{Soundscape, SoundscapeError};
/// # fn example() -> Result<(), SoundscapeError> {
/// let soundscape = Soundscape::new();
/// let source = euphorium::audio_source! {
///     native: bytes(include_bytes!("../examples/music/THE UNFORGIVING.mp3")),
///     wasm: url("/audio/theme.mp3"),
/// };
/// let theme = soundscape.create_sound("theme", source)?;
/// theme.play()?;
/// # Ok(())
/// # }
/// ```
///
/// Use `wasm: bytes(include_bytes!(...))` when you deliberately want audio embedded in the WASM
/// binary instead of fetched from a URL.
#[macro_export]
macro_rules! audio_source {
    (native: file($path:expr), wasm: bytes($wasm_bytes:expr) $(,)?) => {{
        #[cfg(target_arch = "wasm32")]
        let source = $crate::SoundSource::static_bytes($wasm_bytes);
        #[cfg(not(target_arch = "wasm32"))]
        let source = $crate::SoundSource::file($path);
        source
    }};
    (native: bytes($native_bytes:expr), wasm: bytes($wasm_bytes:expr) $(,)?) => {{
        #[cfg(target_arch = "wasm32")]
        let source = $crate::SoundSource::static_bytes($wasm_bytes);
        #[cfg(not(target_arch = "wasm32"))]
        let source = $crate::SoundSource::static_bytes($native_bytes);
        source
    }};
    (native: file($path:expr), wasm: url($url:expr) $(,)?) => {{
        #[cfg(target_arch = "wasm32")]
        let source = $crate::SoundSource::url($url);
        #[cfg(not(target_arch = "wasm32"))]
        let source = $crate::SoundSource::file($path);
        source
    }};
    (native: bytes($native_bytes:expr), wasm: url($url:expr) $(,)?) => {{
        #[cfg(target_arch = "wasm32")]
        let source = $crate::SoundSource::url($url);
        #[cfg(not(target_arch = "wasm32"))]
        let source = $crate::SoundSource::static_bytes($native_bytes);
        source
    }};
}
