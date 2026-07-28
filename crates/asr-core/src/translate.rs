//! Traduccion encadenada detras del ASR.
//!
//! El modelo de reconocimiento **no traduce**: transcribe en el idioma que oye
//! y punto (su vocabulario no tiene ni un token de tarea). Asi que la
//! traduccion es un segundo paso, y eso tiene dos consecuencias que conviene
//! tener presentes: los errores del ASR se propagan, y hay que esperar a que
//! la frase termine.
//!
//! Lo importante del diseno esta en [`SentenceSplitter`]: se traduce por
//! **frases completas**, no por cada fragmento que llega. Traducir "la lluvia
//! en Sevi..." da un resultado que cambia entero con cada palabra nueva.
//! Como el modelo emite puntuacion de forma nativa, esa es la senal de corte,
//! y no hay que esperar al silencio.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::engine::{EngineError, Result};

const FRAME_CONTROL: u8 = 0x02;

/// Si una frase tarda mas que esto, algo va mal: se descarta y se sigue.
const TRANSLATE_TIMEOUT: Duration = Duration::from_secs(30);

const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

// ------------------------------------------------------------------ trait

/// Traductor de texto. Espejo de [`crate::engine::AsrEngine`]: la sesion y la
/// interfaz hablan solo con esto, asi que cambiar NLLB por otra cosa (o por
/// una API) es implementarlo otra vez.
pub trait Translator: Send {
    /// `src` y `tgt` son codigos FLORES-200 (`spa_Latn`, `eng_Latn`, ...).
    fn translate(&mut self, text: &str, src: &str, tgt: &str) -> Result<String>;
    fn shutdown(&mut self) -> Result<()>;
}

pub trait TranslatorFactory: Send + Sync {
    fn build(&self) -> Result<Box<dyn Translator>>;
}

// ------------------------------------------------------- codigos de idioma

/// Locale del ASR -> codigo FLORES-200 de NLLB.
///
/// Los 41 estan comprobados contra el tokenizer: un codigo mal escrito no
/// falla, devuelve el token `unk` y traduce cualquier cosa, asi que mejor no
/// improvisarlos.
pub fn flores_code(locale: &str) -> Option<&'static str> {
    let code = match locale {
        "en-US" | "en-GB" | "en" => "eng_Latn",
        "es-ES" | "es-US" | "es" => "spa_Latn",
        "fr-FR" | "fr-CA" | "fr" => "fra_Latn",
        "it-IT" | "it" => "ita_Latn",
        "pt-BR" | "pt-PT" | "pt" => "por_Latn",
        "nl-NL" | "nl" => "nld_Latn",
        "de-DE" | "de" => "deu_Latn",
        "tr-TR" | "tr" => "tur_Latn",
        "ru-RU" | "ru" => "rus_Cyrl",
        "ar-AR" | "ar" => "arb_Arab",
        "hi-IN" | "hi" => "hin_Deva",
        "ja-JP" | "ja" => "jpn_Jpan",
        "ko-KR" | "ko" => "kor_Hang",
        "vi-VN" | "vi" => "vie_Latn",
        "uk-UA" | "uk" => "ukr_Cyrl",
        "pl-PL" | "pl" => "pol_Latn",
        "sv-SE" | "sv" => "swe_Latn",
        "cs-CZ" | "cs" => "ces_Latn",
        "nb-NO" | "nb" => "nob_Latn",
        "da-DK" | "da" => "dan_Latn",
        "bg-BG" | "bg" => "bul_Cyrl",
        "fi-FI" | "fi" => "fin_Latn",
        "hr-HR" | "hr" => "hrv_Latn",
        "sk-SK" | "sk" => "slk_Latn",
        "zh-CN" | "zh" => "zho_Hans",
        "zh-TW" => "zho_Hant",
        "hu-HU" | "hu" => "hun_Latn",
        "ro-RO" | "ro" => "ron_Latn",
        "et-EE" | "et" => "est_Latn",
        "el-GR" | "el" => "ell_Grek",
        "lt-LT" | "lt" => "lit_Latn",
        "lv-LV" | "lv" => "lvs_Latn",
        "mt-MT" | "mt" => "mlt_Latn",
        "sl-SI" | "sl" => "slv_Latn",
        "he-IL" | "he" => "heb_Hebr",
        "th-TH" | "th" => "tha_Thai",
        "nn-NO" | "nn" => "nno_Latn",
        _ => return None,
    };
    Some(code)
}

