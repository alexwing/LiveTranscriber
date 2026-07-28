//! Salida de audio WASAPI: la otra mitad de la tuberia de `capture`.
//!
//! Recibe bloques de muestras f32 mono por un canal y los reproduce en el
//! dispositivo elegido. El caso de uso es el "microfono virtual": la voz
//! sintetizada se escribe en `CABLE Input` (VB-CABLE) y la reunion la escucha
//! por `CABLE Output`, sin pasar jamas por los altavoces. Eso ultimo no es un
//! detalle: como el habla sintetica no suena por el dispositivo que se captura
//! con loopback, no se realimenta a la propia transcripcion.
//!
//! El formato de entrada es el que produzca el sintetizador (24 kHz mono en
//! Chatterbox y Kokoro); la conversion al formato del dispositivo la hace el
//! motor de audio de Windows via `autoconvert`, igual que en la captura pero
//! en sentido contrario.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::Result;

/// Cuanto esperar el evento del dispositivo antes de mirar si hay que parar.
/// El mismo margen que en la captura y por el mismo motivo.
#[cfg(windows)]
const EVENT_TIMEOUT_MS: u32 = 200;

/// Arranca la reproduccion en su propio hilo.
///
/// - `device_id`: dispositivo de salida, `None` = el predeterminado. Para el
///   microfono virtual aqui va el id de `CABLE Input`.
/// - `sample_rate`: frecuencia de las muestras que llegan por `rx`.
/// - `queued`: contador compartido de muestras pendientes. Quien encola suma;
///   este hilo resta al escribirlas al dispositivo. Es lo que permite mostrar
///   cuanto retraso de voz se ha acumulado sin preguntarselo a nadie.
/// - `startup`: por aqui se responde **una vez** si el dispositivo abrio o no.
///   COM es por hilo, asi que abrir el dispositivo tiene que pasar dentro del
///   hilo; sin este aviso, quien arranca daria el dispositivo por bueno sin
///   saberlo, y un id caducado (VB-CABLE reinstalado, por ejemplo) dejaria al
///   usuario hablando a una reunion que no le oye, sin ningun error visible.
///
/// El hilo termina cuando `running` pasa a false, o cuando el emisor del canal
/// desaparece **y** ya no queda nada por reproducir: lo pendiente se termina de
/// decir, no se corta a media frase. Si muere por un error del dispositivo,
/// deja `running` en false para que quien dependa de el se entere.
#[cfg(windows)]
pub fn spawn_render(
    device_id: Option<String>,
    sample_rate: u32,
    running: Arc<AtomicBool>,
    rx: Receiver<Vec<f32>>,
    queued: Arc<AtomicU64>,
    startup: SyncSender<std::result::Result<(), String>>,
) -> Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("asr-render".into())
        .spawn(move || {
            if let Err(e) = render_loop(
                device_id.as_deref(),
                sample_rate,
                &running,
                &rx,
                &queued,
                &startup,
            ) {
                tracing::error!("reproduccion detenida: {e}");
            }
            running.store(false, Ordering::Relaxed);
            tracing::info!("hilo de reproduccion terminado");
        })
        .map_err(|e| crate::AudioError::Thread(e.to_string()))
}

#[cfg(not(windows))]
pub fn spawn_render(
    _device_id: Option<String>,
    _sample_rate: u32,
    _running: Arc<AtomicBool>,
    _rx: Receiver<Vec<f32>>,
    _queued: Arc<AtomicU64>,
    _startup: SyncSender<std::result::Result<(), String>>,
) -> Result<JoinHandle<()>> {
    Err(crate::AudioError::UnsupportedPlatform)
}

/// Abre e inicializa el dispositivo. Separado para poder responder por el
/// canal de arranque con un solo `match` en vez de interceptar cada `?`.
#[cfg(windows)]
fn open_render(
    device_id: Option<&str>,
    sample_rate: u32,
) -> Result<(wasapi::AudioClient, wasapi::Handle, wasapi::AudioRenderClient)> {
    use wasapi::{Direction, SampleType, StreamMode, WaveFormat};

    crate::capture::ensure_com()?;

    // Se pide el formato del sintetizador y `autoconvert` deja que Windows lo
    // adapte al del dispositivo. Es el espejo exacto de la captura.
    let format = WaveFormat::new(
        32,
        32,
        &SampleType::Float,
        sample_rate as usize,
        crate::TARGET_CHANNELS as usize,
        None,
    );

    let (mut client, buffer_hns) = crate::capture::open_device(&Direction::Render, device_id)?;
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: buffer_hns,
    };
    client.initialize_client(&format, &Direction::Render, &mode)?;

    let event = client.set_get_eventhandle()?;
    let render = client.get_audiorenderclient()?;
    client.start_stream()?;
    Ok((client, event, render))
}

#[cfg(windows)]
fn render_loop(
    device_id: Option<&str>,
    sample_rate: u32,
    running: &AtomicBool,
    rx: &Receiver<Vec<f32>>,
    queued: &AtomicU64,
    startup: &SyncSender<std::result::Result<(), String>>,
) -> Result<()> {
    use std::collections::VecDeque;
    use wasapi::WasapiError;

    let (client, event, render) = match open_render(device_id, sample_rate) {
        Ok(opened) => {
            let _ = startup.send(Ok(()));
            opened
        }
        Err(e) => {
            let _ = startup.send(Err(e.to_string()));
            return Err(e);
        }
    };
    tracing::info!(
        "reproduccion iniciada en {} a {sample_rate} Hz",
        device_id.unwrap_or("(predeterminado)")
    );

    // Bytes pendientes de escribir al dispositivo, ya en formato de cable.
    let mut raw: VecDeque<u8> = VecDeque::with_capacity(64 * 1024);
    let mut source_alive = true;

    while running.load(Ordering::Relaxed) {
        // Primero vaciar el canal para que `raw` tenga cuanto antes lo que
        // haya; el canal no es quien marca el ritmo, lo marca el dispositivo.
        loop {
            match rx.try_recv() {
                Ok(block) => {
                    raw.reserve(block.len() * 4);
                    for sample in block {
                        raw.extend(sample.to_le_bytes());
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    source_alive = false;
                    break;
                }
            }
        }

        // Sin emisor y sin nada pendiente ya no hay razon para seguir. Lo
        // pendiente se reproduce entero: cortar una frase a medias por cerrar
        // el canal seria perder habla ya sintetizada.
        if !source_alive && raw.is_empty() {
            break;
        }

        // El dispositivo dice cuanto acepta; se le da lo que haya de eso.
        let available = client.get_available_space_in_frames()? as usize;
        let frames = available.min(raw.len() / 4);
        if frames > 0 {
            render.write_to_device_from_deque(frames, &mut raw, None)?;
            // Restar lo escrito del contador de pendientes. `saturating` por
            // si quien encola y quien escribe se cruzan en el arranque.
            let _ = queued.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(frames as u64))
            });
        }

        match event.wait_for_event(EVENT_TIMEOUT_MS) {
            Ok(()) => {}
            // Sin consumo en el dispositivo: normal cuando no suena nada.
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
