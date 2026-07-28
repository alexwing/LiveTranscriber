//! Nucleo de LiveTranscriber: motor, sesion, historial y configuracion.
//!
//! No depende de Tauri. Tanto la app de escritorio como `asr-cli` se montan
//! encima de esto, igual que `tapo-cli` y la app comparten `tapo-proto`.

pub mod config;
pub mod engine;
pub mod session;
pub mod sidecar;
pub mod speak;
pub mod transcript;
pub mod translate;

pub use config::{sanitize_file_stem, AppConfig, SpeakConfig};
pub use engine::{AsrEngine, AsrEvent, EngineError, EngineFactory};
pub use session::{Session, SessionConfig, SessionEvent};
pub use sidecar::{PythonSidecar, SidecarConfig};
pub use speak::{
    tts_lang_code, EchoRegistry, SpeechEvent, SpeechPump, SpeechPumpConfig, Synthesized,
    Synthesizer, SynthesizerFactory, TtsConfig, TtsSidecar,
};
pub use transcript::{Entry, Transcript};
pub use translate::{
    flores_code, MtConfig, MtSidecar, SentenceSplitter, TranslatedLine, TranslationPump,
    Translator, TranslatorFactory,
};

// Reexportado para que quien use asr-core no tenga que depender tambien de
// asr-audio solo para nombrar un dispositivo o una fuente.
pub use asr_audio::{list_devices, AudioDevice, CaptureTarget, DeviceKind, Source};
