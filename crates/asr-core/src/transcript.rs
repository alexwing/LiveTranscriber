//! Historial de la transcripcion y volcado a disco.

use std::fmt::Write as _;
use std::path::Path;

use asr_audio::Source;

use crate::translate::TranslatedLine;

/// Una linea cerrada. Se cierra cuando el motor termina un segmento.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub source: Source,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

/// Acumula el texto de cada fuente y lo cierra en lineas.
///
/// Las traducciones van en su propia lista, no colgadas de cada [`Entry`]: se
/// traduce por frases y se transcribe por segmentos, y un segmento puede tener
/// varias frases. Emparejarlas uno a uno daria correspondencias falsas, asi
/// que cada [`TranslatedLine`] se guarda con su propio original.
#[derive(Debug, Default)]
pub struct Transcript {
    entries: Vec<Entry>,
    translations: Vec<TranslatedLine>,
    /// Texto en curso por fuente, con el instante en que empezo.
    open: Vec<(Source, u64, String)>,
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn translations(&self) -> &[TranslatedLine] {
        &self.translations
    }

    pub fn push_translation(&mut self, line: TranslatedLine) {
        self.translations.push(line);
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.open.is_empty() && self.translations.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.translations.clear();
        self.open.clear();
    }

    /// Texto parcial de una fuente, para pintarlo en vivo.
    pub fn pending(&self, source: Source) -> Option<&str> {
        self.open
            .iter()
            .find(|(s, _, _)| *s == source)
            .map(|(_, _, t)| t.as_str())
    }

    pub fn push_delta(&mut self, source: Source, at_ms: u64, text: &str) {
        match self.open.iter_mut().find(|(s, _, _)| *s == source) {
            Some((_, _, buf)) => buf.push_str(text),
            None => self.open.push((source, at_ms, text.to_string())),
        }
    }

    /// Cierra el segmento de una fuente. Devuelve la linea si tenia contenido.
    pub fn close_segment(&mut self, source: Source, at_ms: u64) -> Option<Entry> {
        let idx = self.open.iter().position(|(s, _, _)| *s == source)?;
        let (_, start_ms, text) = self.open.remove(idx);
        let text = text.trim().to_string();
        if text.is_empty() {
            return None;
        }
        let entry = Entry {
            source,
            start_ms,
            end_ms: at_ms.max(start_ms),
            text,
        };
        self.entries.push(entry.clone());
        Some(entry)
    }

    /// Cierra todo lo que quede abierto, por ejemplo al parar la captura.
    pub fn close_all(&mut self, at_ms: u64) -> Vec<Entry> {
        let sources: Vec<Source> = self.open.iter().map(|(s, _, _)| *s).collect();
        sources
            .into_iter()
            .filter_map(|s| self.close_segment(s, at_ms))
            .collect()
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for entry in &self.entries {
            let _ = writeln!(
                out,
                "[{}] [{}] {}",
                clock(entry.start_ms),
                label(entry.source),
                entry.text
            );
        }
        if !self.translations.is_empty() {
            let _ = writeln!(out, "\n--- traduccion ---");
            for line in &self.translations {
                let _ = writeln!(
                    out,
                    "[{}] [{}] {}",
                    clock(line.at_ms),
                    label(line.source),
                    line.translated
                );
            }
        }
        out
    }

    /// Texto con cada frase original seguida de su traduccion.
    pub fn to_bilingual_text(&self) -> String {
        let mut out = String::new();
        for line in &self.translations {
            let _ = writeln!(out, "[{}] {}", clock(line.at_ms), line.original);
            let _ = writeln!(out, "         {}", line.translated);
            let _ = writeln!(out);
        }
        out
    }

