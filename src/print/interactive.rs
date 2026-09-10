//! Interactive graph viewer with vim-style navigation.
//!
//! Shows the text graph in a full-screen view that can be scrolled in all four
//! directions with `h`, `j`, `k` and `l` (arrow keys work as well), and that
//! can be searched with `/`, like in vim.

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{read, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Print, SetAttribute};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, size, Clear, ClearType, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::{execute, queue};
use std::io::{stdout, Error, Write};
use std::str::Chars;
use unicode_width::UnicodeWidthChar;

/// ANSI sequence resetting all colors and attributes.
const RESET: &str = "\x1b[0m";
/// ANSI sequences switching reverse video on and off, used to mark search matches.
const REVERSE: &str = "\x1b[7m";
const NO_REVERSE: &str = "\x1b[27m";

const HELP: &str = "hjkl scroll  gg/G top/end  / search  n next  q quit";

/// Show the graph in an interactive, scrollable view.
///
/// Returns once the user leaves the viewer, restoring the terminal to the
/// state it was in before.
pub fn print_interactive(graph_lines: &[String], text_lines: &[String]) -> Result<(), Error> {
    let lines: Vec<String> = graph_lines
        .iter()
        .zip(text_lines.iter())
        .map(|(g_line, t_line)| format!(" {}  {}", g_line, t_line))
        .collect();

    if lines.is_empty() {
        return Ok(());
    }

    let max_width = lines
        .iter()
        .map(|line| visible_width(line))
        .max()
        .unwrap_or(0);

    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, Hide)?;

    let result = view_loop(&mut out, &lines, max_width);

    execute!(out, Show, LeaveAlternateScreen)?;
    disable_raw_mode()?;

    result
}

/// What the viewer shows, and where.
struct State {
    /// First visible line.
    top: usize,
    /// First visible terminal column.
    left: usize,
    /// Search to highlight and to repeat with `n` and `N`.
    search: Option<Search>,
    /// Search pattern that is currently being typed, if any.
    prompt: Option<Prompt>,
    /// One-off message, shown instead of the help text.
    message: Option<String>,
    /// Set once `g` was pressed, so that `gg` can jump to the first line.
    pending_g: bool,
}

/// Search pattern, prepared for matching.
struct Search {
    /// The pattern as typed by the user.
    text: String,
    /// The pattern as it is matched, i.e. lower case for a case-insensitive search.
    pattern: Vec<char>,
    ignore_case: bool,
    backward: bool,
}

/// A search pattern while it is being typed.
struct Prompt {
    text: String,
    backward: bool,
}

/// Dimensions of the visible part of the graph, in terminal cells.
struct Screen {
    width: usize,
    /// Number of lines, i.e. the terminal height without the status line.
    page: usize,
}

impl State {
    fn new() -> Self {
        Self {
            top: 0,
            left: 0,
            search: None,
            prompt: None,
            message: None,
            pending_g: false,
        }
    }
}

impl Search {
    fn new(text: String, backward: bool) -> Self {
        // "Smart case", as in vim: only search case-sensitively if the user
        // bothered to type an upper case character.
        let ignore_case = !text.chars().any(char::is_uppercase);
        let pattern = normalize(text.chars(), ignore_case);
        Self {
            text,
            pattern,
            ignore_case,
            backward,
        }
    }
}

impl Prompt {
    fn new(backward: bool) -> Self {
        Self {
            text: String::new(),
            backward,
        }
    }
}

