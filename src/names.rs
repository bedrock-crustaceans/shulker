//! Pack names as the game shows them: a header name that is a key of the pack's own
//! `texts/en_US.lang` is replaced by its text, and `§` formatting codes are left out.

/// The name to show for a pack whose header name is `raw`, given the text of its
/// `texts/en_US.lang` when it has one.
pub fn display_name(raw: &str, lang: Option<&str>) -> String {
    let localized = lang.and_then(|lang| translate(lang, raw.trim()));
    let name = strip_formatting(localized.as_deref().unwrap_or(raw));
    let name = name.trim();
    if name.is_empty() {
        // The game falls back to the raw header name; a name of nothing but codes would print
        // as nothing at all.
        raw.trim().to_owned()
    } else {
        name.to_owned()
    }
}

/// The text of `key` in a `.lang` file: `key=text` lines, `##` starting a comment.
fn translate(lang: &str, key: &str) -> Option<String> {
    lang.trim_start_matches('\u{feff}')
        .lines()
        .find_map(|line| {
            let (name, text) = line.split_once('=')?;
            if name.trim() != key {
                return None;
            }
            let text = text.split("##").next().unwrap_or(text);
            // A tab ends the text too: authors put a `#` comment behind one.
            let text = text.split('\t').next().unwrap_or(text);
            Some(text.trim().to_owned())
        })
}

/// `text` without its `§` formatting codes (a `§` and the character after it).
fn strip_formatting(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LANG: &str = "\u{feff}## the pack's texts\npack.name=§2Ruby §lTools\t#\npack.description=Things ## a comment\n";

    #[test]
    fn a_name_that_is_a_lang_key_is_translated() {
        assert_eq!(display_name("pack.name", Some(LANG)), "Ruby Tools");
    }

    #[test]
    fn formatting_codes_are_left_out() {
        assert_eq!(
            display_name("§p[§b1.0.0§p] §pRuby Add-On", None),
            "[1.0.0] Ruby Add-On"
        );
    }

    #[test]
    fn a_key_the_lang_file_lacks_stays_as_written() {
        assert_eq!(display_name("pack.title", Some(LANG)), "pack.title");
        assert_eq!(display_name("pack.name", None), "pack.name");
    }

    #[test]
    fn a_name_of_only_codes_falls_back_to_the_raw_name() {
        assert_eq!(display_name("§a§b", None), "§a§b");
    }
}
