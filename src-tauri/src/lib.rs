//! App de escritorio. Sigue la forma de TapoController: `AppState` gestionado
//! por Tauri, comandos `#[tauri::command]` que lo reciben por `State`, y los
//! workers de verdad viviendo en hilos con nombre fuera del runtime.
//!
//! Aqui no hay logica de audio ni de modelo: todo eso esta en `asr-audio` y
//! `asr-core`. Esta capa solo traduce entre esos crates y la interfaz.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use asr_audio::{AudioDevice, CaptureTarget, DeviceKind, Source};
use asr_core::speak::{EchoRegistry, SpeechEvent, SpeechPump, SpeechPumpConfig, TtsSidecar};
use asr_core::translate::{MtSidecar, TranslatedLine, TranslationPump};
use asr_core::{AppConfig, Session, SessionConfig, SessionEvent, Transcript};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_global_shortcut::GlobalShortcutExt;

const CONFIG_FILE: &str = "transcriber-config.toml";

/// Error que viaja al frontend como `{ message }`, igual que en TapoController.
#[derive(Debug, serde::Serialize)]
pub struct CmdError {
    pub message: String,
}

macro_rules! from_display {
    ($($t:ty),* $(,)?) => {$(
        impl From<$t> for CmdError {
            fn from(e: $t) -> Self {
                Self { message: e.to_string() }
            }
        }
    )*};
}

from_display!(
    anyhow::Error,
    asr_core::EngineError,
    asr_audio::AudioError,
    std::io::Error,
    tauri::Error,
);

impl From<String> for CmdError {
    fn from(message: String) -> Self {
        Self { message }
    }
}

pub type CmdResult<T> = Result<T, CmdError>;

// ------------------------------------------------------- rutas relativas
//
// La configuracion admite rutas relativas (`sidecar/asr_server.py`), pero el
// directorio de trabajo no es de fiar: `tauri dev` lanza el binario desde
// `src-tauri/`, un .exe instalado desde donde le de la gana al usuario, y un
// acceso directo desde otro sitio distinto. Asi que se busca en varias bases
// en vez de asumir una.

/// Sitios donde buscar, en orden de preferencia. `app` es opcional porque el
/// estado se construye antes de que exista el handle.
fn search_bases(app: Option<&AppHandle>) -> Vec<PathBuf> {
    let mut bases = Vec::new();

    if let Ok(cwd) = std::env::current_dir() {
        bases.push(cwd);
    }

    // En desarrollo el binario vive en `target/debug`, asi que la raiz del
    // proyecto esta unos niveles por encima.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let mut candidate = dir.to_path_buf();
            for _ in 0..4 {
                bases.push(candidate.clone());
                if !candidate.pop() {
                    break;
                }
            }
        }
    }

    // En una app empaquetada el sidecar viaja como recurso del bundle.
    if let Some(app) = app {
        if let Ok(resources) = app.path().resource_dir() {
            bases.push(resources);
        }
    }

    bases.dedup();
    bases
}

/// Marca la raiz del proyecto. Sirve como marcador porque, al reves que el
/// sidecar, Tauri no lo copia a ningun sitio: el sidecar esta declarado como
/// recurso, asi que en dev aparece tambien en `target/debug/sidecar/` y no
/// distingue la raiz de verdad.
const ROOT_MARKER: &str = "transcriber-config.example.toml";

/// Comprueba que se puede escribir en un directorio, intentandolo de verdad.
///
/// Mirar permisos en Windows es un lio (ACL, virtualizacion de carpetas), asi
/// que sale mas barato y mas fiable crear un fichero temporal y borrarlo.
fn is_writable(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let probe = dir.join(".livetranscriber-write-test");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Donde vive `transcriber-config.toml`.
///
/// Por orden: uno que ya exista; la raiz del proyecto si estamos en el repo
/// (para que `tauri dev` no lo escriba dentro de `src-tauri`); el directorio del
/// ejecutable **si se puede escribir en el**; y si no, `%APPDATA%`.
///
/// Lo ultimo no es un adorno: instalada con el MSI, la app vive en
/// `Program Files`, donde un usuario sin permisos de administrador no puede
/// escribir. Sin esta salida, guardar la configuracion fallaria y cada cambio
/// hecho en la interfaz se perderia al cerrar.
fn config_location() -> PathBuf {
    let bases = search_bases(None);

    if let Some(existing) = bases
        .iter()
        .map(|base| base.join(CONFIG_FILE))
        .find(|candidate| candidate.exists())
    {
        return existing;
    }

    if let Some(root) = bases.iter().find(|base| base.join(ROOT_MARKER).exists()) {
        return root.join(CONFIG_FILE);
    }

    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        if is_writable(&exe_dir) {
            return exe_dir.join(CONFIG_FILE);
        }
        tracing::info!(
            "no se puede escribir en {}, la configuracion va a APPDATA",
            exe_dir.display()
        );
    }

    let appdata = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("LiveTranscriber");
    let _ = std::fs::create_dir_all(&appdata);
    appdata.join(CONFIG_FILE)
}