/// Draw the view and process key presses until the user quits.
fn view_loop<W: Write>(out: &mut W, lines: &[String], max_width: usize) -> Result<(), Error> {
    let mut state = State::new();

    loop {
        let (width, height) = size()?;
        let screen = Screen {
            // One row is reserved for the status line at the bottom.
            width: (width as usize).max(1),
            page: (height as usize).max(2) - 1,
        };

        state.top = state.top.min(lines.len().saturating_sub(screen.page));
        state.left = state.left.min(max_width.saturating_sub(screen.width));

        draw(out, lines, &state, &screen)?;

        let key = match read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => key,
            // Any other event (e.g. a resize) simply triggers a re-draw.
            _ => continue,
        };

        if state.prompt.is_some() {
            type_search(&mut state, lines, key);
        } else if !navigate(&mut state, lines, key, &screen) {
            break;
        }
    }

    Ok(())
}

/// Process a key press in the normal (i.e. not searching) mode.
///
/// Returns false if the user wants to leave the viewer.
fn navigate(state: &mut State, lines: &[String], key: KeyEvent, screen: &Screen) -> bool {
    let was_g = state.pending_g;
    state.pending_g = false;
    state.message = None;

    let half_page = (screen.page / 2).max(1);
    let full_page = screen.page.saturating_sub(1).max(1);
    let half_width = (screen.width / 2).max(1);

    match key.code {
        KeyCode::Char('g') if !is_ctrl(&key) => {
            if was_g {
                state.top = 0;
            } else {
                state.pending_g = true;
            }
        }
        KeyCode::Char('G') | KeyCode::End => state.top = lines.len(),
        KeyCode::Home => state.top = 0,

        KeyCode::Char('j') | KeyCode::Down | KeyCode::Enter => state.top += 1,
        KeyCode::Char('k') | KeyCode::Up => state.top = state.top.saturating_sub(1),

        KeyCode::Char('l') if !is_ctrl(&key) => state.left += 1,
        KeyCode::Right => state.left += 1,
        KeyCode::Char('h') if !is_ctrl(&key) => state.left = state.left.saturating_sub(1),
        KeyCode::Left => state.left = state.left.saturating_sub(1),

        KeyCode::Char('L') => state.left += half_width,
        KeyCode::Char('H') => state.left = state.left.saturating_sub(half_width),

        KeyCode::Char('0') | KeyCode::Char('^') => state.left = 0,
        KeyCode::Char('$') => state.left = usize::MAX,

        KeyCode::Char('d') if is_ctrl(&key) => state.top += half_page,
        KeyCode::Char('u') if is_ctrl(&key) => state.top = state.top.saturating_sub(half_page),

        KeyCode::Char('f') if is_ctrl(&key) => state.top += full_page,
        KeyCode::Char('b') if is_ctrl(&key) => state.top = state.top.saturating_sub(full_page),
        KeyCode::Char(' ') | KeyCode::PageDown => state.top += full_page,
        KeyCode::PageUp => state.top = state.top.saturating_sub(full_page),

        KeyCode::Char('/') => state.prompt = Some(Prompt::new(false)),
        KeyCode::Char('?') => state.prompt = Some(Prompt::new(true)),
        KeyCode::Char('n') => jump(state, lines, false),
        KeyCode::Char('N') => jump(state, lines, true),

        KeyCode::Char('q') | KeyCode::Esc => return false,
        KeyCode::Char('c') if is_ctrl(&key) => return false,
        _ => {}
    }

    true
}

/// Process a key press while a search pattern is being typed.
///
/// An empty pattern removes the current search, and with it its highlighting.
fn type_search(state: &mut State, lines: &[String], key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            let Some(prompt) = state.prompt.take() else {
                return;
            };
            state.search = if prompt.text.is_empty() {
                None
            } else {
                Some(Search::new(prompt.text, prompt.backward))
            };
            if state.search.is_some() {
                jump(state, lines, false);
            }
        }
        KeyCode::Esc => state.prompt = None,
        KeyCode::Char('c') if is_ctrl(&key) => state.prompt = None,
        KeyCode::Char(chr) if !is_ctrl(&key) => {
            if let Some(prompt) = &mut state.prompt {
                prompt.text.push(chr);
            }
        }
        KeyCode::Backspace => {
            if let Some(prompt) = &mut state.prompt {
                prompt.text.pop();
            }
        }
        _ => {}
    }
}

