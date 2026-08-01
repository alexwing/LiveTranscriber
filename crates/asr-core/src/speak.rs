//! Voz sintetica detras de la traduccion: el tercer eslabon de la cascada.
//!
//! Lo que dices por el microfono se transcribe, se traduce, y este modulo lo
//! **pronuncia** en el idioma destino con una voz clonada de la tuya (o con
//! una voz neutra), escribiendolo en un dispositivo de salida. Con VB-CABLE
//! como dispositivo, la reunion te oye hablar en su idioma.
//!
//! Tres decisiones de diseno vienen de medir, no de teoria:
//!
//! - **Se habla solo lo que sale del microfono.** Las frases de los demas se
//!   leen en pantalla, como siempre; pronunciarlas seria hablar encima de la
//!   reunion.
//! - **Se agrupan frases antes de sintetizar.** Chatterbox tiene un coste fijo
//!   por llamada de ~1 s (medido en la 3060; solo ~0,24 s es re-codificar la
//!   voz de referencia, el resto es arranque del decodificador). Con frases
//!   cortas eso lo deja por debajo de tiempo real (0,84x) y el retraso
//!   creceria sin limite; con bloques de ~250 caracteres pasa de 1x y el
//!   retraso queda acotado. El agrupador junta frases hasta `group_max_chars`
//!   o hasta que la mas vieja lleve `group_max_wait_ms` esperando.
//! - **Lo hablado se apunta en un registro de eco.** Si la propia voz
//!   sintetica vuelve por la captura del sistema (segun como este montada la
//!   reunion, pasa), el ASR la transcribe y la traduccion la volveria a
//!   traducir: es->en->es produce cosas raras. El registro permite reconocer
//!   esas frases y etiquetarlas en vez de re-traducirlas.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::engine::{EngineError, Result};

const FRAME_CONTROL: u8 = 0x02;

/// Sintetizar un bloque tarda ~su duracion de audio, y el sidecar reintenta
/// hasta 3 veces si detecta un truncado (loteria del muestreo de chatterbox,
/// ver tts_server.py). El peor caso realista es 3 intentos de un bloque de
/// 500 caracteres a ~1x: ~90 s. Con margen, para no confundir un cuelgue con
/// un bloque largo con mala suerte.
const SPEAK_TIMEOUT: Duration = Duration::from_secs(180);

const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

// ------------------------------------------------------------------ trait

/// Sintetizador de voz. Espejo de [`crate::translate::Translator`]: quien
/// orquesta habla solo con esto, asi que cambiar Chatterbox por otra cosa es
/// implementarlo otra vez y cambiar la linea donde se construye.
pub trait Synthesizer: Send {
    /// Convierte texto en audio. `lang` es el codigo corto del idioma
    /// (`en`, `es`, ...), ver [`tts_lang_code`]. Bloquea mientras genera.
    fn synthesize(&mut self, text: &str, lang: &str) -> Result<Synthesized>;
    fn shutdown(&mut self) -> Result<()>;
}

/// Audio sintetizado: muestras f32 mono a `rate` Hz, listas para `spawn_render`.
pub struct Synthesized {
    pub samples: Vec<f32>,
    pub rate: u32,
    /// Cuanto tardo el sidecar en generarlo, para el RTFx en los logs.
    pub synth_ms: u64,
}

pub trait SynthesizerFactory: Send + Sync {
    fn build(&self) -> Result<Box<dyn Synthesizer>>;
}

// ------------------------------------------------------- codigos de idioma

