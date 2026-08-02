import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from "react";
import { emit, listen } from "@tauri-apps/api/event";

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
  availableSplits,
  copyText,
  formatClock,
  groupByParagraph,
} from "./tauri";
import { Lang, LANG_NAMES, STRINGS, Strings, initialLang } from "./i18n";

type Partials = Partial<Record<Source, string>>;
type Levels = Partial<Record<Source, { rms: number; gain: number; ceiling: boolean }>>;

/** Las cadenas viajan por contexto: los componentes anidan seis niveles y
 *  pasarlas por props seria ruido en cada firma. */
const I18n = createContext<Strings>(STRINGS.en);
const useT = () => useContext(I18n);

/** Nombre visible de un locale, en el idioma de la interfaz. */
function langName(t: Strings, code: string): string {
  if (code === "auto") return t.langAuto;
  return t.langNames[code] ?? code;
}

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
  const [profiles, setProfiles] = useState<string[]>([]);
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
  const [lang, setLang] = useState<Lang>(initialLang);
  const t = STRINGS[lang];
  // Los listeners de eventos se registran UNA vez, asi que capturarian el
  // idioma del primer render y se quedarian en el para siempre. Con una
  // referencia leen el actual en cada evento.
  const tRef = useRef(t);
  tRef.current = t;

  useEffect(() => localStorage.setItem("tab", tab), [tab]);
  useEffect(() => localStorage.setItem("split", split), [split]);
  useEffect(() => localStorage.setItem("splitPct", String(splitPct)), [splitPct]);
  // Los subtitulos viven en otra ventana, con su propio webview: no comparte
  // este estado y no puede fiarse de `storage`, que no cruza ventanas de
  // forma garantizada. Se le avisa por evento.
  useEffect(() => {
    localStorage.setItem("uiLang", lang);
    emit("ui-lang", lang);
    api.setUiLanguage(lang).catch(() => {});
  }, [lang]);

  // Los modos disponibles dependen de la configuracion, y esta cambia al
  // aplicar un perfil: si el modo elegido deja de tener sentido (vista de
  // reunion en un perfil sin traduccion) hay que enseñar otro, o el usuario
  // se queda mirando cajas vacias sin saber por que.
  //
  // El modo efectivo se CALCULA, no se guarda: `split` conserva siempre lo
  // que tu elegiste. Si se corrigiera el estado, la correccion se
  // persistiria encima de tu preferencia y al volver a activar la traduccion
  // ya no habria nada que restaurar — la habrias perdido para siempre.
  const splitOptions = config ? availableSplits(config) : [];
  const effectiveSplit =
    splitOptions.length === 0 || splitOptions.includes(split) ? split : splitOptions[0];

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
        setProfiles(await api.listProfiles());
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
              tRef.current.engineReady(
                payload.device,
                payload.latency_ms,
                langName(tRef.current, payload.language)
              )
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
            setStatus(tRef.current.sessionStopped);
            break;
        }
      }),
      listen<Entry>("transcript-entry", ({ payload }) => setEntries((e) => [...e, payload])),
      listen<TranslatedLine>("translation", ({ payload }) =>
        setTranslations((t) => [...t, payload])
      ),
      listen<{ device: string; target: string }>("translator-ready", ({ payload }) =>
        setStatus(
          tRef.current.translatorReady(payload.device, langName(tRef.current, payload.target))
        )
      ),
      // Los modelos tardan casi un minuto en cargar y la ventana se quedaba
      // muda todo ese rato, que se siente como que la app se ha colgado. El
      // texto se elige por la ETAPA y no se usa el `message` del backend:
      // Rust no sabe en que idioma esta la ventana.
      listen<{ stage: string; message: string }>("loading", ({ payload }) => {
        const s = tRef.current;
        const byStage: Record<string, string> = {
          start: s.loadingStart,
          voice: s.loadingVoice,
          translator: s.loadingTranslator,
        };
        setStatus(byStage[payload.stage] ?? payload.message);
      }),
      listen<SpeechEvent>("speech-event", ({ payload }) => {
        switch (payload.kind) {
          case "ready":
            setStatus(tRef.current.voiceReady(payload.device));
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
        setStatus(t.starting);
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
        flash(t.nothingToCopy);
        return;
      }
      await copyText(text);
      flash(t.copied);
    } catch (e: any) {
      setError(e.message);
    }
  }

  async function doExport(format: ExportFormat) {
    try {
      const saved = await api.exportTranscript(format);
      flash(t.savedTo(saved));
    } catch (e: any) {
      setError(e.message);
    }
  }

  if (!config) {
    return (
      <main className="app">
        <p className="muted">{t.appLoading}</p>
      </main>
    );
  }

  const lowVolume = Object.values(levels).some((l) => l?.ceiling);
  const autoBlocksTranslation = config.translate && config.language === "auto";
  const speakMisconfigured =
    config.speak.enabled && (!config.translate || !config.capture_mic);
  // Voz por los altavoces con el microfono abierto: el micro recoge la propia
  // voz sintetica, se re-traduce y se vuelve a hablar. No se puede evitar por
  // software (callar mientras suena se come el habla real: "hace eco" y
  // "sigo hablando" pasan a la vez), asi que se avisa y se apunta a la
  // solucion de verdad, que es fisica.
  const speakFeedbackRisk =
    config.speak.enabled &&
    config.capture_mic &&
    config.speak.output_device_id === null;

  // Capturar del mismo cable virtual por el que habla la voz es un lazo
  // cerrado: la app solo se oye a si misma, y como nadie mete audio en el
  // cable hasta que la voz hable --y la voz no habla hasta transcribir
  // algo-- se queda en silencio para siempre. Es facil de hacer sin querer,
  // porque los dos extremos del cable se llaman casi igual.
  const nameOf = (list: AudioDevice[], id: string | null) =>
    (id && list.find((d) => d.id === id)?.name) || "";
  // Un extremo de cable virtual se reconoce por "CABLE Input/Output" seguido
  // del fabricante entre parentesis. Se compara el fabricante para exigir que
  // sean los dos extremos del MISMO cable: buscar solo "cable" en el nombre
  // daba falsos positivos con cualquier auricular que lleve esa palabra.
  const cableEnd = (name: string) => {
    const m = /^\s*cable\s+(input|output)\b\s*(.*)$/i.exec(name);
    return m ? { dir: m[1].toLowerCase(), family: m[2].trim().toLowerCase() } : null;
  };
  const voiceEnd = cableEnd(nameOf(outputs, config.speak.output_device_id));
  const sameCableAsVoice = (name: string) => {
    const end = cableEnd(name);
    return (
      !!voiceEnd &&
      voiceEnd.dir === "input" &&
      !!end &&
      end.dir === "output" &&
      end.family === voiceEnd.family
    );
  };
  const cableLoop =
    config.speak.enabled &&
    ((config.capture_mic && sameCableAsVoice(nameOf(inputs, config.mic_device_id))) ||
      (config.capture_system && sameCableAsVoice(nameOf(outputs, config.system_device_id))));
  // Con mas de ~10 s de retraso la conversacion deja de ser conversacion.
  const speechBehind = speech !== null && speech.queuedMs > 10_000;

  return (
    <I18n.Provider value={t}>
    <main className="app">
      <header className="bar">
        <h1>LiveTranscriber</h1>
        <nav className="tabs">
          <button className={tab === "config" ? "on" : ""} onClick={() => setTab("config")}>
            {t.tabConfig}
          </button>
          <button
            className={tab === "transcript" ? "on" : ""}
            onClick={() => setTab("transcript")}
          >
            {t.tabTranscript}
          </button>
        </nav>
        {/* El idioma de la interfaz va aqui y no en la configuracion: si
            estuviera alli entraria en los perfiles, y cambiar de perfil te
            cambiaria el idioma de la aplicacion. Ademas se puede cambiar en
            marcha, al reves que casi todo lo de configuracion. */}
        <div className="segmented lang-switch" title={t.uiLanguage}>
          {(Object.keys(LANG_NAMES) as Lang[]).map((code) => (
            <button
              key={code}
              className={lang === code ? "on" : ""}
              title={LANG_NAMES[code]}
              onClick={() => setLang(code)}
            >
              {code.toUpperCase()}
            </button>
          ))}
        </div>
        <button className={running ? "stop" : "start"} onClick={toggleRun}>
          {running ? t.stop : t.start}
        </button>
      </header>

      {error && (
        <p className="error" onClick={() => setError("")}>
          {error}
        </p>
      )}
      {status && !error && <p className="status">{status}</p>}
      {autoBlocksTranslation && <p className="warn">{t.warnAutoBlocksTranslation}</p>}
      {lowVolume && <p className="warn">{t.warnLowVolume}</p>}
      {speakMisconfigured && <p className="warn">{t.warnSpeakMisconfigured}</p>}
      {speakFeedbackRisk && <p className="warn">{t.warnSpeakFeedback}</p>}
      {cableLoop && <p className="warn">{t.warnCableLoop}</p>}
      {speech !== null && running && (
        <p className={speechBehind ? "warn" : "status"}>
          {t.speechQueue((speech.queuedMs / 1000).toFixed(1), speech.pending)}
          {speechBehind && t.speechBehind}
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
          profiles={profiles}
          onSaveProfile={async (name) => {
            try {
              setProfiles(await api.saveProfile(name));
              flash(t.profileSaved(name));
            } catch (e: any) {
              setError(e.message);
            }
          }}
          onLoadProfile={async (name) => {
            try {
              const applied = await api.loadProfile(name);
              setConfig(applied.config);
              setOutputDir(await api.outputDir());
              // El perfil puede traer dispositivos que no estaban en la
              // lista que se cargo al abrir la app. Sin releerla, el
              // selector mostraria el nombre equivocado y los avisos que
              // miran nombres (el del lazo de cable) no se dispararian.
              setOutputs(await api.listDevices("output"));
              setInputs(await api.listDevices("input"));
              if (applied.fallbacks.length > 0) {
                // Cambiar de dispositivo en silencio es como acabas grabando
                // de donde no querias: se dice cual y por que.
                const names: Record<string, string> = {
                  sistema: t.deviceSystem,
                  microfono: t.deviceMic,
                  voz: t.deviceVoice,
                };
                const which = applied.fallbacks
                  .map((f) => names[f.what] ?? f.what)
                  .join(", ");
                setError(t.profileFallbacks(name, which));
              } else {
                flash(t.profileApplied(name));
              }
            } catch (e: any) {
              setError(e.message);
            }
          }}
          onDeleteProfile={async (name) => {
            try {
              setProfiles(await api.deleteProfile(name));
              flash(t.profileDeleted(name));
            } catch (e: any) {
              setError(e.message);
            }
          }}
          onRefreshDevices={async () => {
            try {
              setOutputs(await api.listDevices("output"));
              setInputs(await api.listDevices("input"));
              flash(t.devicesRefreshed);
            } catch (e: any) {
              setError(e.message);
            }
          }}
          onPickDir={async () => {
            try {
              const picked = await api.pickOutputDir();
              if (picked) {
                setOutputDir(picked);
                setConfig({ ...config, output_dir: picked });
                flash(t.folderSaved);
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
          split={effectiveSplit}
          setSplit={setSplit}
          splitOptions={splitOptions}
          splitPct={splitPct}
          setSplitPct={setSplitPct}
          voiceOn={config.speak.enabled}
          salaName={langName(t, config.language)}
          salaTargetName={langName(t, config.target_language)}
          micName={langName(t, config.mic_language || config.target_language)}
          micTargetName={langName(t, config.mic_target_language || config.language)}
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
            flash(t.copied);
          }}
        />
      )}

      {toast && <p className="toast">{toast}</p>}
      <footer className="hints">
        {t.hints(config.hotkey_toggle, config.hotkey_overlay)}
      </footer>
    </main>
    </I18n.Provider>
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
  profiles,
  onSaveProfile,
  onLoadProfile,
  onDeleteProfile,
  onRefreshDevices,
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
  profiles: string[];
  onSaveProfile: (name: string) => void;
  onLoadProfile: (name: string) => void;
  onDeleteProfile: (name: string) => void;
  onRefreshDevices: () => void;
  onPickDir: () => void;
  onRevealDir: () => void;
}) {
  const t = useT();
  return (
    <div className="scroll">
      <ProfilesPane
        profiles={profiles}
        running={running}
        onSave={onSaveProfile}
        onLoad={onLoadProfile}
        onDelete={onDeleteProfile}
      />
      <section className="panel">
        <h2>{t.outputFolder}</h2>
        <div className="path-row">
          <code className="path" title={outputDir}>
            {outputDir || "…"}
          </code>
          <button onClick={onPickDir}>{t.change}</button>
          <button onClick={onRevealDir}>{t.open}</button>
        </div>

        <label className="field">
          <span>{t.fileName}</span>
          <input
            type="text"
            className="text-input"
            value={config.output_name}
            placeholder={t.fileNamePlaceholder}
            onChange={(e) => patch({ output_name: e.target.value })}
          />
        </label>
        <p className="note">{t.outputNote1(filenamePreview || "…")}</p>
        <p className="note">{t.outputNote2}</p>
      </section>

      <section className="panel">
        <div className="pane-head">
          <h2>{t.sources}</h2>
          <button title={t.refreshTitle} onClick={onRefreshDevices}>
            {t.refresh}
          </button>
        </div>
        <label className="row">
          <input
            type="checkbox"
            checked={config.capture_system}
            disabled={running}
            onChange={(e) => patch({ capture_system: e.target.checked })}
          />
          <span>{t.systemAudio}</span>
          <Meter level={levels.system} />
        </label>
        <select
          disabled={running || !config.capture_system}
          value={config.system_device_id ?? ""}
          onChange={(e) => patch({ system_device_id: e.target.value || null })}
        >
          <option value="">{t.defaultDevice}</option>
          {outputs.map((d) => (
            <option key={d.id} value={d.id}>
              {d.name}
              {d.is_default ? t.defaultSuffix : ""}
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
          <span>{t.myMic}</span>
          <Meter level={levels.mic} />
        </label>
        <select
          disabled={running || !config.capture_mic}
          value={config.mic_device_id ?? ""}
          onChange={(e) => patch({ mic_device_id: e.target.value || null })}
        >
          <option value="">{t.defaultDevice}</option>
          {inputs.map((d) => (
            <option key={d.id} value={d.id}>
              {d.name}
              {d.is_default ? t.defaultSuffix : ""}
            </option>
          ))}
        </select>
      </section>

      <section className="panel">
        <h2>{t.transcription}</h2>
        <label className="field">
          <span>{t.latency}</span>
          <select
            disabled={running}
            value={config.lookahead}
            onChange={(e) => patch({ lookahead: Number(e.target.value) })}
          >
            {LOOKAHEADS.map((value) => (
              <option key={value} value={value}>
                {value === 0
                  ? t.lookahead0
                  : value === 3
                  ? t.lookahead3
                  : value === 6
                  ? t.lookahead6
                  : t.lookahead13}
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
          <span>{t.normalizeGain}</span>
        </label>
      </section>

      <section className="panel">
        <h2>{t.paragraphs}</h2>
        <label className="field">
          <span>{t.speechPause}</span>
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
          <span>{t.maxParagraph}</span>
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
        <p className="note">{t.paragraphsNote}</p>
      </section>

      <section className="panel">
        <h2>{t.languages}</h2>
        <div className="lang-block">
          <h3 className="pane-title">{t.roomBlock}</h3>
          <div className="lang-pair">
            <select
              disabled={running}
              value={config.language}
              onChange={(e) => patch({ language: e.target.value })}
            >
              {LANGUAGES.map((code) => (
                <option key={code} value={code}>
                  {langName(t, code)}
                </option>
              ))}
            </select>
            <span className="arrow">→</span>
            <select
              disabled={running || !config.translate}
              value={config.target_language}
              onChange={(e) => patch({ target_language: e.target.value })}
            >
              {LANGUAGES.filter((code) => code !== "auto").map((code) => (
                <option key={code} value={code}>
                  {langName(t, code)}
                </option>
              ))}
            </select>
          </div>
          <p className="note">{t.roomNote}</p>
        </div>

        <div className="lang-block">
          <h3 className="pane-title">{t.micBlock}</h3>
          <div className="lang-pair">
            <select
              disabled={running || !config.translate}
              value={config.mic_language || config.target_language}
              onChange={(e) => patch({ mic_language: e.target.value })}
            >
              {LANGUAGES.filter((code) => code !== "auto").map((code) => (
                <option key={code} value={code}>
                  {langName(t, code)}
                </option>
              ))}
            </select>
            <span className="arrow">→</span>
            <select
              disabled={running || !config.translate}
              value={
                config.mic_target_language ||
                (config.language === "auto" ? "" : config.language)
              }
              onChange={(e) => patch({ mic_target_language: e.target.value })}
            >
              {config.language === "auto" && !config.mic_target_language && (
                <option value="">{t.pickRoomFirst}</option>
              )}
              {LANGUAGES.filter((code) => code !== "auto").map((code) => (
                <option key={code} value={code}>
                  {langName(t, code)}
                </option>
              ))}
            </select>
          </div>
          <p className="note">{t.micNote}</p>
        </div>

        <label className="row">
          <input
            type="checkbox"
            checked={config.translate}
            disabled={running}
            onChange={(e) =>
              // Sin traduccion la voz no tiene nada que pronunciar: se apaga
              // a la vez, y su seccion desaparece. Si no, quedaria activada
              // pero invisible, y el arranque fallaria con un error que no
              // se puede arreglar desde esta pantalla.
              patch(
                e.target.checked
                  ? { translate: true }
                  : { translate: false, speak: { ...config.speak, enabled: false } }
              )
            }
          />
          <span>{t.translateParallel}</span>
        </label>
        <p className="note">{t.translateNote}</p>
      </section>

      {config.translate && (
        <SpeakPane
          config={config}
          outputs={outputs}
          running={running}
          patch={patch}
          onRefreshDevices={onRefreshDevices}
        />
      )}
    </div>
  );
}

/// Perfiles con nombre: guardan la configuracion entera de un uso concreto
/// (una reunion en ingles hablando con tu voz, transcribir una charla, ...)
/// para no rehacerla a mano cada vez.
///
/// Va la primera de la pestana a proposito: lo habitual es elegir perfil y no
/// tocar nada mas.
function ProfilesPane({
  profiles,
  running,
  onSave,
  onLoad,
  onDelete,
}: {
  profiles: string[];
  running: boolean;
  onSave: (name: string) => void;
  onLoad: (name: string) => void;
  onDelete: (name: string) => void;
}) {
  const t = useT();
  const [selected, setSelected] = useState("");
  const [newName, setNewName] = useState("");
  const [confirmDelete, setConfirmDelete] = useState(false);

  // Si el perfil elegido desaparece (lo has borrado), no dejar el selector
  // apuntando a un fantasma.
  useEffect(() => {
    if (selected && !profiles.includes(selected)) setSelected("");
  }, [profiles, selected]);

  // Cambiar de perfil cancela un borrado a medias: si no, el "¿Seguro?"
  // seguiria armado apuntando ya a otro.
  useEffect(() => setConfirmDelete(false), [selected]);

  return (
    <section className="panel">
      <h2>{t.profiles}</h2>
      <div className="path-row">
        <select
          disabled={running || profiles.length === 0}
          value={selected}
          onChange={(e) => setSelected(e.target.value)}
        >
          <option value="">
            {profiles.length === 0 ? t.profilesEmpty : t.profilesPick}
          </option>
          {profiles.map((name) => (
            <option key={name} value={name}>
              {name}
            </option>
          ))}
        </select>
        <button disabled={running || !selected} onClick={() => onLoad(selected)}>
          {t.profileApply}
        </button>
        {/* Pide confirmacion: esta pegado a "Aplicar", actua sobre la misma
            seleccion, y un perfil borrado no se recupera. */}
        <button
          disabled={running || !selected}
          className={confirmDelete ? "danger" : ""}
          title={
            confirmDelete ? t.profileDeleteConfirmTitle : t.profileDeleteTitle
          }
          onClick={() => {
            if (confirmDelete) {
              onDelete(selected);
              setConfirmDelete(false);
            } else {
              setConfirmDelete(true);
              window.setTimeout(() => setConfirmDelete(false), 4000);
            }
          }}
        >
          {confirmDelete ? t.profileDeleteSure : t.profileDelete}
        </button>
      </div>

      <div className="path-row">
        <input
          type="text"
          className="text-input"
          disabled={running}
          value={newName}
          placeholder={t.profileNamePlaceholder}
          onChange={(e) => setNewName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && newName.trim()) {
              onSave(newName.trim());
              setNewName("");
            }
          }}
        />
        <button
          disabled={running || !newName.trim()}
          onClick={() => {
            onSave(newName.trim());
            setNewName("");
          }}
        >
          {t.profileSave}
        </button>
      </div>
      <p className="note">{t.profilesNote1}</p>
      <p className="note">{t.profilesNote2}</p>
    </section>
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
  onRefreshDevices,
}: {
  config: AppConfig;
  outputs: AudioDevice[];
  running: boolean;
  patch: (c: Partial<AppConfig>) => void;
  onRefreshDevices: () => void;
}) {
  const t = useT();
  const speak = config.speak;
  const patchSpeak = (changes: Partial<SpeakConfig>) =>
    patch({ speak: { ...speak, ...changes } });

  const cable = outputs.find((d) => d.name.toLowerCase().includes("cable input"));

  return (
    <section className="panel">
      <h2>{t.speakTitle}</h2>
      <label className="row">
        <input
          type="checkbox"
          checked={speak.enabled}
          disabled={running}
          onChange={(e) => patchSpeak({ enabled: e.target.checked })}
        />
        <span>{t.speakEnable}</span>
      </label>
      <p className="note">{t.speakNote}</p>
      <p className="note">
        <strong>{t.speakSetupTitle}</strong> {t.speakSetupIntro}
      </p>
      <ul className="note">
        <li>{t.speakSetup1}</li>
        <li>{t.speakSetup2}</li>
        <li>{t.speakSetup3}</li>
        <li>{t.speakSetup4}</li>
      </ul>

      <label className="field">
        <span>{t.engine}</span>
        <select
          disabled={running || !speak.enabled}
          value={speak.engine}
          onChange={(e) =>
            patchSpeak({ engine: e.target.value as SpeakConfig["engine"] })
          }
        >
          <option value="chatterbox">{t.engineChatterbox}</option>
          <option value="kokoro">{t.engineKokoro}</option>
        </select>
      </label>

      {speak.engine === "chatterbox" ? (
        <>
          <label className="field">
            <span>{t.voiceSample}</span>
            <input
              type="text"
              className="text-input"
              disabled={running || !speak.enabled}
              value={speak.voice_wav ?? ""}
              placeholder={t.voiceWavPlaceholder}
              onChange={(e) => patchSpeak({ voice_wav: e.target.value || null })}
            />
          </label>
          <p className="note">{t.voiceSampleNote}</p>
        </>
      ) : (
        <>
          <label className="field">
            <span>{t.presetVoice}</span>
            <input
              type="text"
              className="text-input"
              disabled={running || !speak.enabled}
              value={speak.kokoro_voice}
              placeholder="af_heart"
              onChange={(e) => patchSpeak({ kokoro_voice: e.target.value })}
            />
          </label>
          <p className="note">{t.presetVoiceNote}</p>
        </>
      )}

      <label className="field">
        <span>{t.speakThrough}</span>
        <select
          disabled={running || !speak.enabled}
          value={speak.output_device_id ?? ""}
          onChange={(e) => patchSpeak({ output_device_id: e.target.value || null })}
        >
          <option value="">{t.defaultDeviceSpeakers}</option>
          {outputs.map((d) => (
            <option key={d.id} value={d.id}>
              {d.name}
              {d.is_default ? t.defaultSuffix : ""}
            </option>
          ))}
        </select>
      </label>
      {speak.enabled && !cable && (
        <p className="note">
          {t.noCableNote}{" "}
          <a href="https://vb-audio.com/Cable/" target="_blank" rel="noreferrer">
            vb-audio.com/Cable
          </a>
          .{" "}
          {/* Es aqui donde se descubre que falta, asi que el refresco tiene
              que estar a mano: instalarlo con la app abierta es lo normal. */}
          <button className="link-button" onClick={onRefreshDevices}>
            {t.noCableRefresh}
          </button>
        </p>
      )}

      <label className="row">
        <input
          type="checkbox"
          checked={speak.mark_echo}
          disabled={running || !speak.enabled}
          onChange={(e) => patchSpeak({ mark_echo: e.target.checked })}
        />
        <span>{t.markEcho}</span>
      </label>
      <p className="note">{t.markEchoNote}</p>

      <label className="field">
        <span>{t.groupSentences}</span>
        <select
          disabled={running || !speak.enabled}
          value={speak.group_max_chars}
          onChange={(e) => patchSpeak({ group_max_chars: Number(e.target.value) })}
        >
          {[150, 250, 350, 500].map((v) => (
            <option key={v} value={v}>
              {t.chars(v)}
            </option>
          ))}
        </select>
      </label>
      <label className="field">
        <span>{t.maxWait}</span>
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
      <p className="note">{t.groupNote}</p>
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
  splitOptions,
  splitPct,
  setSplitPct,
  voiceOn,
  salaName,
  salaTargetName,
  micName,
  micTargetName,
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
  /** Modos que tienen sentido con la configuracion actual. */
  splitOptions: Split[];
  splitPct: number;
  setSplitPct: (n: number) => void;
  voiceOn: boolean;
  salaName: string;
  salaTargetName: string;
  micName: string;
  micTargetName: string;
  onCopyAll: (what: "original" | "translated" | "both") => void;
  onExport: (f: ExportFormat) => void;
  onClear: () => void;
  onOverlay: () => void;
  onCopyLine: (text: string) => void;
}) {
  const t = useT();
  const splitTitles: Record<Split, string> = {
    combined: t.splitCombined,
    "split-v": t.splitV,
    "split-h": t.splitH,
    "only-original": t.splitOriginal,
    "only-translated": t.splitTranslated,
    meeting: t.splitMeeting,
  };
  const isSplit = split === "split-v" || split === "split-h";
  const copyWhat =
    split === "only-translated" ? "translated" : split === "only-original" ? "original" : "both";

  return (
    <>
      <div className="pane-head">
        {/* Con un solo modo posible (sin traduccion solo cabe el original) el
            selector no elige nada: ocupa sitio y sugiere opciones que no hay. */}
        {splitOptions.length > 1 ? (
          <div className="segmented">
            {SPLITS.filter(([id]) => splitOptions.includes(id)).map(([id, glyph]) => (
              <button
                key={id}
                className={split === id ? "on" : ""}
                title={splitTitles[id]}
                onClick={() => setSplit(id)}
              >
                {glyph}
              </button>
            ))}
          </div>
        ) : (
          <span />
        )}
        <div className="actions">
          <button onClick={() => onCopyAll(copyWhat)}>{t.copy}</button>
          <button onClick={onOverlay}>{t.subtitles}</button>
          <button
            onClick={() => onExport(split === "only-translated" ? "translated-srt" : "srt")}
          >
            .srt
          </button>
          <button onClick={() => onExport(copyWhat === "both" ? "bilingual" : "txt")}>
            {copyWhat === "both" ? t.txtBilingual : ".txt"}
          </button>
          <button onClick={onClear}>{t.clear}</button>
        </div>
      </div>

      {split === "meeting" ? (
        <MeetingView
          translations={translations}
          partials={partials}
          voiceOn={voiceOn}
          salaName={salaName}
          salaTargetName={salaTargetName}
          micName={micName}
          micTargetName={micTargetName}
          onCopy={onCopyLine}
        />
      ) : isSplit ? (
        <div className={`panes ${split}`}>
          <section className="pane" style={{ flexBasis: `${splitPct}%` }}>
            <h3 className="pane-title">{t.original}</h3>
            <OriginalList entries={entries} partials={partials} onCopy={onCopyLine} />
          </section>
          <Divider vertical={split === "split-v"} onChange={setSplitPct} />
          <section className="pane">
            <h3 className="pane-title">{t.translation}</h3>
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

/// La vista de reunion bilingue: cuatro cajas. Los demas arriba (2/3) y yo
/// abajo (1/3), con cada IDIOMA en su columna — a la izquierda el de la
/// reunion, a la derecha el mio. Las direcciones quedan invertidas entre
/// filas a proposito, porque asi fluye la traduccion: lo de arriba va de
/// izquierda a derecha (les leo) y lo de abajo de derecha a izquierda (mi
/// voz les habla).
// La vista solo se ofrece con las DOS fuentes activas (ver availableSplits),
// asi que las cuatro cajas siempre tienen sentido. Filtrarlas por la
// configuracion viva ademas escondería lo que una fuente ya habia grabado
// antes de desactivarla.
function MeetingView({
  translations,
  partials,
  voiceOn,
  salaName,
  salaTargetName,
  micName,
  micTargetName,
  onCopy,
}: {
  translations: TranslatedLine[];
  partials: Partials;
  /** Sin voz activada, la caja de abajo a la izquierda no es "lo que tu voz
   *  les dice" sino solo la traduccion de lo que dices. */
  voiceOn: boolean;
  salaName: string;
  salaTargetName: string;
  micName: string;
  micTargetName: string;
  onCopy: (t: string) => void;
}) {
  const t = useT();
  const others = groupByParagraph(translations.filter((l) => l.source === "system"));
  const mine = groupByParagraph(translations.filter((l) => l.source === "mic"));

  const refOthersOriginal = useAutoScroll([translations, partials.system]);
  const refOthersTranslated = useAutoScroll([translations]);
  const refMineTranslated = useAutoScroll([translations]);
  const refMineOriginal = useAutoScroll([translations, partials.mic]);

  return (
    <div className="meeting-grid">
      <section className="pane">
        <h3 className="pane-title">
          {t.othersLabel} — {salaName}
        </h3>
        <div className="scroll transcript">
          {others.length === 0 && !partials.system && (
            <p className="muted">{t.emptyOthers}</p>
          )}
          {others.map((line, i) => (
            <Paragraph
              key={i}
              time={formatClock(line.at_ms)}
              text={line.original}
              echo={line.echo}
              onCopy={onCopy}
            />
          ))}
          {partials.system && <p className="line system partial">{partials.system}</p>}
          <div ref={refOthersOriginal} />
        </div>
      </section>

      <section className="pane">
        <h3 className="pane-title">
          {t.othersLabel} — {salaTargetName}
        </h3>
        <div className="scroll transcript">
          {/* Los ecos no llevan traduccion: son mi propia voz, ya en el
              idioma de la reunion, y quedan marcados en la caja original. */}
          {others
            .filter((line) => !line.echo)
            .map((line, i) => (
              <Paragraph
                key={i}
                time={formatClock(line.at_ms)}
                text={line.translated}
                translated
                onCopy={onCopy}
              />
            ))}
          <div ref={refOthersTranslated} />
        </div>
      </section>

      <section className="pane">
        <h3 className="pane-title">
          {voiceOn ? t.myVoiceSays : t.mineTranslated} — {micTargetName}
        </h3>
        <div className="scroll transcript">
          {/* Los ecos no se pronuncian, asi que aqui tampoco se muestran:
              esta caja es "lo que la voz va a decir", y mentiria. */}
          {mine
            .filter((line) => !line.echo)
            .map((line, i) => (
              <Paragraph
                key={i}
                time={formatClock(line.at_ms)}
                text={line.translated}
                translated
                onCopy={onCopy}
              />
            ))}
          <div ref={refMineTranslated} />
        </div>
      </section>

      <section className="pane">
        <h3 className="pane-title">
          {t.meLabel} — {micName}
        </h3>
        <div className="scroll transcript">
          {mine.length === 0 && !partials.mic && (
            <p className="muted">{t.emptyMine}</p>
          )}
          {/* echo marca la propia voz sintetica captada por el micro: va en
              el idioma de la sala, no en el mio, y sin la marca pareceria
              que lo dije yo. */}
          {mine.map((line, i) => (
            <Paragraph
              key={i}
              time={formatClock(line.at_ms)}
              text={line.original}
              echo={line.echo}
              onCopy={onCopy}
            />
          ))}
          {partials.mic && <p className="line mic partial">{partials.mic}</p>}
          <div ref={refMineOriginal} />
        </div>
      </section>
    </div>
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
  const t = useT();
  const ref = useAutoScroll([entries, partials]);
  const empty = entries.length === 0 && !partials.system && !partials.mic;
  return (
    <div className="scroll transcript">
      {empty && <p className="muted">{t.emptyOriginal}</p>}
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
              <span className="who">
                {source === "system" ? t.whoSystem : t.whoMic}
              </span>
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
  const t = useT();
  const ref = useAutoScroll([lines]);
  const grouped = groupByParagraph(lines);
  return (
    <div className="scroll transcript">
      {grouped.length === 0 && <p className="muted">{t.emptyTranslated}</p>}
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
  const t = useT();
  const ref = useAutoScroll([translations, partials]);
  const grouped = groupByParagraph(translations);
  return (
    <div className="scroll transcript">
      {grouped.length === 0 && <p className="muted">{t.emptyTranslated}</p>}
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
              <span className="who">
                {source === "system" ? t.whoSystem : t.whoMic}
              </span>
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
  const t = useT();
  return (
    <p className={`line ${who ?? ""} ${translated ? "translated" : ""} ${echo ? "echo" : ""}`}>
      {time && <span className="time">{time}</span>}
      {echo ? (
        <span className="who" title={t.echoTitle}>
          {t.whoEcho}
        </span>
      ) : (
        who && (
          <span className="who">{who === "system" ? t.whoSystem : t.whoMic}</span>
        )
      )}
      {translated && !echo && <span className="arrow">→</span>}
      {text}
      <button className="copy-line" title={t.copyParagraph} onClick={() => onCopy(text)}>
        ⧉
      </button>
    </p>
  );
}

function Meter({ level }: { level?: { rms: number; gain: number; ceiling: boolean } }) {
  const t = useT();
  if (!level) return <span className="meter" />;
  const db = 20 * Math.log10(Math.max(level.rms, 1e-6));
  const pct = Math.min(100, Math.max(0, ((db + 60) / 60) * 100));
  return (
    <span className="meter" title={t.meterTitle(level.rms, level.gain)}>
      <span className={`meter-fill ${level.ceiling ? "hot" : ""}`} style={{ width: `${pct}%` }} />
    </span>
  );
}
