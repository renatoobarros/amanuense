# 7. Privacidade e Conformidade LGPD

## 7.1 O que diz a LGPD

A Lei Geral de Proteção de Dados (Lei nº 13.709/2018) estabelece princípios
para o tratamento de dados pessoais. Entre eles, o mais relevante para
este projeto é o **princípio da necessidade** (Art. 6º, III):

> *"limitação do tratamento ao mínimo necessário para a realização de suas
> finalidades, com abrangência dos dados pertinentes, proporcionais e não
> excessivos em relação às finalidades do tratamento de dados"*

Dados de voz são **dados biométricos** (Art. 5º, II), categoria especial
que requer proteção adicional (Art. 11). A voz humana é um identificador
único — pode revelar identidade, estado emocional, condição de saúde e
outros atributos sensíveis.

---

## 7.2 Privacidade por design: o princípio

"Privacy by Design" (PbD) é uma abordagem onde a proteção de privacidade
é incorporada na arquitetura do sistema desde o início, não adicionada
como camada posterior.

Os sete princípios fundamentais do PbD (Ann Cavoukian, 1995):

1. **Proativo, não reativo** — prevenir antes de remediar
2. **Privacidade como padrão** — máxima proteção sem ação do usuário
3. **Privacidade embutida no design** — não como add-on
4. **Funcionalidade total** — sem trade-offs desnecessários
5. **Segurança de ponta a ponta** — durante todo o ciclo de vida
6. **Visibilidade e transparência** — auditável
7. **Respeito pelo usuário** — centrado no usuário

O `whisper-dictate` implementa todos os sete por construção.

---

## 7.3 Análise de fluxo de dados

### Mapa completo de dados em trânsito

```
[Microfone]
     │
     │  sinal de áudio (f32, 16kHz, mono)
     │  ← existe apenas como amostras no buffer Vec<f32> em RAM
     ▼
[AudioCapture::record_to_completion]
     │
     │  Vec<f32> via mpsc::Sender (RAM para RAM)
     │  ← nunca sai do processo, nunca toca disco
     ▼
[Transcriber::transcribe]
     │
     │  &[f32] (referência, sem cópia extra)
     │  processamento na GPU (VRAM)
     │  ← VRAM é memória volátil, limpa no próximo uso
     │
     │  → Vec<f32> (audio_buffer) é dropped ao fim de spawn_blocking
     │    Drop determinístico: memória liberada imediatamente
     │
     │  String (texto transcrito)
     │  ← a partir daqui, apenas texto — sem biometria de voz
     ▼
[TextInjector::type_text]          [set_primary_selection]
     │                                      │
     │  keymap via memfd (RAM)              │  texto via socket Wayland
     │  key events via socket Wayland       │  (IPC local, sem rede)
     │  ← IPC local, sem rede              │
     ▼                                      ▼
[Aplicação com foco]              [Seleção primária do usuário]
     │
     │  texto aparece no campo de texto
     │  ← responsabilidade da aplicação a partir daqui
```

### O que NUNCA acontece

| Ação | Status |
|---|---|
| Gravar áudio em arquivo | ❌ Nunca |
| Gravar texto transcrito em arquivo | ❌ Nunca |
| Enviar áudio pela rede | ❌ Nunca |
| Enviar texto pela rede | ❌ Nunca |
| Criar arquivos temporários | ❌ Nunca (memfd em RAM) |
| Manter histórico de transcrições | ❌ Nunca |
| Criar logs com conteúdo de voz/texto | ❌ Nunca (logs contêm apenas metadados: tamanho, duração, timestamps) |

---

## 7.4 O ciclo de vida dos dados de voz

```
┌────────────────────────────────────────────────────┐
│         CICLO DE VIDA DO ÁUDIO CAPTURADO           │
│                                                    │
│  t=0s: Microfone abre, bytes chegam ao callback   │
│         → acumulados em Vec<f32> na RAM            │
│                                                    │
│  t=Xs: Toggle pressionado, captura para           │
│         → Vec<f32> enviado via mpsc (sem cópia)   │
│         → Vec<f32> original dropped imediatamente │
│                                                    │
│  t=Xs+ε: Inferência começa                        │
│         → &[f32] passado por referência ao Whisper │
│         → Whisper processa na VRAM (temporário)    │
│                                                    │
│  t=Ys: Inferência concluída                        │
│         → spawn_blocking closure encerra           │
│         → Vec<f32> dropped (fim do escopo)         │
│         → VRAM reutilizada na próxima inferência   │
│                                                    │
│  RESULTADO: String com texto                       │
│  O áudio de voz não existe mais em memória         │
└────────────────────────────────────────────────────┘
```

