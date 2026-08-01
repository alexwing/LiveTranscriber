//! Configuracion persistente en TOML, al estilo de `tapo-config.toml`.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Interprete del venv donde estan torch y transformers.
    pub python: PathBuf,
    /// Ruta a `asr_server.py`.
    pub script: PathBuf,
    /// Ruta a `mt_server.py`, el sidecar de traduccion.
    pub mt_script: PathBuf,
    /// Donde viven los modelos descargados (`HF_HOME`). `None` = la caché por
    /// defecto de Hugging Face, en `%USERPROFILE%\.cache\huggingface`.
    ///
    /// Son ~7 GB entre los dos modelos, asi que en un equipo con el disco C:
    /// pequeño conviene moverlos. Se pasa como variable de entorno a los
    /// sidecars, que es la unica forma de que la vea la libreria de Python.
    pub hf_home: Option<PathBuf>,

    /// Traducir en paralelo a la transcripcion.
    pub translate: bool,
    /// A que se traduce lo de la **sala** (lo que yo leo). El nombre del
    /// campo viene de cuando solo habia una direccion de traduccion; se
    /// mantiene para que las configuraciones existentes sigan cargando.
    pub target_language: String,

    /// Idioma de la **sala** (`en-US`, ...) o `auto`: lo que suena en el
    /// sistema. Con traduccion no puede ser `auto`: NLLB necesita saberlo.
    pub language: String,

    /// Idioma del **microfono**: el que hablo yo. Vacio = el mismo al que se
    /// traduce la sala ([`Self::target_language`]), que es lo normal: leo y
    /// hablo en mi idioma. Se persiste solo si el usuario lo cambia.
    pub mic_language: String,
    /// A que se traduce el microfono — y por tanto **lo que pronuncia la voz
    /// sintetica**. Vacio = el idioma de la sala: les hablo en el suyo.
    pub mic_target_language: String,
    /// 0, 3, 6 o 13.
    pub lookahead: u8,
    pub dtype: String,

    /// Transcribir lo que suena en el sistema.
    pub capture_system: bool,
    /// Transcribir tambien el microfono.
    pub capture_mic: bool,
    /// `None` = dispositivo predeterminado.
    pub system_device_id: Option<String>,
    pub mic_device_id: Option<String>,

    /// Cuantos dB por debajo del habla reciente cuenta como silencio. Relativo
    /// a proposito: el nivel absoluto depende del volumen de Windows, la
    /// diferencia entre voz y pausa no.
    pub gate_drop_db: f32,
    /// Suelo absoluto en dBFS, solo para el silencio digital.
    pub gate_floor_dbfs: f32,
    /// Segundos de silencio antes de dejar de alimentar al modelo. Solo afecta
    /// al gasto de GPU.
    pub gate_hold_secs: f32,

    /// Segundos **sin texto nuevo** que cierran un parrafo. Se mira el texto
    /// que suelta el modelo, no el volumen, para que funcione con musica de
    /// fondo: la musica no genera transcripcion.
    pub paragraph_idle_secs: f32,
    /// Tope de duracion de un parrafo, para que un monologo sin pausas no sea
    /// un unico bloque interminable.
    pub paragraph_max_secs: f32,
    /// Compensar el volumen del sistema. Dejalo activado salvo que sepas lo
    /// que haces: el loopback captura post-volumen y sin esto un volumen bajo
    /// deja la transcripcion en nada.
    pub normalize_gain: bool,

    pub hotkey_toggle: String,
    pub hotkey_overlay: String,
    pub overlay_enabled: bool,

    /// Carpeta donde se guardan las transcripciones. Debe ser **absoluta**: una
    /// ruta relativa dependeria del directorio de trabajo, y ese no es de fiar
    /// (con `tauri dev` es `src-tauri/`, y ahi acabaron las primeras sin que
    /// nadie lo pidiera). Ver [`AppConfig::output_dir_absolute`].
    pub output_dir: PathBuf,
    /// Nombre base del fichero. El nombre final lleva la fecha delante:
    /// `AAAA_MM_DD_<nombre>.<ext>`. Ver [`AppConfig::output_filename`].
    pub output_name: String,

    /// Voz sintetica: hablar la traduccion de lo que dices por el microfono.
    /// Es una funcion opcional con su propia seccion `[speak]` en el TOML;
    /// apagada no cuesta nada (ni proceso, ni VRAM).
    pub speak: SpeakConfig,
}

