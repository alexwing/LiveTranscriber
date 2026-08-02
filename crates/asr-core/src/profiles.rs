//! Perfiles de configuracion con nombre.
//!
//! Las combinaciones utiles son muchas y muy distintas entre si —una reunion
//! de Teams en ingles hablando con tu voz, transcribir una charla en espanol
//! sin traducir, subtitular una pelicula— y reconfigurarlo todo a mano cada
//! vez es tedioso y facil de equivocar. Un perfil guarda la foto completa y
//! la devuelve de una pieza.
//!
//! Dos decisiones que conviene entender:
//!
//! - **Los perfiles viven en su propio fichero**, no dentro de
//!   `transcriber-config.toml`. Ese fichero es *el estado actual* de la app y
//!   lo reescribe entera cada vez que tocas algo en la interfaz; meter ahi
//!   los perfiles los expondria a perderse en cada guardado.
//! - **Un perfil NO se lleva las rutas de la instalacion** (el interprete de
//!   Python, los scripts de los sidecars, `hf_home`). Esas describen *esta
//!   maquina*, no como quieres usarla: un perfil guardado antes de reinstalar
//!   el entorno dejaria la app apuntando a un venv que ya no existe. Al
//!   aplicar un perfil se conservan las de la configuracion actual.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::config::AppConfig;

/// Un perfil: un nombre y la configuracion completa que representa.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    pub name: String,
    #[serde(flatten)]
    pub config: AppConfig,
}

/// El fichero de perfiles, tal cual se guarda.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ProfileStore {
    #[serde(default, rename = "profile")]
    pub profiles: Vec<Profile>,
}

/// Nombre del fichero, hermano de `transcriber-config.toml`.
pub const PROFILES_FILE: &str = "transcriber-profiles.toml";

/// Ruta del fichero de perfiles a partir de la del de configuracion: siempre
/// van juntos, para que mover uno no deje al otro huerfano.
pub fn profiles_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .map(|dir| dir.join(PROFILES_FILE))
        .unwrap_or_else(|| PathBuf::from(PROFILES_FILE))
}

impl ProfileStore {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&raw)?)
    }

    /// Guarda la lista entera, **sin dejar el fichero a medias si algo falla**.
    ///
    /// Se escribe a un temporal y se renombra encima, que en el mismo
    /// directorio es atomico. Escribir directamente significa que un corte a
    /// mitad (la app matada, el disco lleno) deja un TOML truncado, y ese
    /// fichero roto no solo impide leer los perfiles: tambien impide
    /// guardarlos, porque guardar empieza por cargar la lista actual.
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let text = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        // En Windows rename falla si el destino existe, asi que se quita antes.
        // Entre medias no hay ventana peligrosa: el temporal ya esta completo.
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn names(&self) -> Vec<String> {
        self.profiles.iter().map(|p| p.name.clone()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == name)
    }

    /// Guarda (o reemplaza) un perfil. Reemplazar es lo que se espera al
    /// repetir un nombre: es "actualizar este perfil con lo que tengo ahora".
    pub fn put(&mut self, name: &str, config: AppConfig) {
        let profile = Profile {
            name: name.to_string(),
            config,
        };
        match self.profiles.iter_mut().find(|p| p.name == name) {
            Some(existing) => *existing = profile,
            None => self.profiles.push(profile),
        }
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.profiles.len();
        self.profiles.retain(|p| p.name != name);
        self.profiles.len() != before
    }
}

/// Un dispositivo del perfil que ya no existe y se ha sustituido por el
/// predeterminado. Se devuelve para poder decirlo: cambiar de dispositivo en
/// silencio es como acabas grabando de donde no querias.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeviceFallback {
    /// `sistema`, `microfono` o `voz`.
    pub what: String,
    /// El id que guardaba el perfil y ya no esta.
    pub missing_id: String,
}

/// Resultado de aplicar un perfil.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AppliedProfile {
    pub config: AppConfig,
    pub fallbacks: Vec<DeviceFallback>,
}

