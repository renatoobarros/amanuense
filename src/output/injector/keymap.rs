use std::collections::HashMap;

fn char_to_keysym(c: char) -> u32 {
    match c {
        '\n' | '\r' => 0xff0d, // Enter
        '\t' => 0xff09,        // Tab
        _ => {
            let cp = c as u32;
            if cp <= 0xff { cp } else { 0x0100_0000 + cp }
        }
    }
}

pub(super) fn build_xkb_string(map: &HashMap<char, u32>) -> String {
    let mut keycodes = String::new();
    let mut symbols = String::new();

    for (&ch, &keycode) in map.iter() {
        let keysym = char_to_keysym(ch);
        keycodes.push_str(&format!("        <K{:03}> = {};\n", keycode, keycode));
        symbols.push_str(&format!(
            "        key <K{:03}> {{ [ 0x{:08x} ] }};\n",
            keycode, keysym
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