/// Scroll to the next line matching the current search, putting it on top.
///
/// With `reverse`, the search direction is inverted, as with `N` in vim.
fn jump(state: &mut State, lines: &[String], reverse: bool) {
    let Some(search) = &state.search else {
        state.message = Some("No previous search".to_string());
        return;
    };

    match find_line(lines, search, state.top, search.backward != reverse) {
        Some(line) => state.top = line,
        None => state.message = Some(format!("Pattern not found: {}", search.text)),
    }
}

/// Find the next line matching `search`, starting after line `from`.
///
/// The search wraps around at the end (or start) of the graph.
fn find_line(lines: &[String], search: &Search, from: usize, backward: bool) -> Option<usize> {
    let count = lines.len();

    (1..=count)
        .map(|offset| {
            if backward {
                (from + count - offset) % count
            } else {
                (from + offset) % count
            }
        })
        .find(|&line| !find_matches(&lines[line], search).is_empty())
}

fn is_ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
}

/// Draw the visible part of the graph, plus the status line at the bottom.
fn draw<W: Write>(
    out: &mut W,
    lines: &[String],
    state: &State,
    screen: &Screen,
) -> Result<(), Error> {
    for row in 0..screen.page {
        queue!(out, MoveTo(0, row as u16), Clear(ClearType::CurrentLine))?;
        if let Some(line) = lines.get(state.top + row) {
            let matches = match &state.search {
                Some(search) => find_matches(line, search),
                None => Vec::new(),
            };
            queue!(
                out,
                Print(slice(line, state.left, screen.width, &matches)),
                Print(RESET)
            )?;
        }
    }

    draw_status(out, lines.len(), state, screen)?;

    out.flush()
}

/// Draw the status line, or the search prompt while a pattern is being typed.
fn draw_status<W: Write>(
    out: &mut W,
    line_count: usize,
    state: &State,
    screen: &Screen,
) -> Result<(), Error> {
    let row = screen.page as u16;
    queue!(out, MoveTo(0, row), Clear(ClearType::CurrentLine))?;

    if let Some(prompt) = &state.prompt {
        let text = format!("{}{}", if prompt.backward { '?' } else { '/' }, prompt.text);
        let cursor = visible_width(&text).min(screen.width - 1);
        return queue!(
            out,
            Print(pad(&text, screen.width)),
            MoveTo(cursor as u16, row),
            Show
        );
    }

    let info = match &state.message {
        Some(message) => message,
        None => HELP,
    };
    let status = format!(
        " {}-{}/{}  {}",
        state.top + 1,
        (state.top + screen.page).min(line_count),
        line_count,
        info
    );

    queue!(
        out,
        Hide,
        SetAttribute(Attribute::Reverse),
        Print(pad(&status, screen.width)),
        SetAttribute(Attribute::Reset)
    )
}

/// Cut a horizontal window of `width` columns out of an ANSI-colored `line`,
/// starting at column `start`.
///
/// Escape sequences are kept, so that colors of the visible part are the same
/// as in the un-scrolled line. Wide characters that are cut in half are
/// replaced by a space. Columns in `matches` are shown in reverse video.
fn slice(line: &str, start: usize, width: usize, matches: &[(usize, usize)]) -> String {
    let mut result = String::new();
    let mut column = 0;
    let mut highlighted = false;
    let mut chars = line.chars();

    while let Some(chr) = chars.next() {
        if chr == '\x1b' {
            let sequence = chars.as_str();
            skip_escape(&mut chars);
            result.push(chr);
            result.push_str(&sequence[..sequence.len() - chars.as_str().len()]);
            if highlighted {
                // The sequence may have switched reverse video off again
                result.push_str(REVERSE);
            }
            continue;
        }
        let char_width = chr.width().unwrap_or(0);
        if column + char_width <= start {
            column += char_width;
            continue;
        }
        if column >= start + width {
            break;
        }
        set_highlight(&mut result, &mut highlighted, is_match(matches, column));
        if column < start || column + char_width > start + width {
            // Only one half of a wide character is visible
            result.push(' ');
        } else {
            result.push(chr);
        }
        column += char_width;
    }

    if highlighted {
        result.push_str(NO_REVERSE);
    }

    result
}

