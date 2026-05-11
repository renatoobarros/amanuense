# whisper-dictate — Documentação Técnica

> Daemon de ditado por voz para Wayland, escrito em Rust.
> Inferência local via Whisper.cpp na GPU, zero disco, conformidade LGPD por design.

---

## Sobre esta documentação

Esta documentação foi escrita com dois objetivos simultâneos: **registrar
as decisões de projeto** do `whisper-dictate` e **servir como material de
aprendizado** para quem quiser entender como sistemas desse tipo são
construídos em Rust.

Cada seção explica não apenas *o que* foi feito, mas *por que* aquela
abordagem foi escolhida em detrimento de alternativas, quais trade-offs
estão envolvidos e quais conceitos fundamentais sustentam cada decisão.

---

## Índice

| # | Documento | Conteúdo |
|---|---|---|
| 1 | [Visão Geral e Arquitetura](./01-visao-geral.md) | Objetivos, restrições, diagrama de arquitetura e princípios de design |
| 2 | [Sistema de Configuração](./02-configuracao.md) | Design do config.toml, validação, resolução de caminhos, extensibilidade |
| 3 | [IPC e Máquina de Estados](./03-ipc-e-estado.md) | Unix Domain Sockets, protocolo de controle, máquina de estados, concorrência com Tokio |
| 4 | [Captura e Processamento de Áudio](./04-audio.md) | Pipeline de áudio, cpal, resample, mixdown, flag atômica, LGPD |
| 5 | [Modelo e Inferência](./05-inferencia.md) | whisper.cpp, gerenciamento de VRAM, segmentação de áudio longo, remoção de overlap |
| 6 | [Saída via Protocolo Wayland Nativo](./06-wayland.md) | zwp_virtual_keyboard_v1, zwp_primary_selection_v1, keymaps XKB dinâmicos, memfd |
| 7 | [Privacidade e Conformidade LGPD](./07-privacidade.md) | Privacidade por design, análise de fluxo de dados, superfície de risco zero |

---

## Mapa de dependências

```
main.rs
  ├── config.rs                  (serde + toml + dirs)
  ├── daemon/
  │   ├── mod.rs                 (tokio + notify-rust)
  │   ├── ipc.rs                 (tokio::net::UnixListener)
  │   ├── audio.rs               (cpal)
  │   ├── model.rs               (whisper-rs)
  │   └── transcriber.rs         (whisper-rs)
  └── output/
      ├── injector.rs            (wayland-client + wayland-protocols + libc)
      └── clipboard.rs           (wayland-client + wayland-protocols)
```

---

## Glossário rápido

| Termo | Significado no contexto |
|---|---|
| **VRAM** | Memória da GPU (RTX 5060 Ti). O modelo Whisper reside aqui permanentemente |
| **GGML** | Formato de arquivo de modelo usado pelo whisper.cpp (quantizado) |
| **q5_0** | Quantização de 5 bits — reduz tamanho e acelera inferência com perda mínima de precisão |
| **Wayland** | Protocolo de display do Linux moderno (substituto do X11) |
| **wlroots** | Biblioteca de compositor Wayland usada pelo Niri, Sway, etc. |
| **XKB** | X Keyboard Extension — sistema de mapeamento de teclas usado pelo Wayland |
| **memfd** | File descriptor em memória RAM pura, sem arquivo em disco |
| **IPC** | Inter-Process Communication — comunicação entre processos |
| **UDS** | Unix Domain Socket — canal de comunicação local via sistema de arquivos |
| **LGPD** | Lei Geral de Proteção de Dados (Lei nº 13.709/2018) |
