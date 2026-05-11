# 3. IPC e Máquina de Estados

## 3.1 Por que Unix Domain Sockets?

A comunicação entre o cliente (`whisper-dictate toggle`) e o daemon poderia
ser implementada de várias formas. Cada alternativa tem trade-offs:

| Mecanismo | Overhead | Dependências | Complexidade |
|---|---|---|---|
| **Unix Domain Socket** | Mínimo | Zero (kernel) | Baixa |
| D-Bus | Médio | dbus-daemon | Alta |
| gRPC | Alto | tonic + protobuf | Muito alta |
| Sinal UNIX (SIGUSR1) | Zero | Zero | Muito baixa |
| Arquivo de lock | Mínimo | Zero | Muito baixa |

**Por que não SIGUSR1?** Sinais Unix são assíncronos e têm restrições severas
sobre o que pode ser feito no handler (apenas funções async-signal-safe).
Não é possível enviar parâmetros ou receber respostas. Para algo além de
"pare agora", sinais são insuficientes.

**Por que não arquivo de lock?** Não há como receber resposta confirmando
que o daemon processou o comando. Haveria race conditions se o daemon
estivesse em transição de estado.

**Unix Domain Socket** oferece comunicação bidirecional, confirmação de
recebimento, suporte a múltiplos comandos futuros e latência de
microssegundos — tudo isso sem nenhuma dependência de middleware.

---

## 3.2 O protocolo de texto simples

```
Cliente → Daemon      Daemon → Cliente
──────────────────    ─────────────────
"toggle\n"        →   "ok\n"
"stop\n"          →   "ok\n"
"status\n"        →   "idle\n" | "recording\n" | "processing\n"
```

### Por que texto em vez de binário?

Protocolos binários (MessagePack, Protocol Buffers) são mais eficientes
para alto volume. Aqui, o volume é trivial (poucos bytes por interação).

Texto tem vantagens concretas para este caso:
- **Depuração trivial:** `echo "status" | nc -U /run/user/1000/whisper-dictate.sock`
- **Sem código de serialização:** sem structs, sem schemas, sem versioning
- **Legibilidade nos logs:** comandos e respostas aparecem literalmente

### Estrutura de uma conexão

```
1. Cliente abre conexão TCP ao socket
2. Cliente escreve "toggle\n" (uma linha)
3. Daemon lê a linha, processa, escreve "ok\n"
4. Conexão é fechada
```

Cada comando abre e fecha uma conexão independente. Isso simplifica o
gerenciamento de estado e evita conexões zumbis.

---

## 3.3 O servidor IPC com Tokio

```rust
pub async fn start_server(
    socket_path: PathBuf,
    cmd_tx: mpsc::Sender<IpcCommand>,
    state_rx: watch::Receiver<DaemonState>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let listener = UnixListener::bind(&socket_path)?;

    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let tx = cmd_tx.clone();
                    let rx = state_rx.clone();
                    tokio::spawn(handle_connection(stream, tx, rx));
                }
                Err(e) => error!("Erro IPC: {}", e),
            }
        }
    });

    Ok(handle)
}
```

### Anatomia do padrão de servidor assíncrono

**`tokio::spawn` externo:** a task do servidor roda concorrentemente com
o loop principal do daemon. Ambos executam no mesmo runtime Tokio sem
bloquear um ao outro.

**`tokio::spawn` interno (por conexão):** cada cliente recebe sua própria
task. Se o processamento de um cliente demorar (hipotético), os demais
clientes não são bloqueados.

**`cmd_tx.clone()`:** `mpsc::Sender` é barato de clonar — compartilha
internamente o mesmo canal. Cada task de conexão tem seu próprio handle
para enviar ao canal do loop principal.

**`state_rx.clone()`:** `watch::Receiver` é um canal de broadcast onde
todos os leitores sempre veem o valor mais recente. Perfeito para
"estado atual do daemon" — não há fila, apenas o snapshot presente.

---

## 3.4 Diferença entre `mpsc` e `watch`

Tokio oferece vários tipos de canal. A escolha correta depende da semântica:

### `mpsc::channel` (Multi-Producer, Single-Consumer)

```rust
let (cmd_tx, mut cmd_rx) = mpsc::channel::<IpcCommand>(8);
```

