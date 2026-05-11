# 6. Saída via Protocolo Wayland Nativo

## 6.1 Por que não usar ferramentas externas?

A decisão de implementar os protocolos Wayland diretamente em vez de
usar `wtype` (injeção de teclado) e `wl-copy` (clipboard) foi motivada
pelo princípio de **zero dependências de runtime**:

| Abordagem | Dependências | Overhead por uso | Limitações |
|---|---|---|---|
| `wtype` + `wl-copy` | 2 binários externos | fork + exec por chamada | Unicode parcial no `wtype` |
| Protocolo nativo | Zero | Conexão já estabelecida | Mais código |

Com ferramentas externas, cada injeção de texto exigiria:
```
fork() → exec("wtype", "--", text) → wait() → exit
```
Para textos longos com muitas chamadas, o overhead acumula. Com
protocolo nativo, a conexão Wayland é estabelecida uma vez na
inicialização e reutilizada em todas as transcrições.

---

## 6.2 O protocolo Wayland: fundamentos

Wayland é um protocolo de comunicação entre compositors (o servidor,
ex: Niri) e clientes (as aplicações). Diferente do X11, onde um único
servidor gerencia tudo, no Wayland cada aplicação se comunica
diretamente com o compositor via um socket Unix dedicado.

```
/run/user/$UID/wayland-0   (socket do compositor)
       │
       ├── kitty ←──────── conexão da aplicação terminal
       ├── firefox ◄─────── conexão do navegador
       └── whisper-dictate ◄─ nossa conexão (virtual keyboard)
```

### Protocolo de mensagens

O Wayland usa um protocolo binário de mensagens. Cada objeto tem um
`id` numérico e recebe **requests** (cliente → compositor) e emite
**events** (compositor → cliente). A crate `wayland-client` abstrai
esse protocolo binário em traits Rust.

---

## 6.3 Protocolos de extensão: unstable vs. stable

O ecossistema Wayland tem protocolos "core" (sempre disponíveis) e
protocolos de extensão. Os dois que usamos são "unstable" — ainda em
desenvolvimento, mas amplamente suportados:

```
wayland-protocols/unstable/
  virtual-keyboard-unstable-v1.xml    → zwp_virtual_keyboard_manager_v1
  primary-selection-unstable-v1.xml   → zwp_primary_selection_device_manager_v1
```

"Unstable" não significa instável para uso — significa que a API pode
mudar em versões futuras do protocolo. Na prática, esses protocolos
são suportados há anos por todos os compositors wlroots.

A crate `wayland-protocols` gera código Rust a partir dos XMLs do
protocolo em tempo de compilação via `wayland-scanner`.

---

## 6.4 Teclado virtual: `zwp_virtual_keyboard_v1`

### Visão geral do protocolo

```
Cliente                              Compositor (Niri)
   │                                      │
   │── get_registry ──────────────────────►│
   │◄── global(zwp_virtual_keyboard_manager_v1) ──│
   │                                      │
   │── create_virtual_keyboard(seat) ─────►│
   │◄── (ok, keyboard object criado) ─────│
   │                                      │
   │── keyboard.keymap(XKB_V1, fd, size) ─►│  define o mapeamento
   │── keyboard.key(time, code, pressed) ──►│  pressiona tecla
   │── keyboard.key(time, code, released) ─►│  solta tecla
   │                                      │
   │  (o compositor injeta o caractere na aplicação com foco)
```

### O desafio do Unicode

Um teclado físico tem teclas fixas. O protocolo de teclado virtual usa
os mesmos mecanismos: envia um código de tecla (`keycode`) que é
mapeado para um caractere via **keymap XKB**.

Para digitar texto arbitrário (incluindo `ã`, `ç`, `é`, termos técnicos
em qualquer idioma), precisamos de uma forma de mapear qualquer codepoint
Unicode para um `keycode`.