/// Locale del ASR (`en-US`) -> codigo de idioma del sintetizador (`en`).
///
/// La lista es la de Chatterbox Multilingual (23 idiomas); Kokoro cubre un
/// subconjunto y el sidecar avisa si se le pide uno que no tiene. Igual que
/// con FLORES, mejor fallar aqui con un error claro que improvisar un codigo.
pub fn tts_lang_code(locale: &str) -> Option<&'static str> {
    let prefix = locale.split('-').next().unwrap_or(locale);
    let code = match prefix {
        "ar" => "ar",
        "da" => "da",
        "de" => "de",
        "el" => "el",
        "en" => "en",
        "es" => "es",
        "fi" => "fi",
        "fr" => "fr",
        "he" => "he",
        "hi" => "hi",
        "it" => "it",
        "ja" => "ja",
        "ko" => "ko",
        "ms" => "ms",
        "nl" => "nl",
        // Ambas variantes escritas del noruego caen al mismo codigo.
        "nb" | "nn" | "no" => "no",
        "pl" => "pl",
        "pt" => "pt",
        "ru" => "ru",
        "sv" => "sv",
        "sw" => "sw",
        "tr" => "tr",
        "zh" => "zh",
        _ => return None,
    };
    Some(code)
}

/// ¿Tiene kokoro voces para este codigo corto? Es un subconjunto de los 23 de
/// chatterbox; sin esta comprobacion al arrancar, un destino como el aleman
/// pasaria el arranque con kokoro y fallaria en cada frase de la reunion.
pub fn kokoro_supports(lang: &str) -> bool {
    matches!(lang, "en" | "es" | "fr" | "hi" | "it" | "pt" | "ja" | "zh")
}

// -------------------------------------------------------- registro de eco

/// Memoria corta de lo que la voz sintetica acaba de decir.
///
/// Existe por el bucle es->en->es: si la voz sintetica vuelve por la captura
/// del sistema, el ASR la transcribe (en ingles) y la traduccion la devolveria
/// al espanol re-traducida, que nunca coincide con lo que se dijo. Antes de
/// traducir una frase del sistema se pregunta aqui; si coincide con algo
/// recien hablado, se etiqueta como eco y no se re-traduce.
///
/// La comparacion es por solape de palabras, no exacta: el ASR no devuelve el
/// texto verbatim ("I'm" vs "I am", numeros, puntuacion).
#[derive(Default)]
pub struct EchoRegistry {
    spoken: Mutex<Vec<Spoken>>,
}

struct Spoken {
    tokens: Vec<String>,
    expires: Instant,
}

/// Cuanta parte de las palabras oidas tiene que estar en lo hablado para
/// considerarlo un eco. 0,7 tolera los errores tipicos del ASR sin dar por
/// eco frases que solo comparten palabras comunes.
const ECHO_OVERLAP: f32 = 0.7;

/// Con menos palabras que esto no hay senal suficiente: "yes" u "ok" apareceran
/// en el habla de cualquiera y marcarlos de eco seria mentir.
const ECHO_MIN_TOKENS: usize = 3;

impl EchoRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn tokens(text: &str) -> Vec<String> {
        text.split_whitespace()
            .map(|word| {
                word.chars()
                    .filter(|c| c.is_alphanumeric())
                    .collect::<String>()
                    .to_lowercase()
            })
            .filter(|w| !w.is_empty())
            .collect()
    }

    /// Apunta un texto recien mandado a reproducir. `valid_for` debe cubrir la
    /// cola pendiente + la duracion del audio + el retardo del ASR en oirlo.
    pub fn record(&self, text: &str, valid_for: Duration) {
        let tokens = Self::tokens(text);
        if tokens.is_empty() {
            return;
        }
        let mut spoken = self.spoken.lock().unwrap();
        let now = Instant::now();
        spoken.retain(|s| s.expires > now);
        spoken.push(Spoken {
            tokens,
            expires: now + valid_for,
        });
    }

    /// ¿Es esto un eco de algo que la voz sintetica acaba de decir?
    pub fn matches(&self, heard: &str) -> bool {
        let heard = Self::tokens(heard);
        if heard.len() < ECHO_MIN_TOKENS {
            return false;
        }
        let mut spoken = self.spoken.lock().unwrap();
        let now = Instant::now();
        spoken.retain(|s| s.expires > now);

        spoken.iter().any(|s| {
            // Multiconjunto: cada palabra hablada cubre una oida como mucho.
            let mut pool = s.tokens.clone();
            let hits = heard
                .iter()
                .filter(|word| {
                    if let Some(i) = pool.iter().position(|t| t == *word) {
                        pool.swap_remove(i);
                        true
                    } else {
                        false
                    }
                })
                .count();
            hits as f32 / heard.len() as f32 >= ECHO_OVERLAP
        })
    }
}