// ------------------------------------------------------- corte por frases

/// Acumula texto suelto y va soltando frases completas.
#[derive(Debug, Default)]
pub struct SentenceSplitter {
    buf: String,
}

impl SentenceSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Anade texto y devuelve las frases que hayan quedado cerradas.
    pub fn push(&mut self, text: &str) -> Vec<String> {
        self.buf.push_str(text);
        let mut out = Vec::new();

        while let Some(end) = self.find_break() {
            let rest = self.buf.split_off(end);
            let sentence = std::mem::replace(&mut self.buf, rest);
            let sentence = sentence.trim().to_string();
            if !sentence.is_empty() {
                out.push(sentence);
            }
            // El espacio que separaba las frases ya no pinta nada, y dejarlo
            // haria que `pending()` mintiera diciendo que hay texto a medias.
            let start = self.buf.len() - self.buf.trim_start().len();
            if start > 0 {
                self.buf.drain(..start);
            }
        }
        out
    }

    /// Suelta lo que quede sin cerrar, por ejemplo al terminar el segmento.
    pub fn flush(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.buf).trim().to_string();
        (!rest.is_empty()).then_some(rest)
    }

    pub fn pending(&self) -> &str {
        &self.buf
    }

    /// Posicion tras el final de la primera frase completa, si la hay.
    fn find_break(&self) -> Option<usize> {
        let bytes: Vec<(usize, char)> = self.buf.char_indices().collect();
        for (i, (offset, ch)) in bytes.iter().enumerate() {
            if !matches!(ch, '.' | '?' | '!' | '\u{2026}') {
                continue;
            }

            // Tras el signo tiene que venir espacio o final de texto; si no,
            // aun no sabemos si la frase acaba ahi.
            let next = bytes.get(i + 1);
            match next {
                None => return None, // final del buffer: puede llegar mas texto
                Some((_, c)) if !c.is_whitespace() => continue,
                _ => {}
            }

            // No cortar dentro de un numero decimal ("3.5").
            let prev_digit = i
                .checked_sub(1)
                .and_then(|j| bytes.get(j))
                .is_some_and(|(_, c)| c.is_ascii_digit());
            let next_digit = bytes[i + 1..]
                .iter()
                .find(|(_, c)| !c.is_whitespace())
                .is_some_and(|(_, c)| c.is_ascii_digit());
            if prev_digit && next_digit {
                continue;
            }

            return Some(offset + ch.len_utf8());
        }
        None
    }
}

// ------------------------------------------------------------ orquestacion

/// Una frase traducida junto a su original.
///
/// `paragraph` agrupa las frases del mismo parrafo: la interfaz junta las que
/// comparten id y las pinta como un bloque. Asi cada frase aparece en cuanto
/// esta lista (~160 ms) en vez de esperar a que el parrafo entero se cierre,
/// pero se sigue viendo por parrafos.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TranslatedLine {
    pub source: asr_audio::Source,
    pub paragraph: u64,
    pub at_ms: u64,
    pub original: String,
    pub translated: String,
    /// La frase es la propia voz sintetica volviendo por la captura del
    /// sistema. No se re-traduce (es->en->es produce cosas raras): se deja
    /// tal cual y la interfaz la marca. `default` para que los TOML e
    /// historiales de antes de este campo sigan cargando.
    #[serde(default)]
    pub echo: bool,
}

/// Convierte el flujo de eventos de sesion en traducciones.
///
/// Traduce **cada frase en cuanto se cierra**, y las etiqueta con el parrafo al
/// que pertenecen para que la interfaz las agrupe. Esa combinacion sale de dos
/// intentos fallidos:
///
/// 1. Traducir por frases y *mostrarlas* por frases: quedaba troceado y no
///    encajaba con la transcripcion, que va por parrafos.
/// 2. Esperar a que el parrafo cerrara y traducirlo entero: NLLB se comia
///    frases (esta entrenado a nivel de frase), y sobre todo la traduccion
///    tardaba una eternidad en aparecer, porque no salia nada hasta el cierre.
///
/// Asi se tienen las dos cosas: latencia de una frase y presentacion por
/// parrafos. Ojo: [`TranslationPump::handle`] **bloquea** mientras traduce, asi
/// que conviene darle su propio hilo y no llamarlo desde el que pinta la interfaz.
pub struct TranslationPump {
    translator: Box<dyn Translator>,
    /// Por fuente: id de parrafo, instante de inicio y frases a medias.
    state: std::collections::HashMap<asr_audio::Source, SourceState>,
    next_paragraph: u64,
    src: String,
    tgt: String,
    /// Memoria de lo que la voz sintetica acaba de decir. Solo cuando la voz
    /// esta activada; ver [`crate::speak::EchoRegistry`].
    echo: Option<std::sync::Arc<crate::speak::EchoRegistry>>,
}

