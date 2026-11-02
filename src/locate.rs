//! Finding a finding's JSON path in the file as written, for the findings that carry a path but
//! no byte span.
//!
//! A small tolerant scanner, not a parser: it accepts what the game accepts around the values
//! (`//` and `/* */` comments, trailing commas) and gives up, returning `None`, on anything it
//! cannot follow.

use std::ops::Range;

use bedrock::addon::diagnostic::{JsonPath, JsonPathSegment};

/// Where a JSON path is in a document.
#[derive(Debug)]
pub enum Located {
    /// The member is there: its key (absent for an array element or the root) and its value.
    Found {
        key: Option<Range<usize>>,
        value: Range<usize>,
    },
    /// The path stops at `parent`; its member `name` is not there.
    Missing { parent: Range<usize>, name: String },
}

impl Located {
    /// The span to mark: the key, or the value when `prefer_value` and the value is on one line.
    pub fn span(&self, text: &str, prefer_value: bool) -> Range<usize> {
        match self {
            Located::Found { key, value } => {
                let one_line = !text[value.clone()].contains('\n');
                match key {
                    Some(key) if !(prefer_value && one_line) => key.clone(),
                    _ if one_line => value.clone(),
                    // A multi-line value without a key: its opening bracket.
                    _ => value.start..value.start + 1,
                }
            }
            Located::Missing { parent, .. } => parent.clone(),
        }
    }
}

pub fn locate(text: &str, path: &JsonPath) -> Option<Located> {
    let scanner = Scanner {
        text: text.as_bytes(),
    };
    let mut value = scanner.value_range(scanner.skip(0))?;
    let mut key = None;
    for segment in path.segments() {
        let found = match segment {
            JsonPathSegment::Key(name) => scanner.member(value.start, name),
            JsonPathSegment::Index(index) => scanner.element(value.start, index),
        };
        match found {
            Some((member_key, member_value)) => {
                key = member_key;
                value = member_value;
            }
            None => {
                // Nothing of the path is in the file: the finding is about content merged in
                // from elsewhere (vanilla's copy of a UI file), so there is nothing to mark.
                let (JsonPathSegment::Key(name), Some(_)) = (segment, &key) else {
                    return None;
                };
                let parent = key.unwrap_or(value.start..value.start + 1);
                return Some(Located::Missing {
                    parent,
                    name: name.to_owned(),
                });
            }
        }
    }
    Some(Located::Found { key, value })
}

struct Scanner<'a> {
    text: &'a [u8],
}

