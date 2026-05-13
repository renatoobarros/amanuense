# 1. Visão Geral e Arquitetura

## 1.1 O problema

Ferramentas de ditado por voz existem há décadas, mas quase todas compartilham
dois problemas fundamentais para um engenheiro que trabalha com dados sensíveis:
**enviam áudio para servidores externos** e **requerem dependências pesadas**.

O `amanuense` nasce de restrições específicas:

- Inferência **100% local** — nenhum byte de áudio sai da máquina
- **Zero gravação em disco** — nem durante, nem após a transcrição
- **VRAM residente** — o modelo carrega uma vez e fica na GPU, sem latência de inicialização
- **Integração nativa com Wayland** — sem ferramentas externas, sem X11
- **Configurável sem recompilação** — arquivo TOML editável em produção
- **Mínimo de dependências** — cada crate no projeto tem um propósito único e insubstituível

---

## 1.2 Hardware de referência

O projeto foi desenhado para o seguinte ambiente, mas funciona em qualquer
máquina com GPU CUDA e compositor Wayland wlroots:

```
CPU:  AMD Ryzen 9 7900X (24 threads) @ 5.65 GHz
GPU:  ASUS RTX 5060 Ti 16GB DDR7 (Discreta)
GPU:  AMD Raphael (Integrada — não usada para inferência)
RAM:  64GB DDR5 5600MHz
OS:   Arch Linux, kernel Zen
WM:   Niri 25.11 (Wayland, wlroots)
```

O modelo `ggml-large-v3-turbo-q5_0.bin` (~547MB) cabe confortavelmente na
VRAM da RTX 5060 Ti. Com 16GB disponíveis, sobra memória para o contexto
de inferência mesmo em transcrições longas.

---

## 1.3 Modelo de execução: daemon + cliente no mesmo binário

A decisão mais importante de arquitetura foi unificar daemon e cliente
em **um único binário** com subcomandos:

```
amanuense daemon        → processo longo, carrega modelo, escuta socket
amanuense toggle        → processo curto, conecta ao socket, envia "toggle"
amanuense stop          → processo curto, conecta ao socket, envia "stop"
amanuense status        → processo curto, consulta estado
amanuense list-devices  → lista dispositivos de áudio localmente
```

### Por que não dois binários separados?

**Distribuição simplificada:** um único arquivo copiado para `~/.cargo/bin/`.
Sem sincronização de versões entre binários.

**Contexto de configuração compartilhado:** tanto o daemon quanto o cliente
precisam saber onde está o socket IPC (definido no `config.toml`). Com um
binário único, ambos carregam a mesma lógica de resolução de caminho.

**Precedente no ecossistema:** essa é a mesma abordagem usada por `git`
(um binário, muitos subcomandos), `systemctl`, `cargo` e a maioria das
ferramentas Unix modernas.

---

## 1.4 Arquitetura em camadas

```
┌─────────────────────────────────────────────────────────────────┐
│                        USUÁRIO / NIRI                           │
│   Mod+Alt+R → amanuense toggle                            │
└──────────────────────────┬──────────────────────────────────────┘
                           │ Unix Domain Socket
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│                    CAMADA DE CONTROLE (IPC)                     │
│   daemon/ipc.rs                                                 │
│   UnixListener → recebe "toggle\n" → envia IpcCommand::Toggle   │
└──────────────────────────┬──────────────────────────────────────┘
                           │ mpsc::Sender<IpcCommand>
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│                  CAMADA DE ORQUESTRAÇÃO (DAEMON)                │
│   daemon/mod.rs                                                 │
│   Máquina de estados: Idle → Recording → Processing → Idle      │
└──────┬───────────────────┬───────────────────────────┬──────────┘
       │                   │                           │
       ▼                   ▼                           ▼
┌─────────────┐   ┌─────────────────┐   ┌─────────────────────────┐
│ CAPTURA     │   │  INFERÊNCIA     │   │  ENTREGA                │
│ audio.rs    │   │  model.rs       │   │  injector.rs            │
│             │   │  transcriber.rs │   │  clipboard.rs           │
│ cpal        │   │  whisper-rs     │   │  zwp_virtual_keyboard   │
│ PipeWire    │   │  CUDA / VRAM    │   │  zwp_primary_selection  │
└──────┬──────┘   └────────┬────────┘   └─────────────────────────┘
       │                   │
       │  Vec<f32>         │  String
       │  (em memória)     │  (em memória)
       └───────────────────┘
              LGPD: nenhum dado toca o disco
```