### Drop determinístico em Rust

Em linguagens com garbage collector (Java, Python, Go), objetos são
liberados em momento indeterminado — o GC decide quando rodar.

Em Rust, o destrutor (`Drop`) é chamado **imediatamente** quando o
owner sai de escopo:

```rust
{
    let audio_buffer: Vec<f32> = receive_audio();
    // `audio_buffer` existe aqui

    let text = transcribe(&audio_buffer);
    // `audio_buffer` ainda existe aqui

} // ← Drop chamado AQUI, exatamente neste ponto
  // memória liberada, sem GC, sem delay
```

Esta garantia é um dos pilares de segurança do Rust e é crucial para
conformidade com LGPD em sistemas de processamento de dados sensíveis.

---

## 7.5 Dados em logs: o que é registrado

Os logs (via `tracing`, enviados ao journal do systemd) contêm apenas
**metadados operacionais**, nunca conteúdo:

```
# O que APARECE nos logs:
[WARN] Modelo carregado na VRAM com sucesso.
[INFO] Captura iniciada.
[INFO] Captura encerrada: 182400 amostras (11.4s)
[INFO] Inferência concluída: 47 caracteres
[INFO] Daemon de volta ao estado Idle.

# O que NUNCA aparece nos logs:
[INFO] Áudio capturado: [0.001, -0.002, 0.003, ...]  ← NUNCA
[INFO] Texto transcrito: "reunião com o cliente amanhã"  ← NUNCA
```

O número de caracteres transcritos (47) é um metadado operacional que
não revela o conteúdo. O número de amostras (182400) revela apenas a
duração (11.4s), não o que foi dito.

---

## 7.6 VRAM: é memória segura?

A VRAM (memória da GPU) é memória volátil — seu conteúdo é perdido
quando a GPU é reiniciada ou o processo encerra. Mas enquanto o daemon
está ativo, os pesos do modelo e os tensores de inferência residem na VRAM.

**O que reside permanentemente na VRAM:**
- Pesos do modelo Whisper (~547MB) — parâmetros aprendidos no treinamento,
  sem dados do usuário

**O que reside temporariamente na VRAM (durante inferência):**
- Mel spectrogram do áudio (< 1MB por chunk de 28s)
- Tensores de ativação do encoder/decoder (< 100MB)

Após a inferência, esses tensores são reutilizados na próxima chamada —
o `whisper.cpp` não zeriza a memória entre usos (performance). Em
termos práticos, os dados da última inferência permanecem na VRAM até
serem sobrescritos pela próxima.

**Implicação:** em um modelo de ameaça onde um processo malicioso com
acesso direto à VRAM (`/dev/nvidia0`) tenta recuperar dados da última
transcrição, há uma janela de vulnerabilidade. Para a esmagadora maioria
dos casos de uso, isso é irrelevante (requer acesso root ou pertencer
ao grupo `video`).

---

## 7.7 Comparação com alternativas

| Ferramenta | Dados de voz vão para | Conformidade LGPD |
|---|---|---|
| Google Voice Typing | Servidores Google | ❌ Requer DPA, base legal |
| Whisper API (OpenAI) | Servidores OpenAI | ❌ Requer DPA, base legal |
| whisper-dictate | RAM local → GPU local | ✅ Sem transferência |
| Whisper Desktop (local) | Arquivo temporário em disco | ⚠️ Melhor, mas rastros em disco |

O `whisper-dictate` é a única das opções listadas que processa e
descarta os dados de voz inteiramente em memória volátil, sem
transferência de rede e sem persistência em disco.

---

## 7.8 Auditabilidade

Uma característica importante da conformidade LGPD é a capacidade de
demonstrar as práticas adotadas. O código do `whisper-dictate` é:

- **Open source:** qualquer pessoa pode auditar o código
- **Autocontido:** todas as dependências são crates Rust no `crates.io`
  ou C/C++ no `whisper.cpp` — auditáveis
- **Sem dependências de rede:** o `Cargo.toml` não inclui nenhuma crate
  de HTTP client (`reqwest`, `hyper`, `ureq`). A ausência dessas crates
  é auditável.
- **Sem acesso a filesystem para dados:** `grep -r "std::fs::write"
  src/` retorna vazio — nenhum módulo de dados escreve em disco.