// ------------------------------------------------------------ orquestacion

/// Lo que la bomba de voz comunica hacia arriba.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpeechEvent {
    /// El sintetizador termino de cargar y ya acepta texto.
    Ready { device: String, rate: u32 },
    /// Estado de la cola. `queued_ms` es el retraso acumulado: el audio que
    /// falta por sonar. Si crece sin parar, se esta hablando mas rapido de lo
    /// que el sintetizador genera.
    Queue { pending_texts: usize, queued_ms: u64 },
    /// Un bloque salio hablado. `synth_ms`/`audio_ms` dan el RTFx real.
    Spoke {
        text: String,
        synth_ms: u64,
        audio_ms: u64,
    },
    Error { message: String },
    Stopped,
}

/// Parametros de la bomba de voz. Los dos de agrupado son el compromiso
/// medido entre latencia y sostenibilidad (ver el comentario del modulo).
#[derive(Debug, Clone)]
pub struct SpeechPumpConfig {
    /// Codigo corto del idioma en que llegan los textos ya traducidos.
    pub lang: String,
    /// Al juntar este tamano se sintetiza aunque no haya pasado el plazo.
    pub group_max_chars: usize,
    /// La frase mas vieja no espera mas que esto aunque el bloque sea corto.
    pub group_max_wait_ms: u64,
}

/// Margen extra de validez de una entrada del registro de eco, por encima de
/// la duracion del propio audio: cubre el retardo del ASR y del corte de
/// frases en volver a oirla.
///
/// Es deliberadamente generoso porque solo alarga la MEMORIA para comparar
/// texto: recordar de mas no cuesta nada. No sirve como ventana para callar
/// al hablante — se intento y se comio frases enteras del usuario, porque
/// "mi voz hace eco" y "sigo hablando encima" ocurren en el mismo instante y
/// el tiempo no los distingue.
const ECHO_GRACE: Duration = Duration::from_secs(10);

/// Une el sintetizador con la salida de audio: recibe textos ya traducidos,
/// los agrupa, los sintetiza y los encola para reproducir, en orden.
///
/// El orden esta garantizado por construccion: un solo hilo sintetiza y
/// encola secuencialmente, asi que no hace falta reordenar nada.
pub struct SpeechPump {
    synth: Box<dyn Synthesizer>,
    cfg: SpeechPumpConfig,
    /// Textos pendientes de agrupar, con el instante en que llego el primero.
    pending: Vec<String>,
    oldest: Option<Instant>,
    render_tx: SyncSender<Vec<f32>>,
    /// Muestras encoladas y aun no escritas al dispositivo. Lo comparte con
    /// el hilo de render, que es quien resta.
    queued_samples: Arc<AtomicU64>,
    rate: u32,
    echo: Option<Arc<EchoRegistry>>,
    events: Sender<SpeechEvent>,
}

