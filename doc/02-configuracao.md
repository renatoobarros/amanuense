# 2. Sistema de Configuração

## 2.1 Por que TOML?

O projeto precisava de um formato de configuração que atendesse a três critérios:

1. **Legível sem treinamento** — o usuário não deve precisar ler documentação
   para entender o arquivo
2. **Comentários suportados** — configurações sem comentários embutidos são
   inutilizáveis em produção
3. **Mapeamento direto para structs Rust** — sem camadas de conversão manual

TOML atende os três. YAML falha no primeiro (indentação sensível a erros).
JSON falha no segundo (sem suporte a comentários). INI falha no terceiro
(sem tipagem nativa).

```toml
# TOML é autoexplicativo
[model]
language = "pt"    # string
use_gpu = true     # booleano
gpu_device = 0     # inteiro
```

---

## 2.2 Mapeamento TOML → Structs Rust com Serde

O `serde` é o ecossistema de serialização/desserialização padrão do Rust.
Ele funciona via **macros procedurais** que geram código de parsing em
tempo de compilação, sem overhead em runtime.

```rust
#[derive(Deserialize, Clone)]
pub struct Config {
    pub model: ModelConfig,
    pub audio: AudioConfig,
    // ...
}
```

O atributo `#[derive(Deserialize)]` instrui o compilador a gerar
automaticamente o código que lê os campos do TOML e os converte para
os tipos Rust corretos. Se um campo no TOML tiver o tipo errado
(ex: `gpu_device = "zero"` em vez de `gpu_device = 0`), o erro é
reportado com localização precisa:

```
Error: invalid type: string "zero", expected i32 at line 8 column 14
```

### Por que derivar `Clone`?

Vários módulos precisam de uma cópia independente da configuração:
- O daemon principal mantém a `Config` original
- A task de áudio (`spawn_blocking`) precisa de `AudioConfig` próprio
- A task de inferência precisa de `InferenceConfig` próprio

Como `spawn_blocking` requer `'static` (os dados devem durar para sempre
do ponto de vista do compilador), não podemos passar referências —
precisamos de ownership. `Clone` resolve isso de forma ergonômica:

```rust
let cfg_audio = config.audio.clone(); // cópia para a task
tokio::task::spawn_blocking(move || {
    AudioCapture::record_to_completion(cfg_audio, audio_tx)
});
```

---

## 2.3 Hierarquia de busca do arquivo de configuração

```rust
pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
    let config_path = match path {
        Some(p) => p.to_path_buf(),           // --config explícito
        None => Self::default_path()?,         // XDG padrão
    };
    // ...
}
```

A ordem de precedência é:

```
1. --config /caminho/explicito.toml    (flag CLI)
2. $XDG_CONFIG_HOME/whisper-dictate/config.toml
3. ~/.config/whisper-dictate/config.toml
```

Isso segue o padrão XDG Base Directory Specification, que é o contrato
padrão para localização de arquivos de configuração em sistemas Linux.
A crate `dirs` resolve `$XDG_CONFIG_HOME` com fallback para `~/.config`
automaticamente.

---

## 2.4 Resolução do símbolo `~`

O `~` não é interpretado pelo Rust — é uma convenção do shell. Se
alguém escreve `path = "~/.local/share/whisper/modelo.bin"` no TOML,
o Rust recebe literalmente a string `"~/.local/..."`.

A resolução é feita explicitamente:

```rust
fn resolve_paths(&mut self) {
    if self.model.path.starts_with('~') {
        if let Some(home) = dirs::home_dir() {
            self.model.path = self.model.path
                .replacen('~', &home.to_string_lossy(), 1);
        }
    }
}
```

`replacen` com `n=1` garante que apenas o primeiro `~` seja substituído,
caso o caminho contenha `~` em posição inesperada.

---

## 2.5 Validação com mensagens de erro acionáveis

Validação de configuração é frequentemente negligenciada, resultando em
erros crípticos em runtime. O padrão adotado aqui: **falhar cedo com
mensagem que diz exatamente como corrigir o problema**.

```rust
if self.inference.segment_duration_secs > 30 {
    anyhow::bail!(
        "[inference] segment_duration_secs = {} é maior que 30s \
         (limite do Whisper). Use um valor entre 15 e 28.",
        self.inference.segment_duration_secs
    );
}
```

Diferença entre `bail!` e `warn!`:

- `bail!` → encerra o daemon imediatamente. Usado para configurações que
  tornariam o software disfuncional (ex: segmento > 30s faria o Whisper
  truncar silenciosamente o áudio).
- `warn!` → continua, mas alerta. Usado para configurações subótimas que
  ainda funcionam (ex: `use_gpu = false` resulta em latência alta, mas
  o software funciona).

---

## 2.6 O campo `effective_prompt()`

```rust
pub fn effective_prompt(&self) -> Option<String> {
    let parts: Vec<&str> = [
        self.system_prompt.as_str(),
        self.initial_prompt.as_str(),
    ]
    .iter()
    .copied()
    .filter(|s| !s.trim().is_empty())
    .collect();

    if parts.is_empty() { None } else { Some(parts.join(" ")) }
}
```

O `whisper.cpp` tem um único campo de "initial prompt" — não há suporte
nativo a "system prompt" como em modelos de chat. A solução é concatenar
`system_prompt` e `initial_prompt` em uma única string antes de passar
ao modelo.

O retorno `Option<String>` em vez de `String` é semântico: `None` significa
"não há prompt" e permite que o caller evite passar uma string vazia ao
Whisper (o que causaria comportamento diferente de não passar nada).

---

## 2.7 Lição sobre design de configuração

> **Regra prática:** nunca faça o usuário precisar ler código-fonte
> para entender o que uma opção faz.

Cada campo do `config.toml` segue a estrutura:

```toml
# Uma linha explicando o que a opção faz
# Uma linha com contexto adicional (quando relevante)
# Uma linha com exemplos ou valores válidos (quando não óbvio)
campo = valor_padrão
```

Configurações opcionais têm um padrão sensato. O usuário médio pode
usar o arquivo sem modificar nada. O usuário avançado tem controle total.
