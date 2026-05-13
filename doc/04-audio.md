# 4. Captura e Processamento de Áudio

## 4.1 O pipeline de áudio

```
Microfone (hardware)
      │  formato nativo (ex: f32, 48kHz, stereo)
      ▼
cpal callback (thread de áudio do SO)
      │  acumula em Vec<f32> via Arc<Mutex<>>
      ▼
AudioCapture::record_to_completion() (thread spawn_blocking)
      │  aguarda STOP_REQUESTED flag
      ▼
process_audio()
      │  1. mixdown: N canais → mono
      │  2. resample: taxa nativa → 16kHz
      ▼
mpsc::Sender<Vec<f32>>
      │  entrega ao loop principal (daemon/mod.rs)
      ▼
Transcriber::transcribe()
```

Nenhuma etapa deste pipeline toca o disco.

---

## 4.2 Por que `cpal`?

O Linux tem múltiplas APIs de áudio: ALSA (baixo nível), PulseAudio
(legado), PipeWire (moderno), JACK (profissional). Cada uma tem sua
própria API em C.

`cpal` (Cross-Platform Audio Library) abstrai todas elas atrás de uma
interface Rust uniforme. No Linux moderno, usa PipeWire diretamente via
a API de compatibilidade ALSA do PipeWire. O resultado:

- O código Rust não menciona PipeWire em nenhum lugar
- Se o sistema usar ALSA puro (sem PipeWire), o código funciona sem alteração
- A mesma crate funcionaria em macOS ou Windows (CoreAudio, WASAPI)

---

## 4.3 Negociação de formato com o dispositivo

O Whisper requer áudio em **f32, mono, 16kHz**. A maioria dos microfones
modernos captura em **f32 ou i16, stereo, 44.1kHz ou 48kHz**.

A estratégia é aceitar o formato nativo do dispositivo e converter depois:

```rust
fn negotiate_config(device: &Device, preferred_rate: u32)
    -> anyhow::Result<(StreamConfig, u32, u16)>
{
    // Tenta 16kHz direto (sem resample)
    for cfg_range in &supported {
        if cfg_range.min_sample_rate().0 <= preferred_rate
            && cfg_range.max_sample_rate().0 >= preferred_rate
        {
            return Ok((config_at_16k, preferred_rate, channels));
        }
    }

    // Fallback: aceita taxa nativa, resample depois
    let native_rate = best.max_sample_rate().0;
    warn!("Taxa {}Hz não suportada. Usando {}Hz com resample.", ...);
    Ok((config_at_native_rate, native_rate, channels))
}
```

**Por que converter depois da captura e não durante?**

O callback de áudio do cpal roda em uma thread de alta prioridade com
deadline de tempo real. Qualquer processamento extra no callback
(resample, conversão) aumenta o risco de underrun (falha de áudio).

Converter em pós-processamento (após a gravação completa) evita esse
risco completamente, sem custo perceptível para o usuário — o resample
de uma gravação de 5 minutos leva milissegundos.

---

## 4.4 A flag atômica `STOP_REQUESTED`

```rust
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
```

Esta é a solução para um problema clássico de sistemas: **como parar
um callback que roda em outra thread?**

### O problema

O callback de áudio do cpal é registrado assim:

```rust
device.build_input_stream(config, move |data: &[f32], _| {
    // Esta closure roda em thread de áudio — NOT o runtime Tokio
    accumulate_samples(data, &buffer, max_samples);
}, ...)?
```

A closure é `move` e roda em uma thread gerenciada pelo SO, fora do
controle do Tokio. Não é possível passar um `tokio::sync::Mutex` ou
`mpsc::Receiver` para dentro — eles não são usáveis fora do runtime
assíncrono.

### A solução: `AtomicBool` estático

`AtomicBool` com `Ordering::Relaxed` é:

- **Thread-safe sem lock:** operações atômicas são garantidas pelo hardware
- **Sem overhead:** uma instrução de CPU (LOCK XCHG ou equivalente)
- **Sem lifetime:** `static` vive para sempre, não há problema de lifetime
- **`Sync + Send` garantido:** o compilador aceita em closures `move`

```rust
// No loop principal (daemon/mod.rs):
AudioCapture::signal_stop();  // → STOP_REQUESTED.store(true, Relaxed)

// No callback de áudio (cpal thread):
if STOP_REQUESTED.load(Ordering::Relaxed) {
    return; // para de acumular
}
```

### Por que `Ordering::Relaxed`?

`Ordering` controla as garantias de ordenamento de memória:

- `SeqCst` (Sequential Consistency): garantia mais forte, mais cara
- `Acquire/Release`: para comunicação produtor/consumidor
- `Relaxed`: sem garantias de ordenamento, apenas atomicidade

