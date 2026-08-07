//! Nucleo de LiveTranscriber: motor, sesion, historial y configuracion.
//!
//! No depende de Tauri. Tanto la app de escritorio como `asr-cli` se montan
//! encima de esto, igual que `tapo-cli` y la app comparten `tapo-proto`.

pub mod config;
pub mod engine;
pub mod profiles;
pub mod session;
pub mod sidecar;
pub mod speak;
pub mod transcript;
pub mod translate;

/// Prepara el lanzamiento de un sidecar de Python **sin ventana de consola**.
///
/// Sin esto, la app instalada abre una ventana negra por cada sidecar (tres:
/// ASR, traduccion y voz). En desarrollo no se ve porque ya hay una terminal,
/// asi que es un fallo que solo aparece en la version compilada.
///
/// Vive aqui y no en cada modulo para que el proximo sidecar no vuelva a
/// olvidarlo.
pub(crate) fn no_console(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: el proceso hijo no recibe consola. Sus stdout y
        // stderr siguen redirigidos por las tuberias, que es de donde
        // leemos: no se pierde ni una linea de log.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

pub use config::{
    config_location, migrate_legacy_config, sanitize_file_stem, AppConfig, SpeakConfig,
    CONFIG_ENV, CONFIG_FILE,
};
pub use engine::{AsrEngine, AsrEvent, EngineError, EngineFactory};
pub use profiles::{
    profiles_path, AppliedProfile, DeviceFallback, DeviceIds, Profile, ProfileStore,
};
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
