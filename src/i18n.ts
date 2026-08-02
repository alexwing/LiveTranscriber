/** Idioma de la interfaz.
 *
 *  Es una preferencia de INTERFAZ, no de configuracion: vive en el navegador
 *  junto a la pestana activa y el modo de vista, y no entra en el TOML. Si
 *  estuviera en `AppConfig` acabaria dentro de los perfiles, y cambiar de
 *  perfil te cambiaria el idioma de la aplicacion — que es exactamente la
 *  confusion que hay que evitar.
 *
 *  El catalogo espanol es la fuente de verdad y el ingles se declara con su
 *  mismo tipo: si al ingles le falta una clave, TypeScript no compila. Sin
 *  eso, una cadena olvidada solo se descubre viendola en pantalla.
 */

export type Lang = "es" | "en";

export const LANG_NAMES: Record<Lang, string> = {
  es: "Español",
  en: "English",
};

const es = {
  // ---------------------------------------------------------------- general
  appLoading: "Cargando…",
  tabConfig: "Configuracion",
  tabTranscript: "Transcripcion",
  start: "Arrancar",
  stop: "Parar",
  starting: "Arrancando… cargar los modelos lleva hasta un minuto",
  sessionStopped: "Sesion detenida",
  uiLanguage: "Idioma de la interfaz",

  engineReady: (device: string, ms: number, lang: string) =>
    `Motor listo en ${device}, latencia ${ms} ms, idioma ${lang}`,
  translatorReady: (device: string, target: string) =>
    `Traductor listo en ${device}, destino ${target}`,
  voiceReady: (device: string) => `Voz sintetica lista en ${device}`,
  // Los eventos de carga traen su etapa, asi que el texto se elige aqui y no
  // viene del backend: el backend no sabe en que idioma esta la ventana.
  loadingStart: "Cargando modelos…",
  loadingVoice: "Voz lista",
  loadingTranslator: "Traductor listo",

  nothingToCopy: "No hay nada que copiar",
  copied: "Copiado",
  savedTo: (path: string) => `Guardado: ${path}`,
  devicesRefreshed: "Dispositivos actualizados",
  folderSaved: "Carpeta guardada",
  hints: (toggle: string, overlay: string) =>
    `${toggle} arranca y para · ${overlay} muestra los subtitulos`,

  // ----------------------------------------------------------------- avisos
  warnAutoBlocksTranslation:
    "La traduccion esta activada pero el idioma es «Detectar automaticamente». El traductor necesita saber desde que idioma parte, asi que elige uno concreto.",
  warnLowVolume:
    "El volumen de Windows esta muy bajo. El bucle de retorno captura DESPUES del control de volumen, asi que la transcripcion saldra pobre aunque la ganancia ya este al maximo.",
  warnSpeakMisconfigured:
    "Hablar con tu voz necesita «Traducir en paralelo» y «Mi microfono» activados: lo que se habla es la traduccion de lo que dices por el micro.",
  warnSpeakFeedback:
    "Tu voz sintetica va a sonar por los ALTAVOCES con el microfono abierto: el micro puede recogerla y volver a traducirla. Ponte auriculares, o elige «CABLE Input» en «Hablar por» para que solo la oiga la reunion.",
  warnCableLoop:
    "Estas capturando del mismo cable virtual por el que habla la voz: la app solo se oiria a si misma y no transcribiria nada. El cable va en un solo sentido, CABLE Input → CABLE Output: deja «CABLE Input» en «Hablar por», pon tu microfono REAL en Fuentes, y elige «CABLE Output» como microfono DENTRO de Teams, no aqui.",
  speechQueue: (secs: string, pending: number) =>
    `Voz sintetica: ${secs} s en cola${pending > 0 ? ` · ${pending} frases esperando` : ""}`,
  speechBehind:
    " — se esta generando mas despacio de lo que hablas; haz una pausa o agrupa mas caracteres",

  // --------------------------------------------------------------- perfiles
  profiles: "Perfiles",
  profilesEmpty: "Todavia no hay perfiles",
  profilesPick: "Elige un perfil…",
  profileApply: "Aplicar",
  profileDelete: "Borrar",
  profileDeleteSure: "¿Seguro?",
  profileDeleteTitle: "Borrar este perfil",
  profileDeleteConfirmTitle: "Pulsa otra vez para borrarlo de verdad",
  profileNamePlaceholder: "Nombre para guardar la configuracion actual",
  profileSave: "Guardar",
  profileSaved: (name: string) => `Perfil "${name}" guardado`,
  profileApplied: (name: string) => `Perfil "${name}" aplicado`,
  profileDeleted: (name: string) => `Perfil "${name}" borrado`,
  profileFallbacks: (name: string, what: string) =>
    `Perfil "${name}" aplicado, pero estos dispositivos ya no existen y han pasado al predeterminado: ${what}. Revisalos en Fuentes.`,
  profilesNote1:
    "Un perfil guarda TODO lo de esta pestana: idiomas, fuentes y sus dispositivos, la voz, los parrafos y la carpeta de salida. Repetir un nombre actualiza ese perfil con lo que tengas ahora.",
  profilesNote2:
    "No guarda las rutas de la instalacion (el Python de los sidecars), porque describen esta maquina y no como quieres usarla. Y si un dispositivo guardado ya no esta —unos auriculares desenchufados, o VB-CABLE desinstalado— se pasa al predeterminado y se te dice cual.",
  // Nombres que devuelve el backend en `fallbacks`.
  deviceSystem: "sistema",
  deviceMic: "microfono",
  deviceVoice: "voz",

  // ------------------------------------------------------ carpeta de salida
  outputFolder: "Carpeta de trabajo",
  change: "Cambiar…",
  open: "Abrir",
  fileName: "Nombre del fichero",
  fileNamePlaceholder: "transcripcion",
  outputNote1: (preview: string) =>
    `La fecha va delante: ${preview}. Si ya existe uno con ese nombre se añade _2, _3… en vez de sobreescribirlo. Los caracteres que Windows no admite se cambian por _`,
  outputNote2:
    "La carpeta tiene que ser absoluta: una ruta relativa dependeria del directorio desde el que se lanza la app, y no sabrias donde han acabado los ficheros.",

  // --------------------------------------------------------------- fuentes
  sources: "Fuentes",
  refresh: "↻ Actualizar",
  refreshTitle:
    "Volver a buscar dispositivos (por ejemplo, tras enchufar unos auriculares o instalar VB-CABLE)",
  systemAudio: "Audio del sistema",
  myMic: "Mi microfono",
  defaultDevice: "Dispositivo predeterminado",
  defaultSuffix: " (predeterminado)",

  // --------------------------------------------------------- transcripcion
  transcription: "Transcripcion",
  latency: "Latencia",
  normalizeGain: "Compensar el volumen del sistema",
  paragraphs: "Parrafos",
  speechPause: "Pausa del habla",
  maxParagraph: "Parrafo maximo",
  paragraphsNote:
    "El parrafo se cierra cuando el modelo lleva ese rato sin transcribir nada nuevo. Se mira el TEXTO, no el volumen: con musica de fondo el nivel no baja nunca, pero la musica tampoco genera transcripcion. El maximo evita que un monologo sin pausas quede como un bloque interminable.",

  // --------------------------------------------------------------- idiomas
  languages: "Idiomas",
  roomBlock: "Sala (el audio del sistema)",
  micBlock: "Microfono (lo que dices tu)",
  roomNote: "En que hablan los demas, y en que lo lees tu.",
  micNote:
    "En que hablas tu, y en que te oyen. Si la voz sintetica esta activada, pronuncia el idioma de la derecha. Sin tocar nada, el microfono es el espejo de la sala: hablas en el idioma en que lees, y se te traduce al de la sala.",
  pickRoomFirst: "(elige la sala primero)",
  translateParallel: "Traducir en paralelo",
  translateNote:
    "Sin traduccion solo se transcribe, todo en el idioma de la sala. El modelo de voz no traduce: lo hace NLLB-200 despues, frase a frase. Los fallos del reconocimiento se arrastran a la traduccion.",

  // ------------------------------------------------------------------- voz
  speakTitle: "Hablar por mi",
  speakEnable: "Pronunciar mi traduccion por un dispositivo de salida",
  speakNote:
    "Lo que dices por el microfono, ya traducido, sale hablado por el dispositivo elegido. Necesita «Mi microfono» activado en Fuentes.",
  speakSetupTitle: "Como se monta con VB-CABLE.",
  speakSetupIntro: "El cable va en un solo sentido, CABLE Input → CABLE Output:",
  speakSetup1: "Aqui, en «Hablar por»: CABLE Input — es donde escribe la voz.",
  speakSetup2:
    "En Fuentes, «Mi microfono»: tu microfono REAL. Si pones aqui el cable, la app solo se oye a si misma.",
  speakSetup3:
    "En Fuentes, «Audio del sistema»: tus altavoces o auriculares — por ahi suena la reunion, que es lo que hay que transcribir.",
  speakSetup4:
    "DENTRO de Teams, como microfono: CABLE Output. Ese es el paso que hace que te oigan.",
  engine: "Motor",
  engineChatterbox: "Chatterbox — mi voz clonada (23 idiomas)",
  engineKokoro: "Kokoro — voz neutra (8 idiomas, mas ligero)",
  voiceSample: "Muestra de mi voz",
  voiceWavPlaceholder: "C:\\...\\mi-voz.wav",
  voiceSampleNote:
    "Un WAV con 10-30 segundos de tu habla, limpia y sin ruido de fondo. La voz clonada imita el tono de la muestra: si la muestra es monotona, la voz tambien lo sera.",
  presetVoice: "Voz preajustada",
  presetVoiceNote:
    "El prefijo dice idioma y genero: af_heart (ingles, f), am_adam (ingles, m), ef_dora (espanol, f), em_alex (espanol, m)…",
  speakThrough: "Hablar por",
  defaultDeviceSpeakers: "Dispositivo predeterminado (los altavoces)",
  noCableNote:
    "No se ve ningun CABLE Input: para que la reunion te oiga sin que suene por los altavoces hace falta VB-CABLE, un dispositivo virtual que se instala aparte.",
  noCableRefresh: "Ya lo he instalado, buscar de nuevo",
  markEcho: "Reconocer mi propia voz sintetica si vuelve por el sistema",
  markEchoNote:
    "Si la reunion devuelve tu voz sintetica, la transcripcion la volveria a traducir (espanol → ingles → espanol sale raro). Con esto se detecta y se marca como eco en vez de re-traducirla.",
  groupSentences: "Agrupar frases",
  chars: (n: number) => `${n} caracteres`,
  maxWait: "Espera maxima",
  groupNote:
    "Con la voz callada, la primera frase se pronuncia al momento. El agrupado actua solo mientras suena: clonar tiene un coste fijo por peticion (~1 s medido) y con frases sueltas generaria mas despacio de lo que hablas; agrupando ~250 caracteres el retraso queda acotado.",

  // ------------------------------------------------------- transcripcion UI
  copy: "Copiar",
  subtitles: "Subtitulos",
  txtBilingual: ".txt bilingue",
  clear: "Limpiar",
  original: "Original",
  translation: "Traduccion",
  copyParagraph: "Copiar este parrafo",
  whoSystem: "sistema",
  whoMic: "micro",
  whoEcho: "tu voz",
  echoTitle: "Tu voz sintetica captada de vuelta por el sistema",
  emptyOriginal: "Nada todavia. Dale a Arrancar y pon algo a sonar.",
  emptyTranslated: "Todavia nada. Cada frase aparece en cuanto esta traducida.",
  emptyOthers: "Nada todavia. Lo que suene en la reunion aparece aqui.",
  emptyMine: "Habla al microfono y tu texto aparece aqui.",
  othersLabel: "Los demas",
  myVoiceSays: "Mi voz les dice",
  mineTranslated: "Lo mio, traducido",
  meLabel: "Yo",

  // --------------------------------------------------------------- overlay
  overlayIdle: "Esperando audio…",
  overlayHide: "Ocultar (se puede volver a abrir desde la bandeja)",

  // --------------------------------------------------- modos de vista
  splitCombined: "Combinado: la traduccion debajo de cada parrafo",
  splitV: "Dividido vertical: uno al lado del otro",
  splitH: "Dividido horizontal: uno encima del otro",
  splitOriginal: "Solo el original",
  splitTranslated: "Solo la traduccion",
  splitMeeting: "Reunion: los demas arriba, yo abajo, un idioma por columna",
  meterTitle: (rms: number, gain: number) =>
    `rms ${rms.toFixed(5)} · ganancia x${gain.toFixed(1)}`,

  // ------------------------------------------------------------- latencias
  lookahead0: "80 ms - minima latencia, mas errores",
  lookahead3: "320 ms - equilibrio recomendado",
  lookahead6: "560 ms - mas preciso",
  lookahead13: "1120 ms - maxima precision",

  // ------------------------------- nombres de idioma de los desplegables
  langAuto: "Detectar automaticamente",
  langNames: {
    "es-ES": "Espanol (Espana)",
    "es-US": "Espanol (America)",
    "en-US": "Ingles (EEUU)",
    "en-GB": "Ingles (Reino Unido)",
    "fr-FR": "Frances",
    "de-DE": "Aleman",
    "it-IT": "Italiano",
    "pt-BR": "Portugues (Brasil)",
    "pt-PT": "Portugues (Portugal)",
    "nl-NL": "Neerlandes",
    "tr-TR": "Turco",
    "ru-RU": "Ruso",
    "ar-AR": "Arabe",
    "hi-IN": "Hindi",
    "ja-JP": "Japones",
    "ko-KR": "Coreano",
    "vi-VN": "Vietnamita",
    "uk-UA": "Ucraniano",
    "zh-CN": "Chino mandarin",
    "pl-PL": "Polaco",
  } as Record<string, string>,
};

