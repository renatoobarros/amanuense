use super::INJECT_KEY_CODE;

/// Gera um keymap XKB mínimo mapeando INJECT_KEY_CODE para um keysym padrão.
pub(super) fn build_keysym_keymap(keysym: u32) -> String {
    format!(
        r#"xkb_keymap {{
    xkb_keycodes "inject" {{
        minimum = 8;
        maximum = 255;
        <INJECT> = {key_code};
    }};
    xkb_types "inject" {{
        include "complete"
    }};
    xkb_compatibility "inject" {{
        include "complete"
    }};
    xkb_symbols "inject" {{
        key <INJECT> {{ [ {keysym_hex} ] }};
    }};
}};"#,
        key_code = INJECT_KEY_CODE + 8, // XKB usa offset +8 em relação ao evdev
        keysym_hex = format_args!("0x{:08x}", keysym),
    )
}

/// Gera um keymap XKB mínimo mapeando INJECT_KEY_CODE para um codepoint Unicode.
///
/// XKB suporta keysyms Unicode diretamente no formato `U<codepoint_hex>`.
/// Ex: 'ã' (U+00E3) → `U00E3`
pub(super) fn build_unicode_keymap(ch: char) -> String {
    let codepoint = ch as u32;

    // XKB keysym para Unicode: 0x01000000 + codepoint (para codepoints > 0x100)
    // Para ASCII e Latin-1 (≤ 0xFF), o keysym é igual ao codepoint diretamente.
    let keysym = if codepoint <= 0x00ff {
        codepoint
    } else {
        0x0100_0000 + codepoint
    };

    build_keysym_keymap(keysym)
}