impl Scanner<'_> {
    /// The first position at or after `at` that is no whitespace and no comment.
    fn skip(&self, mut at: usize) -> usize {
        loop {
            match self.text.get(at..) {
                Some([b' ' | b'\t' | b'\r' | b'\n', ..]) => at += 1,
                Some([0xEF, 0xBB, 0xBF, ..]) if at == 0 => at += 3,
                Some([b'/', b'/', ..]) => {
                    while self.text.get(at).is_some_and(|&b| b != b'\n') {
                        at += 1;
                    }
                }
                Some([b'/', b'*', ..]) => {
                    at += 2;
                    while at < self.text.len() && !self.text[at..].starts_with(b"*/") {
                        at += 1;
                    }
                    at = (at + 2).min(self.text.len());
                }
                _ => return at,
            }
        }
    }

    /// The range of the value starting at `at`.
    fn value_range(&self, at: usize) -> Option<Range<usize>> {
        let end = match self.text.get(at)? {
            b'"' => self.string_end(at)?,
            b'{' | b'[' => self.container_end(at)?,
            _ => {
                let mut end = at;
                while self.text.get(end).is_some_and(|&b| {
                    !matches!(b, b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n' | b'/')
                }) {
                    end += 1;
                }
                (end > at).then_some(end)?
            }
        };
        Some(at..end)
    }

    /// One past the closing quote of the string opening at `at`.
    fn string_end(&self, at: usize) -> Option<usize> {
        let mut i = at + 1;
        loop {
            match self.text.get(i)? {
                b'\\' => i += 2,
                b'"' => return Some(i + 1),
                _ => i += 1,
            }
        }
    }

    /// One past the bracket closing the object or array opening at `at`.
    fn container_end(&self, at: usize) -> Option<usize> {
        let close = if self.text[at] == b'{' { b'}' } else { b']' };
        let mut i = self.skip(at + 1);
        loop {
            match *self.text.get(i)? {
                b if b == close => return Some(i + 1),
                b',' | b':' => i = self.skip(i + 1),
                _ => i = self.skip(self.value_range(i)?.end),
            }
        }
    }

    /// The key and value of member `name` of the object at `at`; a key in another case when no
    /// key matches exactly.
    fn member(&self, at: usize, name: &str) -> Option<(Option<Range<usize>>, Range<usize>)> {
        if self.text.get(at)? != &b'{' {
            return None;
        }
        let mut folded = None;
        let mut i = self.skip(at + 1);
        while self.text.get(i)? == &b'"' {
            let key = i..self.string_end(i)?;
            let colon = self.skip(key.end);
            if self.text.get(colon)? != &b':' {
                return None;
            }
            let value = self.value_range(self.skip(colon + 1))?;
            let written = std::str::from_utf8(&self.text[key.start + 1..key.end - 1]).ok()?;
            if written == name {
                return Some((Some(key), value));
            }
            if folded.is_none() && written.eq_ignore_ascii_case(name) {
                folded = Some((Some(key), value.clone()));
            }
            i = self.skip(value.end);
            if self.text.get(i) == Some(&b',') {
                i = self.skip(i + 1);
            }
        }
        folded
    }

    /// Element `index` of the array at `at`.
    fn element(&self, at: usize, index: usize) -> Option<(Option<Range<usize>>, Range<usize>)> {
        if self.text.get(at)? != &b'[' {
            return None;
        }
        let mut i = self.skip(at + 1);
        for _ in 0..index {
            i = self.skip(self.value_range(i)?.end);
            if self.text.get(i)? != &b',' {
                return None;
            }
            i = self.skip(i + 1);
        }
        Some((None, self.value_range(i)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: &str = r#"{
  // a comment
  "format_version": "1.21.0",
  "minecraft:block": {
    "description": { "identifier": "demo:lamp" },
    "components": { "minecraft:friction": "slippery", },
    "permutations": [ { "condition": "q.x" }, { "condition": "q.y" } ]
  }
}"#;

    fn at(keys: &[&str]) -> JsonPath {
        JsonPath::of_keys(keys)
    }

    #[test]
    fn finds_a_nested_value_past_a_comment_and_a_trailing_comma() {
        let Some(Located::Found { key, value }) = locate(
            BLOCK,
            &at(&["minecraft:block", "components", "minecraft:friction"]),
        ) else {
            panic!("found");
        };
        assert_eq!(&BLOCK[key.unwrap()], "\"minecraft:friction\"");
        assert_eq!(&BLOCK[value], "\"slippery\"");
    }

    #[test]
    fn finds_an_array_element() {
        let mut path = at(&["minecraft:block", "permutations"]);
        path.push_index(1);
        path.push("condition");
        let Some(Located::Found { value, .. }) = locate(BLOCK, &path) else {
            panic!("found");
        };
        assert_eq!(&BLOCK[value], "\"q.y\"");
    }

    #[test]
    fn a_missing_member_points_at_its_parent() {
        let Some(Located::Missing { parent, name }) =
            locate(BLOCK, &at(&["minecraft:block", "description", "states"]))
        else {
            panic!("missing");
        };
        assert_eq!(&BLOCK[parent], "\"description\"");
        assert_eq!(name, "states");
    }

    #[test]
    fn a_path_with_nothing_in_the_file_is_not_located() {
        assert!(locate(BLOCK, &at(&["minecraft:entity", "description"])).is_none());
    }

    #[test]
    fn keys_match_in_another_case_when_none_matches_exactly() {
        let Some(Located::Found { key, .. }) = locate(BLOCK, &at(&["Minecraft:Block"])) else {
            panic!("found");
        };
        assert_eq!(&BLOCK[key.unwrap()], "\"minecraft:block\"");
    }
}
