//! Find and replace in the current file. Match positions are chars.

use regex::{NoExpand, Regex, RegexBuilder};
use std::hash::{Hash, Hasher};

const MAX_MATCHES: usize = 20_000;

#[derive(Default)]
pub struct FindState {
    pub open: bool,
    pub show_replace: bool,
    pub query: String,
    pub replacement: String,
    pub match_case: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub matches: Vec<(usize, usize)>,
    pub current: Option<usize>,
    pub error: Option<String>,
    /// Put the keyboard focus in the query box on the next frame.
    pub focus: bool,
    key: u64,
}

impl FindState {
    fn pattern(&self) -> Result<Option<Regex>, String> {
        if self.query.is_empty() {
            return Ok(None);
        }
        let body = if self.regex { self.query.clone() } else { regex::escape(&self.query) };
        let body = if self.whole_word { format!(r"\b(?:{body})\b") } else { body };
        RegexBuilder::new(&body)
            .case_insensitive(!self.match_case)
            .multi_line(true)
            .build()
            .map(Some)
            .map_err(|_| "Invalid regular expression".to_string())
    }

    /// Recompute the matches if the text or the search changed.
    pub fn refresh(&mut self, text: &str, cursor: usize) {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (text, &self.query, self.match_case, self.whole_word, self.regex).hash(&mut h);
        let key = h.finish();
        if key == self.key {
            return;
        }
        self.key = key;
        self.matches.clear();
        self.error = None;
        match self.pattern() {
            Ok(Some(re)) => {
                let (mut byte, mut chars) = (0, 0);
                for m in re.find_iter(text).filter(|m| !m.is_empty()).take(MAX_MATCHES) {
                    chars += text[byte..m.start()].chars().count();
                    let start = chars;
                    chars += m.as_str().chars().count();
                    byte = m.end();
                    self.matches.push((start, chars));
                }
            }
            Ok(None) => {}
            Err(e) => self.error = Some(e),
        }
        self.current = (!self.matches.is_empty()).then(|| self.index_from(cursor));
    }

    /// The first match at or after `cursor`, wrapping around.
    fn index_from(&self, cursor: usize) -> usize {
        let i = self.matches.partition_point(|m| m.0 < cursor);
        if i < self.matches.len() { i } else { 0 }
    }

    /// Move to the next or previous match and return it.
    pub fn step(&mut self, forward: bool) -> Option<(usize, usize)> {
        let n = self.matches.len();
        if n == 0 {
            return None;
        }
        let i = match (self.current, forward) {
            (Some(c), true) => (c + 1) % n,
            (Some(c), false) => (c + n - 1) % n,
            (None, _) => 0,
        };
        self.current = Some(i);
        Some(self.matches[i])
    }

    pub fn current_match(&self) -> Option<(usize, usize)> {
        self.current.and_then(|c| self.matches.get(c).copied())
    }

    /// Replace the current match; returns where the next search should start.
    pub fn replace_current(&mut self, text: &mut String) -> Option<usize> {
        let (a, b) = self.current_match()?;
        let re = self.pattern().ok()??;
        let (ba, bb) = (crate::editor::byte_index(text, a), crate::editor::byte_index(text, b));
        let new = if self.regex {
            let caps = re.captures_at(text, ba).filter(|c| c.get(0).is_some_and(|m| m.start() == ba && m.end() == bb))?;
            let mut out = String::new();
            caps.expand(&self.replacement, &mut out);
            out
        } else {
            self.replacement.clone()
        };
        text.replace_range(ba..bb, &new);
        Some(a + new.chars().count())
    }

    /// Replace every match; returns how many were replaced.
    pub fn replace_all(&mut self, text: &mut String) -> usize {
        let Ok(Some(re)) = self.pattern() else { return 0 };
        let count = re.find_iter(text).filter(|m| !m.is_empty()).count();
        if count == 0 {
            return 0;
        }
        let new = if self.regex {
            re.replace_all(text, self.replacement.as_str()).into_owned()
        } else {
            re.replace_all(text, NoExpand(&self.replacement)).into_owned()
        };
        *text = new;
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(q: &str, text: &str) -> FindState {
        let mut f = FindState { query: q.into(), ..Default::default() };
        f.refresh(text, 0);
        f
    }

    #[test]
    fn finds_in_char_offsets_ignoring_case() {
        let f = search("čaj", "ČAJ in čaj 😀 čaj");
        assert_eq!(f.matches, vec![(0, 3), (7, 10), (13, 16)]);
    }

    #[test]
    fn options_narrow_the_search() {
        let mut f = FindState { query: "Int".into(), match_case: true, ..Default::default() };
        f.refresh("int Int Integer", 0);
        assert_eq!(f.matches, vec![(4, 7), (8, 11)]);
        f.whole_word = true;
        f.refresh("int Int Integer", 0);
        assert_eq!(f.matches, vec![(4, 7)]);
    }

    #[test]
    fn plain_search_treats_symbols_literally() {
        assert_eq!(search("a.b(", "axb( a.b(").matches, vec![(5, 9)]);
    }

    #[test]
    fn stepping_wraps_around() {
        let mut f = search("a", "a b a");
        assert_eq!(f.current_match(), Some((0, 1)));
        assert_eq!(f.step(true), Some((4, 5)));
        assert_eq!(f.step(true), Some((0, 1)));
        assert_eq!(f.step(false), Some((4, 5)));
    }

    #[test]
    fn a_bad_regex_reports_an_error() {
        let mut f = FindState { query: "(".into(), regex: true, ..Default::default() };
        f.refresh("(", 0);
        assert!(f.error.is_some() && f.matches.is_empty());
    }

    #[test]
    fn replace_one_and_all() {
        let mut t = "a b a".to_string();
        let mut f = search("a", &t);
        f.replacement = "xy$0".into();
        assert_eq!(f.replace_current(&mut t), Some(4));
        assert_eq!(t, "xy$0 b a", "plain replacement is literal");
        let mut t = "a b a".to_string();
        let mut f = search("a", &t);
        f.replacement = "$1".into();
        assert_eq!(f.replace_all(&mut t), 2);
        assert_eq!(t, "$1 b $1");
    }

    #[test]
    fn regex_replacement_expands_groups() {
        let mut t = "x=1, y=2".to_string();
        let mut f = FindState { query: r"(\w)=(\d)".into(), replacement: "$2=$1".into(), regex: true, ..Default::default() };
        f.refresh(&t, 0);
        assert_eq!(f.replace_current(&mut t), Some(3));
        assert_eq!(t, "1=x, y=2");
        let mut t = "x=1, y=2".to_string();
        f.replace_all(&mut t);
        assert_eq!(t, "1=x, 2=y");
    }
}