/// Switch reverse video on or off, unless it is in the wanted state already.
fn set_highlight(result: &mut String, highlighted: &mut bool, wanted: bool) {
    if wanted != *highlighted {
        result.push_str(if wanted { REVERSE } else { NO_REVERSE });
        *highlighted = wanted;
    }
}

fn is_match(matches: &[(usize, usize)], column: usize) -> bool {
    matches
        .iter()
        .any(|(from, to)| column >= *from && column < *to)
}

/// Column ranges of all matches of `search` in an ANSI-colored line.
fn find_matches(line: &str, search: &Search) -> Vec<(usize, usize)> {
    let length = search.pattern.len();
    if length == 0 {
        return Vec::new();
    }

    let visible = visible_chars(line);
    let chars = normalize(visible.iter().map(|(chr, _)| *chr), search.ignore_case);

    let mut result = Vec::new();
    let mut index = 0;
    while index + length <= chars.len() {
        if chars[index..(index + length)] == search.pattern[..] {
            let (last, column) = visible[index + length - 1];
            result.push((visible[index].1, column + last.width().unwrap_or(0)));
            index += length;
        } else {
            index += 1;
        }
    }

    result
}

/// Lower-case characters for a case-insensitive search.
///
/// Only the first character of a lower case mapping is used, to keep one
/// character per character, and thus the mapping to terminal columns.
fn normalize(chars: impl Iterator<Item = char>, ignore_case: bool) -> Vec<char> {
    chars
        .map(|chr| {
            if ignore_case {
                chr.to_lowercase().next().unwrap_or(chr)
            } else {
                chr
            }
        })
        .collect()
}

/// Visible characters of an ANSI-colored line, with the column each starts at.
fn visible_chars(line: &str) -> Vec<(char, usize)> {
    let mut result = Vec::new();
    let mut column = 0;
    let mut chars = line.chars();

    while let Some(chr) = chars.next() {
        if chr == '\x1b' {
            skip_escape(&mut chars);
        } else {
            result.push((chr, column));
            column += chr.width().unwrap_or(0);
        }
    }

    result
}

/// Visible width of an ANSI-colored line, in terminal columns.
fn visible_width(line: &str) -> usize {
    let mut width = 0;
    let mut chars = line.chars();

    while let Some(chr) = chars.next() {
        if chr == '\x1b' {
            skip_escape(&mut chars);
        } else {
            width += chr.width().unwrap_or(0);
        }
    }

    width
}

/// Consume the remainder of an escape sequence, `\x1b` already being consumed.
fn skip_escape(chars: &mut Chars) {
    if chars.clone().next() != Some('[') {
        // Not a CSI sequence, so it is a single character
        chars.next();
        return;
    }
    for chr in chars.by_ref() {
        // Any character in this range terminates a CSI sequence
        if ('@'..='~').contains(&chr) && chr != '[' {
            break;
        }
    }
}

/// Truncate or pad a plain (un-colored) text to exactly `width` columns.
fn pad(text: &str, width: usize) -> String {
    let mut result = String::new();
    let mut column = 0;

    for chr in text.chars() {
        let char_width = chr.width().unwrap_or(0);
        if column + char_width > width {
            break;
        }
        result.push(chr);
        column += char_width;
    }
    result.push_str(&" ".repeat(width - column));

    result
}

#[cfg(test)]
mod tests {
    use super::{find_line, find_matches, pad, slice, visible_width, Search};

    const NONE: [(usize, usize); 0] = [];