/// Configuracion de la voz sintetica. Va aparte del resto porque es una
/// funcion opcional entera: se activa sola, con sus propias dependencias
/// (otro venv, otro modelo) y su propio dispositivo de salida.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SpeakConfig {
    /// Interruptor general. Exige ademas `translate = true` y
    /// `capture_mic = true`: lo que se habla es la traduccion de tu micro.
    pub enabled: bool,
    /// `chatterbox` (tu voz clonada, 23 idiomas, ~3,4 GB de VRAM, en el
    /// filo de tiempo real) o `kokoro` (voz neutra, 8 idiomas, ~0,6 GB,
    /// 40x tiempo real).
    pub engine: String,
    /// Interprete del venv de voz. **No** es el del ASR: el ASR exige
    /// transformers>=5.13 (AutoModelForRNNT) y chatterbox-tts esta probado
    /// con 4.57.x, asi que no caben en el mismo entorno.
    pub python: PathBuf,
    /// Ruta a `tts_server.py`.
    pub script: PathBuf,
    /// Muestra de tu voz para clonar: 10-30 s de habla limpia en WAV.
    /// Solo la usa chatterbox.
    pub voice_wav: Option<PathBuf>,
    /// Voz preajustada de kokoro (`af_heart`, `ef_dora`, `em_alex`, ...).
    pub kokoro_voice: String,
    /// Dispositivo de salida. Para hacer de microfono virtual aqui va el id
    /// de `CABLE Input` (VB-CABLE); `None` = el predeterminado, que suena
    /// por los altavoces y en general no es lo que se quiere.
    pub output_device_id: Option<String>,
    /// Caracteres a juntar antes de sintetizar. Chatterbox tiene ~1 s de
    /// coste fijo por llamada (medido): con frases sueltas queda por debajo
    /// de tiempo real y el retraso crece sin limite; con bloques de ~250
    /// caracteres pasa de 1x y queda acotado.
    pub group_max_chars: usize,
    /// La frase mas vieja no espera mas que esto, aunque el bloque sea corto.
    pub group_max_wait_ms: u64,
    /// Reconocer la propia voz sintetica si vuelve por la captura del
    /// sistema, y marcarla en vez de re-traducirla (es->en->es sale raro).
    pub mark_echo: bool,
}

impl Default for SpeakConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            engine: "chatterbox".to_string(),
            python: PathBuf::from(r"E:\projects\voicebox\backend\venv\Scripts\python.exe"),
            script: PathBuf::from("sidecar/tts_server.py"),
            voice_wav: None,
            kokoro_voice: "af_heart".to_string(),
            output_device_id: None,
            group_max_chars: 250,
            group_max_wait_ms: 2000,
            mark_echo: true,
        }
    }
}

/// Nombre a usar cuando el configurado no deja nada utilizable.
const FALLBACK_NAME: &str = "transcripcion";

/// Caracteres que Windows no admite en un nombre de fichero.
const FORBIDDEN: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Deja un nombre escrito por el usuario en algo valido como nombre de fichero.
///
/// Sustituye lo prohibido por `_`, quita los espacios de los extremos y evita
/// que quede vacio. Tambien colapsa los `_` repetidos, que si no aparecen en
/// cuanto alguien escribe una ruta entera en la casilla.
pub fn sanitize_file_stem(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_underscore = false;
    for ch in name.trim().chars() {
        let ch = if FORBIDDEN.contains(&ch) || ch.is_control() {
            '_'
        } else {
            ch
        };
        if ch == '_' {
            if last_underscore {
                continue;
            }
            last_underscore = true;
        } else {
            last_underscore = false;
        }
        out.push(ch);
    }
    let out = out.trim().trim_matches('_').trim().to_string();
    if out.is_empty() {
        FALLBACK_NAME.to_string()
    } else {
        out
    }
}