**Nossa solução:** reutilizar sempre o mesmo `keycode` (30, mapeado para
`KEY_A` no evdev) e trocar o keymap XKB antes de cada caractere:

```
Para digitar 'ã' (U+00E3):
  1. Gera keymap: <INJECT> → keysym 0x00e3
  2. Envia keymap ao compositor
  3. Envia key(30, press) + key(30, release)
  4. Compositor: tecla 30 + keymap atual = 'ã' ✓

Para digitar 'n' (U+006E):
  1. Gera keymap: <INJECT> → keysym 0x006e
  2. Envia keymap ao compositor
  3. Envia key(30, press) + key(30, release)
  4. Compositor: tecla 30 + keymap atual = 'n' ✓
```

---

## 6.5 Keymaps XKB dinâmicos

O XKB (X Keyboard Extension) é o sistema de mapeamento de teclas
usado pelo Wayland. Um keymap XKB é um arquivo de texto com sintaxe
própria descrevendo todos os mapeamentos de teclas.

### O keymap mínimo que geramos

```
xkb_keymap {
    xkb_keycodes "inject" {
        minimum = 8;
        maximum = 255;
        <INJECT> = 38;    ← código 30 (evdev) + 8 (offset XKB)
    };
    xkb_types "inject" {
        include "complete"
    };
    xkb_compatibility "inject" {
        include "complete"
    };
    xkb_symbols "inject" {
        key <INJECT> { [ 0x000000e3 ] };  ← keysym para 'ã'
    };
};
```

### O offset +8 do XKB

Este é um detalhe histórico crucial: o XKB numera teclas com um offset
de +8 em relação aos códigos evdev/Linux.

- Código evdev de `KEY_A`: 30
- Código XKB de `<INJECT>`: 30 + 8 = 38

Sem esse offset, o compositor ignora os eventos de tecla silenciosamente
— um bug difícil de diagnosticar.

### Keysyms Unicode

O XKB tem dois espaços de keysyms:
- Keysyms "legados" (Latin-1, 0x00–0xFF): o keysym é igual ao codepoint
- Keysyms Unicode (acima de U+00FF): `0x01000000 + codepoint`

```rust
let keysym = if codepoint <= 0x00ff {
    codepoint                    // 'a' = 0x61, 'ã' = 0xe3
} else {
    0x0100_0000 + codepoint     // '中' = 0x01004e2d
};
```

Isso cobre todo o Unicode: português, emojis, CJK, etc.

---

## 6.6 `memfd_create`: keymap em RAM pura

O protocolo Wayland requer que o keymap seja passado como um file
descriptor. A abordagem padrão seria criar um arquivo temporário em
`/tmp`. Mas isso viola o princípio de zero escrita em disco.

`memfd_create` é uma syscall Linux que cria um arquivo anônimo
**exclusivamente em memória RAM** — sem inode, sem diretório, sem
possibilidade de aparação em disco:

```rust
fn create_memfd(name: &str, size: usize) -> anyhow::Result<RawFd> {
    let c_name = CString::new(name).unwrap();

    // SAFETY: chamada syscall válida com argumento bem formado
    let fd = unsafe {
        libc::memfd_create(c_name.as_ptr(), libc::MFD_CLOEXEC)
    };

    // MFD_CLOEXEC: o fd é fechado automaticamente em exec()
    // → não vaza para processos filhos
    ...
}
```

O fluxo completo:
```
1. memfd_create("xkb-keymap", MFD_CLOEXEC)  → fd em RAM
2. write_all(keymap_text)                   → escreve no fd
3. keyboard.keymap(XKB_V1, fd, size)        → passa ao compositor
4. fd é transferido (OwnedFd) → compositor fecha quando não precisar
```

Sem arquivo em `/tmp`, sem traces em disco, sem necessidade de limpeza.

---

## 6.7 Seleção primária: `zwp_primary_selection_v1`

### O que é a seleção primária?