#[derive(Default)]
struct SourceState {
    paragraph: u64,
    at_ms: u64,
    splitter: SentenceSplitter,
}

impl TranslationPump {
    /// `src_locale` y `tgt_locale` son locales del ASR (`es-ES`), no codigos
    /// FLORES; la conversion se hace aqui.
    pub fn new(
        translator: Box<dyn Translator>,
        src_locale: &str,
        tgt_locale: &str,
    ) -> Result<Self> {
        let src = flores_code(src_locale).ok_or_else(|| {
            EngineError::Spawn(format!(
                "no se puede traducir desde {src_locale}: elige un idioma concreto \
                 en vez de la deteccion automatica, el traductor necesita saber cual es"
            ))
        })?;
        let tgt = flores_code(tgt_locale)
            .ok_or_else(|| EngineError::Spawn(format!("idioma destino desconocido: {tgt_locale}")))?;

        Ok(Self {
            translator,
            state: std::collections::HashMap::new(),
            next_paragraph: 1,
            src: src.to_string(),
            tgt: tgt.to_string(),
            echo: None,
        })
    }

    /// Activa el reconocimiento de la propia voz sintetica. Las frases del
    /// sistema que coincidan con algo recien hablado se marcan como eco en
    /// vez de re-traducirse.
    pub fn with_echo_registry(
        mut self,
        registry: std::sync::Arc<crate::speak::EchoRegistry>,
    ) -> Self {
        self.echo = Some(registry);
        self
    }

    pub fn handle(&mut self, event: &crate::session::SessionEvent) -> Vec<TranslatedLine> {
        use crate::session::SessionEvent as Ev;

        match event {
            Ev::Delta { source, at_ms, text } => {
                let next = self.next_paragraph;
                let state = self.state.entry(*source).or_insert_with(|| SourceState {
                    paragraph: next,
                    at_ms: *at_ms,
                    splitter: SentenceSplitter::new(),
                });
                if state.paragraph == next {
                    self.next_paragraph += 1;
                }
                let sentences = state.splitter.push(text);
                let (paragraph, start_ms) = (state.paragraph, state.at_ms);
                self.translate_all(*source, paragraph, start_ms, sentences)
            }
            Ev::SegmentEnd { source, .. } => {
                let Some(state) = self.state.get_mut(source) else {
                    return Vec::new();
                };
                // Lo que quede sin punto final tambien se traduce: el habla no
                // siempre acaba en una frase bien cerrada.
                let rest: Vec<String> = state.splitter.flush().into_iter().collect();
                let (paragraph, start_ms) = (state.paragraph, state.at_ms);
                let out = self.translate_all(*source, paragraph, start_ms, rest);
                // El siguiente parrafo de esta fuente empieza de cero.
                self.state.remove(source);
                out
            }
            _ => Vec::new(),
        }
    }