impl SpeechPump {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        synth: Box<dyn Synthesizer>,
        cfg: SpeechPumpConfig,
        render_tx: SyncSender<Vec<f32>>,
        queued_samples: Arc<AtomicU64>,
        rate: u32,
        echo: Option<Arc<EchoRegistry>>,
        events: Sender<SpeechEvent>,
    ) -> Self {
        Self {
            synth,
            cfg,
            pending: Vec::new(),
            oldest: None,
            render_tx,
            queued_samples,
            rate,
            echo,
            events,
        }
    }

    /// Bucle principal. Bloquea hasta que pase lo primero de: `rx` se cierra
    /// (fin natural: se habla lo pendiente antes de salir), `stop` sube
    /// (parada pedida por el usuario: se calla YA, lo pendiente se descarta),
    /// o `render_alive` cae (la salida de audio murio: se avisa con un
    /// error, porque seguir sintetizando hacia un dispositivo muerto seria
    /// exactamente "crees que te oyen y no te oye nadie").
    pub fn run(
        mut self,
        rx: Receiver<String>,
        stop: Arc<AtomicBool>,
        render_alive: Arc<AtomicBool>,
    ) {
        let mut natural_end = false;
        let mut last_queue_emit = Instant::now();

        loop {
            if stop.load(Ordering::Relaxed) {
                tracing::info!("parada pedida; se descarta lo pendiente de decir");
                break;
            }
            if !render_alive.load(Ordering::Relaxed) {
                let _ = self.events.send(SpeechEvent::Error {
                    message: "la salida de audio se detuvo (¿sigue existiendo el \
                              dispositivo?); la voz queda muda"
                        .to_string(),
                });
                break;
            }

            // Despertar aunque no llegue texto: el plazo del agrupador corre
            // aunque nadie diga nada nuevo.
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(text) => {
                    let text = text.trim().to_string();
                    if !text.is_empty() {
                        if self.pending.is_empty() {
                            self.oldest = Some(Instant::now());
                        }
                        self.pending.push(text);
                        self.emit_queue();
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    natural_end = true;
                    break;
                }
            }

            if self.should_flush() && !self.flush() {
                break;
            }

            // Mientras el audio encolado se drena, que la interfaz vea el
            // retraso bajar; sin esto el numero se congela hasta el
            // siguiente bloque.
            if last_queue_emit.elapsed() >= Duration::from_millis(500)
                && self.queued_samples.load(Ordering::Relaxed) > 0
            {
                self.emit_queue();
                last_queue_emit = Instant::now();
            }
        }

        // Lo que quede pendiente se dice entero SOLO en el fin natural;
        // cortar ahi seria dejar la ultima frase de la reunion a medias.
        // Tras un "Parar" explicito, callar ya es lo que se ha pedido.
        if natural_end && !self.pending.is_empty() {
            self.flush();
        }

        if let Err(e) = self.synth.shutdown() {
            tracing::warn!("el sintetizador no cerro limpiamente: {e}");
        }
        let _ = self.events.send(SpeechEvent::Stopped);
        tracing::info!("bomba de voz terminada");
    }

    fn should_flush(&self) -> bool {
        if self.pending.is_empty() {
            return false;
        }
        // Agrupar existe para amortizar el coste fijo por peticion CUANDO SE
        // VA POR DETRAS. Con la voz callada (nada encolado ni sonando),
        // esperar es latencia pura: la primera frase sale ya, y mientras
        // suena, las siguientes se agrupan solas. Medido: esto quita hasta
        // 2 s del camino hablo->me-oyen en el caso conversacional tipico.
        if self.queued_samples.load(Ordering::Relaxed) == 0 {
            return true;
        }
        let chars: usize = self.pending.iter().map(|t| t.len()).sum();
        if chars >= self.cfg.group_max_chars {
            return true;
        }
        self.oldest
            .is_some_and(|t| t.elapsed() >= Duration::from_millis(self.cfg.group_max_wait_ms))
    }

    /// Sintetiza y encola lo agrupado. Devuelve `false` cuando ya no tiene
    /// sentido seguir (el sidecar murio o la salida de audio se cerro).
    fn flush(&mut self) -> bool {
        let block = self.pending.join(" ");

        let synthesized = match self.synth.synthesize(&block, &self.cfg.lang) {
            // Sidecar muerto: irrecuperable. Hay que mirar tambien `Io`, no
            // solo `Closed`: cuando el proceso de Python cae, lo primero que
            // falla es ESCRIBIR en su pipe (BrokenPipe -> Io), antes de que
            // el lector note el EOF que produciria `Closed`. Comprobado: con
            // `Io` la bomba seguia viva descartando un bloque por frase el
            // resto de la sesion.
            Err(e @ (EngineError::Closed | EngineError::Io(_))) => {
                tracing::error!("el sintetizador murio: {e}");
                let _ = self.events.send(SpeechEvent::Error {
                    message: "el sintetizador murio; la voz queda muda (el motivo \
                              esta en el log, lineas 'sintetizador')"
                        .to_string(),
                });
                return false;
            }
            Err(e) => {
                // Un fallo puntual no tumba la voz: se avisa y se sigue con
                // el siguiente bloque. `pending` se limpia AQUI y no antes,
                // para no perder el bloque cuando el fallo llega a mitad.
                tracing::warn!("no se pudo sintetizar {block:?}: {e}");
                self.pending.clear();
                self.oldest = None;
                let _ = self.events.send(SpeechEvent::Error {
                    message: e.to_string(),
                });
                return true;
            }
            Ok(s) => s,
        };
        self.pending.clear();
        self.oldest = None;

        if synthesized.rate != self.rate {
            // El render se abrio con la frecuencia del `ready`; si el sidecar
            // cambiara de frecuencia a mitad, mejor pararlo que reproducir
            // audio con la velocidad equivocada.
            let _ = self.events.send(SpeechEvent::Error {
                message: format!(
                    "el sintetizador cambio de frecuencia: {} -> {}",
                    self.rate, synthesized.rate
                ),
            });
            return true;
        }

        let audio_ms = (synthesized.samples.len() as u64 * 1000) / self.rate.max(1) as u64;

        // El registro de eco se apunta ANTES de encolar: en cuanto el audio
        // empiece a sonar, el ASR puede empezar a oirlo.
        if let Some(echo) = &self.echo {
            let backlog_ms =
                (self.queued_samples.load(Ordering::Relaxed) * 1000) / self.rate.max(1) as u64;
            let valid = Duration::from_millis(backlog_ms + audio_ms) + ECHO_GRACE;
            echo.record(&block, valid);
        }

        self.queued_samples
            .fetch_add(synthesized.samples.len() as u64, Ordering::Relaxed);
        if self.render_tx.send(synthesized.samples).is_err() {
            tracing::error!("la salida de audio se cerro, parando la voz");
            let _ = self.events.send(SpeechEvent::Error {
                message: "la salida de audio se cerro; la voz queda muda \
                          (¿sigue existiendo el dispositivo?)"
                    .to_string(),
            });
            return false;
        }

        let _ = self.events.send(SpeechEvent::Spoke {
            text: block,
            synth_ms: synthesized.synth_ms,
            audio_ms,
        });
        self.emit_queue();
        true
    }

    fn emit_queue(&self) {
        let queued_ms =
            (self.queued_samples.load(Ordering::Relaxed) * 1000) / self.rate.max(1) as u64;
        let _ = self.events.send(SpeechEvent::Queue {
            pending_texts: self.pending.len(),
            queued_ms,
        });
    }
}