/// Primera ubicacion existente para `path`, o `None`.
fn resolve_existing(app: Option<&AppHandle>, path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        return path.exists().then(|| path.to_path_buf());
    }
    search_bases(app)
        .into_iter()
        .map(|base| base.join(path))
        .find(|candidate| candidate.exists())
}

/// Igual que [`resolve_existing`] pero con un error que dice donde se ha
/// mirado, que es la diferencia entre un fallo diagnosticable y uno que no.
fn require_existing(app: &AppHandle, path: &Path, what: &str) -> Result<PathBuf, CmdError> {
    if let Some(found) = resolve_existing(Some(app), path) {
        return Ok(found);
    }
    if path.is_absolute() {
        return Err(format!("no encuentro {what} en {}", path.display()).into());
    }
    let tried: Vec<String> = search_bases(Some(app))
        .into_iter()
        .map(|base| format!("  {}", base.join(path).display()))
        .collect();
    Err(format!(
        "no encuentro {what} ({}). He mirado en:\n{}",
        path.display(),
        tried.join("\n")
    )
    .into())
}

pub struct AppState {
    config: Mutex<AppConfig>,
    config_path: PathBuf,
    /// Una sesion por fuente activa (sistema y/o microfono).
    sessions: Mutex<Vec<Session>>,
    transcript: Arc<Mutex<Transcript>>,
    running: AtomicBool,
    /// Asidero para callar la voz sintetica al parar. Sin el, "Parar"
    /// dejaria a la voz terminando su cola varios segundos, y un
    /// parar-y-arrancar rapido superpondria dos sintetizadores en VRAM y
    /// dos flujos sobre el mismo cable.
    speech_stop: Mutex<Option<Arc<AtomicBool>>>,
}

impl AppState {
    fn new(config_path: PathBuf) -> Self {
        let config = AppConfig::load(&config_path).unwrap_or_else(|e| {
            tracing::warn!("no se pudo leer la configuracion ({e}), usando la de por defecto");
            AppConfig::default()
        });
        Self {
            config: Mutex::new(config),
            config_path,
            sessions: Mutex::new(Vec::new()),
            transcript: Arc::new(Mutex::new(Transcript::new())),
            running: AtomicBool::new(false),
            speech_stop: Mutex::new(None),
        }
    }
}

// ---------------------------------------------------------------- comandos

#[tauri::command]
fn get_config(state: tauri::State<'_, AppState>) -> CmdResult<AppConfig> {
    // A nivel debug: la interfaz lo pide en cada montaje y en dev React lo
    // duplica, asi que a nivel info solo seria ruido.
    tracing::debug!("get_config");
    Ok(state.config.lock().unwrap().clone())
}

#[tauri::command]
fn save_config(state: tauri::State<'_, AppState>, new: AppConfig) -> CmdResult<()> {
    new.save(&state.config_path)?;
    *state.config.lock().unwrap() = new;
    Ok(())
}

#[tauri::command]
fn list_devices(kind: DeviceKind) -> CmdResult<Vec<AudioDevice>> {
    Ok(asr_audio::list_devices(kind)?)
}

#[tauri::command]
fn is_running(state: tauri::State<'_, AppState>) -> bool {
    tracing::debug!("is_running");
    state.running.load(Ordering::Relaxed)
}

#[tauri::command]
fn start_transcription(app: AppHandle, state: tauri::State<'_, AppState>) -> CmdResult<()> {
    tracing::info!("arranque pedido desde la interfaz");
    start_internal(&app, &state)
}

#[tauri::command]
fn stop_transcription(app: AppHandle, state: tauri::State<'_, AppState>) -> CmdResult<()> {
    stop_internal(&app, &state);
    Ok(())
}

#[tauri::command]
fn get_transcript(state: tauri::State<'_, AppState>) -> CmdResult<Vec<asr_core::Entry>> {
    Ok(state.transcript.lock().unwrap().entries().to_vec())
}

#[tauri::command]
fn get_translations(state: tauri::State<'_, AppState>) -> CmdResult<Vec<TranslatedLine>> {
    Ok(state.transcript.lock().unwrap().translations().to_vec())
}

