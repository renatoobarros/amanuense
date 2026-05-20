use super::INJECT_KEY_CODE;

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
        key_code = INJECT_KEY_CODE + 8,
        keysym_hex = format_args!("0x{:08x}", keysym),
    )
}

pub(super) fn build_unicode_keymap(ch: char) -> String {
    let codepoint = ch as u32;
    let keysym = if codepoint <= 0x00ff {
        codepoint
    } else {
        0x0100_0000 + codepoint
    };
    build_keysym_keymap(keysym)
}