// ------------------------------------------------------ sidecar de Python

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TtsConfig {
    /// Interprete del venv **de voz**, no el del ASR: los dos no caben en el
    /// mismo entorno (el ASR exige transformers>=5.13 y chatterbox-tts esta
    /// probado con 4.57.x), asi que cada sidecar lleva el suyo.
    pub python: PathBuf,
    /// Ruta a `tts_server.py`.
    pub script: PathBuf,
    /// `chatterbox` (clonacion, 23 idiomas, ~3,4 GB de VRAM) o `kokoro`
    /// (voz neutra, 8 idiomas, ~0,6 GB y 40x tiempo real).
    pub engine: String,
    /// Muestra de voz a clonar (10-30 s de habla limpia). Solo chatterbox.
    pub voice_wav: Option<PathBuf>,
    /// Voz preajustada de kokoro (`ef_dora`, `em_alex`, `af_heart`, ...).
    pub kokoro_voice: String,
    /// Idioma a precalentar antes del `ready`. Kokoro carga su tuberia al
    /// primer uso (~3 s medidos); mejor en el arranque que en la primera frase.
    pub warm_lang: Option<String>,
    /// Valor para `HF_HOME`, si los modelos no estan en la cache por defecto.
    pub hf_home: Option<PathBuf>,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            python: PathBuf::from("python"),
            script: PathBuf::from("sidecar/tts_server.py"),
            engine: "chatterbox".to_string(),
            voice_wav: None,
            kokoro_voice: "af_heart".to_string(),
            warm_lang: None,
            hf_home: None,
        }
    }
}