    /// Solo la traduccion, sin marcas de tiempo. Para copiar y pegar.
    pub fn translated_text(&self) -> String {
        self.translations
            .iter()
            .map(|line| line.translated.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Solo el original, sin marcas de tiempo.
    pub fn plain_text(&self) -> String {
        self.entries
            .iter()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn to_srt(&self) -> String {
        self.srt_from(self.entries.iter().map(|e| (e.start_ms, e.end_ms, e.text.as_str())))
    }

    /// SRT de la traduccion. Cada frase dura hasta la siguiente, con un tope
    /// de 5 s: el traductor no da duraciones, solo el instante de cierre.
    pub fn to_translated_srt(&self) -> String {
        let items: Vec<(u64, u64, &str)> = self
            .translations
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let next = self
                    .translations
                    .get(i + 1)
                    .map(|n| n.at_ms)
                    .unwrap_or(line.at_ms + 5_000);
                let end = next.min(line.at_ms + 5_000).max(line.at_ms + 800);
                (line.at_ms, end, line.translated.as_str())
            })
            .collect();
        self.srt_from(items.into_iter())
    }

    fn srt_from<'a>(&self, items: impl Iterator<Item = (u64, u64, &'a str)>) -> String {
        let mut out = String::new();
        for (i, (start, end, text)) in items.enumerate() {
            let _ = writeln!(out, "{}", i + 1);
            let _ = writeln!(out, "{} --> {}", srt_time(start), srt_time(end));
            let _ = writeln!(out, "{text}");
            let _ = writeln!(out);
        }
        out
    }

    pub fn save_text(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, self.to_text())
    }

    pub fn save_srt(&self, path: &Path) -> std::io::Result<()> {
        std::fs::write(path, self.to_srt())
    }
}

fn label(source: Source) -> &'static str {
    match source {
        Source::System => "sistema",
        Source::Mic => "micro",
    }
}

/// `hh:mm:ss` para el volcado en texto.
fn clock(ms: u64) -> String {
    let total = ms / 1000;
    format!("{:02}:{:02}:{:02}", total / 3600, (total / 60) % 60, total % 60)
}

/// `hh:mm:ss,mmm`, que es lo que exige SRT (coma, no punto).
fn srt_time(ms: u64) -> String {
    let total = ms / 1000;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        total / 3600,
        (total / 60) % 60,
        total % 60,
        ms % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srt_usa_coma_para_los_milisegundos() {
        assert_eq!(srt_time(0), "00:00:00,000");
        assert_eq!(srt_time(1_500), "00:00:01,500");
        assert_eq!(srt_time(3_661_042), "01:01:01,042");
    }

    #[test]
    fn acumula_deltas_y_cierra_una_linea() {
        let mut t = Transcript::new();
        t.push_delta(Source::System, 1_000, "hola ");
        t.push_delta(Source::System, 1_200, "que tal");
        let entry = t.close_segment(Source::System, 4_000).expect("hay linea");
        assert_eq!(entry.text, "hola que tal");
        assert_eq!(entry.start_ms, 1_000);
        assert_eq!(entry.end_ms, 4_000);
        assert_eq!(t.entries().len(), 1);
    }

    #[test]
    fn un_segmento_vacio_no_genera_linea() {
        let mut t = Transcript::new();
        t.push_delta(Source::Mic, 0, "   ");
        assert!(t.close_segment(Source::Mic, 100).is_none());
        assert!(t.entries().is_empty());
    }

    #[test]
    fn las_fuentes_no_se_mezclan() {
        let mut t = Transcript::new();
        t.push_delta(Source::System, 0, "de la peli");
        t.push_delta(Source::Mic, 0, "lo que digo yo");
        assert_eq!(t.pending(Source::System), Some("de la peli"));
        assert_eq!(t.pending(Source::Mic), Some("lo que digo yo"));
        t.close_all(1_000);
        assert_eq!(t.entries().len(), 2);
    }

    #[test]
    fn el_srt_va_numerado_desde_uno() {
        let mut t = Transcript::new();
        t.push_delta(Source::System, 0, "primera");
        t.close_segment(Source::System, 1_000);
        t.push_delta(Source::System, 2_000, "segunda");
        t.close_segment(Source::System, 3_000);
        let srt = t.to_srt();
        assert!(srt.starts_with("1\n00:00:00,000 --> 00:00:01,000\nprimera\n"));
        assert!(srt.contains("2\n00:00:02,000 --> 00:00:03,000\nsegunda\n"));
    }
}
