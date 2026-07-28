//! Captura de audio para reconocimiento de voz en Windows.
//!
//! Entrega siempre **16 kHz mono f32**, que es lo que espera el modelo. La
//! conversion desde el formato nativo del dispositivo (tipicamente 48 kHz
//! estereo) la hace el propio motor de audio de Windows: al inicializar el
//! cliente en modo compartido con `autoconvert`, WASAPI activa
//! `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | SRC_DEFAULT_QUALITY` y nos da
//! directamente el formato pedido. Para voz a 16 kHz su calidad sobra, y nos
//! ahorra el downmix y el remuestreo a mano.
//!
//! Tres modos de captura, todos por la misma tuberia:
//!
//! - [`CaptureTarget::Loopback`]: todo lo que suena en un dispositivo de
//!   salida. Es la via para transcribir una pelicula o una llamada de Teams
//!   sin que la app de origen se entere.
//! - [`CaptureTarget::Microphone`]: entrada normal.
//! - [`CaptureTarget::Process`]: loopback de *un solo proceso*. Captura Teams
//!   y nada mas, sin colarse la musica que suene a la vez.

mod capture;
mod device;
mod gate;
mod normalize;

pub use capture::{spawn_capture, CaptureTarget};
pub use device::{list_devices, AudioDevice, DeviceKind};
pub use gate::{rms, GateEvent, SilenceGate};
pub use normalize::Normalizer;

/// Frecuencia de muestreo que exige el modelo.
pub const TARGET_RATE: u32 = 16_000;

/// Canales que exige el modelo.
pub const TARGET_CHANNELS: u16 = 1;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[cfg(windows)]
    #[error("WASAPI: {0}")]
    Wasapi(#[from] wasapi::WasapiError),

    #[error("no se pudo inicializar COM en el hilo de captura: {0}")]
    Com(String),

    #[error("no existe ningun dispositivo con id {0}")]
    DeviceNotFound(String),

    #[error("no se pudo lanzar el hilo de captura: {0}")]
    Thread(String),

    #[error("la captura solo esta implementada en Windows")]
    UnsupportedPlatform,
}

pub type Result<T> = std::result::Result<T, AudioError>;

/// De donde viene un fragmento de audio. Se arrastra hasta la transcripcion
/// para poder distinguir "lo que dijo el otro" de "lo que dije yo".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Audio del sistema (loopback).
    System,
    /// Microfono.
    Mic,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::System => "system",
            Source::Mic => "mic",
        }
    }
}
