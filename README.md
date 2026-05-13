Amanuense

Daemon de ditado por voz para Wayland. Transcreve fala em texto via
Whisper.cpp (GPU), injeta no campo com foco via teclado virtual nativo
e coloca na seleção primária — tudo em memória, sem tocar o disco.

---

## Requisitos de sistema

| Dependência      | Versão mínima | Finalidade                    |
| ---------------- | ------------- | ----------------------------- |
| Rust             | 1.75+         | Compilação                    |
| CUDA Toolkit     | 12.x          | Inferência na GPU             |
| libclang / clang | qualquer      | Build do whisper-rs (bindgen) |
| pkg-config       | qualquer      | Detecção de libs do sistema   |
| PipeWire         | qualquer      | Captura de áudio              |

```bash
# Arch Linux — instala todas as dependências de build
sudo pacman -S rust cuda clang pkg-config pipewire
```

---

## 1. Preparação do modelo

Você já fez o download. Mova o modelo para o caminho padrão:

```bash
mkdir -p ~/.local/share/whisper
mv ~/Downloads/ggml-large-v3-turbo-q5_0.bin ~/.local/share/whisper/
```

Verifique a integridade (opcional):

```bash
ls -lh ~/.local/share/whisper/ggml-large-v3-turbo-q5_0.bin
# Deve ser ~547 MB
```

---

## 2. Configuração

Crie o diretório e copie o arquivo de configuração:

```bash
mkdir -p ~/.config/amanuense
cp config.toml ~/.config/amanuense/config.toml
```

Edite conforme necessário:

```bash
$EDITOR ~/.config/amanuense/config.toml
```

Os campos mais importantes para o seu setup:

```toml
[model]
path = "~/.local/share/whisper/ggml-large-v3-turbo-q5_0.bin"
language = "pt"
use_gpu = true
gpu_device = 0      # RTX 5060 Ti

[audio]
device = "default"  # ou o nome exato do seu microfone
```

Para ver os dispositivos de áudio disponíveis após a instalação:

```bash
amanuense list-devices
```

---

## 3. Compilação com suporte a CUDA

O `whisper-rs` usa `whisper.cpp` via bindgen. O suporte a CUDA é ativado
por variáveis de ambiente em tempo de compilação — **não requer mudança
no código ou no Cargo.toml**:

```bash
# No diretório raiz do projeto:
WHISPER_CUDA=1 cargo build --release
```

> **Por que `WHISPER_CUDA=1`?**
> O build script do `whisper-rs` verifica esta variável para ativar o
> backend CUDA do `whisper.cpp`. Sem ela, a inferência roda na CPU.

Se o CUDA Toolkit estiver em caminho não padrão, adicione:

```bash
WHISPER_CUDA=1 \
CUDA_PATH=/opt/cuda \
cargo build --release
```

A primeira compilação baixa e compila o `whisper.cpp` embutido —
pode levar de 2 a 5 minutos. Compilações subsequentes são incrementais.

### Ajuste de threads sem editar código

O número de threads é configurado em runtime no `config.toml`:

```toml
[model]
n_threads = 4
```

---

## 4. Instalação do binário

```bash
# Copia o binário compilado para o caminho usado pela unit systemd
mkdir -p ~/.local/bin
cp target/release/amanuense ~/.local/bin/

# Verifica a instalação
~/.local/bin/amanuense --version
```

---

## 5. Serviço systemd do usuário

```bash
# Cria o diretório de serviços do usuário (se não existir)
mkdir -p ~/.config/systemd/user/

# Copia as units
cp systemd/amanuense.service systemd/amanuense.path systemd/amanuense-restart.service ~/.config/systemd/user/

# Recarrega o systemd do usuário
systemctl --user daemon-reload

# Habilita o daemon e o watcher de configuração
systemctl --user enable amanuense.service amanuense.path

# Inicia imediatamente (sem precisar reiniciar)
systemctl --user start amanuense.service amanuense.path

# Verifica o status
systemctl --user status amanuense.service amanuense.path
```

Saída esperada no status (após ~3-5s para o modelo carregar):

```
● amanuense.service - Whisper Dictation Daemon
     Loaded: loaded (~/.config/systemd/user/amanuense.service; enabled)
     Active: active (running)
```

