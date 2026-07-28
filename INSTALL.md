# Instalación desde cero

Para poner LiveTranscriber en un Windows recién hecho. Está pensado y probado
para una máquina con GPU NVIDIA; sin ella el modelo va a CPU y no da tiempo real.

## Lo que hay que tener antes

Solo dos cosas, y ninguna la instala el script:

| Requisito | Por qué | Cómo |
|---|---|---|
| **Driver de NVIDIA** | PyTorch necesita CUDA 12.8 | [nvidia.com/drivers](https://www.nvidia.com/Download/index.aspx) |
| **~15 GB libres** | 4 GB de PyTorch + 7 GB de modelos | — |

Python lo instala el script si le pasas `-InstallPython`. Rust y Node solo hacen
falta si quieres **compilar** la aplicación; para provisionar el modelo no.

## La instalación

```bash
cd E:\projects\LiveTranscriber; .\scripts\install.ps1
```

Si no tienes Python:

```bash
cd E:\projects\LiveTranscriber; .\scripts\install.ps1 -InstallPython
```

Si el disco C: va justo, los modelos a otro sitio:

```bash
cd E:\projects\LiveTranscriber; .\scripts\install.ps1 -ModelsDir D:\modelos
```

Se puede volver a ejecutar sin romper nada: reutiliza lo que ya esté. Con
`-Force` rehace el entorno virtual y regenera la configuración.

### Opciones

| Opción | Para qué |
|---|---|
| `-ModelsDir <ruta>` | Modelos fuera de `%USERPROFILE%\.cache`. Escribe `hf_home` en la configuración |
| `-SkipTranslator` | No bajar NLLB. Ahorra ~2,4 GB si no vas a traducir |
| `-SkipBuild` | No compilar la app. Solo Python y modelos |
| `-SkipVerify` | Saltarse la comprobación final. No recomendado |
| `-InstallPython` | Instalar Python 3.12 con winget si no hay ninguno válido |
| `-Force` | Rehacer entorno virtual y configuración |

## Qué hace, por orden

1. **Comprueba el terreno**: 64 bits, espacio en disco, y qué GPU hay. Lee la
   *compute capability* y decide la precisión (ver abajo).
2. **Busca un Python válido**: 3.10 a 3.13. **3.14 no vale** aunque esté
   instalado, porque PyTorch todavía no publica ruedas para él.
3. **Crea el entorno virtual** e instala PyTorch desde el índice `cu128`.
4. **Verifica que la rueda sirve para tu tarjeta.** `cu128` trae de `sm_75`
   hacia arriba, así que cubre desde Turing (RTX 20xx) pero **no** una GTX 10xx.
   Si no encaja, para y lo dice, en vez de fallar luego en tiempo de ejecución.
5. **Instala las dependencias de los sidecars.** Son cuatro: `transformers`,
   `numpy`, `huggingface_hub` y `torch`. Nada de librerías de audio — eso lo hace
   Rust.
6. **Descarga los modelos** cargándolos de verdad, no con un `snapshot_download`.
   Así baja solo lo que usa transformers (el repo del ASR incluye además un
   `.nemo` de 2,4 GB que no tocamos) y de paso se comprueba que los pesos están
   bien.
7. **Escribe `transcriber-config.toml`** con rutas absolutas y la precisión que
   corresponda.
8. **Compila la aplicación** si hay Rust y Node, dejando el instalador en
   `target\release\bundle`.
9. **Verifica**, y esto es lo que importa.

## La precisión se elige sola, y hay un motivo

`bfloat16` necesita **Ampere o superior** (capability 8.0+). En una Turing —una
2080, una 1660— PyTorch **no falla**: lo *emula*. La aplicación iría lentísima sin
ninguna pista de por qué, porque `is_bf16_supported()` devuelve `True` salvo que le
pases `including_emulation=False`.

El instalador lee la capability y escribe `float16` si hace falta. Los sidecars
además lo comprueban al arrancar y avisan por el log, por si alguien edita el TOML
a mano.

El coste es real: medido en este modelo, `float16` transcribe algo peor que
`bfloat16` (mete muletillas que nadie dijo) y va más lento. Con `float32` no hay
pérdida de calidad, pero sube la VRAM y baja el ritmo.

## Dos piezas, y por qué no puede ser una

El instalador de Windows (`.msi` / `.exe` en `target\release\bundle`) trae
**solo la aplicación**: unos pocos MB. No puede traer el entorno de Python ni los
modelos, porque son ~12 GB y meterlos en un MSI no tiene sentido.

Así que el reparto es:

| Pieza | Quién la pone | Tamaño |
|---|---|---|
| Aplicación (ventana, captura, bandeja) | el `.msi` | pocos MB |
| Python + PyTorch | `install.ps1` | ~4 GB |
| Modelos | `install.ps1` | ~7 GB |

El `.msi` lleva los sidecars de Python dentro como recursos del bundle, así que la
app los encuentra sola. Lo que necesita es un intérprete con las dependencias, y esa
ruta la escribe `install.ps1` en la configuración (o se elige desde la interfaz).

**Instalada con el MSI, la configuración no va junto al `.exe`.** En `Program Files`
un usuario sin permisos de administrador no puede escribir, así que la app lo detecta
—intentando escribir de verdad, no mirando permisos— y se pasa a
`%APPDATA%\LiveTranscriber\`. Sin eso, cada cambio hecho en la interfaz se perdería al
cerrar.

## Dónde acaban los modelos, y por qué no se duplican

Van a la caché de Hugging Face, que es **compartida por todo lo que uses en esta
máquina**:

```
%USERPROFILE%\.cache\huggingface\hub\
    models--nvidia--nemotron-3.5-asr-streaming-0.6b\    2,4 GB
    models--facebook--nllb-200-distilled-600M\          4,6 GB
```

Eso tiene una consecuencia útil: si ya los tenías de otro proyecto, el instalador
**no descarga nada**. `fetch_models.py` los abre, comprueba que están completos y
sigue. Lo dice en su salida: *"ya estaba todo en cache"* frente a *"descargado: X GB"*.

Con `-ModelsDir` la caché se mueve a donde digas y se escribe `hf_home` en la
configuración. Los sidecars la reciben como variable de entorno `HF_HOME`, que es la
única forma de que la vea la librería de Python.

El entorno virtual sí es por proyecto y **no** se comparte solo: son ~4,7 GB de
PyTorch. Si apuntas la configuración al `python.exe` de otro proyecto, funciona
—`verify.ps1` lo detecta y lo avisa como `COMPARTIDO`— pero queda atado a que ese
proyecto siga donde está.

## Verificar cuando algo falle

```bash
cd E:\projects\LiveTranscriber; .\scripts\verify.ps1
```

Repasa cada pieza en orden de dependencia y para en la primera que falle diciendo
qué hacer. Lo último que hace es lo que de verdad prueba la instalación: **lanza el
sidecar de ASR, carga el modelo en la GPU y le habla el protocolo real** — espera
el `ready`, le manda tres segundos de audio, pide un `reset` y comprueba que el
segmento se cierra.

No mide calidad de transcripción: le manda un tono, no voz. Que no salga texto es
lo normal y lo dice. Lo que comprueba es que la tubería entera funciona, que es
donde están los fallos de instalación.

## Arrancar

```bash
cd E:\projects\LiveTranscriber; npm run app:dev
```

**Sube el volumen de Windows antes de probar.** El bucle de retorno captura
*después* del control de volumen: con el volumen bajo, al modelo le llega
prácticamente silencio. La app avisa en ámbar cuando le pasa, pero mejor ahorrarse
el susto.

## Si algo va mal

| Síntoma | Causa probable |
|---|---|
| `no encuentro el sidecar` | Ruta relativa y directorio de trabajo distinto. El error lista dónde ha mirado |
| La ventana sale con `ERR_CONNECTION_REFUSED` | En desarrollo hace falta Vite. Usa `npm run app:dev`, no el `.exe` a pelo |
| No entra audio, cero bloques | El dispositivo estaba ocioso: WASAPI no genera eventos si nadie reproduce nada |
| Transcribe muy poco o nada | Volumen de Windows bajo. `cargo run -p asr-cli -- level --from system` lo dice en dos segundos |
| Va lentísimo | Precisión emulada. Comprueba `dtype` con `verify.ps1` |
| No cierra párrafos | Si hay música de fondo es normal que tarde: el corte espera a que el modelo deje de transcribir. Baja `paragraph_idle_secs` |