/// Prepara la configuracion de un perfil para usarla *aqui y ahora*.
///
/// - Conserva las rutas de la instalacion de `current` (ver la cabecera).
/// - Sustituye por el predeterminado cualquier dispositivo que ya no exista,
///   y dice cuales. Un perfil hecho con unos auriculares USB no debe dejar la
///   app muda el dia que no estan enchufados.
pub fn apply(profile: &AppConfig, current: &AppConfig, devices: &DeviceIds) -> AppliedProfile {
    let mut config = profile.clone();
    let mut fallbacks = Vec::new();

    // Las rutas describen la maquina, no el uso: siempre las de ahora.
    config.python = current.python.clone();
    config.script = current.script.clone();
    config.mt_script = current.mt_script.clone();
    config.hf_home = current.hf_home.clone();
    config.speak.python = current.speak.python.clone();
    config.speak.script = current.speak.script.clone();

    let mut check = |id: &mut Option<String>, pool: &HashSet<String>, what: &str| {
        if let Some(wanted) = id.clone() {
            if !pool.contains(&wanted) {
                fallbacks.push(DeviceFallback {
                    what: what.to_string(),
                    missing_id: wanted,
                });
                *id = None; // None = predeterminado del sistema
            }
        }
    };
    check(&mut config.system_device_id, &devices.outputs, "sistema");
    check(&mut config.mic_device_id, &devices.inputs, "microfono");
    check(&mut config.speak.output_device_id, &devices.outputs, "voz");

    AppliedProfile { config, fallbacks }
}

/// Los ids de dispositivo disponibles ahora mismo, por lado.
#[derive(Debug, Default)]
pub struct DeviceIds {
    pub outputs: HashSet<String>,
    pub inputs: HashSet<String>,
}