Para uma flag de parada simples, `Relaxed` é suficiente. Não importa
se o callback vê a flag `true` um ou dois ciclos de CPU depois de ela
ser definida — a diferença é nanossegundos e o áudio perdido é
imperceptível. O custo de `SeqCst` em hot path não se justifica.

---

## 4.5 O buffer compartilhado: `Arc<Mutex<Vec<f32>>>`

```rust
let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(
    (config.max_recording_secs as usize + 10)
    * native_sample_rate as usize
    * channels as usize,
)));
```

### Por que `Arc<Mutex<>>`?

O buffer precisa ser acessível de duas threads simultaneamente:

- **Thread de áudio (cpal):** escreve amostras continuamente
- **Thread principal (spawn_blocking):** lê o buffer ao final da gravação

`Arc` (Atomic Reference Counting) permite compartilhar ownership entre
threads. `Mutex` garante acesso exclusivo — apenas uma thread acessa
o `Vec` por vez.

### Por que `try_lock` no callback?

```rust
fn accumulate_samples(data: &[f32], buffer: &Arc<Mutex<Vec<f32>>>, ...) {
    if let Ok(mut buf) = buffer.try_lock() {
        buf.extend_from_slice(data);
    }
    // Se try_lock falhar: descarta o chunk silenciosamente
}
```

`lock()` bloquearia a thread de áudio se o mutex estivesse ocupado.
Bloquear a thread de áudio causa underrun (falha de captura).

`try_lock()` retorna imediatamente: ou obtém o lock, ou retorna `Err`.
Em caso de falha, o chunk é descartado. Na prática, o mutex raramente
está contido (o thread principal só lê ao final), então descartes são
raros e imperceptíveis (< 1ms de áudio por evento).

### Pré-alocação do buffer

```rust
Vec::with_capacity(max_secs * sample_rate * channels)
```

`Vec` em Rust realoca e copia ao crescer. Para uma gravação de 5 minutos:

- 300s × 16000 Hz × 1 canal = 4.800.000 amostras × 4 bytes = ~18MB

Sem pré-alocação, haveria ~23 realocações durante a gravação
(cada uma dobrando a capacidade). Com pré-alocação, zero realocações.

---

## 4.6 Mixdown N canais → mono

```rust
let mono: Vec<f32> = raw.chunks_exact(channels)
    .map(|frame| frame.iter().sum::<f32>() / channels as f32)
    .collect();
```

O áudio interleaved com N canais é organizado assim na memória:

```
[L0, R0, L1, R1, L2, R2, ...]  (stereo, channels=2)
```

`chunks_exact(channels)` divide o slice em janelas de N elementos,
cada uma representando um frame (um instante no tempo). A média
aritmética dos canais é o mixdown padrão para fala — equivalente ao
que qualquer editor de áudio faz ao "converter para mono".

---

## 4.7 Resample por interpolação linear

```rust
let ratio = native_rate as f64 / target_rate as f64;
// Ex: 48000 / 16000 = 3.0

for i in 0..output_len {
    let pos = i as f64 * ratio;  // posição na fonte
    let idx = pos as usize;       // índice do vizinho esquerdo
    let frac = pos - idx as f64;  // fração para interpolação

    let s0 = mono[idx];
    let s1 = mono.get(idx + 1).copied().unwrap_or(s0);

    resampled.push(s0 + (s1 - s0) * frac as f32);  // lerp
}
```

A interpolação linear (`lerp`) calcula o valor entre dois pontos
vizinhos proporcionalmente à distância fracionária:

```
s0 ────────────────── s1
     ↑
    pos (frac = 0.4)
    valor = s0 + (s1 - s0) × 0.4
```

Para fala, onde as frequências relevantes estão abaixo de 8kHz
(teorema de Nyquist para 16kHz), a interpolação linear introduz
artefatos mínimos. Samplers de alta qualidade (Lanczos, sinc) seriam
desnecessários aqui e adicionariam dependências.

---

## 4.8 Liberação garantida do buffer (LGPD)

```rust
// Em daemon/mod.rs, após receber o buffer:
let result = tokio::task::spawn_blocking(move || {
    // `audio_buffer` foi moved para cá
    Transcriber::transcribe(&model, &inf_config, &audio_buffer)
    // `audio_buffer` é dropped aqui, ao fim da closure
})
.await;
// Após o await, não existe mais nenhuma cópia do áudio em memória
```

O Rust garante que quando um valor sai de escopo, seu destrutor (`Drop`)
é chamado e a memória é liberada. Não há garbage collector que possa
"atrasar" a liberação. O `Vec<f32>` com o áudio é destruído
deterministicamente ao fim da closure de `spawn_blocking`.
