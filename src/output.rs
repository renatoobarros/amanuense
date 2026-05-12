/// output/ — Entrega do texto transcrito ao ambiente do usuário.
///
/// Dois mecanismos independentes:
///   - `injector`  : digita o texto no campo com foco via protocolo Wayland nativo
///   - `clipboard` : coloca o texto na seleção primária (colar com botão do meio)
///
/// Nenhum dado é gravado em disco por estes módulos.
pub mod clipboard;
pub mod injector;
