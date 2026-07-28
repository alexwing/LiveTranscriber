//! Une captura, gate y motor para una fuente de audio.
//!
//! Sigue la forma del worker de ambilight de TapoController: un `AtomicBool`
//! compartido y hilos con nombre, sin runtime asincrono de por medio.
//!
//! Se lanza una sesion por fuente. Para transcribir a la vez lo que suena en el
//! sistema y lo que dices tu, se arrancan dos: cada una con su motor y su
//! marca de [`Source`], que es lo que luego permite separar quien dijo que.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use asr_audio::{
    spawn_capture, CaptureTarget, GateEvent, Normalizer, SilenceGate, Source, TARGET_RATE,
};

use crate::engine::{AsrEvent, EngineError, EngineFactory};

/// Cuantos bloques de audio puede acumular el canal antes de frenar la captura.
/// A 100 ms por bloque, 64 son 6,4 s de colchon: de sobra para un tiron de GPU
/// y poco como para que la transcripcion se descuelgue del audio sin notarlo.
const AUDIO_QUEUE: usize = 64;

/// Cada cuanto despierta la bomba cuando no llega audio, para poder cerrar el
/// parrafo por inactividad del texto sin esperar al siguiente bloque.
const PUMP_TICK: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionConfig {
    pub target: CaptureTarget,
    pub source: Source,
    /// Cuantos dB por debajo del habla reciente cuenta como silencio. Es una
    /// medida relativa a proposito: el nivel absoluto depende del volumen de
    /// Windows, la diferencia entre voz y pausa no.
    pub gate_drop_db: f32,
    /// Suelo absoluto en dBFS. Solo para que el silencio digital cuente
    /// siempre como silencio.
    pub gate_floor_dbfs: f32,
    /// Segundos de silencio antes de dejar de alimentar al modelo. Solo afecta
    /// al gasto de GPU, no a donde acaban los parrafos.
    pub gate_hold_secs: f32,
    /// Segundos **sin texto nuevo** que cierran un parrafo. Esta es la senal
    /// buena: mira lo que transcribe el modelo, no el volumen, asi que
    /// funciona con musica de fondo.
    pub paragraph_idle_secs: f32,
    /// Tope de duracion de un parrafo. Sin el, un monologo sin pausas seria un
    /// unico parrafo interminable.
    pub paragraph_max_secs: f32,
    /// Compensar el volumen del sistema. Practicamente siempre conviene: el
    /// loopback captura post-volumen y sin esto un volumen bajo mata la
    /// transcripcion. Ver [`asr_audio::Normalizer`].
    pub normalize_gain: bool,
}

/// Lo que una sesion comunica hacia arriba, ya etiquetado con su fuente.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEvent {
    Ready {
        source: Source,
        device: String,
        latency_ms: u32,
        language: String,
    },
    Delta {
        source: Source,
        at_ms: u64,
        text: String,
    },
    SegmentEnd {
        source: Source,
        at_ms: u64,
    },
    /// Nivel de entrada. `rms` es el crudo, **antes** de normalizar, que es lo
    /// que hay que mirar para saber si el volumen del sistema esta muy bajo.
    /// `gain` pegado al techo significa justo eso.
    Level {
        source: Source,
        rms: f32,
        gain: f32,
        gain_at_ceiling: bool,
    },
    Error {
        source: Source,
        message: String,
    },
    /// La sesion se ha detenido del todo.
    Stopped {
        source: Source,
    },
}

