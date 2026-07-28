import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import {
  AppConfig,
  AudioDevice,
  Entry,
  ExportFormat,
  LANGUAGES,
  LOOKAHEADS,
  SPLITS,
  SessionEvent,
  Source,
  SpeakConfig,
  SpeechEvent,
  Split,
  Tab,
  TranslatedLine,
  api,
  copyText,
  formatClock,
  groupByParagraph,
} from "./tauri";

type Partials = Partial<Record<Source, string>>;
type Levels = Partial<Record<Source, { rms: number; gain: number; ceiling: boolean }>>;

export default function App() {
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [outputs, setOutputs] = useState<AudioDevice[]>([]);
  const [inputs, setInputs] = useState<AudioDevice[]>([]);
  const [running, setRunning] = useState(false);
  const [entries, setEntries] = useState<Entry[]>([]);
  const [translations, setTranslations] = useState<TranslatedLine[]>([]);
  const [partials, setPartials] = useState<Partials>({});
  const [levels, setLevels] = useState<Levels>({});
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");
  const [toast, setToast] = useState("");
  /// Cola de la voz sintetica. `null` = la voz no esta en marcha. El retraso
  /// acumulado es EL dato a vigilar: si crece sin parar, hablas mas rapido de
  /// lo que el sintetizador genera.
  const [speech, setSpeech] = useState<{ pending: number; queuedMs: number } | null>(null);
  /// Ruta efectiva que devuelve Rust, que puede no ser la del TOML si la
  /// configurada era relativa.
  const [outputDir, setOutputDir] = useState("");
  /// Nombre que tendria el fichero. Lo calcula Rust para no duplicar aqui las
  /// reglas de fecha y saneado.
  const [filenamePreview, setFilenamePreview] = useState("");

  // Preferencias de interfaz: viven en el navegador y no ensucian el TOML.
  const [tab, setTab] = useState<Tab>(
    () => (localStorage.getItem("tab") as Tab) ?? "config"
  );
  const [split, setSplit] = useState<Split>(
    () => (localStorage.getItem("split") as Split) ?? "combined"
  );
  const [splitPct, setSplitPct] = useState(
    () => Number(localStorage.getItem("splitPct")) || 50
  );

  useEffect(() => localStorage.setItem("tab", tab), [tab]);
  useEffect(() => localStorage.setItem("split", split), [split]);
  useEffect(() => localStorage.setItem("splitPct", String(splitPct)), [splitPct]);

  useEffect(() => {
    (async () => {
      try {
        setConfig(await api.getConfig());
        setOutputs(await api.listDevices("output"));
        setInputs(await api.listDevices("input"));
        setRunning(await api.isRunning());
        setEntries(await api.getTranscript());
        setTranslations(await api.getTranslations());
        setOutputDir(await api.outputDir());
      } catch (e: any) {
        setError(e.message);
      }
    })();
  }, []);

  useEffect(() => {
    const off = [
      listen<SessionEvent>("session-event", ({ payload }) => {
        switch (payload.kind) {
          case "ready":
            setStatus(
              `Motor listo en ${payload.device}, latencia ${payload.latency_ms} ms, idioma ${payload.language}`
            );
            break;
          case "delta":
            setPartials((p) => ({
              ...p,
              [payload.source]: (p[payload.source] ?? "") + payload.text,
            }));
            break;
          case "segment_end":
            setPartials((p) => ({ ...p, [payload.source]: "" }));
            break;
          case "level":
            setLevels((l) => ({
              ...l,
              [payload.source]: {
                rms: payload.rms,
                gain: payload.gain,
                ceiling: payload.gain_at_ceiling,
              },
            }));
            break;
          case "error":
            setError(payload.message);
            break;
          case "stopped":
            setStatus("Sesion detenida");
            break;
        }
      }),
      listen<Entry>("transcript-entry", ({ payload }) => setEntries((e) => [...e, payload])),
      listen<TranslatedLine>("translation", ({ payload }) =>
        setTranslations((t) => [...t, payload])
      ),
      listen<{ device: string; target: string }>("translator-ready", ({ payload }) =>
        setStatus(`Traductor listo en ${payload.device}, destino ${payload.target}`)
      ),
      listen<SpeechEvent>("speech-event", ({ payload }) => {
        switch (payload.kind) {
          case "ready":
            setStatus(`Voz sintetica lista en ${payload.device}`);
            setSpeech({ pending: 0, queuedMs: 0 });
            break;
          case "queue":
            setSpeech({ pending: payload.pending_texts, queuedMs: payload.queued_ms });
            break;
          case "spoke":
            break; // la cola ya lo refleja
          case "error":
            setError(payload.message);
            break;
          case "stopped":
            setSpeech(null);
            break;
        }
      }),
      listen<boolean>("running-changed", ({ payload }) => {
        setRunning(payload);
        if (!payload) setSpeech(null);
      }),
      listen<string>("error", ({ payload }) => setError(payload)),
    ];
    return () => {
      off.forEach((p) => p.then((f) => f()));
    };
  }, []);

  const patch = useCallback(
    async (changes: Partial<AppConfig>) => {
      if (!config) return;
      const next = { ...config, ...changes };
      setConfig(next);
      try {
        await api.saveConfig(next);
      } catch (e: any) {
        setError(e.message);
      }
    },
    [config]
  );

  // La vista previa se pide despues de guardar, para que refleje lo que Rust
  // tiene de verdad y no lo que la interfaz cree.
  useEffect(() => {
    if (!config) return;
    api
      .outputFilenamePreview("txt")
      .then(setFilenamePreview)
      .catch(() => setFilenamePreview(""));
  }, [config?.output_name]);

  function flash(message: string) {
    setToast(message);
    window.setTimeout(() => setToast(""), 1800);
  }

  async function toggleRun() {
    setError("");
    try {
      if (running) {
        await api.stop();
      } else {
        setStatus("Arrancando… la primera vez tarda unos segundos");
        await api.start();
        setTab("transcript");
      }
    } catch (e: any) {
      setError(e.message);
      setStatus("");
    }
  }

  async function copyAll(what: "original" | "translated" | "both") {
    try {
      const text = await api.transcriptAsText(what);
      if (!text.trim()) {
        flash("No hay nada que copiar");
        return;
      }
      await copyText(text);
      flash("Copiado");
    } catch (e: any) {
      setError(e.message);
    }
  }

  async function doExport(format: ExportFormat) {
    try {
      const saved = await api.exportTranscript(format);
      flash(`Guardado: ${saved}`);
    } catch (e: any) {
      setError(e.message);
    }
  }

  if (!config) {
    return (
      <main className="app">
        <p className="muted">Cargando…</p>
      </main>
    );
  }

  const lowVolume = Object.values(levels).some((l) => l?.ceiling);
  const autoBlocksTranslation = config.translate && config.language === "auto";
  const speakMisconfigured =
    config.speak.enabled && (!config.translate || !config.capture_mic);
  // Con mas de ~10 s de retraso la conversacion deja de ser conversacion.
  const speechBehind = speech !== null && speech.queuedMs > 10_000;

  return (
    <main className="app">
      <header className="bar">
        <h1>LiveTranscriber</h1>
        <nav className="tabs">
          <button className={tab === "config" ? "on" : ""} onClick={() => setTab("config")}>
            Configuracion
          </button>
          <button
            className={tab === "transcript" ? "on" : ""}
            onClick={() => setTab("transcript")}
          >
            Transcripcion
          </button>
        </nav>
        <button className={running ? "stop" : "start"} onClick={toggleRun}>
          {running ? "Parar" : "Arrancar"}
        </button>
      </header>

      {error && (
        <p className="error" onClick={() => setError("")}>
          {error}
        </p>
      )}
      {status && !error && <p className="status">{status}</p>}
      {autoBlocksTranslation && (
        <p className="warn">
          La traduccion esta activada pero el idioma es <em>Detectar automaticamente</em>.
          El traductor necesita saber desde que idioma parte, asi que elige uno concreto.
        </p>
      )}
      {lowVolume && (
        <p className="warn">
          El volumen de Windows esta muy bajo. El bucle de retorno captura
          <em> despues </em> del control de volumen, asi que la transcripcion saldra
          pobre aunque la ganancia ya este al maximo.
        </p>
      )}
      {speakMisconfigured && (
        <p className="warn">
          Hablar con tu voz necesita <em>Traducir en paralelo</em> y{" "}
          <em>Mi microfono</em> activados: lo que se habla es la traduccion de lo
          que dices por el micro.
        </p>
      )}
      {speech !== null && running && (
        <p className={speechBehind ? "warn" : "status"}>
          Voz sintetica: {(speech.queuedMs / 1000).toFixed(1)} s en cola
          {speech.pending > 0 ? ` · ${speech.pending} frases esperando` : ""}
          {speechBehind &&
            " — se esta generando mas despacio de lo que hablas; haz una pausa o agrupa mas caracteres"}
        </p>
      )}

      {tab === "config" ? (
        <ConfigPane
          config={config}
          outputs={outputs}
          inputs={inputs}
          levels={levels}
          running={running}
          patch={patch}
          outputDir={outputDir}
          filenamePreview={filenamePreview}
          onPickDir={async () => {
            try {
              const picked = await api.pickOutputDir();
              if (picked) {
                setOutputDir(picked);
                setConfig({ ...config, output_dir: picked });
                flash("Carpeta guardada");
              }
            } catch (e: any) {
              setError(e.message);
            }
          }}
          onRevealDir={async () => {
            try {
              await api.revealOutputDir();
            } catch (e: any) {
              setError(e.message);
            }
          }}
        />
      ) : (
        <TranscriptTab
          entries={entries}
          translations={translations}
          partials={partials}
          split={split}
          setSplit={setSplit}
          splitPct={splitPct}
          setSplitPct={setSplitPct}
          translateOn={config.translate}
          onCopyAll={copyAll}
          onExport={doExport}
          onClear={async () => {
            await api.clearTranscript();
            setEntries([]);
            setTranslations([]);
            setPartials({});
          }}
          onOverlay={() => api.toggleOverlay()}
          onCopyLine={async (text) => {
            await copyText(text);
            flash("Copiado");
          }}
        />
      )}

      {toast && <p className="toast">{toast}</p>}
      <footer className="hints">
        {config.hotkey_toggle} arranca y para · {config.hotkey_overlay} muestra los subtitulos
      </footer>
    </main>
  );
}

