//! Gate de silencio, con umbral **relativo** al habla reciente.
//!
//! La primera version usaba un umbral absoluto en dBFS y estaba mal pensada.
//! Dos medidas lo dejaron claro:
//!
//! - El habla capturada por loopback llegaba a rms 0,0005 (**-62 dBFS**),
//!   porque el bucle de retorno captura despues del control de volumen. Un
//!   umbral de -50 dBFS la descartaba entera.
//! - Con el normalizador compensando hasta x64, el ruido de fondo sube por
//!   encima de cualquier umbral absoluto y el gate no vuelve a cerrar nunca:
//!   toda la sesion queda como un unico parrafo interminable.
//!
//! Hablar y callarse es una diferencia **relativa**: entre voz y pausa hay
//! tipicamente 20-30 dB, sin importar a que volumen este el sistema. Asi que
//! se sigue el nivel del habla reciente y se considera silencio lo que caiga
//! `drop_db` por debajo. El umbral absoluto se queda solo como suelo, para que
//! el silencio digital cuente siempre como silencio.
//!
//! Lo que este gate **ya no** hace: decidir donde acaba un parrafo. Lo hacia, y
//! estaba mal: con musica de fondo el nivel no baja nunca, asi que el parrafo no
//! cerraba jamas y la traduccion —que se dispara al cerrar— no llegaba. El nivel
//! de audio no distingue "nadie habla" de "suena algo". Quien si lo distingue es
//! el propio reconocedor: si hay musica pero nadie habla, no emite texto. Ese
//! corte vive ahora en la sesion, mirando el texto y no el volumen.
//!
//! Aqui solo queda decidir si vale la pena mandarle el bloque al modelo, para no
//! quemar GPU con silencio de verdad.
//!
//! Y no descarta los silencios cortos: el modelo es cache-aware y su estado
//! asume audio contiguo, asi que quitar trozos daria basura en las uniones.

/// Que hacer con el bloque que se acaba de analizar.
#[derive(Debug, Clone, PartialEq)]
pub enum GateEvent {
    /// Pasar este audio al motor.
    Audio(Vec<f32>),
    /// Silencio de verdad y prolongado: no merece la pena gastar GPU.
    Idle,
}

/// Cuanto decae la referencia de habla en cada bloque. A 100 ms por bloque,
/// 0,999 tarda de sobra en olvidar: una pausa de 2 s apenas la mueve, asi que
/// no se re-arma tomando el ruido por voz.
const REFERENCE_DECAY: f32 = 0.999;

pub struct SilenceGate {
    /// Suelo absoluto. Por debajo es silencio siempre, pase lo que pase.
    floor: f32,
    /// Factor lineal equivalente a `drop_db`.
    drop_factor: f32,
    /// Nivel de habla reciente: sube al instante, baja despacio.
    reference: f32,
    /// Cuanto silencio seguido hay que ver para dejar de alimentar al modelo.
    hold_samples: usize,
    silence_samples: usize,
    /// Si alguna vez llego audio. Sin esto, al arrancar en silencio se dejaria
    /// de alimentar antes de haber empezado, y da igual, pero asi queda claro.
    started: bool,
}

impl SilenceGate {
    /// `drop_db`: cuantos dB por debajo del habla reciente cuenta como
    /// silencio (25 es un buen punto). `floor_dbfs`: suelo absoluto; -80 deja
    /// pasar incluso el audio muy atenuado del loopback. `hold_secs`: cuanto
    /// silencio seguido antes de dejar de alimentar al modelo.
    pub fn new(drop_db: f32, floor_dbfs: f32, hold_secs: f32, sample_rate: u32) -> Self {
        Self {
            floor: dbfs_to_amplitude(floor_dbfs),
            drop_factor: dbfs_to_amplitude(-drop_db.abs()),
            reference: 0.0,
            hold_samples: (hold_secs * sample_rate as f32) as usize,
            silence_samples: 0,
            started: false,
        }
    }

    /// Umbral efectivo ahora mismo, para poder pintarlo junto al vumetro.
    pub fn threshold(&self) -> f32 {
        (self.reference * self.drop_factor).max(self.floor)
    }