pub struct Session {
    source: Source,
    running: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Session {
    pub fn start(
        cfg: SessionConfig,
        factory: &dyn EngineFactory,
        out: Sender<SessionEvent>,
    ) -> Result<Self, EngineError> {
        let running = Arc::new(AtomicBool::new(true));
        let source = cfg.source;
        let started = Instant::now();

        let (audio_tx, audio_rx) = sync_channel::<Vec<f32>>(AUDIO_QUEUE);
        let (engine_tx, engine_rx) = std::sync::mpsc::channel::<AsrEvent>();

        let mut engine = factory.build(engine_tx)?;

        let capture = spawn_capture(cfg.target.clone(), running.clone(), audio_tx)
            .map_err(|e| EngineError::Spawn(e.to_string()))?;

        // Estado compartido entre el reenviador (que ve el texto) y la bomba
        // (que es quien puede cerrar el parrafo, porque tiene el motor). Es la
        // via para que el corte dependa del texto y no del volumen.
        let last_text_ms = Arc::new(AtomicU64::new(0));
        let paragraph_start_ms = Arc::new(AtomicU64::new(0));
        let has_text = Arc::new(AtomicBool::new(false));

        // Traduce los eventos del motor a eventos de sesion, poniendoles la
        // fuente y el instante. El motor no sabe de donde salio su audio.
        let forward_out = out.clone();
        let fw_last_text = last_text_ms.clone();
        let fw_paragraph_start = paragraph_start_ms.clone();
        let fw_has_text = has_text.clone();
        let forwarder = std::thread::Builder::new()
            .name("asr-forward".into())
            .spawn(move || {
                for event in engine_rx {
                    let at_ms = started.elapsed().as_millis() as u64;
                    match &event {
                        AsrEvent::Delta { .. } => {
                            fw_last_text.store(at_ms, Ordering::Relaxed);
                            // El primer delta marca el inicio del parrafo.
                            if !fw_has_text.swap(true, Ordering::Relaxed) {
                                fw_paragraph_start.store(at_ms, Ordering::Relaxed);
                            }
                        }
                        AsrEvent::SegmentEnd => fw_has_text.store(false, Ordering::Relaxed),
                        _ => {}
                    }
                    let mapped = match event {
                        AsrEvent::Ready {
                            device,
                            latency_ms,
                            language,
                        } => SessionEvent::Ready {
                            source,
                            device,
                            latency_ms,
                            language,
                        },
                        AsrEvent::Delta { text } => SessionEvent::Delta {
                            source,
                            at_ms,
                            text,
                        },
                        AsrEvent::SegmentEnd => SessionEvent::SegmentEnd { source, at_ms },
                        AsrEvent::Error { message } => SessionEvent::Error { source, message },
                    };
                    if forward_out.send(mapped).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| EngineError::Spawn(e.to_string()))?;

        // Bombea audio de la captura al motor, pasando por el gate.
        let pump_running = running.clone();
        let pump = std::thread::Builder::new()
            .name("asr-pump".into())
            .spawn(move || {
                let mut gate = SilenceGate::new(
                    cfg.gate_drop_db,
                    cfg.gate_floor_dbfs,
                    cfg.gate_hold_secs,
                    TARGET_RATE,
                );
                let mut normalizer = Normalizer::new(cfg.normalize_gain);
                let idle_ms = (cfg.paragraph_idle_secs * 1000.0) as u64;
                let max_ms = (cfg.paragraph_max_secs * 1000.0) as u64;

                loop {
                    if !pump_running.load(Ordering::Relaxed) {
                        break;
                    }

                    // Con timeout, no bloqueando: hay que despertar aunque no
                    // llegue audio para poder cerrar el parrafo por inactividad
                    // del texto.
                    let block = match audio_rx.recv_timeout(PUMP_TICK) {
                        Ok(block) => Some(block),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    };

                    // Cerrar parrafo si el modelo lleva un rato sin soltar texto,
                    // o si el parrafo ya se ha hecho demasiado largo. Mirar el
                    // texto y no el volumen es lo que hace que esto funcione con
                    // musica de fondo: la musica no genera transcripcion.
                    if has_text.load(Ordering::Relaxed) {
                        let now = started.elapsed().as_millis() as u64;
                        let quiet = now.saturating_sub(last_text_ms.load(Ordering::Relaxed));
                        let length = now.saturating_sub(paragraph_start_ms.load(Ordering::Relaxed));
                        if quiet >= idle_ms || length >= max_ms {
                            // El motor confirmara con su propio SegmentEnd, que
                            // es quien pone `has_text` a false.
                            if let Err(e) = engine.reset() {
                                let _ = out.send(SessionEvent::Error {
                                    source,
                                    message: e.to_string(),
                                });
                                break;
                            }
                        }
                    }

                    let Some(mut block) = block else { continue };

                    // El nivel se mide antes de normalizar: es el dato que dice
                    // si el volumen del sistema esta demasiado bajo.
                    let raw_rms = asr_audio::rms(&block);
                    normalizer.process(&mut block);

                    let _ = out.send(SessionEvent::Level {
                        source,
                        rms: raw_rms,
                        gain: normalizer.gain(),
                        gain_at_ceiling: normalizer.at_ceiling(),
                    });

                    // El gate decide con el nivel crudo (es lo que distingue voz
                    // de pausa) pero al motor le llega el audio normalizado.
                    let result = match gate.push(raw_rms, block) {
                        GateEvent::Audio(samples) => engine.feed(&samples),
                        GateEvent::Idle => Ok(()),
                    };

                    if let Err(e) = result {
                        let _ = out.send(SessionEvent::Error {
                            source,
                            message: e.to_string(),
                        });
                        break;
                    }
                }

                pump_running.store(false, Ordering::Relaxed);
                if let Err(e) = engine.shutdown() {
                    tracing::warn!("el motor no cerro limpiamente: {e}");
                }
                let _ = out.send(SessionEvent::Stopped { source });
            })
            .map_err(|e| EngineError::Spawn(e.to_string()))?;

        Ok(Self {
            source,
            running,
            threads: vec![capture, pump, forwarder],
        })
    }

    pub fn source(&self) -> Source {
        self.source
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    /// Pide la parada. No bloquea; usa [`Session::join`] para esperar.
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    /// Para y espera a que todos los hilos terminen.
    pub fn join(mut self) {
        self.stop();
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}