impl DeviceIds {
    /// Los del sistema. Si la enumeracion falla no se inventa nada: se
    /// devuelven conjuntos vacios, con lo que todos los dispositivos del
    /// perfil caen al predeterminado, que es el comportamiento seguro.
    pub fn from_system() -> Self {
        let collect = |kind| {
            asr_audio::list_devices(kind)
                .map(|list| list.into_iter().map(|d| d.id).collect())
                .unwrap_or_default()
        };
        Self {
            outputs: collect(asr_audio::DeviceKind::Output),
            inputs: collect(asr_audio::DeviceKind::Input),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(outputs: &[&str], inputs: &[&str]) -> DeviceIds {
        DeviceIds {
            outputs: outputs.iter().map(|s| s.to_string()).collect(),
            inputs: inputs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn un_perfil_guardado_se_recupera_igual() {
        let mut store = ProfileStore::default();
        let mut cfg = AppConfig::default();
        cfg.language = "en-US".to_string();
        cfg.target_language = "es-ES".to_string();
        cfg.speak.enabled = true;
        store.put("Teams en ingles", cfg.clone());

        let text = toml::to_string_pretty(&store).expect("serializa");
        let back: ProfileStore = toml::from_str(&text).expect("deserializa");
        let got = back.get("Teams en ingles").expect("esta");
        assert_eq!(got.config.language, "en-US");
        assert_eq!(got.config.target_language, "es-ES");
        assert!(got.config.speak.enabled);
    }

    #[test]
    fn guardar_con_el_mismo_nombre_reemplaza_en_vez_de_duplicar() {
        let mut store = ProfileStore::default();
        store.put("Reunion", AppConfig::default());
        let mut otra = AppConfig::default();
        otra.language = "fr-FR".to_string();
        store.put("Reunion", otra);

        assert_eq!(store.profiles.len(), 1, "no debe duplicar el nombre");
        assert_eq!(store.get("Reunion").unwrap().config.language, "fr-FR");
    }

    #[test]
    fn un_dispositivo_que_ya_no_existe_cae_al_predeterminado() {
        let mut perfil = AppConfig::default();
        perfil.mic_device_id = Some("{micro-usb-que-ya-no-esta}".to_string());
        perfil.system_device_id = Some("{altavoces}".to_string());

        let aplicado = apply(&perfil, &AppConfig::default(), &ids(&["{altavoces}"], &[]));

        assert_eq!(aplicado.config.mic_device_id, None, "cae al predeterminado");
        assert_eq!(
            aplicado.config.system_device_id,
            Some("{altavoces}".to_string()),
            "el que si existe se respeta"
        );
        assert_eq!(aplicado.fallbacks.len(), 1);
        assert_eq!(aplicado.fallbacks[0].what, "microfono");
    }

    #[test]
    fn el_dispositivo_de_la_voz_tambien_cae_si_falta() {
        // El caso real: un perfil hecho con VB-CABLE en una maquina donde
        // luego se desinstala.
        let mut perfil = AppConfig::default();
        perfil.speak.output_device_id = Some("{cable-input}".to_string());

        let aplicado = apply(&perfil, &AppConfig::default(), &ids(&["{altavoces}"], &[]));

        assert_eq!(aplicado.config.speak.output_device_id, None);
        assert_eq!(aplicado.fallbacks[0].what, "voz");
    }

    #[test]
    fn aplicar_un_perfil_no_pisa_las_rutas_de_la_instalacion() {
        // Un perfil guardado antes de reinstalar el entorno dejaria la app
        // apuntando a un venv que ya no existe.
        let mut perfil = AppConfig::default();
        perfil.python = PathBuf::from(r"C:\viejo\python.exe");
        perfil.speak.python = PathBuf::from(r"C:\viejo\tts\python.exe");
        perfil.language = "de-DE".to_string();

        let mut actual = AppConfig::default();
        actual.python = PathBuf::from(r"E:\bueno\python.exe");
        actual.speak.python = PathBuf::from(r"E:\bueno\tts\python.exe");

        let aplicado = apply(&perfil, &actual, &ids(&[], &[]));

        assert_eq!(aplicado.config.python, actual.python, "manda la actual");
        assert_eq!(aplicado.config.speak.python, actual.speak.python);
        assert_eq!(aplicado.config.language, "de-DE", "el uso si viene del perfil");
    }

    #[test]
    fn sin_dispositivos_enumerables_todo_cae_al_predeterminado() {
        // Si la enumeracion falla, mejor el predeterminado que un id muerto.
        let mut perfil = AppConfig::default();
        perfil.mic_device_id = Some("{algo}".to_string());
        perfil.system_device_id = Some("{otro}".to_string());

        let aplicado = apply(&perfil, &AppConfig::default(), &DeviceIds::default());

        assert_eq!(aplicado.config.mic_device_id, None);
        assert_eq!(aplicado.config.system_device_id, None);
        assert_eq!(aplicado.fallbacks.len(), 2);
    }

    #[test]
    fn guardar_deja_el_fichero_completo_y_sin_temporales() {
        let dir = std::env::temp_dir().join("livetranscriber-test-perfiles");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(PROFILES_FILE);

        let mut store = ProfileStore::default();
        store.put("uno", AppConfig::default());
        store.save(&path).expect("guarda");

        // Guardar dos veces seguidas debe funcionar: el renombrado sobre un
        // fichero que ya existe es justo el caso que falla en Windows si no
        // se borra antes.
        store.put("dos", AppConfig::default());
        store.save(&path).expect("guarda encima");

        let back = ProfileStore::load(&path).expect("carga");
        assert_eq!(back.names(), vec!["uno", "dos"]);
        assert!(
            !path.with_extension("toml.tmp").exists(),
            "no debe quedar el temporal"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn borrar_dice_si_habia_algo_que_borrar() {
        let mut store = ProfileStore::default();
        store.put("uno", AppConfig::default());
        assert!(store.remove("uno"));
        assert!(!store.remove("uno"), "borrar dos veces no es un exito");
    }
}