/// Sufijo y extension segun el formato de exportacion.
fn format_parts(format: &str) -> (&'static str, &'static str) {
    match format {
        "srt" => ("", "srt"),
        "translated-srt" => ("_traducida", "srt"),
        "bilingual" => ("_bilingue", "txt"),
        _ => ("", "txt"),
    }
}

/// Construye el nombre a partir de una fecha ya formateada. Existe aparte para
/// poder probarlo sin depender de que dia sea hoy.
fn compose_filename(date: &str, name: &str, format: &str) -> String {
    let (suffix, ext) = format_parts(format);
    format!("{date}_{}{suffix}.{ext}", sanitize_file_stem(name))
}

/// Carpeta por defecto: `Documentos\LiveTranscriber` del usuario.
pub fn default_output_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(|home| {
            PathBuf::from(home)
                .join("Documents")
                .join("LiveTranscriber")
        })
        .unwrap_or_else(|| PathBuf::from("."))
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            python: PathBuf::from(
                r"E:\projects\nemotron-3.5-asr-streaming-0.6b\.venv\Scripts\python.exe",
            ),
            script: PathBuf::from("sidecar/asr_server.py"),
            mt_script: PathBuf::from("sidecar/mt_server.py"),
            hf_home: None,
            translate: false,
            target_language: "en-US".to_string(),
            language: "auto".to_string(),
            mic_language: String::new(),
            mic_target_language: String::new(),
            lookahead: 3,
            dtype: "bfloat16".to_string(),
            capture_system: true,
            capture_mic: false,
            system_device_id: None,
            mic_device_id: None,
            gate_drop_db: 25.0,
            gate_floor_dbfs: -80.0,
            gate_hold_secs: 2.0,
            paragraph_idle_secs: 1.2,
            paragraph_max_secs: 30.0,
            normalize_gain: true,
            hotkey_toggle: "CmdOrControl+Shift+T".to_string(),
            hotkey_overlay: "CmdOrControl+Shift+O".to_string(),
            overlay_enabled: false,
            output_dir: default_output_dir(),
            output_name: "transcripcion".to_string(),
            speak: SpeakConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&raw)?)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Carpeta de salida en absoluto.
    ///
    /// Si la configurada es relativa se ignora y se usa la de por defecto: una
    /// ruta relativa se resolveria contra el directorio de trabajo, que cambia
    /// segun como se lance la app, y el usuario no tendria forma de saber donde
    /// han acabado sus ficheros.
    pub fn output_dir_absolute(&self) -> PathBuf {
        if self.output_dir.is_absolute() {
            self.output_dir.clone()
        } else {
            default_output_dir()
        }
    }

    /// Igual que [`Self::output_dir_absolute`] pero creando la carpeta.
    pub fn ensure_output_dir(&self) -> std::io::Result<PathBuf> {
        let dir = self.output_dir_absolute();
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Nombre del fichero para hoy: `AAAA_MM_DD_<nombre>[_sufijo].<ext>`.
    ///
    /// La fecha es local, no UTC: si son las 00:30 en Madrid, el fichero debe
    /// llevar la fecha de hoy aqui, no la de ayer en Greenwich.
    pub fn output_filename(&self, format: &str) -> String {
        let date = chrono::Local::now().format("%Y_%m_%d").to_string();
        compose_filename(&date, &self.output_name, format)
    }

    /// Ruta libre dentro de la carpeta de salida, creandola si hace falta.
    ///
    /// Si ya existe un fichero con ese nombre se añade `_2`, `_3`… No se
    /// sobreescribe: dos exportaciones del mismo dia con el mismo nombre son
    /// normales, y perder la primera en silencio no lo es.
    pub fn next_output_path(&self, format: &str) -> std::io::Result<PathBuf> {
        let dir = self.ensure_output_dir()?;
        let filename = self.output_filename(format);
        let path = dir.join(&filename);
        if !path.exists() {
            return Ok(path);
        }

        let stem = std::path::Path::new(&filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(FALLBACK_NAME)
            .to_string();
        let ext = std::path::Path::new(&filename)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("txt")
            .to_string();

        for n in 2..10_000 {
            let candidate = dir.join(format!("{stem}_{n}.{ext}"));
            if !candidate.exists() {
                return Ok(candidate);
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("demasiados ficheros llamados {stem} en {}", dir.display()),
        ))
    }

    pub fn mt(&self) -> crate::translate::MtConfig {
        crate::translate::MtConfig {
            python: self.python.clone(),
            script: self.mt_script.clone(),
            dtype: self.dtype.clone(),
            hf_home: self.hf_home.clone(),
        }
    }

    pub fn sidecar(&self) -> crate::sidecar::SidecarConfig {
        crate::sidecar::SidecarConfig {
            python: self.python.clone(),
            script: self.script.clone(),
            language: self.language.clone(),
            lookahead: self.lookahead,
            dtype: self.dtype.clone(),
            hf_home: self.hf_home.clone(),
        }
    }

    /// Idioma en que transcribir **el microfono**. Si no se ha elegido uno,
    /// con traduccion cae al idioma en que leo la sala (en una reunion en
    /// ingles yo sigo hablando espanol) y sin traduccion se transcribe igual
    /// que la sala, como siempre.
    pub fn mic_asr_language(&self) -> String {
        if !self.mic_language.is_empty() {
            return self.mic_language.clone();
        }
        if self.translate {
            self.target_language.clone()
        } else {
            self.language.clone()
        }
    }

    /// Idioma que pronuncia la **voz sintetica**: aquel al que se traduce el
    /// microfono. Si no se ha elegido uno, el de la sala.
    pub fn voice_language(&self) -> String {
        if !self.mic_target_language.is_empty() {
            return self.mic_target_language.clone();
        }
        self.language.clone()
    }

    pub fn tts(&self) -> crate::speak::TtsConfig {
        crate::speak::TtsConfig {
            python: self.speak.python.clone(),
            script: self.speak.script.clone(),
            engine: self.speak.engine.clone(),
            voice_wav: self.speak.voice_wav.clone(),
            kokoro_voice: self.speak.kokoro_voice.clone(),
            // La voz pronuncia el idioma al que se traduce el microfono;
            // precalentarlo mueve la carga perezosa de kokoro al arranque.
            warm_lang: crate::speak::tts_lang_code(&self.voice_language())
                .map(str::to_string),
            hf_home: self.hf_home.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_toml_da_la_vuelta_completa() {
        let cfg = AppConfig::default();
        let text = toml::to_string_pretty(&cfg).expect("serializa");
        let back: AppConfig = toml::from_str(&text).expect("deserializa");
        assert_eq!(back.lookahead, cfg.lookahead);
        assert_eq!(back.language, cfg.language);
        assert_eq!(back.gate_drop_db, cfg.gate_drop_db);
        assert_eq!(back.gate_floor_dbfs, cfg.gate_floor_dbfs);
        assert_eq!(back.target_language, cfg.target_language);
    }

    #[test]
    fn un_toml_parcial_completa_con_los_valores_por_defecto() {
        let cfg: AppConfig = toml::from_str(r#"language = "es-ES""#).expect("parsea");
        assert_eq!(cfg.language, "es-ES");
        assert_eq!(cfg.lookahead, 3, "el resto sale del Default");
    }

    #[test]
    fn una_ruta_de_salida_relativa_se_descarta() {
        // Las primeras versiones traian output_dir = "." y las transcripciones
        // acababan en el directorio de trabajo del proceso, que con `tauri dev`
        // es src-tauri/. Una relativa ya no se respeta.
        let cfg: AppConfig = toml::from_str(r#"output_dir = ".""#).expect("parsea");
        assert!(cfg.output_dir_absolute().is_absolute());
        assert_eq!(cfg.output_dir_absolute(), default_output_dir());
    }

    #[test]
    fn el_nombre_lleva_la_fecha_delante() {
        assert_eq!(
            compose_filename("2026_07_28", "reunion", "txt"),
            "2026_07_28_reunion.txt"
        );
        assert_eq!(
            compose_filename("2026_07_28", "reunion", "srt"),
            "2026_07_28_reunion.srt"
        );
    }

    #[test]
    fn cada_formato_tiene_su_sufijo() {
        assert_eq!(
            compose_filename("2026_07_28", "clase", "translated-srt"),
            "2026_07_28_clase_traducida.srt"
        );
        assert_eq!(
            compose_filename("2026_07_28", "clase", "bilingual"),
            "2026_07_28_clase_bilingue.txt"
        );
    }

    #[test]
    fn los_caracteres_que_windows_no_admite_se_sustituyen() {
        assert_eq!(sanitize_file_stem("con/barra"), "con_barra");
        assert_eq!(sanitize_file_stem("dos:puntos?"), "dos_puntos");
        assert_eq!(sanitize_file_stem(r"C:\ruta\entera"), "C_ruta_entera");
    }

    #[test]
    fn un_nombre_vacio_o_solo_simbolos_cae_al_de_reserva() {
        assert_eq!(sanitize_file_stem(""), "transcripcion");
        assert_eq!(sanitize_file_stem("   "), "transcripcion");
        assert_eq!(sanitize_file_stem("///"), "transcripcion");
    }

    #[test]
    fn se_respetan_espacios_y_acentos_de_dentro() {
        assert_eq!(
            sanitize_file_stem("reunión de dirección"),
            "reunión de dirección"
        );
    }

    #[test]
    fn no_sobreescribe_si_ya_existe_ese_nombre_hoy() {
        let dir = std::env::temp_dir().join("livetranscriber-test-nombres");
        let _ = std::fs::remove_dir_all(&dir);

        let mut cfg = AppConfig::default();
        cfg.output_dir = dir.clone();
        cfg.output_name = "prueba".to_string();

        let first = cfg.next_output_path("txt").expect("primera ruta");
        std::fs::write(&first, "contenido").expect("escribe");

        let second = cfg.next_output_path("txt").expect("segunda ruta");
        assert_ne!(first, second, "no debe reutilizar la misma ruta");
        assert!(
            second.file_name().unwrap().to_str().unwrap().contains("_2."),
            "la segunda deberia acabar en _2, fue {second:?}"
        );
        // La primera sigue intacta.
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "contenido");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_output_dir_crea_la_carpeta_si_no_existe() {
        let base = std::env::temp_dir().join("livetranscriber-test-outdir");
        let _ = std::fs::remove_dir_all(&base);
        let nested = base.join("sub").join("carpeta");

        let mut cfg = AppConfig::default();
        cfg.output_dir = nested.clone();
        let dir = cfg.ensure_output_dir().expect("la crea");

        assert_eq!(dir, nested);
        assert!(nested.is_dir(), "deberia existir tras la llamada");
        // Llamarlo de nuevo no debe fallar.
        assert!(cfg.ensure_output_dir().is_ok());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn una_ruta_de_salida_absoluta_se_respeta() {
        let cfg: AppConfig =
            toml::from_str("output_dir = 'D:\\\\mis transcripciones'").expect("parsea");
        assert_eq!(
            cfg.output_dir_absolute(),
            PathBuf::from("D:\\mis transcripciones")
        );
    }

    #[test]
    fn el_micro_hereda_el_espejo_de_la_sala_si_no_se_elige() {
        // Sala en ingles traducida al espanol: sin tocar nada, el micro se
        // transcribe en espanol y su traduccion (la voz) sale en ingles.
        let cfg: AppConfig = toml::from_str(
            "language = \"en-US\"\ntarget_language = \"es-ES\"\ntranslate = true\n",
        )
        .expect("parsea");
        assert_eq!(cfg.mic_asr_language(), "es-ES");
        assert_eq!(cfg.voice_language(), "en-US");
    }

    #[test]
    fn el_micro_elegido_a_mano_manda_sobre_el_espejo() {
        let cfg: AppConfig = toml::from_str(
            "language = \"en-US\"\ntarget_language = \"es-ES\"\ntranslate = true\n\
             mic_language = \"fr-FR\"\nmic_target_language = \"de-DE\"\n",
        )
        .expect("parsea");
        assert_eq!(cfg.mic_asr_language(), "fr-FR");
        assert_eq!(cfg.voice_language(), "de-DE");
    }

    #[test]
    fn sin_traduccion_el_micro_se_transcribe_como_la_sala() {
        // El comportamiento de siempre: un solo idioma para todo.
        let cfg: AppConfig =
            toml::from_str("language = \"es-ES\"\ntranslate = false\n").expect("parsea");
        assert_eq!(cfg.mic_asr_language(), "es-ES");
    }

    #[test]
    fn un_toml_sin_seccion_speak_carga_con_la_voz_apagada() {
        // Las configuraciones anteriores a la voz sintetica no tienen
        // `[speak]`; deben seguir cargando y con la funcion desactivada.
        let cfg: AppConfig = toml::from_str(r#"language = "es-ES""#).expect("parsea");
        assert!(!cfg.speak.enabled);
        assert_eq!(cfg.speak.engine, "chatterbox");
    }

    #[test]
    fn la_seccion_speak_da_la_vuelta_completa_en_toml() {
        let mut cfg = AppConfig::default();
        cfg.speak.enabled = true;
        cfg.speak.engine = "kokoro".to_string();
        cfg.speak.voice_wav = Some(PathBuf::from(r"C:\voces\mia.wav"));
        cfg.speak.output_device_id = Some("{0.0.0.00000000}.{abc}".to_string());

        let text = toml::to_string_pretty(&cfg).expect("serializa");
        assert!(text.contains("[speak]"), "deberia tener su propia seccion");

        let back: AppConfig = toml::from_str(&text).expect("deserializa");
        assert!(back.speak.enabled);
        assert_eq!(back.speak.engine, "kokoro");
        assert_eq!(back.speak.voice_wav, cfg.speak.voice_wav);
        assert_eq!(back.speak.output_device_id, cfg.speak.output_device_id);
    }

    #[test]
    fn una_seccion_speak_parcial_completa_con_los_valores_por_defecto() {
        let cfg: AppConfig =
            toml::from_str("[speak]\nenabled = true\n").expect("parsea");
        assert!(cfg.speak.enabled);
        assert_eq!(cfg.speak.group_max_chars, 250, "el resto sale del Default");
        assert!(cfg.speak.mark_echo);
    }

    #[test]
    fn una_config_vieja_con_el_campo_de_umbral_absoluto_sigue_cargando() {
        // `gate_threshold_dbfs` ya no existe: era un umbral absoluto y estaba
        // mal planteado. Serde ignora los campos que no conoce, asi que las
        // configuraciones anteriores no se rompen.
        let cfg: AppConfig =
            toml::from_str("language = \"es-ES\"\ngate_threshold_dbfs = -50.0\n").expect("parsea");
        assert_eq!(cfg.gate_drop_db, 25.0);
    }
}