impl SynthesizerFactory for TtsConfig {
    fn build(&self) -> Result<Box<dyn Synthesizer>> {
        Ok(Box::new(TtsSidecar::spawn(self)?))
    }
}

/// Lo que el sidecar dice al arrancar.
#[derive(Debug, Clone)]
pub struct TtsReady {
    pub device: String,
    pub rate: u32,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Wire {
    Ready {
        device: String,
        rate: u32,
    },
    Audio {
        id: u64,
        /// PCM i16 little-endian en base64. En base64 y no en crudo para no
        /// romper el protocolo de una linea JSON por mensaje; el sobrecoste
        /// (~33% de un audio que tarda segundos en generarse) es irrelevante.
        pcm: String,
        rate: u32,
        ms: u64,
    },
    Error {
        id: u64,
        message: String,
    },
}

pub struct TtsSidecar {
    child: Child,
    stdin: Option<ChildStdin>,
    replies: Receiver<Wire>,
    reader: Option<JoinHandle<()>>,
    next_id: u64,
}

impl TtsSidecar {
    pub fn spawn(cfg: &TtsConfig) -> Result<Self> {
        let mut command = Command::new(&cfg.python);
        command
            .arg(&cfg.script)
            .arg("--engine")
            .arg(&cfg.engine)
            .arg("--kokoro-voice")
            .arg(&cfg.kokoro_voice)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(wav) = &cfg.voice_wav {
            command.arg("--voice-wav").arg(wav);
        }
        if let Some(lang) = &cfg.warm_lang {
            command.arg("--warm-lang").arg(lang);
        }
        if let Some(home) = &cfg.hf_home {
            command.env("HF_HOME", home);
        }

        let mut child = command.spawn().map_err(|e| {
            EngineError::Spawn(format!(
                "no se pudo lanzar el sintetizador con {}: {e}",
                cfg.python.display()
            ))
        })?;

        let stdin = child.stdin.take().ok_or(EngineError::Closed)?;
        let stdout = child.stdout.take().ok_or(EngineError::Closed)?;
        let stderr = child.stderr.take().ok_or(EngineError::Closed)?;

        let (tx, rx) = channel();
        let reader = spawn_reader(stdout, tx);
        spawn_logger(stderr);

        Ok(Self {
            child,
            stdin: Some(stdin),
            replies: rx,
            reader: Some(reader),
            next_id: 1,
        })
    }

    /// Bloquea hasta que el modelo termina de cargar. Chatterbox tarda ~21 s
    /// en frio (medido); sin esperar aqui, la primera frase se comeria ese
    /// arranque como si fuera latencia.
    pub fn wait_ready(&self, timeout: Duration) -> Result<TtsReady> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(EngineError::Spawn(
                    "el sintetizador no arranco a tiempo".to_string(),
                ));
            }
            match self.replies.recv_timeout(left) {
                Ok(Wire::Ready { device, rate }) => return Ok(TtsReady { device, rate }),
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Err(EngineError::Closed),
            }
        }
    }
}

fn spawn_reader<R: Read + Send + 'static>(stdout: R, tx: Sender<Wire>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("tts-sidecar-out".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Wire>(&line) {
                    Ok(msg) => {
                        if tx.send(msg).is_err() {
                            break;
                        }
                    }
                    Err(e) => tracing::warn!("linea ininteligible del sintetizador ({e}): {line}"),
                }
            }
        })
        .expect("spawn tts-sidecar-out")
}

fn spawn_logger<R: Read + Send + 'static>(stderr: R) {
    std::thread::Builder::new()
        .name("tts-sidecar-err".into())
        .spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(std::result::Result::ok) {
                tracing::info!(target: "sintetizador", "{line}");
            }
        })
        .expect("spawn tts-sidecar-err");
}

fn write_frame(out: &mut ChildStdin, payload: &[u8]) -> std::io::Result<()> {
    out.write_all(&(payload.len() as u32).to_le_bytes())?;
    out.write_all(&[FRAME_CONTROL])?;
    out.write_all(payload)?;
    out.flush()
}

