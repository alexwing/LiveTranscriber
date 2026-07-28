//! La frontera entre "de donde sale el audio" y "quien lo convierte en texto".
//!
//! Hoy el unico motor es un sidecar de Python que corre el modelo con
//! transformers ([`crate::sidecar::PythonSidecar`]). Esa eleccion es un
//! compromiso: reutiliza codigo ya probado y funciona hoy, pero arrastra un
//! entorno de PyTorch de varios gigas.
//!
//! Todo lo que hay por encima (captura, gate, sesion, interfaz) habla solo con
//! este trait. Sustituirlo por un motor ONNX en Rust puro es implementarlo otra
//! vez y cambiar una linea donde se construye; nada mas se entera.

use std::sync::mpsc::Sender;

/// Lo que un motor comunica hacia arriba mientras trabaja.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AsrEvent {
    /// El motor termino de arrancar y ya acepta audio.
    Ready {
        device: String,
        latency_ms: u32,
        language: String,
    },
    /// Trozo de texto nuevo. Llegan continuamente, hay que ir concatenando.
    Delta { text: String },
    /// El segmento se cerro. Lo que venga despues empieza de cero.
    SegmentEnd,
    /// Algo fue mal, pero el motor sigue vivo.
    Error { message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("no se pudo arrancar el motor: {0}")]
    Spawn(String),

    #[error("el motor se cerro inesperadamente")]
    Closed,

    #[error("error de E/S hablando con el motor: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, EngineError>;

/// Un motor de reconocimiento de voz alimentado por streaming.
///
/// Se le entrega audio en 16 kHz mono f32 y va emitiendo [`AsrEvent`] por el
/// canal que se le paso al construirlo.
pub trait AsrEngine: Send {
    /// Entrega audio. Debe ser contiguo dentro de un segmento.
    fn feed(&mut self, samples: &[f32]) -> Result<()>;

    /// Cierra el segmento actual y deja el estado limpio para el siguiente.
    /// Se llama cuando el gate detecta un silencio largo.
    fn reset(&mut self) -> Result<()>;

    /// Termina el motor y libera sus recursos.
    fn shutdown(&mut self) -> Result<()>;
}

/// Como construir un motor. Existe para que quien orquesta no tenga que saber
/// que hay un Python detras.
pub trait EngineFactory: Send + Sync {
    fn build(&self, events: Sender<AsrEvent>) -> Result<Box<dyn AsrEngine>>;
}