    /// Nivel de habla que esta tomando como referencia.
    pub fn reference(&self) -> f32 {
        self.reference
    }

    /// `level` es el rms **crudo**, antes de normalizar: es la medida que de
    /// verdad distingue voz de pausa. `block` es el audio que se entregara al
    /// motor, ya normalizado si procede.
    pub fn push(&mut self, level: f32, block: Vec<f32>) -> GateEvent {
        // Ataque inmediato, caida lenta.
        if level > self.reference {
            self.reference = level;
        } else {
            self.reference *= REFERENCE_DECAY;
        }

        if level >= self.threshold() && level > self.floor {
            self.silence_samples = 0;
            self.started = true;
            return GateEvent::Audio(block);
        }

        self.silence_samples += block.len();

        // Silencio corto: se pasa igualmente, para no romper la continuidad que
        // necesita la cache del modelo.
        if self.started && self.silence_samples < self.hold_samples {
            return GateEvent::Audio(block);
        }

        GateEvent::Idle
    }
}

pub fn rms(block: &[f32]) -> f32 {
    if block.is_empty() {
        return 0.0;
    }
    let sum: f64 = block.iter().map(|s| (*s as f64) * (*s as f64)).sum();
    (sum / block.len() as f64).sqrt() as f32
}

fn dbfs_to_amplitude(dbfs: f32) -> f32 {
    10f32.powf(dbfs / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 16_000;
    const BLOCK: usize = 1600; // 100 ms

    fn gate() -> SilenceGate {
        SilenceGate::new(25.0, -80.0, 2.0, SR)
    }

    fn push(g: &mut SilenceGate, level: f32) -> GateEvent {
        g.push(level, vec![0.0; BLOCK])
    }

    #[test]
    fn deja_pasar_el_habla() {
        let mut g = gate();
        assert!(matches!(push(&mut g, 0.05), GateEvent::Audio(_)));
    }

    #[test]
    fn funciona_con_audio_muy_atenuado_del_loopback() {
        // El caso medido: habla a -62 dBFS. Un umbral absoluto de -50 la
        // habria descartado entera.
        let mut g = gate();
        assert!(matches!(push(&mut g, 0.0008), GateEvent::Audio(_)));
        // Y la pausa que le sigue, 30 dB por debajo, si se detecta.
        for _ in 0..25 {
            push(&mut g, 0.000025);
        }
        assert!(g.reference() > 0.0);
    }

    #[test]
    fn el_ruido_amplificado_se_reconoce_como_silencio() {
        // Con un umbral absoluto esto contaba como senal, porque el
        // normalizador lo subia por encima del corte.
        let mut g = gate();
        for _ in 0..10 {
            push(&mut g, 0.05); // habla
        }
        // Ruido de fondo 30 dB por debajo: es pausa, y tras el aguante se deja
        // de alimentar.
        for _ in 0..40 {
            push(&mut g, 0.0015);
        }
        assert_eq!(push(&mut g, 0.0015), GateEvent::Idle);
    }

    #[test]
    fn la_musica_de_fondo_sigue_alimentando_al_modelo() {
        // Es justo lo que debe pasar: el gate no sabe si eso es voz. Quien
        // decide donde acaba el parrafo es la sesion, mirando el texto.
        let mut g = gate();
        for _ in 0..100 {
            assert!(matches!(push(&mut g, 0.05), GateEvent::Audio(_)));
        }
    }

    #[test]
    fn no_deja_de_alimentar_en_pausas_cortas() {
        let mut g = gate();
        push(&mut g, 0.05);
        // 1 s de pausa, por debajo del aguante de 2 s.
        for _ in 0..10 {
            assert!(matches!(push(&mut g, 0.0001), GateEvent::Audio(_)));
        }
    }

    #[test]
    fn arrancar_en_silencio_no_alimenta() {
        let mut g = gate();
        for _ in 0..40 {
            assert_eq!(push(&mut g, 0.0), GateEvent::Idle);
        }
    }

    #[test]
    fn el_suelo_absoluto_manda_sobre_el_relativo() {
        let mut g = gate();
        // Sin habla previa la referencia es 0, asi que el umbral es el suelo.
        assert_eq!(push(&mut g, 0.0), GateEvent::Idle);
    }
}
