# 5. Modelo e Inferência

## 5.1 O que é o Whisper?

O Whisper é um modelo de reconhecimento de fala (ASR) desenvolvido pela
OpenAI e publicado como open-source. Sua arquitetura é um **encoder-decoder
Transformer**:

```
Áudio (mel spectrogram)
       │
       ▼
  [Encoder]          Extrai representações do sinal de áudio
       │
       ▼
  [Decoder]          Gera texto token por token (autoregressive)
       │
       ▼
  Texto transcrito
```

O modelo foi treinado em 680.000 horas de áudio multilíngue, o que lhe
confere robustez a sotaques, ruído de fundo e terminologia variada.

### Variante usada: `large-v3-turbo-q5_0`

| Componente | Significado                                                                 |
| ---------- | --------------------------------------------------------------------------- |
| `large-v3` | Versão 3 do modelo grande (1.5B parâmetros no original)                     |
| `turbo`    | Versão destilada com decoder reduzido — 8x mais rápido, precisão comparável |
| `q5_0`     | Quantização de 5 bits — reduz pesos de f32 (32 bits) para 5 bits            |

A quantização `q5_0` reduz o modelo de ~3GB (fp32) para ~547MB com
perda de precisão imperceptível para fala em português. O `whisper.cpp`
implementa kernels CUDA otimizados para inferência com pesos quantizados
diretamente na GPU.

---

## 5.2 `whisper-rs`: bindings Rust para whisper.cpp

O `whisper.cpp` é uma implementação em C/C++ do Whisper otimizada para
inferência local. O `whisper-rs` gera bindings Rust via `bindgen` em
tempo de compilação.

```
whisper.cpp (C/C++ + CUDA)
       ▲
       │  FFI (Foreign Function Interface)
       │  gerado por bindgen em build.rs
       ▼
whisper-rs (Rust wrapper seguro)
       ▲
       │  API Rust idiomática
       ▼
transcriber.rs (nosso código)
```

### Por que FFI em vez de reimplementar em Rust?

O `whisper.cpp` tem kernels CUDA altamente otimizados, suporte a
múltiplos backends (CUDA, Metal, OpenCL, CPU) e anos de tuning de
performance. Reimplementar isso em Rust levaria meses e dificilmente
alcançaria a mesma performance.

O custo do FFI para chamadas de longa duração (inferência de segundos)
é desprezível — o overhead de cruzar a fronteira C/Rust é nanosegundos.

---

## 5.3 Gerenciamento do modelo: `WhisperModel`

```rust
pub struct WhisperModel {
    ctx: Mutex<WhisperContext>,
}

unsafe impl Send for WhisperModel {}
unsafe impl Sync for WhisperModel {}
```

### O problema de Send e Sync com FFI

O `WhisperContext` do `whisper-rs` envolve um ponteiro opaco C
(`*mut whisper_context`). Ponteiros crus não são `Send` nem `Sync`
por padrão no Rust — o compilador não pode verificar a thread-safety
de código C arbitrário.

Precisamos de `Arc<WhisperModel>` para compartilhar o modelo entre o
loop principal e a task de inferência. `Arc` requer `Send + Sync`.

O `unsafe impl` é uma afirmação explícita: _"Eu, programador, garanto
que o acesso é thread-safe."_ Essa garantia é cumprida pelo `Mutex`:

```rust
pub fn create_state(&self) -> anyhow::Result<WhisperState> {
    let ctx = self.ctx.lock()?;  // acesso exclusivo garantido
    ctx.create_state()
}
```

**A regra:** use `unsafe impl Send/Sync` apenas quando você tem certeza
da thread-safety e pode documentar a razão. É uma das poucas situações
em que `unsafe` é necessário e correto no código de nível de aplicação.

### Estado por sessão vs. contexto compartilhado

```rust
// Contexto: pesos do modelo na VRAM (compartilhado, imutável durante inferência)
let model = Arc::new(WhisperModel::load(&config.model)?);

// Estado: tensores intermediários de uma sessão (criado por sessão)
let mut state = model.create_state()?;
```

- **`WhisperContext`** contém os pesos da rede — gigabytes na VRAM
  carregados uma vez. Compartilhado via `Arc`, nunca modificado.
- **`WhisperState`** contém os tensores de ativação de uma sessão de
  inferência — alocados e liberados a cada transcrição.

Criar um novo `WhisperState` por sessão garante que transcrições
consecutivas sejam independentes, sem contaminação de contexto.

---

## 5.4 O problema do áudio longo

O Whisper processa janelas de no máximo **30 segundos** de áudio. Isso é
uma limitação arquitetural do modelo: o encoder foi treinado com
mel spectrograms de tamanho fixo (80 filtros × 3000 frames = 30s).

Para uma gravação de 3 minutos, há duas abordagens:

**Abordagem 1: Chunking sem overlap**

```
|── 30s ──|── 30s ──|── 30s ──|── 30s ──|── 30s ──|── 30s ──|
```

Problema: palavras no limite de cada chunk são cortadas ao meio,
gerando transcrições truncadas.

**Abordagem 2: Chunking com overlap (adotada)**

```
|──── 28s ────|
          |──── 28s ────|
                    |──── 28s ────|
```

O overlap de 2 segundos garante que cada palavra apareça em pelo menos
dois segmentos, e o segmento com melhor contexto produz o texto correto.

---

## 5.5 Segmentação com overlap

