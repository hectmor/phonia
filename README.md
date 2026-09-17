# phonia -- fase 0

Spike de línea de comandos que valida, de punta a punta, el camino de audio de **phonia**
(el futuro reproductor TIDAL hi-fi con TUI + daemon): login PKCE -> `playbackinfo` HiRes ->
descarga de segmentos DASH -> decodificación FLAC -> salida ALSA *bit-perfect* a un DAC USB.

No hay TUI ni daemon todavía: esto es un CLI de una sola pasada para probar que cada eslabón
de la cadena funciona con hardware real (un Fosi Audio DS2 en `hw:1,0` durante el desarrollo).

## Comandos

- **`phonia login`** -- Inicia sesión en TIDAL con el flujo PKCE (obligatorio: el flujo de
  device-code nunca recibe la entitlement `HI_RES_LOSSLESS`, aunque la cuenta la tenga). Abre
  una URL en el navegador (o la imprime si no se pudo abrir sola); tras iniciar sesión, TIDAL
  redirige a una página de error ("oops"), eso es normal -- copia la URL completa de la barra
  de direcciones y pégala en la terminal. Guarda la sesión en
  `~/.config/phonia/session.json` (permisos `0600`).

- **`phonia play <TRACK_ID> [--device hw:1,0] [--quality hires|lossless] [--save-mp4 <ruta>]`**
  -- Descarga y reproduce una pista por su ID. Consulta `playbackinfo`, descarga el manifiesto
  DASH (HiRes) o el archivo directo (Lossless/High/Low), lo decodifica y lo saca por ALSA.
  `--save-mp4` además guarda los bytes descargados en disco (útil para inspeccionar el fMP4).

- **`phonia play-file <ruta> [--device hw:1,0]`** -- Decodifica y reproduce un archivo local
  (FLAC o fMP4) por la misma ruta de salida ALSA, sin tocar TIDAL. Sirve para probar el DAC de
  forma aislada.

- **`phonia probe-device [--device hw:1,0]`** -- Abre el dispositivo ALSA indicado en modo
  reproducción (sin escribir nada) y lista qué formatos (`S16_LE`, `S24_3LE`, `S24_LE`,
  `S32_LE`) y qué frecuencias (44.1 kHz .. 384 kHz) acepta de forma nativa.

## Cómo verificar que la salida es bit-perfect

Al reproducir con `phonia play` o `phonia play-file` contra un dispositivo `hw:N,D`, tras el
primer bloque de audio se imprime el contenido de
`/proc/asound/card<N>/pcm<D>p/sub0/hw_params` seguido de un veredicto:

```
Fuente: FLAC 24-bit/96000 Hz 2ch → hw:1,0 S24_3LE 96000 Hz  ✔ BIT-PERFECT
```

o, si algo no cuadra (frecuencia o formato distintos a los negociados, o el dispositivo no es
`hw:N,D`):

```
Fuente: FLAC 24-bit/96000 Hz 2ch → hw:1,0 S24_3LE  ✖ CONVERTED (la tarjeta reporta 48000 Hz en vez de 96000 Hz)
```

También puedes comprobarlo a mano en otra terminal mientras suena algo:

```sh
cat /proc/asound/card1/pcm0p/sub0/hw_params
```

Si ves `closed`, nada tiene el dispositivo abierto en ese momento.

## Importante: PipeWire no debe tener el DAC abierto

`phonia` abre el dispositivo ALSA (`hw:1,0` por defecto) directamente, sin pasar por
`plughw`/`default`/`dmix`, porque cualquiera de esas capas puede remuestrear o mezclar el
audio y rompe la garantía de bit-perfect. Si PipeWire (u otra aplicación) ya tiene el DAC
abierto, `phonia` fallará al abrir el dispositivo con un error EBUSY explicando que hay que
liberarlo primero (por ejemplo, pausando la reproducción hacia esa tarjeta desde PipeWire, o
silenciando/deshabilitando su perfil para esa tarjeta mientras se usa `phonia`).

## Estado de la fase 0

- `cargo build` y `cargo test` pasan limpio; `cargo clippy` sin warnings.
- Lo que sigue quedando fuera de esta fase (llegará en fases posteriores): streaming real sin
  buffering completo en memoria, TUI, daemon, gapless, `%0Nd` en plantillas de segmento DASH,
  soporte de cifrado de manifiesto.

## Tech stack

Every dependency here exists to protect one single guarantee end to end: **no bit gets touched
in a way that isn't reversible**. This section explains what each piece of the stack does and,
more importantly, *why* it was chosen.