    /// Traduce frase a frase. NLLB esta entrenado a nivel de frase: con un
    /// parrafo entero se come contenido (medido: de "…capturar el audio del
    /// sistema. Eso ya funciona bien." solo devolvia la primera).
    fn translate_all(
        &mut self,
        source: asr_audio::Source,
        paragraph: u64,
        at_ms: u64,
        sentences: Vec<String>,
    ) -> Vec<TranslatedLine> {
        let mut out = Vec::new();
        for sentence in sentences {
            // La propia voz sintetica volviendo no se re-traduce: es->en->es
            // nunca devuelve lo que se dijo. Se deja la frase tal cual,
            // marcada, y la interfaz decide como pintarla. Se comprueba en
            // LAS DOS fuentes a proposito: por el sistema vuelve cuando la
            // reunion la reenvia, y por el MICROFONO cuando suena por los
            // altavoces (la salida predeterminada) y el micro la recoge —
            // ese caso ademas se volveria a hablar, y eso es un bucle de
            // realimentacion hablandose a si mismo.
            if let Some(echo) = &self.echo {
                if echo.matches(&sentence) {
                    out.push(TranslatedLine {
                        source,
                        paragraph,
                        at_ms,
                        translated: sentence.clone(),
                        original: sentence,
                        echo: true,
                    });
                    continue;
                }
            }
            match self.translator.translate(&sentence, &self.src, &self.tgt) {
                Ok(translated) => out.push(TranslatedLine {
                    source,
                    paragraph,
                    at_ms,
                    original: sentence,
                    translated,
                    echo: false,
                }),
                // Una frase que falla no debe tirar el parrafo entero.
                Err(e) => tracing::warn!("no se pudo traducir {sentence:?}: {e}"),
            }
        }
        out
    }

    pub fn shutdown(&mut self) {
        if let Err(e) = self.translator.shutdown() {
            tracing::warn!("el traductor no cerro limpiamente: {e}");
        }
    }
}

// ------------------------------------------------------ sidecar de Python

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MtConfig {
    pub python: PathBuf,
    pub script: PathBuf,
    pub dtype: String,
    /// Valor para `HF_HOME`, si los modelos no estan en la cache por defecto.
    pub hf_home: Option<PathBuf>,
}

impl Default for MtConfig {
    fn default() -> Self {
        Self {
            python: PathBuf::from("python"),
            script: PathBuf::from("sidecar/mt_server.py"),
            dtype: "bfloat16".to_string(),
            hf_home: None,
        }
    }
}

impl TranslatorFactory for MtConfig {
    fn build(&self) -> Result<Box<dyn Translator>> {
        Ok(Box::new(MtSidecar::spawn(self)?))
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Wire {
    Ready { device: String },
    Translation { id: u64, text: String, ms: u64 },
    Error { id: u64, message: String },
}

pub struct MtSidecar {
    child: Child,
    stdin: Option<ChildStdin>,
    replies: Receiver<Wire>,
    reader: Option<JoinHandle<()>>,
    next_id: u64,
}

impl MtSidecar {
    pub fn spawn(cfg: &MtConfig) -> Result<Self> {
        let mut command = Command::new(&cfg.python);
        command
            .arg(&cfg.script)
            .arg("--dtype")
            .arg(&cfg.dtype)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(home) = &cfg.hf_home {
            command.env("HF_HOME", home);
        }

        let mut child = command.spawn().map_err(|e| {
            EngineError::Spawn(format!(
                "no se pudo lanzar el traductor con {}: {e}",
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

    /// Bloquea hasta que el modelo termina de cargar. Sin esto la primera
    /// frase se comeria los segundos de arranque como si fuera latencia.
    pub fn wait_ready(&self, timeout: Duration) -> Result<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(EngineError::Spawn(
                    "el traductor no arranco a tiempo".to_string(),
                ));
            }
            match self.replies.recv_timeout(left) {
                Ok(Wire::Ready { device }) => return Ok(device),
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Err(EngineError::Closed),
            }
        }
    }
}

fn spawn_reader<R: Read + Send + 'static>(stdout: R, tx: Sender<Wire>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("mt-sidecar-out".into())
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
                    Err(e) => tracing::warn!("linea ininteligible del traductor ({e}): {line}"),
                }
            }
        })
        .expect("spawn mt-sidecar-out")
}

fn spawn_logger<R: Read + Send + 'static>(stderr: R) {
    std::thread::Builder::new()
        .name("mt-sidecar-err".into())
        .spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(std::result::Result::ok) {
                tracing::info!(target: "traductor", "{line}");
            }
        })
        .expect("spawn mt-sidecar-err");
}

fn write_frame(out: &mut ChildStdin, payload: &[u8]) -> std::io::Result<()> {
    out.write_all(&(payload.len() as u32).to_le_bytes())?;
    out.write_all(&[FRAME_CONTROL])?;
    out.write_all(payload)?;
    out.flush()
}