```rust
let step = segment_samples.saturating_sub(overlap_samples);
// Ex: 28s - 2s = 26s de avanço por iteração

let mut pos = 0;
while pos < total_samples {
    let end = (pos + segment_samples).min(total_samples);
    let chunk = &audio[pos..end];

    let text = run_segment(&mut state, config, chunk, prompt, n_past, ...)?;

    // Remove duplicatas do overlap
    let cleaned = remove_overlap_prefix(&all_parts, &text);
    all_parts.push(cleaned);

    pos += step;
}
```

`saturating_sub` é um detalhe importante: se `overlap_samples >=
segment_samples` (configuração inválida), em vez de underflow (que
causaria passo negativo ou zero em inteiros), retorna 0. A validação
no `config.rs` previne esse caso, mas o código é defensivo.

---

## 5.6 Remoção de overlap: `remove_overlap_prefix`

```rust
fn remove_overlap_prefix(previous_parts: &[String], current: &str) -> String {
    for n_words in [8usize, 4, 2] {
        let suffix = last_n_words_of_parts(previous_parts, n_words);
        let current_lower = current.to_lowercase();
        let suffix_lower = suffix.to_lowercase();

        if let Some(pos) = current_lower.find(&suffix_lower) {
            if pos < current.len() / 3 {
                // Match no primeiro terço = overlap real
                let after = current[pos + suffix.len()..].trim();
                if !after.is_empty() {
                    return after.to_string();
                }
            }
        }
    }
    current.to_string()
}
```

### A lógica em detalhe

O Whisper às vezes começa um novo segmento repetindo as últimas palavras
do segmento anterior (efeito do contexto via `n_past`). O objetivo é
detectar e remover essa repetição.

**Tentativas em ordem decrescente de especificidade (8 → 4 → 2 palavras):**

- 8 palavras: match mais específico, menos falsos positivos
- 4 palavras: mais permissivo, captura overlaps curtos
- 2 palavras: mínimo para evitar remoção excessiva

**Por que verificar `pos < current.len() / 3`?**

Se as 4 últimas palavras do segmento anterior aparecerem no final do
segmento atual (e não no início), provavelmente não é overlap — é
coincidência linguística (ex: a mesma frase sendo dita novamente).
O overlap real quase sempre aparece no início do novo segmento.

**Case-insensitive:** o Whisper às vezes capitaliza diferentemente a
mesma palavra em segmentos diferentes ("Modelo" vs. "modelo"). A
comparação em lowercase evita falha de detecção por capitalização.

---

## 5.7 Configuração dos parâmetros de inferência

```rust
let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });

params.set_language(Some("pt"));
params.set_initial_prompt(prompt);
params.set_n_past(n_past);
params.set_token_timestamps(false);
params.set_suppress_blank(true);
params.set_suppress_non_speech_tokens(true);
params.set_translate(false);
params.set_print_progress(false);
```

### `SamplingStrategy::Greedy { best_of: 1 }`

O Whisper suporta dois modos de decodificação:

- **Greedy:** escolhe o token mais provável a cada passo (rápido)
- **BeamSearch:** explora múltiplos caminhos (mais preciso, mais lento)

Para um sistema de ditado interativo, `Greedy` oferece o melhor
equilíbrio: alta velocidade com qualidade suficiente para fala clara.
`best_of: 1` significa que apenas um candidato é gerado.

### `set_language(Some("pt"))`

Forçar o idioma tem dois benefícios:

1. **Performance:** elimina os ~1.5s de latência do detector de idioma
2. **Precisão:** o modelo usa os pesos especializados em português
   desde o primeiro token, em vez de tentar "descobrir" o idioma

### `set_suppress_non_speech_tokens(true)`

Remove tokens especiais como `[BLANK_AUDIO]`, `[Music]`, `(risos)` etc.
da saída. Para um sistema de ditado, esses tokens são ruído.

### `set_print_progress(false)` e similares

O `whisper.cpp` por padrão escreve progresso no stderr. Em um daemon
de produção, isso polui o journal do systemd com saída não estruturada.
Todos os flags de print são desabilitados; o progresso é registrado
via `tracing` de forma estruturada.

---

## 5.8 Filtragem de artefatos

```rust
const ARTIFACTS: &[&str] = &[
    "[BLANK_AUDIO]", "[blank_audio]",
    "(silêncio)", "(Silêncio)",
    "[Music]", "[music]",
    // ...
];

fn should_skip_segment(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() { return true; }
    for artifact in ARTIFACTS { if t == *artifact { return true; } }
    // Ignora segmentos só com pontuação
    if t.chars().all(|c| !c.is_alphanumeric()) { return true; }
    false
}
```

O Whisper gera esses tokens quando detecta silêncio ou ruído sem fala.
Sem filtragem, eles apareceriam no texto final injetado no campo de texto
do usuário — claramente indesejável.

A comparação por igualdade exata (`t == *artifact`) é intencional: evitar
falsos positivos onde "[Music]" aparece como parte de um título falado.

---

## 5.9 `spawn_blocking`: inferência sem bloquear o runtime Tokio

```rust
// Em daemon/mod.rs:
let result = tokio::task::spawn_blocking(move || {
    Transcriber::transcribe(&model, &inf_config, &audio_buffer)
}).await;
```

O Tokio é um runtime assíncrono cooperativo. Quando uma future é
`await`ed, ela cede o controle do thread ao runtime, que pode executar
outras tasks. **Mas isso só funciona se a future não bloquear o thread.**

Inferência de machine learning é CPU/GPU intensiva e pode levar segundos.
Se rodasse diretamente em uma task Tokio, bloquearia o runtime inteiro —
nenhum outro evento (IPC, sinais) seria processado.

`spawn_blocking` resolve isso: a closure roda em um thread pool separado
(dedicado para tarefas bloqueantes), e o runtime Tokio permanece livre.
O `await` na task principal aguarda o resultado sem bloquear.
