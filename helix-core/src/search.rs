use crate::RopeSlice;

pub fn find_nth_next(
    text: RopeSlice,
    ch: char,
    mut pos: usize,
    n: usize,
    inclusive: bool,
) -> Option<usize> {
    if pos >= text.len_chars() {
        return None;
    }

    // start searching right after pos
    let mut chars = text.chars_at(pos + 1);

    for _ in 0..n {
        loop {
            let c = chars.next()?;

            pos += 1;

            if c == ch {
                break;
            }
        }
    }

    if !inclusive {
        pos -= 1;
    }

    Some(pos)
}

pub fn find_nth_prev(
    text: RopeSlice,
    ch: char,
    mut pos: usize,
    n: usize,
    inclusive: bool,
) -> Option<usize> {
    // start searching right before pos
    let mut chars = text.chars_at(pos);

    for _ in 0..n {
        loop {
            let c = chars.prev()?;

            pos = pos.saturating_sub(1);

            if c == ch {
                break;
            }
        }
    }

    if !inclusive {
        pos += 1;
    }

    Some(pos)
}

use crate::movement::Direction;
use regex_automata::{dense, DenseDFA, Error as RegexError, DFA};
use std::ops::Range;

/// Based on https://github.com/alacritty/alacritty/blob/3e867a056018c507d79396cb5c5b4b8309c609c2/alacritty_terminal/src/term/search.rs
struct Searcher {
    /// Locate end of match searching right.
    right_fdfa: DenseDFA<Vec<usize>, usize>,
    /// Locate start of match searching right.
    right_rdfa: DenseDFA<Vec<usize>, usize>,

    /// Locate start of match searching left.
    left_fdfa: DenseDFA<Vec<usize>, usize>,
    /// Locate end of match searching left.
    left_rdfa: DenseDFA<Vec<usize>, usize>,
}

impl Searcher {
    pub fn new(pattern: &str) -> Result<Searcher, RegexError> {
        // Check case info for smart case
        let has_uppercase = pattern.chars().any(|c| c.is_uppercase());

        // Create Regex DFAs for all search directions.
        let mut builder = dense::Builder::new();
        let builder = builder.case_insensitive(!has_uppercase);

        let left_fdfa = builder.clone().reverse(true).build(pattern)?;
        let left_rdfa = builder
            .clone()
            .anchored(true)
            .longest_match(true)
            .build(pattern)?;

        let right_fdfa = builder.clone().build(pattern)?;
        let right_rdfa = builder
            .anchored(true)
            .longest_match(true)
            .reverse(true)
            .build(pattern)?;

        Ok(Searcher {
            right_fdfa,
            right_rdfa,
            left_fdfa,
            left_rdfa,
        })
    }
    pub fn search_prev(&self, text: RopeSlice, offset: usize) -> Option<Range<usize>> {
        let text = text.slice(..offset);
        let start = self.rfind(text, &self.left_fdfa)?;
        let end = self.find(text.slice(start..), &self.left_rdfa)?;

        Some(start..start + end)
    }

    pub fn search_next(&self, text: RopeSlice, offset: usize) -> Option<Range<usize>> {
        let text = text.slice(offset..);
        let end = self.find(text, &self.right_fdfa)?;
        let start = self.rfind(text.slice(..end), &self.right_rdfa)?;

        Some(offset + start..offset + end)
    }

    /// Find the next regex match.
    ///
    /// This will always return the side of the first match which is farthest from the start point.
    fn find(&self, text: RopeSlice, dfa: &impl DFA) -> Option<usize> {
        // TOOD: needs to change to rfind condition if searching reverse
        // TODO: check this inside main search
        // if dfa.is_anchored() && start > 0 {
        //     return None;
        // }

        let mut state = dfa.start_state();
        let mut last_match = if dfa.is_dead_state(state) {
            return None;
        } else if dfa.is_match_state(state) {
            Some(0)
        } else {
            None
        };

        for chunk in text.chunks() {
            for (i, &b) in chunk.as_bytes().iter().enumerate() {
                state = unsafe { dfa.next_state_unchecked(state, b) };
                if dfa.is_match_or_dead_state(state) {
                    if dfa.is_dead_state(state) {
                        return last_match;
                    }
                    last_match = Some(i + 1);
                }
            }
        }

        last_match
    }

    fn rfind(&self, text: RopeSlice, dfa: &impl DFA) -> Option<usize> {
        // if dfa.is_anchored() && start < bytes.len() {
        //     return None;
        // }

        let mut state = dfa.start_state();
        let mut last_match = if dfa.is_dead_state(state) {
            return None;
        } else if dfa.is_match_state(state) {
            Some(text.len_bytes())
        } else {
            None
        };

        // This is basically chunks().rev()
        let (mut chunks, _, _, _) = text.chunks_at_byte(text.len_bytes());

        while let Some(chunk) = chunks.prev() {
            for (i, &b) in chunk.as_bytes().iter().enumerate().rev() {
                state = unsafe { dfa.next_state_unchecked(state, b) };
                if dfa.is_match_or_dead_state(state) {
                    if dfa.is_dead_state(state) {
                        return last_match;
                    }
                    last_match = Some(i);
                }
            }
        }
        last_match
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_search_next() {
        use crate::Rope;
        let text = Rope::from("hello world!");

        let searcher = Searcher::new(r"\w+").unwrap();

        let result = searcher.search_next(text.slice(..), 0).unwrap();
        let fragment = text.slice(result.start..result.end);
        assert_eq!("hello", fragment);

        let result = searcher.search_next(text.slice(..), result.end).unwrap();
        let fragment = text.slice(result.start..result.end);
        assert_eq!("world", fragment);

        let result = searcher.search_next(text.slice(..), result.end);
        assert!(result.is_none());
    }

    #[test]
    fn test_search_prev() {
        use crate::Rope;
        let text = Rope::from("hello world!");

        let searcher = Searcher::new(r"\w+").unwrap();

        let result = searcher
            .search_prev(text.slice(..), text.len_bytes())
            .unwrap();
        let fragment = text.slice(result.start..result.end);
        assert_eq!("world", fragment);

        let result = searcher.search_prev(text.slice(..), result.start).unwrap();
        let fragment = text.slice(result.start..result.end);
        assert_eq!("hello", fragment);

        let result = searcher.search_prev(text.slice(..), result.start);
        assert!(result.is_none());
    }
}
