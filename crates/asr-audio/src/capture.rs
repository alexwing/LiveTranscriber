//! Hilos de captura WASAPI. Todos entregan lo mismo aguas abajo: bloques de
//! muestras f32 mono a 16 kHz.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::Result;

/// Cuanto acumular antes de mandar aguas abajo. 100 ms no satura el canal y
/// queda muy por debajo del chunk mas pequeno del modelo (105 ms).
const SEND_SAMPLES: usize = crate::TARGET_RATE as usize / 10;

/// Buffer a pedir cuando no podemos preguntarle su periodo al dispositivo.
#[cfg(windows)]
const DEFAULT_BUFFER_HNS: i64 = 200_000; // 20 ms en unidades de 100 ns

/// Cuanto esperar un evento antes de volver a mirar si nos han mandado parar.
/// Un dispositivo de salida en silencio puede no generar eventos, asi que
/// agotar este plazo es normal y no significa que algo vaya mal.
#[cfg(windows)]
const EVENT_TIMEOUT_MS: u32 = 200;

/// De donde sacar el audio.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum CaptureTarget {
    /// Todo lo que suena por un dispositivo de salida. `None` = el predeterminado.
    Loopback { device_id: Option<String> },
    /// Un microfono. `None` = el predeterminado.
    Microphone { device_id: Option<String> },
    /// Solo el audio de un proceso, y opcionalmente el de sus hijos. Sirve para
    /// transcribir Teams sin que se cuele la musica que suene a la vez.
    Process { pid: u32, include_children: bool },
}

/// Arranca la captura en su propio hilo. El hilo termina cuando `running` pasa
/// a false, cuando el receptor del canal desaparece, o ante un error de WASAPI
/// (en cuyo caso deja `running` en false para que quien mande se entere).
#[cfg(windows)]
pub fn spawn_capture(
    target: CaptureTarget,
    running: Arc<AtomicBool>,
    tx: SyncSender<Vec<f32>>,
) -> Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("asr-capture".into())
        .spawn(move || {
            if let Err(e) = capture_loop(&target, &running, &tx) {
                tracing::error!("captura detenida: {e}");
            }
            running.store(false, Ordering::Relaxed);
            tracing::info!("hilo de captura terminado");
        })
        .map_err(|e| crate::AudioError::Thread(e.to_string()))
}

#[cfg(not(windows))]
pub fn spawn_capture(
    _target: CaptureTarget,
    _running: Arc<AtomicBool>,
    _tx: SyncSender<Vec<f32>>,
) -> Result<JoinHandle<()>> {
    Err(crate::AudioError::UnsupportedPlatform)
}

/// `RPC_E_CHANGED_MODE`: el hilo ya tenia COM inicializado en otro modo.
#[cfg(windows)]
const RPC_E_CHANGED_MODE: i32 = 0x8001_0106u32 as i32;

/// COM tiene que estar inicializado en cada hilo que toque WASAPI.
///
/// Llamarlo de mas es inofensivo: si ya estaba en MTA devuelve S_FALSE, que no
/// es error. Y si el hilo ya estaba en STA (le pasa al hilo de comandos de
/// Tauri, que WebView2 deja asi) tampoco lo tratamos como fallo: enumerar y
/// capturar funcionan igual, y reventar ahi dejaria la app sin dispositivos.
#[cfg(windows)]
pub(crate) fn ensure_com() -> Result<()> {
    let hr = wasapi::initialize_mta();
    if hr.is_ok() || hr.0 == RPC_E_CHANGED_MODE {
        return Ok(());
    }
    Err(crate::AudioError::Com(format!("HRESULT 0x{:08x}", hr.0)))
}