- **Rust** -- a systems language with no garbage collector and no hidden allocations was a hard
  requirement for the audio output path: a GC pause or an unexpected allocation on the hot path
  can produce an audible glitch. Rust also makes the "never silently convert through a float"
  invariant (see `decode`/`output::alsa` below) something the type system helps enforce, not just
  a comment.

- **[`clap`](https://docs.rs/clap)** -- parses the CLI's subcommands and flags (`login`, `play`,
  `play-file`, `probe-device`). Declarative, well-tested, and there was no reason to hand-roll
  argument parsing for a project this size.

- **[`tokio`](https://docs.rs/tokio)** -- the async runtime. Needed because talking to TIDAL
  (`reqwest`, `tidlers`) is inherently async I/O. The decode+ALSA-write loop, by contrast, is
  synchronous, CPU/IO-bound blocking work, so it runs on its own thread via
  `tokio::task::spawn_blocking` instead of the async executor's thread pool -- blocking that pool
  would stall every other async task (including the Ctrl+C listener) for the whole playback.

- **[`reqwest`](https://docs.rs/reqwest)** -- the HTTP client used both for TIDAL's REST API
  (`playbackinfopostpaywall`) and for downloading DASH segments / direct audio URLs from TIDAL's
  CDN. One client instance is reused for the whole session (connection pooling, consistent
  `User-Agent`).

- **[`tidlers`](https://docs.rs/tidlers)** -- the Rust TIDAL client. It implements the actual
  OAuth/PKCE dance (building the `code_challenge`, exchanging the authorization code for tokens,
  refreshing them) -- reimplementing OAuth crypto by hand would be pure risk with no upside. Its
  `playbackinfo` response type and `DashManifest`, however, are incomplete for phase 0's needs
  (see `tidal.rs`/`dash.rs` below), so those two pieces are implemented directly against TIDAL's
  API instead of forking the crate's internals (most of what's needed is `pub(crate)`).

- **`base64` + `serde` / `serde_json`** -- TIDAL's `playbackinfo` response embeds its actual
  manifest as a base64-encoded string, which decodes to either JSON (LOW/HIGH/LOSSLESS: a plain
  direct-download URL) or DASH/XML (HiRes: a segmented manifest). `serde_json` deserializes the
  outer response and the JSON-manifest case; `base64` decodes the embedded blob before either
  path can proceed.

- **[`quick-xml`](https://docs.rs/quick-xml)** -- parses the DASH (MPD) manifest XML for HiRes
  tracks in `dash.rs`: `SegmentTemplate`, `SegmentTimeline`, `BaseURL`, segment counting. Chosen
  as a low-level, allocation-conscious streaming XML reader rather than a full DOM parser, since
  all we need is a handful of attributes and text nodes out of a well-known, small manifest.
  Note from hard-won experience: `quick-xml` does **not** unescape XML entities for you by
  default, and it delivers `&amp;`/`&lt;`/etc. inside text content as *separate*
  `Event::GeneralRef` events rather than inline in `Event::Text` -- both attribute values and
  `<BaseURL>` text have to be explicitly unescaped/reassembled, or pre-signed CDN URLs (which
  routinely contain `&`-separated query parameters escaped as `&amp;`) come out corrupted and get
  rejected by the CDN with a 404/403.

- **[`symphonia`](https://docs.rs/symphonia)** -- the pure-Rust audio decoder (FLAC standalone
  and FLAC-in-fMP4/DASH, both of which TIDAL uses depending on quality tier). Chosen because it's
  the most mature pure-Rust decoder available (also used by Firefox) and because its FLAC decoder
  decodes each subframe into raw, left-justified `i32` samples -- `copy_to_vec_interleaved::<i32>`
  on that output is a pure identity copy, no rounding, no float round-trip. That property is the
  foundation of the whole bit-perfect guarantee: the moment any sample got routed through `f32`,
  the guarantee would be silently broken.

- **[`alsa`](https://docs.rs/alsa) (Rust bindings for `libasound`)** -- the only way to talk to
  Linux audio hardware directly without hand-writing FFI. Used to open `hw:N,D` devices directly
  (never `default`/`plughw`/`dmix`, all of which can resample or mix and would break
  bit-perfectness by definition), negotiate a lossless integer hardware format, and pack the
  decoder's left-justified `i32` samples into that format with pure bit shifts.

- **[`anyhow`](https://docs.rs/anyhow)** -- error handling with contextual, human-readable
  messages (in Spanish, since this phase is operated directly by a person, not consumed as a
  library) at every fallible step, from "no saved session, run `phonia login`" to "the device
  doesn't support any lossless integer format for a 24-bit source."

## Licencia

MIT. Consulta el archivo [LICENSE](LICENSE).
