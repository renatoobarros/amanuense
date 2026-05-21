use std::collections::HashMap;

pub(super) fn build_xkb_string(map: &HashMap<char, u32>) -> String {
    let mut keycodes = String::new();
    let mut symbols = String::new();

    for (&ch, &keycode) in map.iter() {
        let keysym_str = match ch {
            ' ' => "space".to_string(),
            '\n' | '\r' => "Return".to_string(),
            '\t' => "Tab".to_string(),
            _ => {
                let cp = ch as u32;
                if cp <= 0xff {
                    format!("0x{:04x}", cp)
                } else {
                    format!("0x{:08x}", 0x0100_0000 + cp)
                }
            }
        };

        keycodes.push_str(&format!("        <K{:03}> = {};\n", keycode, keycode));
        symbols.push_str(&format!(
            "        key <K{:03}> {{ [ {} ] }};\n",
            keycode, keysym_str
        ));
    }

    format!(
        r#"xkb_keymap {{
    xkb_keycodes "inject" {{
        minimum = 8;
        maximum = 255;
{}    }};
    xkb_types "inject" {{
        include "complete"
    }};
    xkb_compatibility "inject" {{
        include "complete"
    }};
    xkb_symbols "inject" {{
{}    }};
}};"#,
        keycodes, symbols
    )
}