#[cfg(windows)]
fn capture_loop(
    target: &CaptureTarget,
    running: &AtomicBool,
    tx: &SyncSender<Vec<f32>>,
) -> Result<()> {
    use std::collections::VecDeque;
    use wasapi::{Direction, SampleType, StreamMode, WasapiError, WaveFormat};

    ensure_com()?;

    // Pedimos directamente el formato que quiere el modelo. Con `autoconvert`
    // el crate activa AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | SRC_DEFAULT_QUALITY,
    // asi que el downmix a mono y el remuestreo a 16 kHz los hace el motor de
    // audio de Windows. Para voz sobra, y nos ahorra arrastrar un resampler.
    let format = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        crate::TARGET_RATE as usize,
        crate::TARGET_CHANNELS as usize,
        None,
    );

    let (mut client, buffer_hns) = build_client(target)?;
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: buffer_hns,
    };
    // Pedir Direction::Capture sobre un dispositivo abierto como Render es
    // justo lo que hace que el crate active AUDCLNT_STREAMFLAGS_LOOPBACK.
    client.initialize_client(&format, &Direction::Capture, &mode)?;

    let event = client.set_get_eventhandle()?;
    let capture = client.get_audiocaptureclient()?;
    client.start_stream()?;
    tracing::info!("captura iniciada: {target:?}");

    let mut raw: VecDeque<u8> = VecDeque::with_capacity(64 * 1024);
    let mut pending: Vec<f32> = Vec::with_capacity(SEND_SAMPLES * 2);

    while running.load(Ordering::Relaxed) {
        capture.read_from_device_to_deque(&mut raw)?;

        // El formato negociado es mono f32, asi que cada 4 bytes es una muestra.
        while raw.len() >= 4 {
            let bytes = [
                raw.pop_front().unwrap(),
                raw.pop_front().unwrap(),
                raw.pop_front().unwrap(),
                raw.pop_front().unwrap(),
            ];
            pending.push(f32::from_le_bytes(bytes));
        }

        if pending.len() >= SEND_SAMPLES {
            let chunk = std::mem::replace(&mut pending, Vec::with_capacity(SEND_SAMPLES * 2));
            // Si el consumidor se ha ido, capturar mas no sirve de nada.
            if tx.send(chunk).is_err() {
                tracing::info!("el consumidor cerro el canal, parando captura");
                break;
            }
        }

        match event.wait_for_event(EVENT_TIMEOUT_MS) {
            Ok(()) => {}
            // Silencio en el dispositivo: normal, seguimos.
            Err(WasapiError::EventTimeout) => {}
            Err(e) => {
                let _ = client.stop_stream();
                return Err(e.into());
            }
        }
    }

    client.stop_stream()?;
    Ok(())
}

/// Devuelve el cliente ya abierto y el tamano de buffer a pedirle.
#[cfg(windows)]
fn build_client(target: &CaptureTarget) -> Result<(wasapi::AudioClient, i64)> {
    use wasapi::{AudioClient, Direction};

    match target {
        CaptureTarget::Process {
            pid,
            include_children,
        } => {
            // En loopback por proceso get_device_period() no funciona, y segun
            // la doc del crate el buffer que se pase da igual.
            let client = AudioClient::new_application_loopback_client(*pid, *include_children)?;
            Ok((client, DEFAULT_BUFFER_HNS))
        }
        CaptureTarget::Loopback { device_id } => {
            open_device(&Direction::Render, device_id.as_deref())
        }
        CaptureTarget::Microphone { device_id } => {
            open_device(&Direction::Capture, device_id.as_deref())
        }
    }
}

#[cfg(windows)]
fn open_device(
    direction: &wasapi::Direction,
    device_id: Option<&str>,
) -> Result<(wasapi::AudioClient, i64)> {
    use wasapi::DeviceEnumerator;

    let enumerator = DeviceEnumerator::new()?;
    let device = match device_id {
        None => enumerator.get_default_device(direction)?,
        Some(wanted) => {
            let collection = enumerator.get_device_collection(direction)?;
            let mut found = None;
            for device in &collection {
                let device = device?;
                if device.get_id().map(|id| id == wanted).unwrap_or(false) {
                    found = Some(device);
                    break;
                }
            }
            found.ok_or_else(|| crate::AudioError::DeviceNotFound(wanted.to_string()))?
        }
    };

    let client = device.get_iaudioclient()?;
    let buffer_hns = client
        .get_device_period()
        .map(|(_, min)| min)
        .unwrap_or(DEFAULT_BUFFER_HNS);
    Ok((client, buffer_hns))
}