// --------------------------------------------------------- pestana de config

function ConfigPane({
  config,
  outputs,
  inputs,
  levels,
  running,
  patch,
  outputDir,
  filenamePreview,
  onPickDir,
  onRevealDir,
}: {
  config: AppConfig;
  outputs: AudioDevice[];
  inputs: AudioDevice[];
  levels: Levels;
  running: boolean;
  patch: (c: Partial<AppConfig>) => void;
  outputDir: string;
  filenamePreview: string;
  onPickDir: () => void;
  onRevealDir: () => void;
}) {
  return (
    <div className="scroll">
      <section className="panel">
        <h2>Carpeta de trabajo</h2>
        <div className="path-row">
          <code className="path" title={outputDir}>
            {outputDir || "…"}
          </code>
          <button onClick={onPickDir}>Cambiar…</button>
          <button onClick={onRevealDir}>Abrir</button>
        </div>

        <label className="field">
          <span>Nombre del fichero</span>
          <input
            type="text"
            className="text-input"
            value={config.output_name}
            placeholder="transcripcion"
            onChange={(e) => patch({ output_name: e.target.value })}
          />
        </label>
        <p className="note">
          La fecha va delante: <code>{filenamePreview || "…"}</code>. Si ya existe
          uno con ese nombre se añade <code>_2</code>, <code>_3</code>… en vez de
          sobreescribirlo. Los caracteres que Windows no admite se cambian por
          <code> _ </code>.
        </p>
        <p className="note">
          La carpeta tiene que ser absoluta: una ruta relativa dependeria del
          directorio desde el que se lanza la app, y no sabrias donde han acabado
          los ficheros.
        </p>
      </section>

      <section className="panel">
        <h2>Fuentes</h2>
        <label className="row">
          <input
            type="checkbox"
            checked={config.capture_system}
            disabled={running}
            onChange={(e) => patch({ capture_system: e.target.checked })}
          />
          <span>Audio del sistema</span>
          <Meter level={levels.system} />
        </label>
        <select
          disabled={running || !config.capture_system}
          value={config.system_device_id ?? ""}
          onChange={(e) => patch({ system_device_id: e.target.value || null })}
        >
          <option value="">Dispositivo predeterminado</option>
          {outputs.map((d) => (
            <option key={d.id} value={d.id}>
              {d.name}
              {d.is_default ? " (predeterminado)" : ""}
            </option>
          ))}
        </select>

        <label className="row">
          <input
            type="checkbox"
            checked={config.capture_mic}
            disabled={running}
            onChange={(e) => patch({ capture_mic: e.target.checked })}
          />
          <span>Mi microfono</span>
          <Meter level={levels.mic} />
        </label>
        <select
          disabled={running || !config.capture_mic}
          value={config.mic_device_id ?? ""}
          onChange={(e) => patch({ mic_device_id: e.target.value || null })}
        >
          <option value="">Dispositivo predeterminado</option>
          {inputs.map((d) => (
            <option key={d.id} value={d.id}>
              {d.name}
              {d.is_default ? " (predeterminado)" : ""}
            </option>
          ))}
        </select>
      </section>

      <section className="panel">
        <h2>Transcripcion</h2>
        <label className="field">
          <span>Idioma hablado</span>
          <select
            disabled={running}
            value={config.language}
            onChange={(e) => patch({ language: e.target.value })}
          >
            {LANGUAGES.map(([code, name]) => (
              <option key={code} value={code}>
                {name}
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span>Latencia</span>
          <select
            disabled={running}
            value={config.lookahead}
            onChange={(e) => patch({ lookahead: Number(e.target.value) })}
          >
            {LOOKAHEADS.map(([value, name]) => (
              <option key={value} value={value}>
                {name}
              </option>
            ))}
          </select>
        </label>
        <label className="row">
          <input
            type="checkbox"
            checked={config.normalize_gain}
            disabled={running}
            onChange={(e) => patch({ normalize_gain: e.target.checked })}
          />
          <span>Compensar el volumen del sistema</span>
        </label>
      </section>

      <section className="panel">
        <h2>Parrafos</h2>
        <label className="field">
          <span>Pausa del habla</span>
          <select
            disabled={running}
            value={config.paragraph_idle_secs}
            onChange={(e) => patch({ paragraph_idle_secs: Number(e.target.value) })}
          >
            {[0.6, 0.8, 1.2, 1.8, 2.5].map((v) => (
              <option key={v} value={v}>
                {v} s
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span>Parrafo maximo</span>
          <select
            disabled={running}
            value={config.paragraph_max_secs}
            onChange={(e) => patch({ paragraph_max_secs: Number(e.target.value) })}
          >
            {[15, 20, 30, 45, 60].map((v) => (
              <option key={v} value={v}>
                {v} s
              </option>
            ))}
          </select>
        </label>
        <p className="note">
          El parrafo se cierra cuando el modelo lleva ese rato sin transcribir
          nada nuevo. Se mira el <em>texto</em>, no el volumen: con musica de
          fondo el nivel no baja nunca, pero la musica tampoco genera
          transcripcion. El maximo evita que un monologo sin pausas quede como un
          bloque interminable.
        </p>
      </section>

      <section className="panel">
        <h2>Traduccion</h2>
        <label className="row">
          <input
            type="checkbox"
            checked={config.translate}
            disabled={running}
            onChange={(e) => patch({ translate: e.target.checked })}
          />
          <span>Traducir en paralelo</span>
        </label>
        <label className="field">
          <span>Traducir a</span>
          <select
            disabled={running || !config.translate}
            value={config.target_language}
            onChange={(e) => patch({ target_language: e.target.value })}
          >
            {LANGUAGES.filter(([code]) => code !== "auto").map(([code, name]) => (
              <option key={code} value={code}>
                {name}
              </option>
            ))}
          </select>
        </label>
        <p className="note">
          El modelo de voz no traduce: lo hace NLLB-200 despues, un parrafo
          entero cada vez. Los fallos del reconocimiento se arrastran a la
          traduccion.
        </p>
      </section>

      <SpeakPane config={config} outputs={outputs} running={running} patch={patch} />
    </div>
  );
}

/// Seccion de la voz sintetica. Es una funcion opcional entera: se activa
/// aqui, independiente del resto, y apagada no cuesta nada (ni proceso ni
/// VRAM). Habla SOLO la traduccion de lo que dices por el microfono.
function SpeakPane({
  config,
  outputs,
  running,
  patch,
}: {
  config: AppConfig;
  outputs: AudioDevice[];
  running: boolean;
  patch: (c: Partial<AppConfig>) => void;
}) {
  const speak = config.speak;
  const patchSpeak = (changes: Partial<SpeakConfig>) =>
    patch({ speak: { ...speak, ...changes } });

  const cable = outputs.find((d) => d.name.toLowerCase().includes("cable input"));

  return (
    <section className="panel">
      <h2>Hablar por mi</h2>
      <label className="row">
        <input
          type="checkbox"
          checked={speak.enabled}
          disabled={running}
          onChange={(e) => patchSpeak({ enabled: e.target.checked })}
        />
        <span>Pronunciar mi traduccion por un dispositivo de salida</span>
      </label>
      <p className="note">
        Lo que dices por el microfono, ya traducido, sale hablado por el
        dispositivo elegido. Con VB-CABLE como dispositivo y{" "}
        <code>CABLE Output</code> como microfono de la reunion, los demas te
        oyen en su idioma. Necesita <em>Traducir en paralelo</em> y{" "}
        <em>Mi microfono</em> activados.
      </p>

      <label className="field">
        <span>Motor</span>
        <select
          disabled={running || !speak.enabled}
          value={speak.engine}
          onChange={(e) =>
            patchSpeak({ engine: e.target.value as SpeakConfig["engine"] })
          }
        >
          <option value="chatterbox">Chatterbox — mi voz clonada (23 idiomas)</option>
          <option value="kokoro">Kokoro — voz neutra (8 idiomas, mas ligero)</option>
        </select>
      </label>

      {speak.engine === "chatterbox" ? (
        <>
          <label className="field">
            <span>Muestra de mi voz</span>
            <input
              type="text"
              className="text-input"
              disabled={running || !speak.enabled}
              value={speak.voice_wav ?? ""}
              placeholder={"C:\\...\\mi-voz.wav"}
              onChange={(e) => patchSpeak({ voice_wav: e.target.value || null })}
            />
          </label>
          <p className="note">
            Un WAV con 10-30 segundos de tu habla, limpia y sin ruido de fondo.
            La voz clonada imita el tono de la muestra: si la muestra es
            monotona, la voz tambien lo sera.
          </p>
        </>
      ) : (
        <>
          <label className="field">
            <span>Voz preajustada</span>
            <input
              type="text"
              className="text-input"
              disabled={running || !speak.enabled}
              value={speak.kokoro_voice}
              placeholder="af_heart"
              onChange={(e) => patchSpeak({ kokoro_voice: e.target.value })}
            />
          </label>
          <p className="note">
            El prefijo dice idioma y genero: <code>af_heart</code> (ingles, f),{" "}
            <code>am_adam</code> (ingles, m), <code>ef_dora</code> (espanol, f),{" "}
            <code>em_alex</code> (espanol, m)…
          </p>
        </>
      )}

      <label className="field">
        <span>Hablar por</span>
        <select
          disabled={running || !speak.enabled}
          value={speak.output_device_id ?? ""}
          onChange={(e) => patchSpeak({ output_device_id: e.target.value || null })}
        >
          <option value="">Dispositivo predeterminado (los altavoces)</option>
          {outputs.map((d) => (
            <option key={d.id} value={d.id}>
              {d.name}
              {d.is_default ? " (predeterminado)" : ""}
            </option>
          ))}
        </select>
      </label>
      {speak.enabled && !cable && (
        <p className="note">
          No se ve ningun <code>CABLE Input</code>: para que la reunion te oiga
          sin que suene por los altavoces hace falta{" "}
          <a href="https://vb-audio.com/Cable/" target="_blank" rel="noreferrer">
            VB-CABLE
          </a>
          , un dispositivo virtual que se instala aparte.
        </p>
      )}

      <label className="row">
        <input
          type="checkbox"
          checked={speak.mark_echo}
          disabled={running || !speak.enabled}
          onChange={(e) => patchSpeak({ mark_echo: e.target.checked })}
        />
        <span>Reconocer mi propia voz sintetica si vuelve por el sistema</span>
      </label>
      <p className="note">
        Si la reunion devuelve tu voz sintetica, la transcripcion la volveria a
        traducir (espanol → ingles → espanol sale raro). Con esto se detecta y
        se marca como eco en vez de re-traducirla.
      </p>

      <label className="field">
        <span>Agrupar frases</span>
        <select
          disabled={running || !speak.enabled}
          value={speak.group_max_chars}
          onChange={(e) => patchSpeak({ group_max_chars: Number(e.target.value) })}
        >
          {[150, 250, 350, 500].map((v) => (
            <option key={v} value={v}>
              {v} caracteres
            </option>
          ))}
        </select>
      </label>
      <label className="field">
        <span>Espera maxima</span>
        <select
          disabled={running || !speak.enabled}
          value={speak.group_max_wait_ms}
          onChange={(e) => patchSpeak({ group_max_wait_ms: Number(e.target.value) })}
        >
          {[1000, 2000, 3000, 5000].map((v) => (
            <option key={v} value={v}>
              {v / 1000} s
            </option>
          ))}
        </select>
      </label>
      <p className="note">
        Clonar tiene un coste fijo por peticion (~1 s medido): con frases
        sueltas la voz genera mas despacio de lo que hablas y el retraso crece
        sin parar; agrupando ~250 caracteres queda acotado. Menos espera = sale
        antes pero rinde menos.
      </p>
    </section>
  );
}

// ---------------------------------------------------- pestana de transcripcion

function TranscriptTab({
  entries,
  translations,
  partials,
  split,
  setSplit,
  splitPct,
  setSplitPct,
  translateOn,
  onCopyAll,
  onExport,
  onClear,
  onOverlay,
  onCopyLine,
}: {
  entries: Entry[];
  translations: TranslatedLine[];
  partials: Partials;
  split: Split;
  setSplit: (s: Split) => void;
  splitPct: number;
  setSplitPct: (n: number) => void;
  translateOn: boolean;
  onCopyAll: (what: "original" | "translated" | "both") => void;
  onExport: (f: ExportFormat) => void;
  onClear: () => void;
  onOverlay: () => void;
  onCopyLine: (text: string) => void;
}) {
  const isSplit = split === "split-v" || split === "split-h";
  const copyWhat =
    split === "only-translated" ? "translated" : split === "only-original" ? "original" : "both";

  return (
    <>
      <div className="pane-head">
        <div className="segmented">
          {SPLITS.map(([id, glyph, title]) => (
            <button
              key={id}
              className={split === id ? "on" : ""}
              title={title}
              disabled={!translateOn && id !== "only-original"}
              onClick={() => setSplit(id)}
            >
              {glyph}
            </button>
          ))}
        </div>
        <div className="actions">
          <button onClick={() => onCopyAll(copyWhat)}>Copiar</button>
          <button onClick={onOverlay}>Subtitulos</button>
          <button
            onClick={() => onExport(split === "only-translated" ? "translated-srt" : "srt")}
          >
            .srt
          </button>
          <button onClick={() => onExport(copyWhat === "both" ? "bilingual" : "txt")}>.txt</button>
          <button onClick={onClear}>Limpiar</button>
        </div>
      </div>

      {isSplit ? (
        <div className={`panes ${split}`}>
          <section className="pane" style={{ flexBasis: `${splitPct}%` }}>
            <h3 className="pane-title">Original</h3>
            <OriginalList entries={entries} partials={partials} onCopy={onCopyLine} />
          </section>
          <Divider vertical={split === "split-v"} onChange={setSplitPct} />
          <section className="pane">
            <h3 className="pane-title">Traduccion</h3>
            <TranslatedList lines={translations} onCopy={onCopyLine} />
          </section>
        </div>
      ) : split === "only-translated" ? (
        <TranslatedList lines={translations} onCopy={onCopyLine} />
      ) : split === "only-original" ? (
        <OriginalList entries={entries} partials={partials} onCopy={onCopyLine} />
      ) : (
        <CombinedList
          translations={translations}
          partials={partials}
          onCopy={onCopyLine}
        />
      )}
    </>
  );
}

function OriginalList({
  entries,
  partials,
  onCopy,
}: {
  entries: Entry[];
  partials: Partials;
  onCopy: (t: string) => void;
}) {
  const ref = useAutoScroll([entries, partials]);
  const empty = entries.length === 0 && !partials.system && !partials.mic;
  return (
    <div className="scroll transcript">
      {empty && <p className="muted">Nada todavia. Dale a Arrancar y pon algo a sonar.</p>}
      {entries.map((entry, i) => (
        <Paragraph
          key={i}
          time={formatClock(entry.start_ms)}
          who={entry.source}
          text={entry.text}
          onCopy={onCopy}
        />
      ))}
      {(["system", "mic"] as Source[]).map(
        (source) =>
          partials[source] && (
            <p key={source} className={`line ${source} partial`}>
              <span className="who">{source === "system" ? "sistema" : "micro"}</span>
              {partials[source]}
            </p>
          )
      )}
      <div ref={ref} />
    </div>
  );
}

function TranslatedList({
  lines,
  onCopy,
}: {
  lines: TranslatedLine[];
  onCopy: (t: string) => void;
}) {
  const ref = useAutoScroll([lines]);
  const grouped = groupByParagraph(lines);
  return (
    <div className="scroll transcript">
      {grouped.length === 0 && (
        <p className="muted">
          Todavia nada. Cada frase aparece en cuanto esta traducida.
        </p>
      )}
      {grouped.map((line, i) => (
        <Paragraph
          key={i}
          time={formatClock(line.at_ms)}
          who={line.source}
          text={line.translated}
          translated
          echo={line.echo}
          onCopy={onCopy}
        />
      ))}
      <div ref={ref} />
    </div>
  );
}

function CombinedList({
  translations,
  partials,
  onCopy,
}: {
  translations: TranslatedLine[];
  partials: Partials;
  onCopy: (t: string) => void;
}) {
  const ref = useAutoScroll([translations, partials]);
  const grouped = groupByParagraph(translations);
  return (
    <div className="scroll transcript">
      {grouped.length === 0 && (
        <p className="muted">
          Todavia nada. Cada frase aparece en cuanto esta traducida.
        </p>
      )}
      {grouped.map((line, i) => (
        <div key={i} className="pair">
          <Paragraph
            time={formatClock(line.at_ms)}
            who={line.source}
            text={line.original}
            echo={line.echo}
            onCopy={onCopy}
          />
          {/* Un eco no lleva traduccion: es la propia voz, ya en el idioma
              destino, y re-traducirla es justo lo que se evita. */}
          {!line.echo && <Paragraph text={line.translated} translated onCopy={onCopy} />}
        </div>
      ))}
      {(["system", "mic"] as Source[]).map(
        (source) =>
          partials[source] && (
            <p key={source} className={`line ${source} partial`}>
              <span className="who">{source === "system" ? "sistema" : "micro"}</span>
              {partials[source]}
            </p>
          )
      )}
      <div ref={ref} />
    </div>
  );
}

/** Baja al final cuando llega texto nuevo. */
function useAutoScroll(deps: unknown[]) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    ref.current?.scrollIntoView({ behavior: "smooth" });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  return ref;
}

function Divider({
  vertical,
  onChange,
}: {
  vertical: boolean;
  onChange: (pct: number) => void;
}) {
  return (
    <div
      className={`divider ${vertical ? "v" : "h"}`}
      onPointerDown={(e) => {
        const panes = e.currentTarget.parentElement!;
        const move = (ev: PointerEvent) => {
          const box = panes.getBoundingClientRect();
          const pct = vertical
            ? ((ev.clientX - box.left) / box.width) * 100
            : ((ev.clientY - box.top) / box.height) * 100;
          onChange(Math.min(80, Math.max(20, pct)));
        };
        const up = () => {
          window.removeEventListener("pointermove", move);
          window.removeEventListener("pointerup", up);
        };
        window.addEventListener("pointermove", move);
        window.addEventListener("pointerup", up);
      }}
    />
  );
}

function Paragraph({
  time,
  who,
  text,
  translated,
  echo,
  onCopy,
}: {
  time?: string;
  who?: Source;
  text: string;
  translated?: boolean;
  /** La propia voz sintetica volviendo por el sistema: atenuada y etiquetada. */
  echo?: boolean;
  onCopy: (text: string) => void;
}) {
  return (
    <p className={`line ${who ?? ""} ${translated ? "translated" : ""} ${echo ? "echo" : ""}`}>
      {time && <span className="time">{time}</span>}
      {echo ? (
        <span className="who" title="Tu voz sintetica captada de vuelta por el sistema">
          tu voz
        </span>
      ) : (
        who && <span className="who">{who === "system" ? "sistema" : "micro"}</span>
      )}
      {translated && !echo && <span className="arrow">→</span>}
      {text}
      <button className="copy-line" title="Copiar este parrafo" onClick={() => onCopy(text)}>
        ⧉
      </button>
    </p>
  );
}

function Meter({ level }: { level?: { rms: number; gain: number; ceiling: boolean } }) {
  if (!level) return <span className="meter" />;
  const db = 20 * Math.log10(Math.max(level.rms, 1e-6));
  const pct = Math.min(100, Math.max(0, ((db + 60) / 60) * 100));
  return (
    <span
      className="meter"
      title={`rms ${level.rms.toFixed(5)} · ganancia x${level.gain.toFixed(1)}`}
    >
      <span className={`meter-fill ${level.ceiling ? "hot" : ""}`} style={{ width: `${pct}%` }} />
    </span>
  );
}