    #[test]
    fn slice_plain_text() {
        assert_eq!(slice("abcdef", 0, 3, &NONE), "abc");
        assert_eq!(slice("abcdef", 2, 3, &NONE), "cde");
        assert_eq!(slice("abcdef", 4, 10, &NONE), "ef");
        assert_eq!(slice("abcdef", 10, 3, &NONE), "");
    }

    #[test]
    fn slice_keeps_colors() {
        let line = "\x1b[31mabc\x1b[0mdef";
        assert_eq!(slice(line, 2, 3, &NONE), "\x1b[31mc\x1b[0mde");
        assert_eq!(slice(line, 0, 2, &NONE), "\x1b[31mab");
    }

    #[test]
    fn slice_splits_wide_chars() {
        // '世' is two columns wide
        assert_eq!(slice("a世b", 0, 3, &NONE), "a世");
        assert_eq!(slice("a世b", 2, 2, &NONE), " b");
        assert_eq!(slice("a世b", 0, 2, &NONE), "a ");
    }

    #[test]
    fn slice_highlights_matches() {
        assert_eq!(slice("abcd", 0, 4, &[(1, 3)]), "a\x1b[7mbc\x1b[27md");
        // Highlighting is restored after a color that may have reset it
        assert_eq!(
            slice("ab\x1b[0mcd", 0, 4, &[(0, 4)]),
            "\x1b[7mab\x1b[0m\x1b[7mcd\x1b[27m"
        );
        // Only the visible part of a match is highlighted
        assert_eq!(slice("abcd", 2, 2, &[(1, 3)]), "\x1b[7mc\x1b[27md");
    }

    #[test]
    fn width_ignores_colors() {
        assert_eq!(visible_width("\x1b[31mabc\x1b[0m"), 3);
        assert_eq!(visible_width("a世b"), 4);
    }

    #[test]
    fn pad_to_width() {
        assert_eq!(pad("abc", 5), "abc  ");
        assert_eq!(pad("abcdef", 3), "abc");
        assert_eq!(pad("a世", 2), "a ");
    }

    #[test]
    fn find_matches_in_line() {
        let search = Search::new("ab".to_string(), false);
        assert_eq!(find_matches("xabyab", &search), vec![(1, 3), (4, 6)]);
        assert_eq!(find_matches("xyz", &search), vec![]);
        // Columns are those of the visible text, colors are skipped
        assert_eq!(find_matches("\x1b[31mx\x1b[0mab", &search), vec![(1, 3)]);
        // Columns account for the width of characters
        assert_eq!(find_matches("世ab", &search), vec![(2, 4)]);
    }

    #[test]
    fn find_matches_smart_case() {
        // A lower case pattern matches any case ...
        let search = Search::new("commit".to_string(), false);
        assert_eq!(find_matches("Commit", &search), vec![(0, 6)]);
        // ... an upper case character makes the search case-sensitive
        let search = Search::new("Commit".to_string(), false);
        assert_eq!(find_matches("commit", &search), vec![]);
        assert_eq!(find_matches("Commit", &search), vec![(0, 6)]);
    }

    #[test]
    fn find_line_wraps_around() {
        let lines: Vec<String> = ["aa", "bb", "cc", "bb"]
            .iter()
            .map(|line| line.to_string())
            .collect();
        let search = Search::new("bb".to_string(), false);

        assert_eq!(find_line(&lines, &search, 0, false), Some(1));
        assert_eq!(find_line(&lines, &search, 1, false), Some(3));
        // Searching past the last match continues at the first one
        assert_eq!(find_line(&lines, &search, 3, false), Some(1));
        assert_eq!(find_line(&lines, &search, 3, true), Some(1));
        assert_eq!(find_line(&lines, &search, 0, true), Some(3));

        let search = Search::new("zz".to_string(), false);
        assert_eq!(find_line(&lines, &search, 0, false), None);
    }
}