Com essa configuração, qualquer alteração em
`~/.config/amanuense/config.toml` dispara um `try-restart` automático do
daemon para aplicar os novos valores.

---

## 6. Atalho no Niri

Adicione ao `~/.config/niri/config.kdl`:

```kdl
binds {
    // Pressione uma vez para iniciar, uma vez para finalizar e transcrever
    Mod+Alt+R { spawn "amanuense" "toggle"; }

    // Opcional: parada forçada sem transcrever
    Mod+Alt+Shift+R { spawn "amanuense" "stop"; }
}
```

Recarregue a configuração do Niri:

```bash
niri msg action reload-config
```

---

## 7. Verificação do fluxo completo

1. Posicione o cursor em qualquer campo de texto (terminal, editor, navegador)
2. Pressione `Mod+Alt+R` — aparece a notificação "🎙️ Gravando..."
3. Fale normalmente em português
4. Pressione `Mod+Alt+R` novamente — a notificação fecha
5. Após alguns segundos (inferência), o texto aparece no campo e na seleção primária

---

## 8. Diagnóstico

### Ver logs em tempo real

```bash
# Logs do daemon (modo verbose)
RUST_LOG=whisper_dictate=info systemctl --user restart amanuense
journalctl --user -u amanuense -f

# Para debug completo (muito verboso)
RUST_LOG=debug systemctl --user restart amanuense
journalctl --user -u amanuense -f
```

### Problemas comuns

**"zwp_virtual_keyboard_manager_v1 não encontrado"**
O Niri precisa da configuração `input virtual-keyboard`. Verifique se
a versão do Niri é 25.x+ e se o protocolo está habilitado.

**"Falha ao carregar modelo Whisper: CUDA"**
Verifique se compilou com `WHISPER_CUDA=1` e se o driver NVIDIA está ativo:

```bash
nvidia-smi
```

**"Dispositivo de áudio não encontrado"**
Liste os dispositivos disponíveis e atualize o config.toml:

```bash
amanuense list-devices
```

**Notificação não aparece**
Verifique se há um daemon de notificações ativo (dunst, mako, etc.):

```bash
systemctl --user status mako   # ou dunst
```

### Verificar estado do daemon

```bash
amanuense status
# Saída: idle | recording | processing
```

---

## 9. Estrutura do projeto (referência)

```
amanuense/
├── Cargo.toml                        Dependências e perfil de release
├── config.toml                       Configuração de exemplo (copiar para ~/.config/)
├── systemd/
│   ├── amanuense.service       Unit systemd do usuário
│   ├── amanuense.path          Watcher systemd para mudanças no config.toml
│   └── amanuense-restart.service Reinício automático após mudança de config
└── src/
    ├── main.rs                       Entry point e subcomandos CLI
    ├── config.rs                     Leitura e validação do config.toml
    ├── daemon.rs                     Orquestração dos submódulos do daemon
    ├── daemon/
    │   ├── runtime.rs                Loop principal e máquina de estados
    │   ├── state_machine.rs          Transições e ações de estado
    │   ├── notifications.rs          Notificações de início/conclusão
    │   ├── shutdown.rs               Handler de SIGTERM/SIGINT
    │   ├── ipc.rs                    Servidor Unix Domain Socket
    │   ├── audio.rs                  Captura e coordenação de áudio
    │   ├── audio/
    │   │   ├── device.rs             Seleção/negociação de dispositivo e formato
    │   │   ├── stream.rs             Callback/stream de captura cpal
    │   │   └── dsp.rs                Mixdown e resample
    │   ├── model.rs                  Carregamento do modelo na VRAM
    │   ├── transcriber.rs            Entrada da transcrição
    │   └── transcriber/
    │       ├── segmentation.rs       Segmentação para áudio longo
    │       ├── params.rs             Parâmetros e execução do Whisper
    │       └── postprocess.rs        Filtro de artefatos e deduplicação de overlap
    ├── output.rs                     Declaração dos submódulos
    └── output/
        ├── injector.rs               API do injetor de texto
        ├── injector/
        │   ├── keymap.rs             Geração de keymap XKB
        │   ├── memfd.rs              Envio de keymap via memfd
        │   └── protocol.rs           Estado e eventos Wayland
        └── clipboard.rs              Seleção primária via protocolo Wayland nativo
```