export type Strings = typeof es;

const en: Strings = {
  appLoading: "Loading…",
  tabConfig: "Settings",
  tabTranscript: "Transcript",
  start: "Start",
  stop: "Stop",
  starting: "Starting… loading the models takes up to a minute",
  sessionStopped: "Session stopped",
  uiLanguage: "Interface language",

  engineReady: (device, ms, lang) =>
    `Engine ready on ${device}, latency ${ms} ms, language ${lang}`,
  translatorReady: (device, target) => `Translator ready on ${device}, target ${target}`,
  voiceReady: (device) => `Synthetic voice ready on ${device}`,
  loadingStart: "Loading models…",
  loadingVoice: "Voice ready",
  loadingTranslator: "Translator ready",

  nothingToCopy: "Nothing to copy",
  copied: "Copied",
  savedTo: (path) => `Saved: ${path}`,
  devicesRefreshed: "Devices refreshed",
  folderSaved: "Folder saved",
  hints: (toggle, overlay) => `${toggle} starts and stops · ${overlay} shows the subtitles`,

  warnAutoBlocksTranslation:
    "Translation is on but the language is “Detect automatically”. The translator needs to know which language it starts from, so pick a specific one.",
  warnLowVolume:
    "Windows volume is very low. Loopback captures AFTER the volume control, so the transcript will be poor even with the gain already maxed out.",
  warnSpeakMisconfigured:
    "Speaking with your voice needs “Translate in parallel” and “My microphone” enabled: what gets spoken is the translation of what you say into the mic.",
  warnSpeakFeedback:
    "Your synthetic voice will play through the SPEAKERS with the microphone open: the mic may pick it up and translate it again. Use headphones, or pick “CABLE Input” under “Speak through” so only the meeting hears it.",
  warnCableLoop:
    "You are capturing from the same virtual cable the voice speaks into: the app would only hear itself and transcribe nothing. The cable runs one way, CABLE Input → CABLE Output: leave “CABLE Input” under “Speak through”, put your REAL microphone under Sources, and pick “CABLE Output” as the microphone INSIDE Teams, not here.",
  speechQueue: (secs, pending) =>
    `Synthetic voice: ${secs} s queued${pending > 0 ? ` · ${pending} sentences waiting` : ""}`,
  speechBehind:
    " — it is generating slower than you speak; pause for a moment or group more characters",

  profiles: "Profiles",
  profilesEmpty: "No profiles yet",
  profilesPick: "Pick a profile…",
  profileApply: "Apply",
  profileDelete: "Delete",
  profileDeleteSure: "Sure?",
  profileDeleteTitle: "Delete this profile",
  profileDeleteConfirmTitle: "Click again to really delete it",
  profileNamePlaceholder: "Name to save the current settings under",
  profileSave: "Save",
  profileSaved: (name) => `Profile "${name}" saved`,
  profileApplied: (name) => `Profile "${name}" applied`,
  profileDeleted: (name) => `Profile "${name}" deleted`,
  profileFallbacks: (name, what) =>
    `Profile "${name}" applied, but these devices no longer exist and fell back to the default: ${what}. Check them under Sources.`,
  profilesNote1:
    "A profile saves EVERYTHING on this tab: languages, sources and their devices, the voice, paragraphs and the output folder. Reusing a name updates that profile with what you have now.",
  profilesNote2:
    "It does not save the installation paths (the Python for the sidecars), because those describe this machine and not how you want to use it. And if a saved device is gone — headphones unplugged, or VB-CABLE uninstalled — it falls back to the default and you are told which one.",
  deviceSystem: "system",
  deviceMic: "microphone",
  deviceVoice: "voice",

  outputFolder: "Working folder",
  change: "Change…",
  open: "Open",
  fileName: "File name",
  fileNamePlaceholder: "transcript",
  outputNote1: (preview) =>
    `The date goes first: ${preview}. If one with that name already exists, _2, _3… is appended instead of overwriting it. Characters Windows does not allow are replaced with _`,
  outputNote2:
    "The folder must be absolute: a relative path would depend on the directory the app was launched from, and you would not know where the files ended up.",

  sources: "Sources",
  refresh: "↻ Refresh",
  refreshTitle:
    "Look for devices again (for example after plugging in headphones or installing VB-CABLE)",
  systemAudio: "System audio",
  myMic: "My microphone",
  defaultDevice: "Default device",
  defaultSuffix: " (default)",

  transcription: "Transcription",
  latency: "Latency",
  normalizeGain: "Compensate for system volume",
  paragraphs: "Paragraphs",
  speechPause: "Speech pause",
  maxParagraph: "Maximum paragraph",
  paragraphsNote:
    "A paragraph closes when the model has gone that long without transcribing anything new. It watches the TEXT, not the volume: with background music the level never drops, but music does not produce transcription either. The maximum keeps a pause-free monologue from becoming one endless block.",

  languages: "Languages",
  roomBlock: "Room (the system audio)",
  micBlock: "Microphone (what you say)",
  roomNote: "What the others speak, and what you read it in.",
  micNote:
    "What you speak, and what they hear you in. If the synthetic voice is on, it speaks the language on the right. Left alone, the microphone mirrors the room: you speak the language you read in, and you are translated into the room's.",
  pickRoomFirst: "(pick the room first)",
  translateParallel: "Translate in parallel",
  translateNote:
    "Without translation there is only transcription, all in the room's language. The speech model does not translate: NLLB-200 does it afterwards, sentence by sentence. Recognition mistakes carry over into the translation.",

  speakTitle: "Speak for me",
  speakEnable: "Speak my translation through an output device",
  speakNote:
    "What you say into the microphone, already translated, comes out spoken through the chosen device. Needs “My microphone” enabled under Sources.",
  speakSetupTitle: "How to wire it up with VB-CABLE.",
  speakSetupIntro: "The cable runs one way, CABLE Input → CABLE Output:",
  speakSetup1: "Here, under “Speak through”: CABLE Input — that is where the voice writes.",
  speakSetup2:
    "Under Sources, “My microphone”: your REAL microphone. Put the cable here and the app only hears itself.",
  speakSetup3:
    "Under Sources, “System audio”: your speakers or headphones — that is where the meeting comes out, which is what needs transcribing.",
  speakSetup4:
    "INSIDE Teams, as the microphone: CABLE Output. That is the step that makes them hear you.",
  engine: "Engine",
  engineChatterbox: "Chatterbox — my cloned voice (23 languages)",
  engineKokoro: "Kokoro — neutral voice (8 languages, lighter)",
  voiceSample: "Sample of my voice",
  voiceWavPlaceholder: "C:\\...\\my-voice.wav",
  voiceSampleNote:
    "A WAV with 10-30 seconds of your speech, clean and without background noise. The cloned voice imitates the sample's delivery: if the sample is monotone, so is the voice.",
  presetVoice: "Preset voice",
  presetVoiceNote:
    "The prefix gives language and gender: af_heart (English, f), am_adam (English, m), ef_dora (Spanish, f), em_alex (Spanish, m)…",
  speakThrough: "Speak through",
  defaultDeviceSpeakers: "Default device (the speakers)",
  noCableNote:
    "No CABLE Input in sight: for the meeting to hear you without it playing through the speakers you need VB-CABLE, a virtual device installed separately.",
  noCableRefresh: "Already installed it, look again",
  markEcho: "Recognise my own synthetic voice if it comes back through the system",
  markEchoNote:
    "If the meeting sends your synthetic voice back, the transcript would translate it again (Spanish → English → Spanish gets strange). This detects it and marks it as an echo instead of re-translating it.",
  groupSentences: "Group sentences",
  chars: (n) => `${n} characters`,
  maxWait: "Maximum wait",
  groupNote:
    "With the voice silent, the first sentence is spoken right away. Grouping only kicks in while it is speaking: cloning has a fixed cost per request (~1 s measured) and with single sentences it would generate slower than you speak; grouping ~250 characters keeps the lag bounded.",

  copy: "Copy",
  subtitles: "Subtitles",
  txtBilingual: ".txt bilingual",
  clear: "Clear",
  original: "Original",
  translation: "Translation",
  copyParagraph: "Copy this paragraph",
  whoSystem: "system",
  whoMic: "mic",
  whoEcho: "your voice",
  echoTitle: "Your synthetic voice picked back up through the system",
  emptyOriginal: "Nothing yet. Hit Start and play something.",
  emptyTranslated: "Nothing yet. Each sentence appears as soon as it is translated.",
  emptyOthers: "Nothing yet. Whatever plays in the meeting shows up here.",
  emptyMine: "Speak into the microphone and your text appears here.",
  othersLabel: "The others",
  myVoiceSays: "My voice tells them",
  mineTranslated: "Mine, translated",
  meLabel: "Me",

  overlayIdle: "Waiting for audio…",
  overlayHide: "Hide (you can reopen it from the tray)",

  splitCombined: "Combined: the translation under each paragraph",
  splitV: "Split vertically: side by side",
  splitH: "Split horizontally: one above the other",
  splitOriginal: "Original only",
  splitTranslated: "Translation only",
  splitMeeting: "Meeting: the others on top, you below, one language per column",
  meterTitle: (rms, gain) => `rms ${rms.toFixed(5)} · gain x${gain.toFixed(1)}`,

  lookahead0: "80 ms - lowest latency, more mistakes",
  lookahead3: "320 ms - recommended balance",
  lookahead6: "560 ms - more accurate",
  lookahead13: "1120 ms - highest accuracy",

  langAuto: "Detect automatically",
  langNames: {
    "es-ES": "Spanish (Spain)",
    "es-US": "Spanish (Latin America)",
    "en-US": "English (US)",
    "en-GB": "English (UK)",
    "fr-FR": "French",
    "de-DE": "German",
    "it-IT": "Italian",
    "pt-BR": "Portuguese (Brazil)",
    "pt-PT": "Portuguese (Portugal)",
    "nl-NL": "Dutch",
    "tr-TR": "Turkish",
    "ru-RU": "Russian",
    "ar-AR": "Arabic",
    "hi-IN": "Hindi",
    "ja-JP": "Japanese",
    "ko-KR": "Korean",
    "vi-VN": "Vietnamese",
    "uk-UA": "Ukrainian",
    "zh-CN": "Chinese (Mandarin)",
    "pl-PL": "Polish",
  },
};

export const STRINGS: Record<Lang, Strings> = { es, en };

/** Idioma inicial: el guardado, y si no lo hay, el del sistema. Por defecto
 *  ingles, que es el que mas gente entiende. */
export function initialLang(): Lang {
  const saved = localStorage.getItem("uiLang");
  if (saved === "es" || saved === "en") return saved;
  return navigator.language?.toLowerCase().startsWith("es") ? "es" : "en";
}