/// Texto listo para el portapapeles. `what`: "original", "translated" o "both".
#[tauri::command]
fn transcript_as_text(state: tauri::State<'_, AppState>, what: String) -> CmdResult<String> {
    let transcript = state.transcript.lock().unwrap();
    Ok(match what.as_str() {
        "translated" => transcript.translated_text(),
        "both" => transcript.to_bilingual_text(),
        _ => transcript.plain_text(),
    })
}

#[tauri::command]
fn clear_transcript(state: tauri::State<'_, AppState>) -> CmdResult<()> {
    state.transcript.lock().unwrap().clear();
    Ok(())
}

/// Carpeta de salida efectiva, en absoluto. La interfaz la muestra tal cual
/// para que nunca haya duda de donde acaban los ficheros.
#[tauri::command]
fn output_dir(state: tauri::State<'_, AppState>) -> CmdResult<PathBuf> {
    Ok(state.config.lock().unwrap().output_dir_absolute())
}

/// Abre el selector de carpeta y, si el usuario elige una, la guarda.
///
/// Es `async` a proposito: `blocking_pick_folder` no puede correr en el hilo
/// principal (bloquearia el bucle de eventos que el propio dialogo necesita),
/// y los comandos async de Tauri van fuera de el.
#[tauri::command]
async fn pick_output_dir(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> CmdResult<Option<PathBuf>> {
    let start = state.config.lock().unwrap().output_dir_absolute();

    let handle = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        handle
            .dialog()
            .file()
            .set_title("Carpeta para las transcripciones")
            .set_directory(&start)
            .blocking_pick_folder()
    })
    .await
    .map_err(|e| CmdError {
        message: format!("el selector de carpeta fallo: {e}"),
    })?;

    let Some(picked) = picked else {
        return Ok(None); // el usuario cancelo
    };
    // `simplified` deja rutas de Windows normales en vez de UNC.
    let chosen = picked.simplified().into_path().map_err(|e| CmdError {
        message: format!("ruta no utilizable: {e}"),
    })?;

    std::fs::create_dir_all(&chosen)?;
    let mut config = state.config.lock().unwrap();
    config.output_dir = chosen.clone();
    config.save(&state.config_path)?;
    Ok(Some(chosen))
}

/// Abre la carpeta de salida en el explorador.
#[tauri::command]
fn reveal_output_dir(state: tauri::State<'_, AppState>) -> CmdResult<()> {
    let dir = state.config.lock().unwrap().ensure_output_dir()?;
    std::process::Command::new("explorer")
        .arg(&dir)
        .spawn()
        .map_err(|e| CmdError {
            message: format!("no se pudo abrir {}: {e}", dir.display()),
        })?;
    Ok(())
}

/// Nombre que tendria el fichero si se exportara ahora, para poder mostrarlo.
#[tauri::command]
fn output_filename_preview(
    state: tauri::State<'_, AppState>,
    format: String,
) -> CmdResult<String> {
    Ok(state.config.lock().unwrap().output_filename(&format))
}

/// Vuelca la transcripcion en la carpeta de salida.
///
/// La interfaz solo dice el formato: la carpeta, el nombre y la fecha los pone
/// la configuracion, y de la unicidad se encarga `next_output_path`.
#[tauri::command]
fn export_transcript(state: tauri::State<'_, AppState>, format: String) -> CmdResult<PathBuf> {
    let path = state.config.lock().unwrap().next_output_path(&format)?;

    let transcript = state.transcript.lock().unwrap();
    if transcript.entries().is_empty() && transcript.translations().is_empty() {
        return Err("no hay nada que exportar todavia".to_string().into());
    }
    match format.as_str() {
        "srt" => transcript.save_srt(&path)?,
        "translated-srt" => std::fs::write(&path, transcript.to_translated_srt())?,
        "bilingual" => std::fs::write(&path, transcript.to_bilingual_text())?,
        _ => transcript.save_text(&path)?,
    }
    Ok(path)
}

#[tauri::command]
fn toggle_overlay(app: AppHandle) -> CmdResult<bool> {
    let Some(window) = app.get_webview_window("overlay") else {
        return Err("no existe la ventana de overlay".to_string().into());
    };
    let visible = window.is_visible().unwrap_or(false);
    if visible {
        window.hide()?;
    } else {
        window.show()?;
        let _ = window.set_always_on_top(true);
    }
    Ok(!visible)
}

// ------------------------------------------------------- arranque y parada

