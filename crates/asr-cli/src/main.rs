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
#[command(name = "asr-cli", about = "Headless capture and transcription")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Which {
    /// Everything playing on the system (loopback).
    System,
    /// The microphone.
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
    /// List input and output devices.
    Devices,

    /// Capture and show the level, without loading the model. Useful to confirm
    /// that loopback is delivering audio before bringing the GPU into it.
    Level {
        #[arg(long, value_enum, default_value = "system")]
        from: Which,
        #[arg(long)]
        device_id: Option<String>,
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        /// Capture only this process's audio (loopback by PID).
        #[arg(long)]
        pid: Option<u32>,
    },

    /// Transcribe for real, starting the Python sidecar.
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
        /// Interpreter of the venv with torch and transformers.
        #[arg(long)]
        python: Option<PathBuf>,
        /// Path to asr_server.py.
        #[arg(long, default_value = "sidecar/asr_server.py")]
        script: PathBuf,
        /// Save the transcript to this .txt when it finishes.
        #[arg(long)]
        save_txt: Option<PathBuf>,
        /// Save the transcript to this .srt when it finishes.
        #[arg(long)]
        save_srt: Option<PathBuf>,
        /// Translate to this locale as well (en-US, de-DE...). Requires
        /// --language to be specific: the translator needs to know the source.
        #[arg(long)]
        translate_to: Option<String>,
        /// Path to mt_server.py.
        #[arg(long, default_value = "sidecar/mt_server.py")]
        mt_script: PathBuf,
    },

    /// Synthesize a text and play it on an output device. It is the virtual
    /// microphone test without a meeting: it sends the voice to CABLE Input
    /// and, if something recording from CABLE Output hears it, the loop works.
    Speak {
        /// Text to say.
        #[arg(long)]
        text: String,
        /// Language of the text (short code: en, es, de...).
        #[arg(long, default_value = "en")]
        lang: String,
        /// Output device (id from `asr-cli devices`). Without it, the
        /// default one: it plays through the speakers.
        #[arg(long)]
        device_id: Option<String>,
        /// chatterbox (cloning) or kokoro (neutral voice, lighter).
        #[arg(long, default_value = "chatterbox")]
        engine: String,
        /// WAV with the voice to clone (required with chatterbox).
        #[arg(long)]
        voice_wav: Option<PathBuf>,
        /// Preset kokoro voice.
        #[arg(long, default_value = "af_heart")]
        kokoro_voice: String,
        /// Interpreter of the voice venv (not the ASR one).
        #[arg(long)]
        python: Option<PathBuf>,
        /// Path to tts_server.py.
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
        (DeviceKind::Output, "OUTPUTS (capturable by loopback)"),
        (DeviceKind::Input, "INPUTS (microphones)"),
    ] {
        println!("\n{title}");
        println!("{}", "-".repeat(title.len()));
        for device in list_devices(kind).context("enumerating devices")? {
            let mark = if device.is_default { "*" } else { " " };
            println!("{mark} {}\n    id: {}", device.name, device.id);
        }
    }
    println!("\n(* = default)");
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
    println!("capturing from {target:?} for {seconds}s...");
    println!("(if the level stays at 0, no audio is coming in)\n");

    let running = Arc::new(AtomicBool::new(true));
    let (tx, rx) = sync_channel::<Vec<f32>>(64);
    let handle = spawn_capture(target, running.clone(), tx).context("starting the capture")?;

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
                        "{}  rms {:.5}  gain x{:.1}{}",
                        meter(asr_audio::rms(&block)),
                        rms,
                        normalizer.gain(),
                        if normalizer.at_ceiling() { "  <-- at ceiling" } else { "" }
                    );
                }
            }
            Err(_) => {
                if !running.load(Ordering::Relaxed) {
                    println!("the capture stopped on its own (look at the errors above)");
                    break;
                }
                println!("... no data");
            }
        }
    }

    running.store(false, Ordering::Relaxed);
    let _ = handle.join();

    let secs = samples as f64 / asr_audio::TARGET_RATE as f64;
    println!("\n{blocks} blocks, {samples} samples = {secs:.2}s of audio at 16 kHz mono");
    println!("raw rms peak {peak:.5}  (final gain x{:.1})", normalizer.gain());

    if blocks == 0 {
        println!("\nNot a single block arrived. With loopback that means the device");
        println!("was completely idle: Windows generates no events if nobody is");
        println!("playing anything. Put on music or a video and try again.");
    } else if peak == 0.0 {
        println!("\nAudio arrived but all zeros: the device is muted.");
    } else if normalizer.at_ceiling() {
        println!("\nWarning: normalization stayed at the ceiling. The system volume");
        println!("is very low and loopback captures after the volume, so the");
        println!("transcription will come out poor. Turn up the Windows volume.");
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
    // La configuracion de verdad, no `default()`: sin esto la herramienta
    // validaba un mundo distinto del que ejecuta la aplicacion, y el README
    // mandaba probar el circuito con unos valores que solo existian en la
    // maquina de quien lo escribio.
    let defaults = AppConfig::load(&asr_core::config_location()).unwrap_or_default();
    let mut sidecar = defaults.sidecar();
    sidecar.python = args.python.unwrap_or(defaults.python);
    sidecar.script = args.script;
    sidecar.language = args.language;
    sidecar.lookahead = args.lookahead;

    anyhow::ensure!(
        sidecar.script.exists(),
        "cannot find the sidecar script at {}",
        sidecar.script.display()
    );
    // El valor por defecto es vacio a proposito (ver AppConfig::default), asi
    // que sin --python no hay nada que lanzar. Decirlo aqui evita intentar
    // ejecutar la cadena vacia y que el error sea del sistema operativo.
    anyhow::ensure!(
        !sidecar.python.as_os_str().is_empty(),
        "no Python interpreter given: pass --python <path to the venv python.exe>"
    );
    anyhow::ensure!(
        sidecar.python.exists(),
        "cannot find the Python interpreter at {} (pass it with --python)",
        sidecar.python.display()
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
                "to translate you have to pass --language with a specific language: \
                 the translator needs to know which one it starts from"
            );
            let mt = MtConfig {
                python: sidecar.python.clone(),
                script: args.mt_script,
                dtype: sidecar.dtype.clone(),
                hf_home: sidecar.hf_home.clone(),
            };
            anyhow::ensure!(
                mt.script.exists(),
                "cannot find the translation sidecar at {}",
                mt.script.display()
            );
            println!("starting the translator (the first time it downloads NLLB)...");
            let mt_sidecar = MtSidecar::spawn(&mt)?;
            let device = mt_sidecar.wait_ready(Duration::from_secs(300))?;
            println!("translator ready on {device}, target {target}");
            // La bomba lleva un par (origen, destino) por fuente. El CLI
            // captura una sola, y el usuario pide "transcribe --language X y
            // traduce a Y": ese es el par de su fuente; el de la otra no se
            // usa, asi que se pasa el mismo.
            let pair = (sidecar.language.as_str(), target);
            Some(TranslationPump::new(Box::new(mt_sidecar), pair, pair)?)
        }
    };

    println!("starting the model (takes a few seconds)...");
    let (tx, rx) = std::sync::mpsc::channel::<SessionEvent>();
    let session = Session::start(cfg, &sidecar, tx).context("starting the session")?;

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
                println!("engine ready on {device}, latency {latency_ms} ms, language {language}");
                println!("--- transcript ---");
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
                    tracing::debug!("level {rms:.5}");
                }
            }
            SessionEvent::Error { message, .. } => eprintln!("\n[error] {message}"),
            SessionEvent::Stopped { .. } => {
                println!("\nthe session stopped");
                break;
            }
        }
    }

    println!("\n--- end ---");
    session.join();
    transcript.close_all(args.seconds * 1000);
    if let Some(mut pump) = pump {
        pump.shutdown();
    }

    if transcript.is_empty() {
        println!("nothing was transcribed");
    } else {
        println!("\n{}", transcript.to_text());
    }

    if let Some(path) = args.save_txt {
        transcript.save_text(&path)?;
        println!("saved {}", path.display());
    }
    if let Some(path) = args.save_srt {
        transcript.save_srt(&path)?;
        println!("saved {}", path.display());
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
    // La configuracion de verdad, no `default()`: sin esto la herramienta
    // validaba un mundo distinto del que ejecuta la aplicacion, y el README
    // mandaba probar el circuito con unos valores que solo existian en la
    // maquina de quien lo escribio.
    let defaults = AppConfig::load(&asr_core::config_location()).unwrap_or_default();
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
        "cannot find the voice sidecar at {}",
        tts.script.display()
    );
    anyhow::ensure!(
        !tts.python.as_os_str().is_empty(),
        "no voice interpreter given: pass --python <path to the .venv-tts python.exe>"
    );
    anyhow::ensure!(
        tts.python.exists(),
        "cannot find the voice venv interpreter at {} (pass it with --python)",
        tts.python.display()
    );
    if tts.engine == "chatterbox" {
        anyhow::ensure!(
            tts.voice_wav.is_some(),
            "chatterbox clones a voice: pass --voice-wav with 10-30 s of clean \
             speech, or use --engine kokoro for a neutral voice"
        );
    }

    println!("starting the synthesizer (chatterbox takes ~21 s cold)...");
    let mut sidecar = TtsSidecar::spawn(&tts)?;
    let ready = sidecar.wait_ready(Duration::from_secs(300))?;
    println!("synthesizer ready on {} at {} Hz", ready.device, ready.rate);

    let synthesized = sidecar.synthesize(&args.text, &args.lang)?;
    let audio_secs = synthesized.samples.len() as f32 / synthesized.rate as f32;
    println!(
        "synthesized: {audio_secs:.2}s of audio in {} ms (RTFx {:.2}x)",
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
    .context("starting the audio output")?;

    // Sin esperar el arranque, un dispositivo inexistente imprimiria "done"
    // sin haber sonado nada.
    match startup_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => anyhow::bail!("could not open the output device: {e}"),
        Err(_) => anyhow::bail!("the audio output did not respond on open"),
    }

    tx.send(synthesized.samples)?;
    drop(tx); // el hilo de render termina solo cuando acabe de reproducir
    println!("playing...");
    let _ = handle.join();
    println!("done");

    sidecar.shutdown()?;
    Ok(())
}
