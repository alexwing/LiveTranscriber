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
    /// Locale destino (`en-US`, `de-DE`, ...). El origen es [`Self::language`],
    /// que por eso no puede ser `auto` si se traduce: NLLB necesita saberlo.
    pub target_language: String,

    /// Locale (`es-ES`, `en-US`, ...) o `auto`.
    pub language: String,
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
    fn una_config_vieja_con_el_campo_de_umbral_absoluto_sigue_cargando() {
        // `gate_threshold_dbfs` ya no existe: era un umbral absoluto y estaba
        // mal planteado. Serde ignora los campos que no conoce, asi que las
        // configuraciones anteriores no se rompen.
        let cfg: AppConfig =
            toml::from_str("language = \"es-ES\"\ngate_threshold_dbfs = -50.0\n").expect("parsea");
        assert_eq!(cfg.gate_drop_db, 25.0);
    }
}
