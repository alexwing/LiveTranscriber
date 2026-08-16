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

use asr_core::CONFIG_FILE;

/// Error que viaja al frontend como `{ message }`, igual que en TapoController.
///
/// Se construye SIEMPRE por `new`, y `new` lo escribe en el log. Antes estos
/// errores solo existian en el banner de la interfaz: el usuario copiaba el
/// mensaje y el log no tenia ni rastro de el, asi que reconstruir que habia
/// pasado dependia de lo que la persona recordara. Un error que se le enseña
/// al usuario y no se registra es medio error.
#[derive(Debug, serde::Serialize)]
pub struct CmdError {
    pub message: String,
}

impl CmdError {
    fn new(message: impl Into<String>) -> Self {
        let message = message.into();
        tracing::error!("{message}");
        Self { message }
    }
}

macro_rules! from_display {
    ($($t:ty),* $(,)?) => {$(
        impl From<$t> for CmdError {
            fn from(e: $t) -> Self {
                Self::new(e.to_string())
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
        Self::new(message)
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

/// Donde vive `transcriber-config.toml`.
///
/// La decide `asr-core`, no esta capa: tambien la necesita `asr-cli`, y tener
/// dos implementaciones de "donde esta la configuracion" es justo el fallo que
/// hacia que el instalador y la aplicacion instalada no se encontraran.
fn config_location() -> PathBuf {
    if let Some(from) = asr_core::migrate_legacy_config() {
        tracing::info!(
            "configuracion traida de {} a la ubicacion canonica",
            from.display()
        );
    }
    asr_core::config_location()
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

/// El instalador, con su ruta de verdad.
///
/// Instalada la aplicacion, `install.ps1` viaja dentro del bundle: decir
/// "ejecuta scripts\install.ps1" a secas manda al usuario a buscar un fichero
/// que no esta en su directorio de trabajo, sino entre los recursos.
fn installer_hint(app: &AppHandle) -> String {
    match resolve_existing(Some(app), Path::new("scripts/install.ps1")) {
        Some(found) => format!("\"{}\"", found.display()),
        None => "scripts\\install.ps1".to_string(),
    }
}

/// Igual que [`resolve_existing`] pero con un error que dice donde se ha
/// mirado, que es la diferencia entre un fallo diagnosticable y uno que no.
///
/// Distingue tres estados, no dos. "Sin configurar" y "configurado pero no
/// esta" se arreglan de forma distinta, y confundirlos mandaba al usuario a
/// buscar un fichero que nunca tuvo que existir.
fn require_existing(app: &AppHandle, path: &Path, what: &str) -> Result<PathBuf, CmdError> {
    if path.as_os_str().is_empty() {
        let where_ = app
            .try_state::<AppState>()
            .map(|s| s.config_path.display().to_string())
            .unwrap_or_else(|| CONFIG_FILE.to_string());
        return Err(format!(
            "no {what} is configured. Run {} to provision it, \
             or set the path by hand in {where_}",
            installer_hint(app)
        )
        .into());
    }
    if let Some(found) = resolve_existing(Some(app), path) {
        return Ok(found);
    }
    if path.is_absolute() {
        let where_ = app
            .try_state::<AppState>()
            .map(|s| format!(" (from {})", s.config_path.display()))
            .unwrap_or_default();
        return Err(format!(
            "cannot find {what} at {}{where_}. If that path belongs to another \
             machine, run {} to rewrite it",
            path.display(),
            installer_hint(app)
        )
        .into());
    }
    let tried: Vec<String> = search_bases(Some(app))
        .into_iter()
        .map(|base| format!("  {}", base.join(path).display()))
        .collect();
    Err(format!(
        "cannot find {what} ({}). Looked in:\n{}",
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
        // Un fichero que EXISTE pero no parsea no se sustituye en silencio.
        //
        // Antes se caia a `default()` con solo un warning al log, y en cuanto
        // el usuario tocaba cualquier ajuste, `save_config` escribia esos
        // valores por defecto ENCIMA de su fichero. Un TOML momentaneamente
        // ilegible —una llave a medio escribir— se convertia en perdida
        // definitiva de todo, sin ejecutar nada: bastaba abrir la app.
        //
        // Ahora se aparta con fecha antes de tocar nada, para que lo que habia
        // siga existiendo aunque la app arranque por defecto.
        // Arrancar SIN fichero no es lo mismo que arrancar con uno vacio, y
        // hasta ahora se veian igual: `load` devuelve los valores por defecto
        // y la aplicacion seguia como si tuviera configuracion, hasta que al
        // pulsar Arrancar soltaba un error que aconseja reinstalar. Decirlo
        // aqui, alto, es la diferencia entre un minuto y una tarde.
        if !config_path.exists() {
            tracing::warn!(
                "arrancando SIN fichero de configuracion: {} no existe. \
                 Se usan valores por defecto, que no traen interprete de Python.",
                config_path.display()
            );
        }
        let config = match AppConfig::load(&config_path) {
            Ok(config) => config,
            Err(e) => {
                tracing::error!("no se pudo leer {} ({e})", config_path.display());
                if config_path.exists() {
                    // Segundos desde epoch en vez de una fecha bonita: evita
                    // arrastrar chrono hasta esta capa por un nombre de copia.
                    let stamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let backup = config_path.with_extension(format!("toml.broken-{stamp}"));
                    match std::fs::copy(&config_path, &backup) {
                        Ok(_) => tracing::error!(
                            "configuracion ilegible apartada en {}; se arranca por defecto",
                            backup.display()
                        ),
                        Err(e) => tracing::error!("y tampoco se pudo copiar a un .broken: {e}"),
                    }
                }
                AppConfig::default()
            }
        };
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
    Ok(refresh_config(&state))
}

/// Relee la configuracion del disco y adopta lo que encuentre.
///
/// El estado se cargaba UNA vez, al arrancar, y no se volvia a mirar. Eso
/// tenia dos consecuencias feas: editar el fichero por fuera con la ventana
/// abierta no hacia nada, y —peor— el siguiente guardado desde la interfaz
/// escribia encima lo que hubiera en memoria, borrando la edicion externa.
/// Paso de verdad: se corrigieron a mano unas rutas de Python, la ventana
/// abierta siguio con las viejas, y arrancar fallaba pidiendo un fichero que
/// ya no existia.
///
/// Si el fichero no parsea se conserva lo que hay en memoria: media
/// configuracion es peor que una vieja pero entera.
fn refresh_config(state: &AppState) -> AppConfig {
    adopt_if_changed(&state.config, &state.config_path)
}

/// El nucleo de [`refresh_config`], sin `AppState` para poder probarlo.
/// Identifica el fichero que se acaba de leer: tamaño y fecha.
///
/// Existe por un fallo que costo tres intentos: la aplicacion insistia en usar
/// unas rutas que NO estaban en el fichero que yo inspeccionaba. Habia dos
/// ficheros en juego y el log no permitia distinguirlos, porque solo decia la
/// ruta —identica en los dos— y nunca QUE contenia. Con el tamaño y la fecha
/// se ve al instante si la aplicacion esta leyendo otra cosa.
fn config_stamp(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(m) => {
            let secs = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("{} bytes, mtime {secs}", m.len())
        }
        Err(e) => format!("sin metadatos: {e}"),
    }
}

fn adopt_if_changed(held: &Mutex<AppConfig>, path: &Path) -> AppConfig {
    // Un fichero que NO esta no son "valores por defecto": es un fichero que
    // no esta. `AppConfig::load` devuelve Ok(default()) en ese caso, asi que
    // sin esta comprobacion desaparecer el fichero un instante bastaba para
    // que la aplicacion tirara la configuracion buena que tenia en memoria y
    // se quedara sin interprete, con un error que ademas aconseja reinstalar.
    // Paso de verdad. Lo que hay en memoria es mejor que nada.
    if !path.exists() {
        tracing::warn!(
            "{} no existe ahora mismo; se conserva la configuracion en memoria",
            path.display()
        );
        return held.lock().unwrap().clone();
    }
    match AppConfig::load(path) {
        Ok(fresh) => {
            let mut held = held.lock().unwrap();
            if *held != fresh {
                tracing::info!("la configuracion de {} ha cambiado por fuera; adoptada", path.display());
                *held = fresh;
            }
            held.clone()
        }
        Err(e) => {
            tracing::warn!(
                "no se pudo releer {} ({e}); se sigue con la que hay en memoria",
                path.display()
            );
            held.lock().unwrap().clone()
        }
    }
}

#[tauri::command]
fn save_config(state: tauri::State<'_, AppState>, new: AppConfig) -> CmdResult<()> {
    // Si el fichero cambio por fuera desde la ultima vez que lo leimos, lo que
    // llega de la interfaz no lo tiene en cuenta y lo pisaria. Guardar es lo
    // que el usuario ha pedido, asi que se guarda, pero antes se aparta una
    // copia: nada se pierde en silencio.
    if let Ok(on_disk) = AppConfig::load(&state.config_path) {
        let held = state.config.lock().unwrap().clone();
        if on_disk != held && on_disk != new {
            let backup = state.config_path.with_extension("toml.overwritten");
            match std::fs::copy(&state.config_path, &backup) {
                Ok(_) => tracing::warn!(
                    "la configuracion habia cambiado por fuera; copia en {}",
                    backup.display()
                ),
                Err(e) => tracing::warn!("cambio por fuera y no se pudo copiar: {e}"),
            }
        }
    }
    new.save(&state.config_path)?;
    *state.config.lock().unwrap() = new;
    Ok(())
}

#[tauri::command]
fn list_devices(kind: DeviceKind) -> CmdResult<Vec<AudioDevice>> {
    Ok(asr_audio::list_devices(kind)?)
}

// ---------------------------------------------------------------- perfiles
//
// Las combinaciones utiles son muchas (una reunion en ingles hablando con tu
// voz, transcribir una charla en espanol, subtitular una pelicula...) y
// rehacerlas a mano cada vez es tedioso. Ver `asr_core::profiles` para las
// dos decisiones de diseno: viven en su propio fichero, y no se llevan las
// rutas de la instalacion.

fn profiles_file(state: &AppState) -> PathBuf {
    asr_core::profiles_path(&state.config_path)
}

fn load_store(state: &AppState) -> Result<asr_core::ProfileStore, CmdError> {
    asr_core::ProfileStore::load(&profiles_file(state)).map_err(|e| CmdError::new(format!("could not read the profiles: {e}")))
}

#[tauri::command]
fn list_profiles(state: tauri::State<'_, AppState>) -> CmdResult<Vec<String>> {
    Ok(load_store(&state)?.names())
}

/// Guarda la configuracion actual con un nombre. Repetir nombre actualiza,
/// que es lo que se espera al volver a guardar sobre un perfil.
#[tauri::command]
fn save_profile(state: tauri::State<'_, AppState>, name: String) -> CmdResult<Vec<String>> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("the profile needs a name".to_string().into());
    }
    let mut store = load_store(&state)?;
    store.put(&name, state.config.lock().unwrap().clone());
    store.save(&profiles_file(&state)).map_err(|e| CmdError::new(format!("could not save the profile: {e}")))?;
    Ok(store.names())
}

/// Aplica un perfil: lo deja como configuracion actual y lo persiste.
///
/// Devuelve tambien que dispositivos del perfil ya no existen y han caido al
/// predeterminado, para que la interfaz pueda decirlo en vez de cambiarlos
/// en silencio.
#[tauri::command]
fn load_profile(
    state: tauri::State<'_, AppState>,
    name: String,
) -> CmdResult<asr_core::AppliedProfile> {
    if state.running.load(Ordering::Relaxed) {
        return Err("stop before switching profiles"
            .to_string()
            .into());
    }
    let store = load_store(&state)?;
    let profile = store
        .get(&name)
        .ok_or_else(|| CmdError::from(format!("no profile named {name:?}")))?;

    let applied = {
        let current = state.config.lock().unwrap();
        asr_core::profiles::apply(&profile.config, &current, &asr_core::DeviceIds::from_system())
    };

    applied.config.save(&state.config_path)?;
    *state.config.lock().unwrap() = applied.config.clone();
    tracing::info!("perfil {name:?} aplicado");
    Ok(applied)
}

#[tauri::command]
fn delete_profile(state: tauri::State<'_, AppState>, name: String) -> CmdResult<Vec<String>> {
    let mut store = load_store(&state)?;
    if !store.remove(&name) {
        return Err(format!("no profile named {name:?}").into());
    }
    store.save(&profiles_file(&state)).map_err(|e| CmdError::new(format!("could not save the profile list: {e}")))?;
    Ok(store.names())
}

#[tauri::command]
fn is_running(state: tauri::State<'_, AppState>) -> bool {
    tracing::debug!("is_running");
    state.running.load(Ordering::Relaxed)
}

#[tauri::command]
fn start_transcription(app: AppHandle, state: tauri::State<'_, AppState>) -> CmdResult<()> {
    tracing::info!("arranque pedido desde la interfaz");
    // Releer AQUI es lo que importa: es el momento en que la configuracion se
    // convierte en procesos, y una ventana lleva abierta lo que lleve.
    refresh_config(&state);
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
    .map_err(|e| CmdError::new(format!("the folder picker failed: {e}")))?;

    let Some(picked) = picked else {
        return Ok(None); // el usuario cancelo
    };
    // `simplified` deja rutas de Windows normales en vez de UNC.
    let chosen = picked.simplified().into_path().map_err(|e| CmdError::new(format!("unusable path: {e}")))?;

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
        .map_err(|e| CmdError::new(format!("could not open {}: {e}", dir.display())))?;
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
        return Err("nothing to export yet".to_string().into());
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
        return Err("the overlay window does not exist".to_string().into());
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
    let resolved = require_existing(app, &sidecar.script, "the ASR sidecar")
        .and_then(|script| {
            sidecar.script = script;
            require_existing(app, &sidecar.python, "Python interpreter")
        });
    match resolved {
        Ok(python) => {
            // Decir QUE se va a usar, no solo que fallo si falla. Sin esta
            // linea, un arranque correcto no dejaba constancia de con que
            // interprete corria, y comparar "lo que creo que lee" con "lo que
            // lee" era imposible desde el log.
            tracing::info!("interprete del ASR: {}", python.display());
            sidecar.python = python;
        }
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
        return Err("no source is enabled".to_string().into());
    }

    // Las validaciones de idioma van ANTES de montar nada, para que el error
    // sea siempre el claro: sin esto, con el idioma en "auto" y la voz
    // activada el primero en quejarse era el sintetizador, con un mensaje
    // que no apuntaba a la causa.
    if config.translate {
        if config.language == "auto" {
            state.running.store(false, Ordering::SeqCst);
            return Err("To translate you have to pick a specific language for the \
                        room instead of automatic detection: the translator \
                        needs to know which language it starts from."
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
                "the microphone language ({mic_lang}) is not a valid locale: \
                 pick it in the settings"
            )
            .into());
        }
    }

    if config.speak.enabled && (!config.translate || !config.capture_mic) {
        state.running.store(false, Ordering::SeqCst);
        return Err("Speaking with your voice needs 'Translate in parallel' and \
                    'My microphone' both enabled: what gets spoken is the \
                    translation of what you say into the mic."
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
        serde_json::json!({ "stage": "start", "message": "Loading models…" }),
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
                Err(_) => Err(format!("starting {que} panicked").into()),
            },
        }
    }

    // Se recogen los dos SIEMPRE, aunque el primero falle: si no, el hilo
    // superviviente dejaria un sidecar de Python vivo agarrado a la VRAM.
    let speech_result = join_stage(speech_handle, "the voice");
    let translator_result = join_stage(translator_handle, "the translator");

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
    mt.script = require_existing(app, &mt.script, "the translation sidecar")?;
    mt.python = require_existing(app, &mt.python, "Python interpreter")?;

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
        serde_json::json!({ "stage": "translator", "message": "Translator ready" }),
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
        .map_err(|e| CmdError::new(format!("could not spawn the translation thread: {e}")))?;

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
            "no synthesizer for {voice_language}: chatterbox speaks 23 \
             languages and that one is not among them"
        )
    })?;
    // Kokoro cubre menos idiomas que chatterbox; sin esto el arranque pasa
    // y cada frase de la reunion falla una a una.
    if config.speak.engine == "kokoro" && !asr_core::speak::kokoro_supports(lang) {
        return Err(format!(
            "kokoro has no voices for {voice_language}: switch to the \
             chatterbox engine or pick another language for the mic translation"
        )
        .into());
    }

    let mut tts = config.tts();
    tts.script = require_existing(app, &tts.script, "the voice sidecar")?;
    tts.python = require_existing(app, &tts.python, "the voice venv interpreter")?;
    if tts.engine == "chatterbox" {
        let Some(wav) = tts.voice_wav.clone() else {
            return Err("Cloning your voice needs a sample WAV: record 10-30 seconds \
                        of clean speech and pick it in the settings, or switch to \
                        the kokoro engine (neutral voice)."
                .to_string()
                .into());
        };
        tts.voice_wav = Some(require_existing(app, &wav, "the voice sample")?);
    }

    let sidecar = TtsSidecar::spawn(&tts)?;
    // Chatterbox tarda ~21 s en frio (medido); sin esperar aqui, la primera
    // frase de la reunion se comeria el arranque como si fuera latencia.
    let ready = sidecar.wait_ready(std::time::Duration::from_secs(180)).map_err(|e| {
        CmdError {
            message: format!(
                "the synthesizer did not start ({e}); the reason is in the log, \
                 on the 'synthesizer' lines (unreadable voice WAV? venv without \
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
        serde_json::json!({ "stage": "voice", "message": "Voice ready" }),
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
                "could not open the voice output device: {e}. Does it still \
                 exist? (if you reinstalled VB-CABLE its id changed: pick it \
                 again in the settings)"
            )
            .into())
        }
        Err(_) => {
            return Err("the voice audio output did not respond when opening"
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
        .map_err(|e| CmdError::new(format!("could not spawn the speech event thread: {e}")))?;

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
        .map_err(|e| CmdError::new(format!("could not spawn the speech thread: {e}")))?;

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

/// El menu de la bandeja lo dibuja el sistema operativo, no el webview, asi
/// que sus textos no pueden salir del catalogo de `i18n.ts`. Son cuatro: se
/// duplican aqui y la interfaz avisa del idioma con `set_ui_language`.
fn tray_menu(app: &AppHandle, lang: &str) -> tauri::Result<Menu<tauri::Wry>> {
    let [show, toggle, overlay, quit] = if lang == "es" {
        [
            "Mostrar / ocultar",
            "Arrancar / parar",
            "Subtitulos en pantalla",
            "Salir",
        ]
    } else {
        ["Show / hide", "Start / stop", "On-screen subtitles", "Quit"]
    };
    let show = MenuItem::with_id(app, "show", show, true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", toggle, true, None::<&str>)?;
    let overlay = MenuItem::with_id(app, "overlay", overlay, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", quit, true, None::<&str>)?;
    Menu::with_items(app, &[&show, &toggle, &overlay, &quit])
}

/// Cambia el idioma del menu de la bandeja. No devuelve error: es cosmetico y
/// no vale la pena que un fallo aqui estropee el cambio de idioma en pantalla.
#[tauri::command]
fn set_ui_language(app: AppHandle, lang: String) {
    let Some(tray) = app.tray_by_id("main-tray") else {
        return;
    };
    match tray_menu(&app, &lang).and_then(|menu| tray.set_menu(Some(menu))) {
        Ok(()) => {}
        Err(e) => tracing::warn!("no se pudo cambiar el idioma de la bandeja: {e}"),
    }
}

fn build_tray(app: &AppHandle, lang: &str) -> tauri::Result<()> {
    let menu = tray_menu(app, lang)?;

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

/// Arranca el log, a fichero y no a stdout.
///
/// La aplicacion se compila con `windows_subsystem = "windows"`, asi que no
/// tiene consola: todo lo que se escribia en stdout se perdia. Y sin embargo
/// INSTALL.md, el README y varios mensajes de error mandaban al usuario a
/// "mirar el log". No habia ninguno.
///
/// Va junto a la configuracion, en `%APPDATA%\LiveTranscriber\logs\`, con
/// rotacion diaria para que una sesion larga no deje un fichero eterno. Sigue
/// saliendo por stdout ademas, que es lo util en `tauri dev`.
fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::prelude::*;

    let filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info".into())
    };

    let dir = asr_core::config_location()
        .parent()
        .map(|p| p.join("logs"));

    // Si el fichero no se puede abrir, la aplicacion arranca igual: quedarse
    // sin log es molesto, no arrancar es peor.
    let file_layer = dir.and_then(|dir| {
        std::fs::create_dir_all(&dir).ok()?;
        let appender = tracing_appender::rolling::daily(&dir, "live-transcriber.log");
        let (writer, guard) = tracing_appender::non_blocking(appender);
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(false)
            .with_filter(filter());
        Some((layer, guard, dir))
    });

    match file_layer {
        Some((layer, guard, dir)) => {
            tracing_subscriber::registry()
                .with(tracing_subscriber::fmt::layer().with_filter(filter()))
                .with(layer)
                .init();
            tracing::info!("log en {}", dir.display());
            Some(guard)
        }
        None => {
            tracing_subscriber::registry()
                .with(tracing_subscriber::fmt::layer().with_filter(filter()))
                .init();
            None
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // El guardia hay que mantenerlo vivo TODO el proceso: al soltarlo se cierra
    // el hilo que escribe, y las ultimas lineas —justo las del fallo que se
    // esta investigando— se quedan sin volcar.
    let _log_guard = init_logging();

    let config_path = config_location();
    tracing::info!(
        "configuracion en {} ({})",
        config_path.display(),
        config_stamp(&config_path)
    );

    tauri::Builder::default()
        // Una sola ventana, y va PRIMERO: el plugin exige ser el primero para
        // poder abortar la segunda instancia antes de que monte nada.
        //
        // Sin esto se podian tener dos abiertas a la vez. Paso de verdad y
        // costo caro de diagnosticar: una ventana vieja, con la configuracion
        // anterior en memoria y de una build sin relectura, fallaba al
        // arrancar; y como las dos escriben en el MISMO fichero de log, el
        // fallo de la vieja parecia venir de la recien abierta.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tracing::info!("ya habia una instancia; se trae al frente en vez de abrir otra");
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
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

                    // Comparar PARSEANDO, no como texto. El plugin devuelve
                    // "shift+control+KeyT" y la configuracion dice
                    // "CmdOrControl+Shift+T": como cadenas no coinciden nunca,
                    // asi que los atajos globales se registraban, se recibian
                    // y no hacian absolutamente nada.
                    let matches = |accel: &str| {
                        accel
                            .parse::<tauri_plugin_global_shortcut::Shortcut>()
                            .map(|want| &want == shortcut)
                            .unwrap_or(false)
                    };

                    if matches(&config.hotkey_overlay) {
                        let _ = toggle_overlay(app.clone());
                    } else if matches(&config.hotkey_toggle) {
                        if state.running.load(Ordering::Relaxed) {
                            stop_internal(app, &state);
                        } else if let Err(e) = {
                            // Igual que el boton: releer antes de convertir la
                            // configuracion en procesos.
                            refresh_config(&state);
                            start_internal(app, &state)
                        } {
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
            list_profiles,
            save_profile,
            load_profile,
            delete_profile,
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
            set_ui_language,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            // El idioma real lo dice la interfaz nada mas montarse; hasta
            // entonces el menu no esta abierto, asi que este valor no se ve.
            build_tray(&handle, "en")?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Escribe un TOML minimo con la ruta de Python que se le diga.
    fn write_config(dir: &Path, python: &str) -> PathBuf {
        let path = dir.join("transcriber-config.toml");
        std::fs::write(&path, format!("python = '{python}'\n")).expect("escribe");
        path
    }

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lt-test-{name}"));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// Cambiar el fichero por fuera con la aplicacion abierta tiene que verse.
    ///
    /// Este test existe por un fallo real: se corrigieron a mano unas rutas de
    /// Python, la ventana que ya estaba abierta siguio con las viejas en
    /// memoria, y arrancar fallaba pidiendo un fichero borrado. El estado se
    /// leia una sola vez y no se volvia a mirar.
    #[test]
    fn adopta_lo_que_cambia_por_fuera() {
        let dir = tmpdir("adopta");
        let path = write_config(&dir, r"C:\viejo\python.exe");
        let held = Mutex::new(AppConfig::load(&path).expect("carga"));
        assert_eq!(held.lock().unwrap().python, PathBuf::from(r"C:\viejo\python.exe"));

        write_config(&dir, r"C:\nuevo\python.exe");
        let got = adopt_if_changed(&held, &path);

        assert_eq!(got.python, PathBuf::from(r"C:\nuevo\python.exe"));
        assert_eq!(held.lock().unwrap().python, PathBuf::from(r"C:\nuevo\python.exe"));
    }

    /// Si el fichero DESAPARECE, lo que hay en memoria se conserva.
    ///
    /// `AppConfig::load` devuelve los valores por defecto para un fichero que
    /// no existe, asi que sin la comprobacion previa un borrado momentaneo
    /// —o cualquier cosa que lo haga ilegible un instante— tiraba la
    /// configuracion buena y dejaba la aplicacion sin interprete.
    #[test]
    fn si_el_fichero_desaparece_se_conserva_la_memoria() {
        let dir = tmpdir("desaparece");
        let path = write_config(&dir, r"C:\bueno\python.exe");
        let held = Mutex::new(AppConfig::load(&path).expect("carga"));

        std::fs::remove_file(&path).expect("borra");
        let got = adopt_if_changed(&held, &path);

        assert_eq!(got.python, PathBuf::from(r"C:\bueno\python.exe"));
        assert_eq!(
            held.lock().unwrap().python,
            PathBuf::from(r"C:\bueno\python.exe")
        );
    }

    /// Un fichero a medio escribir no debe dejar la aplicacion sin ajustes:
    /// media configuracion es peor que una vieja pero entera.
    #[test]
    fn un_toml_roto_no_borra_lo_que_hay_en_memoria() {
        let dir = tmpdir("roto");
        let path = write_config(&dir, r"C:\bueno\python.exe");
        let held = Mutex::new(AppConfig::load(&path).expect("carga"));

        std::fs::write(&path, "python = 'sin cerrar\n[speak").expect("escribe basura");
        let got = adopt_if_changed(&held, &path);

        assert_eq!(got.python, PathBuf::from(r"C:\bueno\python.exe"));
    }
}