---

## 1.5 Fluxo de execução detalhado

### Inicialização (uma vez, na subida do sistema)

```
systemd --user
  └─ ExecStart: amanuense daemon
       ├─ Config::load()              lê ~/.config/amanuense/config.toml
       ├─ WhisperModel::load()        carrega ggml-*.bin na VRAM (~3-5s)
       ├─ TextInjector::new()         conecta ao Wayland, cria teclado virtual
       ├─ ipc::start_server()         cria /run/user/$UID/amanuense.sock
       └─ main_loop()                 bloqueia em tokio::select!, estado: Idle
```

### Ciclo de gravação (a cada uso)

```
[Mod+Alt+R]
  └─ amanuense toggle
       └─ ipc::send_command("toggle")
            └─ daemon recebe IpcCommand::Toggle
                 ├─ AudioCapture::record_to_completion() em spawn_blocking
                 ├─ Microfone aberto (cpal)
                 ├─ notify_send("🎙️ Gravando...")
                 └─ estado: Recording

[Mod+Alt+R novamente]
  └─ amanuense toggle
       └─ daemon recebe IpcCommand::Toggle
            ├─ AudioCapture::signal_stop()    flag atômica → callback para
            ├─ audio_buffer → canal mpsc      Vec<f32> entregue ao loop
            ├─ notificação de início fechada
            └─ estado: Processing

[Inferência]
  └─ tokio::task::spawn_blocking
       └─ Transcriber::transcribe()
            ├─ segmenta áudio se > 28s
            ├─ whisper-rs::full() por segmento (CUDA)
            ├─ consolida texto
            └─ retorna String

[Entrega]
  ├─ TextInjector::type_text()        digita no campo com foco
  ├─ set_primary_selection()          coloca na seleção primária
  ├─ notify_send("✅ Transcrição")    preview do texto na notificação
  └─ estado: Idle (modelo permanece na VRAM)
```

---

## 1.6 Princípios de design

### Princípio 1: Cada crate tem exatamente uma razão de estar no projeto

Antes de adicionar uma dependência, a pergunta é: _"Posso implementar
isso em menos código do que seria necessário para usar a crate?"_

Se sim, implementamos. Se não, usamos a crate. Essa disciplina mantém
o grafo de dependências auditável e o tempo de compilação controlado.

| Crate            | Justificativa para inclusão                                                              |
| ---------------- | ---------------------------------------------------------------------------------------- |
| `whisper-rs`     | ~50.000 linhas de C++ de whisper.cpp — inviável reimplementar                            |
| `cpal`           | Abstração cross-platform sobre PipeWire/ALSA — ~5.000 linhas de código de plataforma     |
| `tokio`          | Runtime assíncrono industrial — `async/await` sem Tokio exigiria implementar um executor |
| `wayland-client` | Bindings do protocolo Wayland — o protocolo em si é complexo e bem especificado          |
| `serde + toml`   | Desserialização type-safe — implementar um parser TOML do zero não faz sentido           |
| `clap`           | Parse de CLI com help automático — justificável pelo conforto operacional                |
| `notify-rust`    | D-Bus é complexo — a crate abstrai o protocolo inteiro                                   |
| `tracing`        | Observabilidade estruturada — superior ao `println!` em produção                         |
| `dirs`           | Resolução XDG cross-platform — 10 linhas que evitam bugs de caminho                      |
| `anyhow`         | Propagação de erros ergonômica — elimina boilerplate de `Box<dyn Error>`                 |
| `libc`           | `memfd_create` sem unsafe manual — acesso a syscalls Linux                               |

### Princípio 2: Falhas são locais, nunca globais

Cada módulo retorna `anyhow::Result<T>`. Erros se propagam com contexto
via `?` e são tratados no nível que tem informação suficiente para
decidir o que fazer. Nenhum `panic!` em código de produção (apenas em
inicialização, onde falhar cedo é correto).

### Princípio 3: O modelo nunca é descarregado

Carregar o `large-v3-turbo` na VRAM demora ~3-5 segundos. Descarregar
e recarregar a cada uso tornaria a ferramenta inutilizável. O daemon
mantém o modelo residente e apenas abre/fecha o microfone conforme
necessário — o overhead de cada uso é apenas o tempo de inferência.

### Princípio 4: Sem estado em disco

Nenhum módulo do projeto chama `std::fs::write` ou equivalente com
dados do usuário. Logs vão para o journal do systemd (stdout/stderr),
não para arquivos. A configuração é somente leitura.