No Unix, existe uma distinção entre dois buffers de texto:

- **Clipboard** (`Ctrl+C` / `Ctrl+V`): explícito, persiste
- **Seleção primária** (selecionar texto → colar com botão do meio):
  implícito, efêmero

Para um sistema de ditado, a seleção primária é mais útil: o usuário
pode colar o texto transcrito com o botão do meio do mouse imediatamente,
sem precisar de `Ctrl+V`.

### O modelo de "dono" da seleção

No Wayland, a seleção primária funciona com o modelo produtor/consumidor:

```
Compositor                    Nosso processo
     │                             │
     │◄── set_selection(source) ───│  "agora sou o dono do texto X"
     │                             │
     │  [usuário clica botão do meio em outra janela]
     │                             │
     │── send(mime, fd) ──────────►│  "me dê o texto no formato MIME"
     │◄── write(fd, text) ─────────│  escrevemos o texto no fd
     │                             │
     │  [outra janela recebe o texto]
     │                             │
     │── cancelled ───────────────►│  "outro processo assumiu a seleção"
     │                             │  (podemos encerrar a thread)
```

A thread dedicada para a seleção primária é necessária porque o processo
deve estar disponível para responder ao evento `send` enquanto for o
dono da seleção. Quando o usuário seleciona texto em outra janela,
recebemos `cancelled` e a thread encerra.

### Thread por seleção + `SELECTION_THREAD`

```rust
static SELECTION_THREAD: Mutex<Option<thread::JoinHandle<()>>> = Mutex::new(None);

pub fn set_primary_selection(text: &str) -> anyhow::Result<()> {
    // Substitui a thread anterior (se existir)
    if let Ok(mut guard) = SELECTION_THREAD.lock() {
        if let Some(prev) = guard.take() {
            drop(prev); // thread anterior será cancelada pelo compositor
        }
    }

    let handle = thread::spawn(move || {
        run_selection_owner(text)
    });

    *SELECTION_THREAD.lock().unwrap() = Some(handle);
    Ok(())
}
```

`static Mutex<Option<JoinHandle>>` é o padrão Rust para "estado global
com exclusão mútua". Sem `unsafe`, sem variáveis globais mutáveis diretas.

---

## 6.8 O padrão `Dispatch` do wayland-client

A crate `wayland-client` usa um padrão de despacho de eventos baseado
em traits:

```rust
impl Dispatch<ZwpPrimarySelectionSourceV1, ()> for SelectionState {
    fn event(
        state: &mut Self,
        _source: &ZwpPrimarySelectionSourceV1,
        event: zwp_primary_selection_source_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            Event::Send { mime_type: _, fd } => serve_text(&state.text, fd),
            Event::Cancelled => state.done = true,
            _ => {}
        }
    }
}
```

Cada tipo de objeto Wayland que queremos usar requer um `impl Dispatch<Tipo, ()>`.
A maioria das implementações são vazias (eventos que não nos interessam),
mas o compilador **exige** que todas sejam implementadas — garantindo que
nenhum evento seja silenciosamente ignorado por acidente.

O tipo unitário `()` no segundo parâmetro é para "user data" — dados
extras associados ao objeto. Não precisamos de dados extras, então
usamos `()`.

---

## 6.9 Conexões Wayland independentes por módulo

O `injector.rs` e o `clipboard.rs` estabelecem conexões Wayland
independentes:

```rust
// injector.rs
let conn = Connection::connect_to_env()?;  // conexão 1

// clipboard.rs (em thread separada)
let conn = Connection::connect_to_env()?;  // conexão 2
```

Isso é intencional: cada protocolo tem seu próprio ciclo de vida e
thread de event loop. Compartilhar uma única conexão exigiria
sincronização adicional entre o injector (síncrono, chamado do daemon)
e a thread de seleção (roda independentemente até `cancelled`).

Conexões Wayland são baratas — são simplesmente sockets Unix.