fn start_internal(app: &AppHandle, state: &AppState) -> CmdResult<()> {
    if state.running.swap(true, Ordering::SeqCst) {
        return Ok(()); // ya estaba en marcha
    }

    let config = state.config.lock().unwrap().clone();
    let mut sidecar = config.sidecar();

    // Si algo falla aqui hay que soltar el flag, o la app se queda creyendo
    // que esta transcribiendo y el boton no vuelve.
    let resolved = require_existing(app, &sidecar.script, "el sidecar")
        .and_then(|script| {
            sidecar.script = script;
            require_existing(app, &sidecar.python, "el interprete de Python")
        });
    match resolved {
        Ok(python) => sidecar.python = python,
        Err(e) => {
            state.running.store(false, Ordering::SeqCst);
            return Err(e);
        }
    }

    let mut wanted: Vec<(Source, CaptureTarget)> = Vec::new();
    if config.capture_system {
        wanted.push((
            Source::System,
            CaptureTarget::Loopback {
                device_id: config.system_device_id.clone(),
            },
        ));
    }
    if config.capture_mic {
        wanted.push((
            Source::Mic,
            CaptureTarget::Microphone {
                device_id: config.mic_device_id.clone(),
            },
        ));
    }
    if wanted.is_empty() {
        state.running.store(false, Ordering::SeqCst);
        return Err("no hay ninguna fuente activada".to_string().into());
    }

    // Las validaciones de idioma van ANTES de montar nada, para que el error
    // sea siempre el claro: sin esto, con el idioma en "auto" y la voz
    // activada el primero en quejarse era el sintetizador, con un mensaje
    // que no apuntaba a la causa.
    if config.translate {
        if config.language == "auto" {
            state.running.store(false, Ordering::SeqCst);
            return Err("Para traducir hay que elegir un idioma concreto para la \
                        sala en vez de la deteccion automatica: el traductor \
                        necesita saber desde que idioma parte."
                .to_string()
                .into());
        }
        // El idioma del micro pasa a ser idioma de transcripcion, no solo de
        // traduccion: si el TOML trae uno que el modelo no transcribe, el
        // arranque pasaria y cada frase del micro moriria una a una.
        let mic_lang = config.mic_asr_language();
        if config.capture_mic && asr_core::flores_code(&mic_lang).is_none() {
            state.running.store(false, Ordering::SeqCst);
            return Err(format!(
                "el idioma del microfono ({mic_lang}) no es un locale valido: \
                 eligelo en la configuracion"
            )
            .into());
        }
    }

    if config.speak.enabled && (!config.translate || !config.capture_mic) {
        state.running.store(false, Ordering::SeqCst);
        return Err("Hablar con tu voz necesita 'Traducir en paralelo' y \
                    'Transcribir el microfono' activados: lo que se habla \
                    es la traduccion de lo que dices por el micro."
            .to_string()
            .into());
    }

    // Los dos modelos pesados se cargan A LA VEZ, cada uno en su hilo. Son
    // procesos independientes y cargarlos en serie sumaba las dos esperas:
    // medido aqui, 46 s la voz + 31 s el traductor = 77 s con la interfaz
    // muda, que se siente como que la app se ha colgado. En paralelo el
    // arranque cuesta lo que tarde el mas lento.
    //
    // Cada etapa avisa por `loading` segun termina, para que la ventana
    // cuente lo que esta pasando en vez de quedarse en "Arrancando...".
    let _ = app.emit(
        "loading",
        serde_json::json!({ "stage": "start", "message": "Cargando modelos…" }),
    );

    let speech_handle = if config.speak.enabled {
        let app2 = app.clone();
        let cfg2 = config.clone();
        Some(std::thread::spawn(move || start_speech(&app2, &cfg2)))
    } else {
        None
    };

    let translator_handle = if config.translate {
        let app2 = app.clone();
        let cfg2 = config.clone();
        Some(std::thread::spawn(move || start_translator(&app2, &cfg2)))
    } else {
        None
    };

    /// Recoge un hilo de arranque, convirtiendo el panico en un error legible
    /// en vez de dejar la app creyendo que sigue viva.
    fn join_stage<T>(
        handle: Option<std::thread::JoinHandle<Result<T, CmdError>>>,
        que: &str,
    ) -> Result<Option<T>, CmdError> {
        match handle {
            None => Ok(None),
            Some(h) => match h.join() {
                Ok(Ok(value)) => Ok(Some(value)),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(format!("el arranque de {que} entro en panico").into()),
            },
        }
    }

    // Se recogen los dos SIEMPRE, aunque el primero falle: si no, el hilo
    // superviviente dejaria un sidecar de Python vivo agarrado a la VRAM.
    let speech_result = join_stage(speech_handle, "la voz");
    let translator_result = join_stage(translator_handle, "el traductor");

    let speech = match speech_result {
        Ok(s) => s,
        Err(e) => {
            // El traductor que si arranco se cierra al soltar su sidecar.
            drop(translator_result);
            state.running.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };
    let translator = match translator_result {
        Ok(t) => t,
        Err(e) => {
            drop(speech);
            state.running.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };

    if let Some(started) = &speech {
        *state.speech_stop.lock().unwrap() = Some(started.stop.clone());
    }

    let mut translation_tx = None;
    if let Some(translator) = translator {
        match start_translation(app, &config, state.transcript.clone(), speech, translator) {
            Ok(sender) => translation_tx = Some(sender),
            Err(e) => {
                state.running.store(false, Ordering::SeqCst);
                return Err(e);
            }
        }
    }

    let (tx, rx) = std::sync::mpsc::channel::<SessionEvent>();
    let mut started = Vec::new();
    for (source, target) in wanted {
        let session_cfg = SessionConfig {
            target,
            source,
            gate_drop_db: config.gate_drop_db,
            gate_floor_dbfs: config.gate_floor_dbfs,
            gate_hold_secs: config.gate_hold_secs,
            paragraph_idle_secs: config.paragraph_idle_secs,
            paragraph_max_secs: config.paragraph_max_secs,
            normalize_gain: config.normalize_gain,
        };
        // Cada fuente transcribe en su idioma: la sala en el suyo y el
        // microfono en el mio (yo sigo hablando espanol aunque la sala vaya
        // en ingles). Es lo que permite que cada una se traduzca luego en su
        // sentido.
        let mut source_sidecar = sidecar.clone();
        if source == Source::Mic {
            source_sidecar.language = config.mic_asr_language();
        }
        match Session::start(session_cfg, &source_sidecar, tx.clone()) {
            Ok(session) => started.push(session),
            Err(e) => {
                // Si una fuente falla, no dejamos la otra a medias.
                for session in started {
                    session.join();
                }
                state.running.store(false, Ordering::SeqCst);
                return Err(e.into());
            }
        }
    }
    drop(tx); // el canal muere cuando mueran todas las sesiones

    *state.sessions.lock().unwrap() = started;
    spawn_event_pump(app.clone(), state.transcript.clone(), rx, translation_tx);
    let _ = app.emit("running-changed", true);
    Ok(())
}

/// Lo que el hilo de traduccion necesita saber de la voz sintetica: por donde
/// mandarle los textos, el registro para reconocer el eco de la propia voz, y
/// el asidero de parada que se guarda en el estado para que "Parar" calle.
struct SpeechWiring {
    texts: std::sync::mpsc::Sender<String>,
    echo: Option<Arc<EchoRegistry>>,
    stop: Arc<AtomicBool>,
}

/// Carga NLLB y espera a que este listo. Es la mitad cara del arranque del
/// traductor, y va aparte para poder correrla a la vez que la de la voz.
fn start_translator(app: &AppHandle, config: &AppConfig) -> Result<MtSidecar, CmdError> {
    let mut mt = config.mt();
    mt.script = require_existing(app, &mt.script, "el sidecar de traduccion")?;
    mt.python = require_existing(app, &mt.python, "el interprete de Python")?;

    let sidecar = MtSidecar::spawn(&mt)?;
    // Cargar NLLB lleva medio minuto la primera vez; sin esperar aqui, la
    // primera frase se comeria ese tiempo como si fuera latencia.
    let device = sidecar.wait_ready(std::time::Duration::from_secs(180))?;
    tracing::info!("traductor listo en {device}");
    let _ = app.emit(
        "translator-ready",
        serde_json::json!({ "device": device, "target": config.target_language }),
    );
    let _ = app.emit(
        "loading",
        serde_json::json!({ "stage": "translator", "message": "Traductor listo" }),
    );
    Ok(sidecar)
}

/// Cablea el traductor ya cargado con su hilo. Devuelve por donde mandarle
/// los eventos.
///
/// Va en un hilo aparte porque traducir bloquea ~160 ms por frase, y hacerlo
/// en la bomba de eventos frenaria el texto en vivo y el vumetro.
fn start_translation(
    app: &AppHandle,
    config: &AppConfig,
    transcript: Arc<Mutex<Transcript>>,
    speech: Option<SpeechWiring>,
    sidecar: MtSidecar,
) -> Result<std::sync::mpsc::Sender<SessionEvent>, CmdError> {
    let mut pump = TranslationPump::new(
        Box::new(sidecar),
        (&config.language, &config.target_language),
        (&config.mic_asr_language(), &config.voice_language()),
    )?;
    if let Some(echo) = speech.as_ref().and_then(|s| s.echo.clone()) {
        pump = pump.with_echo_registry(echo);
    }

    let (tx, rx) = std::sync::mpsc::channel::<SessionEvent>();
    let app = app.clone();
    std::thread::Builder::new()
        .name("asr-translate".into())
        .spawn(move || {
            for event in rx {
                let lines = pump.handle(&event);
                if pump.is_dead() {
                    // Sin esto la bomba quedaria zombi: cada frase perdida
                    // con un warn en el log, para siempre, con la interfaz
                    // diciendo que todo va bien.
                    let _ = app.emit(
                        "error",
                        "El traductor se ha muerto: no se va a traducir ni hablar \
                         nada mas. Para y vuelve a arrancar (el motivo esta en el \
                         log, lineas 'traductor')."
                            .to_string(),
                    );
                    break;
                }
                for line in lines {
                    // A la voz va solo lo tuyo: las frases del microfono, ya
                    // traducidas. Las de los demas se leen, no se pronuncian.
                    // Y los ecos de la propia voz sintetica, tampoco.
                    if let Some(speech) = &speech {
                        if line.source == Source::Mic && !line.echo {
                            let _ = speech.texts.send(line.translated.clone());
                        }
                    }
                    transcript.lock().unwrap().push_translation(line.clone());
                    let _ = app.emit("translation", line);
                }
            }
            pump.shutdown();
            // Al soltar `speech` se cierra el canal de textos, y la bomba de
            // voz termina sola despues de decir lo que tenga pendiente.
            tracing::info!("hilo de traduccion terminado");
        })
        .map_err(|e| CmdError {
            message: format!("no se pudo lanzar el hilo de traduccion: {e}"),
        })?;

    Ok(tx)
}

/// Levanta el sintetizador, la salida de audio y la bomba que los une.
/// Devuelve el cableado que el hilo de traduccion necesita.
///
/// El apagado no tiene boton: cuando el hilo de traduccion muere, el canal de
/// textos se cierra, la bomba dice lo pendiente y para el sintetizador, y al
/// soltar la cola de audio el hilo de render termina de reproducir y se va.
/// Cada eslabon cae cuando cae el anterior, igual que el resto de la app.
fn start_speech(app: &AppHandle, config: &AppConfig) -> Result<SpeechWiring, CmdError> {
    // La voz pronuncia el idioma al que se traduce el microfono: dice a los
    // demas, en su idioma, lo que yo hablo en el mio.
    let voice_language = config.voice_language();
    let lang = asr_core::tts_lang_code(&voice_language).ok_or_else(|| {
        format!(
            "no hay sintetizador para {voice_language}: chatterbox habla 23 \
             idiomas y ese no esta entre ellos"
        )
    })?;
    // Kokoro cubre menos idiomas que chatterbox; sin esto el arranque pasa
    // y cada frase de la reunion falla una a una.
    if config.speak.engine == "kokoro" && !asr_core::speak::kokoro_supports(lang) {
        return Err(format!(
            "kokoro no tiene voces para {voice_language}: cambia al motor \
             chatterbox o elige otro idioma para la traduccion del micro"
        )
        .into());
    }

    let mut tts = config.tts();
    tts.script = require_existing(app, &tts.script, "el sidecar de voz")?;
    tts.python = require_existing(app, &tts.python, "el interprete del venv de voz")?;
    if tts.engine == "chatterbox" {
        let Some(wav) = tts.voice_wav.clone() else {
            return Err("Para clonar tu voz hace falta un WAV de muestra: \
                        graba 10-30 segundos de habla limpia y eligelo en la \
                        configuracion, o cambia al motor kokoro (voz neutra)."
                .to_string()
                .into());
        };
        tts.voice_wav = Some(require_existing(app, &wav, "la muestra de voz")?);
    }

    let sidecar = TtsSidecar::spawn(&tts)?;
    // Chatterbox tarda ~21 s en frio (medido); sin esperar aqui, la primera
    // frase de la reunion se comeria el arranque como si fuera latencia.
    let ready = sidecar.wait_ready(std::time::Duration::from_secs(180)).map_err(|e| {
        CmdError {
            message: format!(
                "el sintetizador no arranco ({e}); el motivo esta en el log, \
                 en las lineas 'sintetizador' (¿WAV de voz ilegible? ¿venv sin \
                 chatterbox?)"
            ),
        }
    })?;
    tracing::info!(
        "sintetizador listo en {} a {} Hz ({})",
        ready.device,
        ready.rate,
        tts.engine
    );
    let _ = app.emit(
        "loading",
        serde_json::json!({ "stage": "voice", "message": "Voz lista" }),
    );

    // La salida de audio: para hacer de microfono virtual, el id de CABLE
    // Input. El contador de muestras pendientes es el que luego se ve en la
    // interfaz como retraso de voz acumulado.
    let render_alive = Arc::new(AtomicBool::new(true));
    let queued = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (render_tx, render_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(32);
    let (startup_tx, startup_rx) =
        std::sync::mpsc::sync_channel::<Result<(), String>>(1);
    asr_audio::spawn_render(
        config.speak.output_device_id.clone(),
        ready.rate,
        render_alive.clone(),
        render_rx,
        queued.clone(),
        startup_tx,
    )?;
    // Esperar a que el dispositivo abra de verdad. Sin esto, un id caducado
    // (VB-CABLE reinstalado cambia los ids) devolveria Ok, la interfaz diria
    // "voz lista", y el usuario hablaria toda la reunion sin que le oigan.
    match startup_rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            return Err(format!(
                "no se pudo abrir el dispositivo de salida de la voz: {e}. \
                 ¿Sigue existiendo? (si has reinstalado VB-CABLE, su id \
                 cambio: vuelve a elegirlo en la configuracion)"
            )
            .into())
        }
        Err(_) => {
            return Err("la salida de audio de la voz no respondio al abrir"
                .to_string()
                .into())
        }
    }

    let echo = config
        .speak
        .mark_echo
        .then(|| Arc::new(EchoRegistry::new()));

    // Los eventos de la voz suben a la interfaz por su propio hilo, como los
    // de sesion: la bomba no debe esperar a que la ventana pinte.
    let (event_tx, event_rx) = std::sync::mpsc::channel::<SpeechEvent>();
    let _ = event_tx.send(SpeechEvent::Ready {
        device: ready.device.clone(),
        rate: ready.rate,
    });
    let event_app = app.clone();
    std::thread::Builder::new()
        .name("asr-speech-ui".into())
        .spawn(move || {
            for event in event_rx {
                let _ = event_app.emit("speech-event", &event);
            }
        })
        .map_err(|e| CmdError {
            message: format!("no se pudo lanzar el hilo de eventos de voz: {e}"),
        })?;

    let pump = SpeechPump::new(
        Box::new(sidecar),
        SpeechPumpConfig {
            lang: lang.to_string(),
            group_max_chars: config.speak.group_max_chars,
            group_max_wait_ms: config.speak.group_max_wait_ms,
        },
        render_tx,
        queued,
        ready.rate,
        echo.clone(),
        event_tx,
    );

    let stop = Arc::new(AtomicBool::new(false));
    let (text_tx, text_rx) = std::sync::mpsc::channel::<String>();
    let pump_stop = stop.clone();
    std::thread::Builder::new()
        .name("asr-speech".into())
        .spawn(move || pump.run(text_rx, pump_stop, render_alive))
        .map_err(|e| CmdError {
            message: format!("no se pudo lanzar el hilo de voz: {e}"),
        })?;

    Ok(SpeechWiring {
        texts: text_tx,
        echo,
        stop,
    })
}

fn stop_internal(app: &AppHandle, state: &AppState) {
    state.running.store(false, Ordering::SeqCst);
    // Las sesiones primero, y la voz DESPUES. Al reves, la ultima frase se
    // perdia siempre: entre que se dice y que llega traducida a la voz pasa
    // mas de un segundo (ASR + cierre de frase + NLLB), asi que callar antes
    // de recoger las sesiones garantizaba tirarla.
    let sessions = std::mem::take(&mut *state.sessions.lock().unwrap());
    for session in sessions {
        session.join();
    }
    // Ya no entra audio nuevo: lo que quede en la cola de voz se dice y la
    // bomba termina sola al cerrarse el canal de textos. El asidero se suelta
    // igualmente, por si el usuario para con la cola muy larga.
    if let Some(stop) = state.speech_stop.lock().unwrap().take() {
        stop.store(true, Ordering::Relaxed);
    }
    // Lo que quedara a medias se cierra como una linea mas.
    let closed = state.transcript.lock().unwrap().close_all(0);
    for entry in closed {
        let _ = app.emit("transcript-entry", entry);
    }
    let _ = app.emit("running-changed", false);
}

/// Traduce eventos de sesion en eventos de ventana, actualizando el historial
/// por el camino. Vive en su propio hilo y muere cuando el canal se cierra.
fn spawn_event_pump(
    app: AppHandle,
    transcript: Arc<Mutex<Transcript>>,
    rx: std::sync::mpsc::Receiver<SessionEvent>,
    translation: Option<std::sync::mpsc::Sender<SessionEvent>>,
) {
    std::thread::Builder::new()
        .name("asr-ui-pump".into())
        .spawn(move || {
            for event in rx {
                // El traductor recibe una copia y trabaja a su ritmo, sin
                // frenar el texto en vivo.
                if let Some(tx) = &translation {
                    let _ = tx.send(event.clone());
                }
                match &event {
                    SessionEvent::Delta { source, at_ms, text } => {
                        transcript.lock().unwrap().push_delta(*source, *at_ms, text);
                    }
                    SessionEvent::SegmentEnd { source, at_ms } => {
                        let closed = transcript.lock().unwrap().close_segment(*source, *at_ms);
                        if let Some(entry) = closed {
                            let _ = app.emit("transcript-entry", entry);
                        }
                    }
                    _ => {}
                }
                // El evento crudo tambien sube: la interfaz pinta el parcial en
                // vivo y el vumetro con esto.
                let _ = app.emit("session-event", &event);
            }
            tracing::info!("bomba de eventos terminada");
        })
        .expect("spawn asr-ui-pump");
}

// -------------------------------------------------------- bandeja y atajos

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Mostrar / ocultar", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Arrancar / parar", true, None::<&str>)?;
    let overlay = MenuItem::with_id(app, "overlay", "Subtitulos en pantalla", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Salir", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &toggle, &overlay, &quit])?;

    TrayIconBuilder::with_id("main-tray")
        .icon(app.default_window_icon().unwrap().clone())
        .tooltip("LiveTranscriber")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_or_hide(app),
            "toggle" => {
                tracing::info!("arranque/parada pedido desde la bandeja");
                let state = app.state::<AppState>();
                if state.running.load(Ordering::Relaxed) {
                    stop_internal(app, &state);
                } else if let Err(e) = start_internal(app, &state) {
                    let _ = app.emit("error", e.message);
                }
            }
            "overlay" => {
                let _ = toggle_overlay(app.clone());
            }
            "quit" => {
                let state = app.state::<AppState>();
                stop_internal(app, &state);
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn show_or_hide(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
    } else {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn register_shortcuts(app: &AppHandle, config: &AppConfig) {
    let manager = app.global_shortcut();
    for (accelerator, what) in [
        (config.hotkey_toggle.as_str(), "toggle"),
        (config.hotkey_overlay.as_str(), "overlay"),
    ] {
        if accelerator.trim().is_empty() {
            continue;
        }
        if let Err(e) = manager.register(accelerator) {
            // Un atajo ocupado por otra app no debe impedir que arranque.
            tracing::warn!("no se pudo registrar el atajo {accelerator} ({what}): {e}");
        }
    }
}

// --------------------------------------------------------------- arranque

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config_path = config_location();
    tracing::info!("configuracion en {}", config_path.display());

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    // Solo al pulsar; si no, cada atajo dispara dos veces.
                    if event.state() != tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        return;
                    }
                    tracing::info!("atajo pulsado: {shortcut}");
                    let state = app.state::<AppState>();
                    let config = state.config.lock().unwrap().clone();
                    let pressed = shortcut.to_string();

                    if pressed == config.hotkey_overlay {
                        let _ = toggle_overlay(app.clone());
                    } else if pressed == config.hotkey_toggle {
                        if state.running.load(Ordering::Relaxed) {
                            stop_internal(app, &state);
                        } else if let Err(e) = start_internal(app, &state) {
                            // Tambien al log: si solo va a la interfaz, un
                            // fallo disparado por el atajo es invisible.
                            tracing::error!("no se pudo arrancar: {}", e.message);
                            let _ = app.emit("error", e.message);
                        }
                    }
                })
                .build(),
        )
        .manage(AppState::new(config_path))
        .invoke_handler(tauri::generate_handler![
            get_config,
            save_config,
            list_devices,
            is_running,
            start_transcription,
            stop_transcription,
            get_transcript,
            get_translations,
            transcript_as_text,
            clear_transcript,
            export_transcript,
            output_filename_preview,
            output_dir,
            pick_output_dir,
            reveal_output_dir,
            toggle_overlay,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            build_tray(&handle)?;
            let config = app.state::<AppState>().config.lock().unwrap().clone();
            register_shortcuts(&handle, &config);
            if config.overlay_enabled {
                if let Some(window) = handle.get_webview_window("overlay") {
                    let _ = window.show();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Cerrar la ventana principal manda la app a la bandeja; salir de
            // verdad es la opcion del menu de la bandeja.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error arrancando LiveTranscriber");
}
