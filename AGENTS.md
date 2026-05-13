# AGENTS.md — amanuense

> Para documentação mais completa, consulte `.github/copilot-instructions.md`.

## Build

```bash
# Desenvolvimento (CPU)
cargo build

# Release com CUDA (GPU)
WHISPER_CUDA=1 cargo build --release

# CUDA em caminho não padrão
WHISPER_CUDA=1 CUDA_PATH=/opt/cuda cargo build --release
```

**Importante:** O suporte a CUDA é ativado exclusivamente pela variável `WHISPER_CUDA=1` em tempo de build — não requer mudanças no código ou Cargo.toml.

## Teste e Lint

```bash
cargo test                          # Suite completa
cargo test <nome>                   # Teste específico
cargo test -- --list                # Lista todos os testes
cargo clippy --all-targets          # Lint
cargo fmt --all -- --check          # Format check
```

## Execução e Debug

```bash
# Ver logs do daemon em execução
RUST_LOG=whisper_dictate=info systemctl --user restart amanuense
journalctl --user -u amanuense -f

# Debug completo (muito verboso)
RUST_LOG=debug systemctl --user restart amanuense
```

## Arquitetura

- **Binário único** com subcomandos: `daemon`, `toggle`, `stop`, `status`, `list-devices`
- **Daemon + IPC:** O mesmo binário serve como daemon (long-lived) e cliente (short-lived)
- **IPC via Unix Domain Socket:** protocolo texto com newline (`toggle`, `stop`, `status`)
- **Estado dirigido por transição:** `Idle -> Recording -> Processing -> Idle` em `daemon/runtime.rs`

## Convenções Importantes

- **Idioma:** Comentários, logs e mensagens para o usuário em português
- **Configuração:** Carregada em `src/config.rs`; precedeência: `--config` > XDG default
- **Privacidade:** Sem persistência em disco, sem rede, keymaps via `memfd_create` (RAM)
- **Wayland:** Protocolos nativos (`zwp_virtual_keyboard_v1`, `zwp_primary_selection_v1`) — sem ferramentas externas como `wtype`/`wl-copy`

## Estrutura de Diretórios

```
src/
├── main.rs           # Entry point + CLI
├── config.rs         # Leitura do config.toml
├── daemon.rs         # Orquestração
├── daemon/
│   ├── runtime.rs    # Loop principal + state machine
│   ├── audio.rs      # Captura via cpal
│   ├── model.rs      # Carregamento Whisper
│   └── ipc.rs        # Unix socket server
└── output/
    ├── injector.rs   # Wayland virtual keyboard
    └── clipboard.rs # Seleção primária
```