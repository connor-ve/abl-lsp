//! A small table of common OpenEdge error messages with explanations, read from
//! `builtin/messages.txt`. Used by `abl explain`.

use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct Message {
    pub number: u32,
    /// Typical message text, with `<placeholders>`.
    pub text: String,
    pub explanation: String,
    pub fixes: Vec<String>,
    /// Related lint rules (`find-without-no-error`) or message numbers.
    pub see: Vec<String>,
}

const SOURCE: &str = include_str!("../builtin/messages.txt");

pub fn all() -> &'static [Message] {
    static ALL: OnceLock<Vec<Message>> = OnceLock::new();
    ALL.get_or_init(|| parse(SOURCE))
}

fn parse(src: &str) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::new();
    for line in src.lines() {
        let line = line.trim_end();
        if let Some(head) = line.strip_prefix('@') {
            let (num, text) = head.split_once(' ').unwrap_or((head, ""));
            if let Ok(number) = num.parse() {
                out.push(Message {
                    number,
                    text: text.trim().to_string(),
                    explanation: String::new(),
                    fixes: Vec::new(),
                    see: Vec::new(),
                });
            }
            continue;
        }
        let Some(m) = out.last_mut() else { continue };
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(f) = line.strip_prefix("fix:") {
            m.fixes.push(f.trim().to_string());
        } else if let Some(s) = line.strip_prefix("see:") {
            m.see.extend(s.split(',').map(|x| x.trim().to_string()));
        } else {
            if !m.explanation.is_empty() {
                m.explanation.push(' ');
            }
            m.explanation.push_str(line.trim());
        }
    }
    out
}

pub fn by_number(n: u32) -> Option<&'static Message> {
    all().iter().find(|m| m.number == n)
}

/// The message number in a pasted error line such as
/// `** Customer record not on file. (138)`, or in `138` / `(138)` alone.
pub fn number_in(input: &str) -> Option<u32> {
    let t = input.trim();
    if let Ok(n) = t.trim_matches(|c| c == '(' || c == ')').parse() {
        return Some(n);
    }
    // The last "(digits)" group.
    let close = t.rfind(')')?;
    let open = t[..close].rfind('(')?;
    t[open + 1..close].trim().parse().ok()
}

/// Messages whose text or explanation contains every word of `query`.
pub fn search(query: &str) -> Vec<&'static Message> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect();
    if words.is_empty() {
        return Vec::new();
    }
    all()
        .iter()
        .filter(|m| {
            let hay = format!("{} {}", m.text, m.explanation).to_ascii_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_parses() {
        let all = all();
        assert!(all.len() >= 20);
        let m = by_number(138).unwrap();
        assert!(m.text.contains("record not on file"));
        assert!(!m.explanation.is_empty());
        assert!(!m.fixes.is_empty());
        assert_eq!(m.see, ["find-without-no-error"]);
        // Every entry has an explanation and at least one fix, and numbers are unique.
        let mut nums: Vec<u32> = all.iter().map(|m| m.number).collect();
        for m in all {
            assert!(!m.explanation.is_empty(), "{}", m.number);
            assert!(!m.fixes.is_empty(), "{}", m.number);
            assert!(m.text.ends_with(&format!("({})", m.number)), "{}", m.number);
        }
        nums.dedup();
        assert_eq!(nums.len(), all.len());
    }

    #[test]
    fn numbers_from_pasted_lines() {
        assert_eq!(number_in("138"), Some(138));
        assert_eq!(number_in("(138)"), Some(138));
        assert_eq!(
            number_in("** Customer record not on file. (138)"),
            Some(138)
        );
        assert_eq!(
            number_in("** Entry 5 is outside the range of list a,b (x). (560)"),
            Some(560)
        );
        assert_eq!(number_in("no number"), None);
    }

    #[test]
    fn search_matches_all_words() {
        let hits: Vec<u32> = search("lock table").iter().map(|m| m.number).collect();
        assert!(hits.contains(&915), "{hits:?}");
        assert!(search("zzzz").is_empty());
    }
}
