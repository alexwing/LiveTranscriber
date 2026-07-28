//! Implementacion de [`AsrEngine`] sobre un proceso Python.
//!
//! El formato de cable esta descrito en `sidecar/asr_server.py`. Aqui solo
//! importa que stdin lleva frames con prefijo de longitud y stdout devuelve
//! una linea JSON por mensaje.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::engine::{AsrEngine, AsrEvent, EngineError, EngineFactory, Result};

const FRAME_PCM: u8 = 0x01;
const FRAME_CONTROL: u8 = 0x02;

/// Cuanto esperar a que el proceso se cierre solo antes de matarlo.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SidecarConfig {
    /// Interprete a usar; normalmente el `python.exe` del venv del modelo.
    pub python: PathBuf,
    /// Ruta a `asr_server.py`.
    pub script: PathBuf,
    /// Locale (`es-ES`, `en-US`, ...) o `auto`.
    pub language: String,
    /// 0, 3, 6 o 13. Menos es menos latencia y mas errores.
    pub lookahead: u8,
    /// `bfloat16` salvo que haya un motivo para lo contrario. En tarjetas
    /// anteriores a Ampere el propio sidecar lo baja a `float16` y avisa.
    pub dtype: String,
    /// Valor para `HF_HOME`, si los modelos no estan en la cache por defecto.
    pub hf_home: Option<PathBuf>,
}

impl Default for SidecarConfig {
    fn default() -> Self {
        Self {
            python: PathBuf::from("python"),
            script: PathBuf::from("sidecar/asr_server.py"),
            language: "auto".to_string(),
            lookahead: 3,
            dtype: "bfloat16".to_string(),
            hf_home: None,
        }
    }
}

impl EngineFactory for SidecarConfig {
    fn build(&self, events: Sender<AsrEvent>) -> Result<Box<dyn AsrEngine>> {
        Ok(Box::new(PythonSidecar::spawn(self, events)?))
    }
}

/// Lo que el sidecar escribe por stdout. Se mantiene aparte de [`AsrEvent`]
/// para que cambiar el formato de cable no arrastre al resto del programa.
#[derive(serde::Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Wire {
    Ready {
        device: String,
        latency_ms: u32,
        language: String,
    },
    Delta {
        text: String,
    },
    SegmentEnd,
    Error {
        message: String,
    },
}

impl From<Wire> for AsrEvent {
    fn from(w: Wire) -> Self {
        match w {
            Wire::Ready {
                device,
                latency_ms,
                language,
            } => AsrEvent::Ready {
                device,
                latency_ms,
                language,
            },
            Wire::Delta { text } => AsrEvent::Delta { text },
            Wire::SegmentEnd => AsrEvent::SegmentEnd,
            Wire::Error { message } => AsrEvent::Error { message },
        }
    }
}

pub struct PythonSidecar {
    child: Child,
    /// `None` una vez cerrado; cerrar stdin es la senal de fin para Python.
    stdin: Option<ChildStdin>,
    readers: Vec<JoinHandle<()>>,
}

impl PythonSidecar {
    pub fn spawn(cfg: &SidecarConfig, events: Sender<AsrEvent>) -> Result<Self> {
        let mut command = Command::new(&cfg.python);
        command
            .arg(&cfg.script)
            .arg("--language")
            .arg(&cfg.language)
            .arg("--lookahead")
            .arg(cfg.lookahead.to_string())
            .arg("--dtype")
            .arg(&cfg.dtype)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Por variable de entorno porque es la unica forma de que huggingface_hub
        // la vea; no hay parametro para esto.
        if let Some(home) = &cfg.hf_home {
            command.env("HF_HOME", home);
        }

        let mut child = command.spawn().map_err(|e| {
            EngineError::Spawn(format!("no se pudo lanzar {}: {e}", cfg.python.display()))
        })?;

        let stdin = child.stdin.take().ok_or(EngineError::Closed)?;
        let stdout = child.stdout.take().ok_or(EngineError::Closed)?;
        let stderr = child.stderr.take().ok_or(EngineError::Closed)?;

        let readers = vec![
            spawn_event_reader(stdout, events),
            spawn_log_reader(stderr),
        ];

        Ok(Self {
            child,
            stdin: Some(stdin),
            readers,
        })
    }
}

fn spawn_event_reader<R: Read + Send + 'static>(
    stdout: R,
    events: Sender<AsrEvent>,
) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("asr-sidecar-out".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::debug!("stdout del sidecar cerrado: {e}");
                        break;
                    }
                };
                if line.trim().is_empty() {
                    continue;
                }
                let event = match serde_json::from_str::<Wire>(&line) {
                    Ok(w) => AsrEvent::from(w),
                    Err(e) => {
                        tracing::warn!("linea ininteligible del sidecar ({e}): {line}");
                        continue;
                    }
                };
                if events.send(event).is_err() {
                    tracing::debug!("nadie escucha los eventos, dejando de leer");
                    break;
                }
            }
        })
        .expect("spawn asr-sidecar-out")
}

fn spawn_log_reader<R: Read + Send + 'static>(stderr: R) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("asr-sidecar-err".into())
        .spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(std::result::Result::ok) {
                tracing::info!(target: "sidecar", "{line}");
            }
        })
        .expect("spawn asr-sidecar-err")
}

fn write_frame(out: &mut ChildStdin, frame_type: u8, payload: &[u8]) -> std::io::Result<()> {
    out.write_all(&(payload.len() as u32).to_le_bytes())?;
    out.write_all(&[frame_type])?;
    out.write_all(payload)?;
    out.flush()
}

impl AsrEngine for PythonSidecar {
    fn feed(&mut self, samples: &[f32]) -> Result<()> {
        let stdin = self.stdin.as_mut().ok_or(EngineError::Closed)?;
        let mut payload = Vec::with_capacity(samples.len() * 4);
        for sample in samples {
            payload.extend_from_slice(&sample.to_le_bytes());
        }
        write_frame(stdin, FRAME_PCM, &payload)?;
        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        let stdin = self.stdin.as_mut().ok_or(EngineError::Closed)?;
        write_frame(stdin, FRAME_CONTROL, br#"{"cmd":"reset"}"#)?;
        Ok(())
    }

    fn shutdown(&mut self) -> Result<()> {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = write_frame(stdin, FRAME_CONTROL, br#"{"cmd":"shutdown"}"#);
        }
        // Cerrar stdin es lo que de verdad le dice a Python que se acabo.
        self.stdin.take();

        let deadline = Instant::now() + SHUTDOWN_GRACE;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Ok(None) => {
                    tracing::warn!("el sidecar no se cerro a tiempo, matandolo");
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
                Err(e) => {
                    tracing::warn!("no se pudo esperar al sidecar: {e}");
                    let _ = self.child.kill();
                    break;
                }
            }
        }

        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        Ok(())
    }
}

impl Drop for PythonSidecar {
    fn drop(&mut self) {
        if self.stdin.is_some() {
            let _ = self.shutdown();
        }
    }
}