impl Translator for MtSidecar {
    fn translate(&mut self, text: &str, src: &str, tgt: &str) -> Result<String> {
        let id = self.next_id;
        self.next_id += 1;

        let request = serde_json::json!({
            "cmd": "translate", "id": id, "text": text, "src": src, "tgt": tgt,
        });
        let stdin = self.stdin.as_mut().ok_or(EngineError::Closed)?;
        write_frame(stdin, request.to_string().as_bytes())?;

        // El id vuelve en la respuesta, asi que las rezagadas se descartan sin
        // confundirlas con la que esperamos.
        let deadline = Instant::now() + TRANSLATE_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(EngineError::Spawn(format!(
                    "la traduccion {id} no llego a tiempo"
                )));
            }
            match self.replies.recv_timeout(left) {
                Ok(Wire::Translation { id: got, text, ms }) if got == id => {
                    tracing::debug!("frase traducida en {ms} ms");
                    return Ok(text);
                }
                Ok(Wire::Error { id: got, message }) if got == id => {
                    return Err(EngineError::Spawn(message))
                }
                Ok(other) => {
                    tracing::debug!("respuesta rezagada del traductor: {other:?}");
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

impl Drop for MtSidecar {
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
    fn suelta_la_frase_cuando_llega_el_punto_y_un_espacio() {
        let mut s = SentenceSplitter::new();
        assert!(s.push("La lluvia en Sevilla").is_empty());
        // Sin espacio detras no sabemos si la frase acabo.
        assert!(s.push(" es una maravilla.").is_empty());
        assert_eq!(s.push(" Y "), vec!["La lluvia en Sevilla es una maravilla."]);
    }

    #[test]
    fn corta_varias_frases_de_una_tacada() {
        let mut s = SentenceSplitter::new();
        let frases = s.push("Uno. Dos! Tres? ");
        assert_eq!(frases, vec!["Uno.", "Dos!", "Tres?"]);
        assert_eq!(s.pending(), "");
    }

    #[test]
    fn no_parte_los_numeros_decimales() {
        let mut s = SentenceSplitter::new();
        assert!(s.push("Son 3.5 grados").is_empty());
        assert_eq!(s.push(" ahora. Fin. "), vec!["Son 3.5 grados ahora.", "Fin."]);
    }

    #[test]
    fn flush_devuelve_lo_que_quedo_colgando() {
        let mut s = SentenceSplitter::new();
        s.push("Frase sin terminar");
        assert_eq!(s.flush().as_deref(), Some("Frase sin terminar"));
        assert_eq!(s.flush(), None);
    }

    #[test]
    fn aguanta_acentos_y_signos_multibyte() {
        let mut s = SentenceSplitter::new();
        let frases = s.push("¿Qué tal estás? Bien… ");
        assert_eq!(frases, vec!["¿Qué tal estás?", "Bien…"]);
    }

    #[test]
    fn el_espacio_suelto_no_genera_frases_vacias() {
        let mut s = SentenceSplitter::new();
        assert!(s.push("   ").is_empty());
        assert_eq!(s.push("... "), vec!["..."]);
    }

    #[test]
    fn los_locales_del_asr_tienen_codigo_flores() {
        // Los 19 "listos para transcribir" del model card son los que mas
        // importa que funcionen.
        for locale in [
            "en-US", "en-GB", "es-US", "es-ES", "fr-FR", "fr-CA", "it-IT", "pt-BR", "pt-PT",
            "nl-NL", "de-DE", "tr-TR", "ru-RU", "ar-AR", "hi-IN", "ja-JP", "ko-KR", "vi-VN",
            "uk-UA",
        ] {
            assert!(flores_code(locale).is_some(), "falta el mapeo de {locale}");
        }
    }

    #[test]
    fn auto_no_tiene_codigo_porque_nllb_necesita_saber_el_origen() {
        assert_eq!(flores_code("auto"), None);
        assert_eq!(flores_code("klingon"), None);
    }

    #[test]
    fn las_variantes_de_un_idioma_comparten_codigo() {
        assert_eq!(flores_code("es-ES"), flores_code("es-US"));
        assert_eq!(flores_code("pt-BR"), flores_code("pt-PT"));
        // Salvo el chino, que cambia de escritura.
        assert_ne!(flores_code("zh-CN"), flores_code("zh-TW"));
    }
}
