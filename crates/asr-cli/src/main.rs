//! Banco de pruebas de LiveTranscriber sin interfaz.
//!
//!   asr-cli devices                      lista dispositivos
//!   asr-cli level --seconds 10           mide nivel de entrada (sin modelo)
//!   asr-cli run --seconds 30             transcribe de verdad

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use asr_audio::{list_devices, spawn_capture, CaptureTarget, DeviceKind, Source};
use asr_core::speak::{Synthesizer, TtsSidecar};
use asr_core::translate::{MtConfig, MtSidecar, TranslationPump};
use asr_core::{AppConfig, Session, SessionConfig, SessionEvent, Transcript};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "asr-cli", about = "Captura y transcripcion sin interfaz")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Which {
    /// Todo lo que suena en el sistema (loopback).
    System,
    /// El microfono.
    Mic,
}

impl Which {
    fn target(self, device_id: Option<String>) -> CaptureTarget {
        match self {
            Which::System => CaptureTarget::Loopback { device_id },
            Which::Mic => CaptureTarget::Microphone { device_id },
        }
    }

    fn source(self) -> Source {
        match self {
            Which::System => Source::System,
            Which::Mic => Source::Mic,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Lista dispositivos de entrada y salida.
    Devices,

    /// Captura y muestra el nivel, sin cargar el modelo. Sirve para confirmar
    /// que el loopback entrega audio antes de meter la GPU de por medio.
    Level {
        #[arg(long, value_enum, default_value = "system")]
        from: Which,
        #[arg(long)]
        device_id: Option<String>,
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        /// Captura solo el audio de este proceso (loopback por PID).
        #[arg(long)]
        pid: Option<u32>,
    },

    /// Transcribe de verdad, arrancando el sidecar de Python.
    Run {
        #[arg(long, value_enum, default_value = "system")]
        from: Which,
        #[arg(long)]
        device_id: Option<String>,
        #[arg(long, default_value_t = 30)]
        seconds: u64,
        #[arg(long)]
        pid: Option<u32>,
        #[arg(long, default_value = "auto")]
        language: String,
        #[arg(long, default_value_t = 3)]
        lookahead: u8,
        /// Interprete del venv con torch y transformers.
        #[arg(long)]
        python: Option<PathBuf>,
        /// Ruta a asr_server.py.
        #[arg(long, default_value = "sidecar/asr_server.py")]
        script: PathBuf,
        /// Guarda la transcripcion en este .txt al terminar.
        #[arg(long)]
        save_txt: Option<PathBuf>,
        /// Guarda la transcripcion en este .srt al terminar.
        #[arg(long)]
        save_srt: Option<PathBuf>,
        /// Traducir tambien a este locale (en-US, de-DE...). Exige que
        /// --language sea concreto: el traductor necesita saber el origen.
        #[arg(long)]
        translate_to: Option<String>,
        /// Ruta a mt_server.py.
        #[arg(long, default_value = "sidecar/mt_server.py")]
        mt_script: PathBuf,
    },

    /// Sintetiza un texto y lo reproduce en un dispositivo de salida. Es la
    /// prueba del microfono virtual sin reunion: manda la voz a CABLE Input
    /// y, si algo grabando de CABLE Output la oye, el circuito funciona.
    Speak {
        /// Texto a decir.
        #[arg(long)]
        text: String,
        /// Idioma del texto (codigo corto: en, es, de...).
        #[arg(long, default_value = "en")]
        lang: String,
        /// Dispositivo de salida (id de `asr-cli devices`). Sin el, el
        /// predeterminado: suena por los altavoces.
        #[arg(long)]
        device_id: Option<String>,
        /// chatterbox (clonacion) o kokoro (voz neutra, mas ligero).
        #[arg(long, default_value = "chatterbox")]
        engine: String,
        /// WAV con la voz a clonar (obligatorio con chatterbox).
        #[arg(long)]
        voice_wav: Option<PathBuf>,
        /// Voz preajustada de kokoro.
        #[arg(long, default_value = "af_heart")]
        kokoro_voice: String,
        /// Interprete del venv de voz (no es el del ASR).
        #[arg(long)]
        python: Option<PathBuf>,
        /// Ruta a tts_server.py.
        #[arg(long, default_value = "sidecar/tts_server.py")]
        script: PathBuf,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    match Cli::parse().command {
        Command::Devices => devices(),
        Command::Level {
            from,
            device_id,
            seconds,
            pid,
        } => level(from, device_id, seconds, pid),
        Command::Run {
            from,
            device_id,
            seconds,
            pid,
            language,
            lookahead,
            python,
            script,
            save_txt,
            save_srt,
            translate_to,
            mt_script,
        } => run(RunArgs {
            from,
            device_id,
            seconds,
            pid,
            language,
            lookahead,
            python,
            script,
            save_txt,
            save_srt,
            translate_to,
            mt_script,
        }),
        Command::Speak {
            text,
            lang,
            device_id,
            engine,
            voice_wav,
            kokoro_voice,
            python,
            script,
        } => speak(SpeakArgs {
            text,
            lang,
            device_id,
            engine,
            voice_wav,
            kokoro_voice,
            python,
            script,
        }),
    }
}

fn devices() -> Result<()> {
    for (kind, title) in [
        (DeviceKind::Output, "SALIDAS (capturables por loopback)"),
        (DeviceKind::Input, "ENTRADAS (microfonos)"),
    ] {
        println!("\n{title}");
        println!("{}", "-".repeat(title.len()));
        for device in list_devices(kind).context("enumerando dispositivos")? {
            let mark = if device.is_default { "*" } else { " " };
            println!("{mark} {}\n    id: {}", device.name, device.id);
        }
    }
    println!("\n(* = predeterminado)");
    Ok(())
}

fn target_for(which: Which, device_id: Option<String>, pid: Option<u32>) -> CaptureTarget {
    match pid {
        Some(pid) => CaptureTarget::Process {
            pid,
            include_children: true,
        },
        None => which.target(device_id),
    }
}

fn level(which: Which, device_id: Option<String>, seconds: u64, pid: Option<u32>) -> Result<()> {
    let target = target_for(which, device_id, pid);
    println!("capturando de {target:?} durante {seconds}s...");
    println!("(si el nivel se queda en 0, no esta entrando audio)\n");

    let running = Arc::new(AtomicBool::new(true));
    let (tx, rx) = sync_channel::<Vec<f32>>(64);
    let handle = spawn_capture(target, running.clone(), tx).context("arrancando la captura")?;

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut blocks = 0usize;
    let mut samples = 0usize;
    let mut peak = 0.0f32;
    // El mismo normalizador que usa la sesion, para ver cuanta ganancia haria
    // falta y si se queda corta.
    let mut normalizer = asr_audio::Normalizer::new(true);

    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(mut block) => {
                let rms = asr_audio::rms(&block);
                normalizer.process(&mut block);
                peak = peak.max(rms);
                blocks += 1;
                samples += block.len();
                if blocks % 5 == 0 {
                    println!(
                        "{}  rms {:.5}  ganancia x{:.1}{}",
                        meter(asr_audio::rms(&block)),
                        rms,
                        normalizer.gain(),
                        if normalizer.at_ceiling() { "  <-- al tope" } else { "" }
                    );
                }
            }
            Err(_) => {
                if !running.load(Ordering::Relaxed) {
                    println!("la captura se detuvo sola (mira los errores de arriba)");
                    break;
                }
                println!("... sin datos");
            }
        }
    }

    running.store(false, Ordering::Relaxed);
    let _ = handle.join();

    let secs = samples as f64 / asr_audio::TARGET_RATE as f64;
    println!("\n{blocks} bloques, {samples} muestras = {secs:.2}s de audio a 16 kHz mono");
    println!("pico rms crudo {peak:.5}  (ganancia final x{:.1})", normalizer.gain());

    if blocks == 0 {
        println!("\nNo llego ni un bloque. Con loopback eso significa que el dispositivo");
        println!("estaba completamente ocioso: Windows no genera eventos si nadie");
        println!("reproduce nada. Pon musica o un video y repite.");
    } else if peak == 0.0 {
        println!("\nLlego audio pero todo a cero: el dispositivo esta silenciado.");
    } else if normalizer.at_ceiling() {
        println!("\nAviso: la normalizacion se quedo al tope. El volumen del sistema");
        println!("esta muy bajo y el loopback captura despues del volumen, asi que la");
        println!("transcripcion saldra pobre. Sube el volumen de Windows.");
    }
    Ok(())
}

fn meter(rms: f32) -> String {
    // Escala logaritmica: rms 1.0 -> lleno, 0.001 (-60 dBFS) -> vacio.
    let db = 20.0 * rms.max(1e-6).log10();
    let filled = (((db + 60.0) / 60.0).clamp(0.0, 1.0) * 30.0) as usize;
    format!("[{}{}]", "#".repeat(filled), " ".repeat(30 - filled))
}

struct RunArgs {
    from: Which,
    device_id: Option<String>,
    seconds: u64,
    pid: Option<u32>,
    language: String,
    lookahead: u8,
    python: Option<PathBuf>,
    script: PathBuf,
    save_txt: Option<PathBuf>,
    save_srt: Option<PathBuf>,
    translate_to: Option<String>,
    mt_script: PathBuf,
}

fn run(args: RunArgs) -> Result<()> {
    let defaults = AppConfig::default();
    let mut sidecar = defaults.sidecar();
    sidecar.python = args.python.unwrap_or(defaults.python);
    sidecar.script = args.script;
    sidecar.language = args.language;
    sidecar.lookahead = args.lookahead;

    anyhow::ensure!(
        sidecar.script.exists(),
        "no encuentro el script del sidecar en {}",
        sidecar.script.display()
    );

    let source = args.from.source();
    let cfg = SessionConfig {
        target: target_for(args.from, args.device_id, args.pid),
        source,
        gate_drop_db: defaults.gate_drop_db,
        gate_floor_dbfs: defaults.gate_floor_dbfs,
        gate_hold_secs: defaults.gate_hold_secs,
        paragraph_idle_secs: defaults.paragraph_idle_secs,
        paragraph_max_secs: defaults.paragraph_max_secs,
        normalize_gain: defaults.normalize_gain,
    };

    // El traductor primero: si no arranca, mejor saberlo antes de abrir el audio.
    let mut pump = match args.translate_to.as_deref() {
        None => None,
        Some(target) => {
            anyhow::ensure!(
                sidecar.language != "auto",
                "para traducir hay que pasar --language con un idioma concreto: \
                 el traductor necesita saber desde cual parte"
            );
            let mt = MtConfig {
                python: sidecar.python.clone(),
                script: args.mt_script,
                dtype: sidecar.dtype.clone(),
                hf_home: sidecar.hf_home.clone(),
            };
            anyhow::ensure!(
                mt.script.exists(),
                "no encuentro el sidecar de traduccion en {}",
                mt.script.display()
            );
            println!("arrancando el traductor (la primera vez descarga NLLB)...");
            let mt_sidecar = MtSidecar::spawn(&mt)?;
            let device = mt_sidecar.wait_ready(Duration::from_secs(300))?;
            println!("traductor listo en {device}, destino {target}");
            Some(TranslationPump::new(
                Box::new(mt_sidecar),
                &sidecar.language,
                target,
            )?)
        }
    };

    println!("arrancando el modelo (tarda unos segundos)...");
    let (tx, rx) = std::sync::mpsc::channel::<SessionEvent>();
    let session = Session::start(cfg, &sidecar, tx).context("arrancando la sesion")?;

    let mut transcript = Transcript::new();
    let deadline = Instant::now() + Duration::from_secs(args.seconds);
    let mut last_level_print = Instant::now();

    while Instant::now() < deadline {
        let Ok(event) = rx.recv_timeout(Duration::from_millis(500)) else {
            continue;
        };

        // Traducir bloquea unos 160 ms por frase; en el CLI da igual hacerlo
        // aqui mismo, en la app va en su propio hilo.
        if let Some(pump) = pump.as_mut() {
            for line in pump.handle(&event) {
                println!("\n  → {}", line.translated);
                transcript.push_translation(line);
            }
        }

        match event {
            SessionEvent::Ready {
                device,
                latency_ms,
                language,
                ..
            } => {
                println!("motor listo en {device}, latencia {latency_ms} ms, idioma {language}");
                println!("--- transcripcion ---");
            }
            SessionEvent::Delta { at_ms, text, .. } => {
                print!("{text}");
                use std::io::Write;
                let _ = std::io::stdout().flush();
                transcript.push_delta(source, at_ms, &text);
            }
            SessionEvent::SegmentEnd { at_ms, .. } => {
                if transcript.close_segment(source, at_ms).is_some() {
                    println!();
                }
            }
            SessionEvent::Level { rms, .. } => {
                // Solo de vez en cuando, para no tapar el texto.
                if last_level_print.elapsed() > Duration::from_secs(5) {
                    last_level_print = Instant::now();
                    tracing::debug!("nivel {rms:.5}");
                }
            }
            SessionEvent::Error { message, .. } => eprintln!("\n[error] {message}"),
            SessionEvent::Stopped { .. } => {
                println!("\nla sesion se detuvo");
                break;
            }
        }
    }

    println!("\n--- fin ---");
    session.join();
    transcript.close_all(args.seconds * 1000);
    if let Some(mut pump) = pump {
        pump.shutdown();
    }

    if transcript.is_empty() {
        println!("no se transcribio nada");
    } else {
        println!("\n{}", transcript.to_text());
    }

    if let Some(path) = args.save_txt {
        transcript.save_text(&path)?;
        println!("guardado {}", path.display());
    }
    if let Some(path) = args.save_srt {
        transcript.save_srt(&path)?;
        println!("guardado {}", path.display());
    }
    Ok(())
}

struct SpeakArgs {
    text: String,
    lang: String,
    device_id: Option<String>,
    engine: String,
    voice_wav: Option<PathBuf>,
    kokoro_voice: String,
    python: Option<PathBuf>,
    script: PathBuf,
}

fn speak(args: SpeakArgs) -> Result<()> {
    let defaults = AppConfig::default();
    let mut tts = defaults.tts();
    tts.engine = args.engine;
    tts.script = args.script;
    tts.kokoro_voice = args.kokoro_voice;
    tts.voice_wav = args.voice_wav;
    tts.warm_lang = Some(args.lang.clone());
    if let Some(python) = args.python {
        tts.python = python;
    }

    anyhow::ensure!(
        tts.script.exists(),
        "no encuentro el sidecar de voz en {}",
        tts.script.display()
    );
    anyhow::ensure!(
        tts.python.exists(),
        "no encuentro el interprete del venv de voz en {} (pasalo con --python)",
        tts.python.display()
    );
    if tts.engine == "chatterbox" {
        anyhow::ensure!(
            tts.voice_wav.is_some(),
            "chatterbox clona una voz: pasa --voice-wav con 10-30 s de habla \
             limpia, o usa --engine kokoro para una voz neutra"
        );
    }

    println!("arrancando el sintetizador (chatterbox tarda ~21 s en frio)...");
    let mut sidecar = TtsSidecar::spawn(&tts)?;
    let ready = sidecar.wait_ready(Duration::from_secs(300))?;
    println!("sintetizador listo en {} a {} Hz", ready.device, ready.rate);

    let synthesized = sidecar.synthesize(&args.text, &args.lang)?;
    let audio_secs = synthesized.samples.len() as f32 / synthesized.rate as f32;
    println!(
        "sintetizado: {audio_secs:.2}s de audio en {} ms (RTFx {:.2}x)",
        synthesized.synth_ms,
        audio_secs / (synthesized.synth_ms as f32 / 1000.0).max(0.001),
    );

    let running = Arc::new(AtomicBool::new(true));
    let queued = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (tx, rx) = sync_channel::<Vec<f32>>(4);
    let (startup_tx, startup_rx) = sync_channel::<std::result::Result<(), String>>(1);
    queued.fetch_add(synthesized.samples.len() as u64, Ordering::Relaxed);
    let handle = asr_audio::spawn_render(
        args.device_id,
        synthesized.rate,
        running.clone(),
        rx,
        queued,
        startup_tx,
    )
    .context("arrancando la salida de audio")?;

    // Sin esperar el arranque, un dispositivo inexistente imprimiria "hecho"
    // sin haber sonado nada.
    match startup_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => anyhow::bail!("no se pudo abrir el dispositivo de salida: {e}"),
        Err(_) => anyhow::bail!("la salida de audio no respondio al abrir"),
    }

    tx.send(synthesized.samples)?;
    drop(tx); // el hilo de render termina solo cuando acabe de reproducir
    println!("reproduciendo...");
    let _ = handle.join();
    println!("hecho");

    sidecar.shutdown()?;
    Ok(())
}