/// PCM i16 little-endian -> f32 en [-1, 1].
fn decode_pcm(b64: &str) -> Result<Vec<f32>> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| EngineError::Spawn(format!("PCM ilegible del sintetizador: {e}")))?;
    Ok(bytes
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as f32 / 32768.0)
        .collect())
}

impl Synthesizer for TtsSidecar {
    fn synthesize(&mut self, text: &str, lang: &str) -> Result<Synthesized> {
        let id = self.next_id;
        self.next_id += 1;

        let request = serde_json::json!({
            "cmd": "speak", "id": id, "text": text, "lang": lang,
        });
        let stdin = self.stdin.as_mut().ok_or(EngineError::Closed)?;
        write_frame(stdin, request.to_string().as_bytes())?;

        // El id vuelve en la respuesta; las rezagadas se descartan sin
        // confundirlas con la que esperamos. Mismo esquema que el traductor.
        let deadline = Instant::now() + SPEAK_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(EngineError::Spawn(format!(
                    "la sintesis {id} no llego a tiempo"
                )));
            }
            match self.replies.recv_timeout(left) {
                Ok(Wire::Audio {
                    id: got,
                    pcm,
                    rate,
                    ms,
                }) if got == id => {
                    let samples = decode_pcm(&pcm)?;
                    tracing::debug!(
                        "bloque sintetizado en {ms} ms ({:.2}s de audio)",
                        samples.len() as f32 / rate.max(1) as f32
                    );
                    return Ok(Synthesized {
                        samples,
                        rate,
                        synth_ms: ms,
                    });
                }
                Ok(Wire::Error { id: got, message }) if got == id => {
                    return Err(EngineError::Spawn(message))
                }
                Ok(other) => {
                    tracing::debug!("respuesta rezagada del sintetizador: {other:?}");
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Err(EngineError::Closed),
            }
        }
    }

    fn shutdown(&mut self) -> Result<()> {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = write_frame(stdin, br#"{"cmd":"shutdown"}"#);
        }
        self.stdin.take();

        let deadline = Instant::now() + SHUTDOWN_GRACE;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    // Matar aqui no es opcional: un hijo de Python huerfano se
                    // queda agarrado a la VRAM (medido: uno retenia 11,6 GB).
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        Ok(())
    }
}

