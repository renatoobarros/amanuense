/// Gera um keymap XKB em lote a partir de uma lista de mapeamentos (caractere -> evdev_code).
pub(super) fn build_bulk_keymap(char_map: &[(char, u32)]) -> String {
    let mut keycodes = String::new();
    let mut symbols = String::new();

    for &(ch, evdev_code) in char_map {
        let xkb_code = evdev_code + 8; // XKB usa offset +8
        keycodes.push_str(&format!("        <K{}> = {};\n", xkb_code, xkb_code));

        let codepoint = ch as u32;
        let keysym = if codepoint <= 0x00ff {
            codepoint
        } else {
            0x0100_0000 + codepoint
        };

        symbols.push_str(&format!(
            "        key <K{}> {{ [ 0x{:08x} ] }};\n",
            xkb_code, keysym
        ));
    }

    format!(
        r#"xkb_keymap {{
    xkb_keycodes "inject" {{
        minimum = 8;
        maximum = 255;
{keycodes}    }};
    xkb_types "inject" {{
        include "complete"
    }};
    xkb_compatibility "inject" {{
        include "complete"
    }};
    xkb_symbols "inject" {{
{symbols}    }};
}};"#
    )
}