Usado para **comandos** (Toggle, Stop). Características:
- Múltiplos produtores podem enviar (várias conexões IPC simultâneas)
- Um único consumidor lê (o loop principal do daemon)
- Mensagens são enfileiradas — nenhuma é perdida
- Capacidade limitada (8): se o loop estiver ocupado e chegarem >8
  comandos sem processamento, o sender bloqueia (backpressure)

### `watch::channel` (Broadcast de valor único)

```rust
let (state_tx, state_rx) = watch::channel(DaemonState::Idle);
```

Usado para **estado** (Idle, Recording, Processing). Características:
- Um único produtor (o loop principal)
- Múltiplos consumidores (cada conexão IPC que recebe `status`)
- Somente o valor mais recente é mantido — sem fila
- Leitores nunca bloqueiam o escritor

A diferença é semântica: comandos precisam ser processados um por um
(fila), enquanto estado é um snapshot (valor atual).

---

## 3.5 A máquina de estados

```
         Toggle                Toggle / Stop
┌────────┐ ────────► ┌───────────┐ ──────────────► ┌────────────┐
│  Idle  │           │ Recording │                  │ Processing │
└────────┘ ◄──────── └───────────┘                  └────────────┘
               ▲                                          │
               └──────────────────────────────────────────┘
                           inferência concluída
```

### Por que uma máquina de estados explícita?

Sem máquina de estados, o código seria uma série de flags booleanas:
`is_recording`, `is_processing`, etc. Esse padrão leva a estados
inconsistentes: `is_recording = true && is_processing = true`? O que
isso significa?

Com estados mutuamente exclusivos, a lógica é inequívoca:

```rust
match (cmd, current_state) {
    (Toggle, Idle)       => start_recording(),
    (Toggle, Recording)  => stop_recording(),
    (Toggle, Processing) => { /* ignora */ }
    (Stop,   Recording)  => stop_recording(),
    (Stop,   Processing) => force_idle(),
    _ => {}
}
```

Cada combinação (comando, estado) tem um comportamento definido. Não há
estado não tratado. O compilador Rust verifica exaustividade do `match`.

### `watch::Sender<DaemonState>` como fonte de verdade

O estado é armazenado em `watch::Sender` (no loop principal) e lido
via `watch::Receiver` (nas tasks IPC). Isso garante:

- **Consistência:** o estado só muda através de `state_tx.send()`
- **Visibilidade:** qualquer task pode ler o estado atual com `*state_rx.borrow()`
- **Thread-safety:** o `watch` é internamente sincronizado — sem `Mutex`

---

## 3.6 O modo cliente (`send_command`)

```rust
pub async fn send_command(socket_path: &PathBuf, command: &str) -> anyhow::Result<String> {
    let mut stream = UnixStream::connect(socket_path).await
        .map_err(|_| anyhow::anyhow!(
            "Não foi possível conectar ao daemon. \
             Verifique: systemctl --user status whisper-dictate"
        ))?;

    stream.write_all(format!("{}\n", command).as_bytes()).await?;

    let mut reader = BufReader::new(&mut stream);
    let mut response = String::new();
    reader.read_line(&mut response).await?;

    Ok(response.trim().to_string())
}
```

O cliente é intencionalmente simples: conecta, escreve, lê, encerra.
O erro de conexão tem uma mensagem de diagnóstico acionável — em vez de
"connection refused", o usuário vê exatamente o comando para diagnosticar.

### Por que `BufReader` para leitura?

`read_line` lê até encontrar `\n`. Sem `BufReader`, seria necessário
implementar a lógica de acumulação de bytes manualmente. `BufReader`
abstrai isso com buffer interno eficiente.

---

## 3.7 Limpeza do socket ao encerrar

```rust
// Na inicialização: remove socket antigo (crash anterior)
if socket_path.exists() {
    std::fs::remove_file(&socket_path)?;
    warn!("Socket anterior removido: {}", socket_path.display());
}

// No shutdown (SIGTERM):
if socket_path.exists() {
    let _ = std::fs::remove_file(&socket_path);
}
```

Sockets Unix Domain **persistem no sistema de arquivos** após o processo
encerrar. Se o daemon crashar sem cleanup, o socket antigo permanece.
Na próxima inicialização, `bind()` falharia com "Address already in use".

A solução é remover o socket no início (se existir) e novamente no
shutdown limpo. O `let _ =` no shutdown ignora erros de remoção
(o socket pode já ter sido removido por outra razão) — correto aqui
porque estamos encerrando de qualquer forma.
