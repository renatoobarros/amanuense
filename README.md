# whisper-dictate

Daemon de ditado por voz para Wayland. Transcreve fala em texto via
Whisper.cpp (GPU), injeta no campo com foco via teclado virtual nativo
e coloca na seleção primária — tudo em memória, sem tocar o disco.

---

## Requisitos de sistema

| Dependência | Versão mínima | Finalidade |
|---|---|---|
| Rust | 1.75+ | Compilação |
| CUDA Toolkit | 12.x | Inferência na GPU |
| libclang / clang | qualquer | Build do whisper-rs (bindgen) |
| pkg-config | qualquer | Detecção de libs do sistema |
| PipeWire | qualquer | Captura de áudio |

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
mkdir -p ~/.config/whisper-dictate
cp config.toml ~/.config/whisper-dictate/config.toml
```

Edite conforme necessário:

```bash
$EDITOR ~/.config/whisper-dictate/config.toml
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
whisper-dictate list-devices
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

### Ajuste pontual no transcriber.rs (n_threads)

Antes de compilar, abra `src/daemon/transcriber.rs` e localize a linha:

```rust
params.set_n_threads(config.n_past_tokens.max(1));
```

Substitua por:

```rust
params.set_n_threads(4); // ajuste para o número de threads desejado
```

> Isso será refatorado em uma versão futura quando o campo `n_threads`
> for adicionado ao `InferenceConfig`. Por ora, edite diretamente.

---

## 4. Instalação do binário

```bash
# Copia o binário compilado para ~/.cargo/bin (já está no PATH)
cp target/release/whisper-dictate ~/.cargo/bin/

# Verifica a instalação
whisper-dictate --version
```

---

## 5. Serviço systemd do usuário

```bash
# Cria o diretório de serviços do usuário (se não existir)
mkdir -p ~/.config/systemd/user/

# Copia a unit
cp whisper-dictate.service ~/.config/systemd/user/

# Recarrega o systemd do usuário
systemctl --user daemon-reload

# Habilita para iniciar junto com a sessão gráfica
systemctl --user enable whisper-dictate.service

# Inicia imediatamente (sem precisar reiniciar)
systemctl --user start whisper-dictate.service

# Verifica o status
systemctl --user status whisper-dictate.service
```

Saída esperada no status (após ~3-5s para o modelo carregar):

```
● whisper-dictate.service - Whisper Dictation Daemon
     Loaded: loaded (~/.config/systemd/user/whisper-dictate.service; enabled)
     Active: active (running)
```

---

## 6. Atalho no Niri

Adicione ao `~/.config/niri/config.kdl`:

```kdl
binds {
    // Pressione uma vez para iniciar, uma vez para finalizar e transcrever
    Mod+Alt+R { spawn "whisper-dictate" "toggle"; }

    // Opcional: parada forçada sem transcrever
    Mod+Alt+Shift+R { spawn "whisper-dictate" "stop"; }
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
RUST_LOG=whisper_dictate=info systemctl --user restart whisper-dictate
journalctl --user -u whisper-dictate -f

# Para debug completo (muito verboso)
RUST_LOG=debug systemctl --user restart whisper-dictate
journalctl --user -u whisper-dictate -f
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
whisper-dictate list-devices
```

**Notificação não aparece**
Verifique se há um daemon de notificações ativo (dunst, mako, etc.):
```bash
systemctl --user status mako   # ou dunst
```

### Verificar estado do daemon

```bash
whisper-dictate status
# Saída: idle | recording | processing
```

---

## 9. Estrutura do projeto (referência)

```
whisper-dictate/
├── Cargo.toml                        Dependências e perfil de release
├── config.toml                       Configuração de exemplo (copiar para ~/.config/)
├── whisper-dictate.service           Unit systemd do usuário
└── src/
    ├── main.rs                       Entry point e subcomandos CLI
    ├── config.rs                     Leitura e validação do config.toml
    ├── daemon/
    │   ├── mod.rs                    Loop principal e máquina de estados
    │   ├── ipc.rs                    Servidor Unix Domain Socket
    │   ├── audio.rs                  Captura de áudio via cpal/PipeWire
    │   ├── model.rs                  Carregamento do modelo na VRAM
    │   └── transcriber.rs            Inferência com segmentação para áudio longo
    └── output/
        ├── mod.rs                    Declaração dos submódulos
        ├── injector.rs               Teclado virtual via protocolo Wayland nativo
        └── clipboard.rs              Seleção primária via protocolo Wayland nativo
```