impl Drop for TtsSidecar {
    fn drop(&mut self) {
        if self.stdin.is_some() {
            let _ = self.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn los_locales_habituales_tienen_codigo_de_voz() {
        for locale in ["en-US", "es-ES", "de-DE", "fr-FR", "pt-BR", "ja-JP"] {
            assert!(tts_lang_code(locale).is_some(), "falta el mapeo de {locale}");
        }
    }

    #[test]
    fn los_locales_sin_voz_devuelven_none() {
        // Vietnamita y ucraniano tienen ASR y traduccion pero no voz: mejor
        // un error claro al arrancar que un sidecar fallando por frase.
        assert_eq!(tts_lang_code("vi-VN"), None);
        assert_eq!(tts_lang_code("uk-UA"), None);
        assert_eq!(tts_lang_code("auto"), None);
    }

    #[test]
    fn las_variantes_del_noruego_comparten_codigo() {
        assert_eq!(tts_lang_code("nb-NO"), Some("no"));
        assert_eq!(tts_lang_code("nn-NO"), Some("no"));
    }

    #[test]
    fn el_eco_reconoce_la_frase_aunque_el_asr_cambie_palabras() {
        let reg = EchoRegistry::new();
        reg.record(
            "The first part consists of capturing the system audio.",
            Duration::from_secs(30),
        );
        // El ASR oye casi lo mismo, con puntuacion y mayusculas distintas.
        assert!(reg.matches("the first part consists of capturing the system audio"));
        // Y con alguna palabra perdida por el camino.
        assert!(reg.matches("first part consists of capturing the audio"));
    }

    #[test]
    fn una_frase_distinta_no_cuenta_como_eco() {
        let reg = EchoRegistry::new();
        reg.record(
            "The first part consists of capturing the system audio.",
            Duration::from_secs(30),
        );
        assert!(!reg.matches("could you share your screen please"));
    }

    #[test]
    fn las_frases_cortas_nunca_son_eco() {
        // "yes" u "ok" las dice cualquiera; marcarlas de eco seria mentir.
        let reg = EchoRegistry::new();
        reg.record("yes ok fine", Duration::from_secs(30));
        assert!(!reg.matches("yes"));
        assert!(!reg.matches("ok fine"));
    }

    #[test]
    fn el_eco_caduca() {
        let reg = EchoRegistry::new();
        reg.record("this sentence expires immediately", Duration::from_millis(0));
        std::thread::sleep(Duration::from_millis(5));
        assert!(!reg.matches("this sentence expires immediately"));
    }

    /// Sintetizador de mentira: un segundo de silencio por peticion, al
    /// instante. Suficiente para probar el agrupador sin GPU.
    struct FakeSynth;

    impl Synthesizer for FakeSynth {
        fn synthesize(&mut self, _text: &str, _lang: &str) -> Result<Synthesized> {
            Ok(Synthesized {
                samples: vec![0.0; 24_000],
                rate: 24_000,
                synth_ms: 1,
            })
        }
        fn shutdown(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn pump_for_test(
        queued: Arc<AtomicU64>,
        render_tx: SyncSender<Vec<f32>>,
    ) -> (SpeechPump, std::sync::mpsc::Receiver<SpeechEvent>) {
        let (event_tx, event_rx) = channel();
        let pump = SpeechPump::new(
            Box::new(FakeSynth),
            SpeechPumpConfig {
                lang: "en".to_string(),
                group_max_chars: 250,
                group_max_wait_ms: 2000,
            },
            render_tx,
            queued,
            24_000,
            None,
            event_tx,
        );
        (pump, event_rx)
    }

    #[test]
    fn con_la_voz_callada_la_primera_frase_no_espera_al_agrupador() {
        // Esperar 2 s de agrupado con la cola vacia seria latencia pura.
        let queued = Arc::new(AtomicU64::new(0));
        let (render_tx, render_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(4);
        let (pump, _events) = pump_for_test(queued, render_tx);

        let (text_tx, text_rx) = channel::<String>();
        let stop = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let handle = std::thread::spawn(move || pump.run(text_rx, stop, alive));

        text_tx.send("Hola.".to_string()).expect("envia");
        // Mucho antes del group_max_wait_ms de 2000 tiene que haber audio.
        let audio = render_rx.recv_timeout(Duration::from_millis(700));
        assert!(audio.is_ok(), "la frase deberia sintetizarse sin esperar");

        drop(text_tx);
        let _ = handle.join();
    }

    #[test]
    fn con_audio_pendiente_las_frases_cortas_se_agrupan() {
        // Simula voz aun sonando: el contador de muestras pendientes no esta
        // a cero, asi que una frase corta debe esperar a agrupar.
        let queued = Arc::new(AtomicU64::new(48_000)); // 2 s sin reproducir
        let (render_tx, render_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(4);
        let (pump, _events) = pump_for_test(queued, render_tx);

        let (text_tx, text_rx) = channel::<String>();
        let stop = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let handle = std::thread::spawn(move || pump.run(text_rx, stop, alive));

        text_tx.send("Hola.".to_string()).expect("envia");
        let audio = render_rx.recv_timeout(Duration::from_millis(700));
        assert!(
            audio.is_err(),
            "con la voz sonando, una frase corta debe esperar al agrupador"
        );

        drop(text_tx);
        // Al cerrarse el canal, lo pendiente se dice entero.
        let audio = render_rx.recv_timeout(Duration::from_secs(2));
        assert!(audio.is_ok(), "lo pendiente debe salir al terminar");
        let _ = handle.join();
    }

    #[test]
    fn el_pcm_en_base64_vuelve_a_f32() {
        use base64::Engine as _;
        // Dos muestras: 0 y el maximo positivo.
        let raw: [u8; 4] = [0, 0, 0xFF, 0x7F];
        let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
        let samples = decode_pcm(&b64).expect("decodifica");
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0], 0.0);
        assert!((samples[1] - 0.99997).abs() < 1e-4);
    }
}
