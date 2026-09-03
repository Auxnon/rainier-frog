// vim emulation for tui_textarea. based on:
// https://github.com/rhysd/tui-textarea/blob/main/examples/vim.rs
use std::fmt;

#[cfg(feature = "arboard")]
use arboard::Clipboard;
use color_eyre::eyre::Result;
use ratatui::{
  style::{Color, Modifier, Style},
  text::Line,
  widgets::{Block, Borders},
};
use ratatui_textarea::{CursorMove, DataCursor, Input, Key, Scrolling, TextArea};
use tokio::sync::mpsc::UnboundedSender;

use crate::action::Action;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
  #[default]
  Normal,
  Insert,
  Visual,
  Replace,
  Operator(char),
}

pub enum SelectionDirection {
  Forward,
  Backward,
  Neutral,
}

fn get_selection_direction(
  range: ((usize, usize), (usize, usize)),
  cursor: DataCursor,
) -> SelectionDirection {
  let (start, end) = range;
  if cursor == start && cursor == end {
    SelectionDirection::Neutral
  } else if cursor == end {
    SelectionDirection::Forward
  } else {
    SelectionDirection::Backward
  }
}

impl Mode {
  pub fn block<'a>(&self) -> Block<'a> {
    let help = match self {
      Self::Normal => "type i to enter insert mode, v to enter visual mode",
      Self::Insert => "type Esc to back to normal mode",
      Self::Visual => "type y to yank, type d to delete, type Esc to back to normal mode",
      Self::Replace => "type character to replace underlined",
      Self::Operator(_) => "move cursor to apply operator",
    };
    let title = format!(" {self} MODE ({help}) ");
    Block::default().borders(Borders::ALL).title_bottom(Line::from(title).right_aligned())
  }

  pub fn cursor_style(&self) -> Style {
    match self {
      Self::Normal => Style::default().fg(Color::Reset).add_modifier(Modifier::REVERSED),
      Self::Insert => Style::default()
        .fg(Color::LightBlue)
        .add_modifier(Modifier::SLOW_BLINK | Modifier::REVERSED),
      Self::Visual => Style::default().fg(Color::LightYellow).add_modifier(Modifier::REVERSED),
      Self::Replace => Style::default()
        .fg(Color::LightMagenta)
        .add_modifier(Modifier::UNDERLINED | Modifier::REVERSED),
      Self::Operator(_) => Style::default().fg(Color::LightGreen).add_modifier(Modifier::REVERSED),
    }
  }
}

impl fmt::Display for Mode {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
    match self {
      Self::Normal => write!(f, "NORMAL"),
      Self::Insert => write!(f, "INSERT"),
      Self::Visual => write!(f, "VISUAL"),
      Self::Replace => write!(f, "REPLACE"),
      Self::Operator(c) => write!(f, "OPERATOR({c})"),
    }
  }
}

// How the Vim emulation state transitions
pub enum Transition {
  Nop,
  Mode(Mode),
  Pending(Input),
}

// State of Vim emulation
#[derive(Default, Clone)]
pub struct Vim {
  pub mode: Mode,
  pub pending: Input, // Pending input to handle a sequence with two keys like gg
  command_tx: Option<UnboundedSender<Action>>,
}

impl Vim {
  pub fn new(mode: Mode) -> Self {
    Self { mode, pending: Input::default(), command_tx: None }
  }

  pub fn with_pending(self, pending: Input) -> Self {
    Self { mode: self.mode, pending, command_tx: None }
  }

  pub fn register_action_handler(&mut self, tx: Option<UnboundedSender<Action>>) -> Result<()> {
    self.command_tx = tx;
    Ok(())
  }

  pub fn transition(&self, input: Input, textarea: &mut TextArea<'_>) -> Transition {
    if input.key == Key::Null {
      return Transition::Nop;
    }

    match self.mode {
      Mode::Normal | Mode::Visual | Mode::Operator(_) => {
        match input {
          Input { key: Key::Char('h'), .. } | Input { key: Key::Left, .. } => {
            textarea.move_cursor(CursorMove::Back)
          },
          Input { key: Key::Char('j'), .. } | Input { key: Key::Down, .. } => {
            textarea.move_cursor(CursorMove::Down)
          },
          Input { key: Key::Char('k'), .. } | Input { key: Key::Up, .. } => {
            textarea.move_cursor(CursorMove::Up)
          },
          Input { key: Key::Char('l'), .. } | Input { key: Key::Right, .. } => {
            textarea.move_cursor(CursorMove::Forward)
          },
          Input { key: Key::Char('w'), ctrl: false, .. }
            if matches!(self.mode, Mode::Operator(_) | Mode::Visual)
              && matches!(self.pending, Input { key: Key::Char('i'), ctrl: false, .. }) =>
          {
            select_inner_word(textarea, self.mode == Mode::Visual); // `iw`: `ciw`/`diw`/`yiw`/`viw`
          },
          Input { key: Key::Char('W'), ctrl: false, .. }
            if matches!(self.mode, Mode::Operator(_) | Mode::Visual)
              && matches!(self.pending, Input { key: Key::Char('i'), ctrl: false, .. }) =>
          {
            select_inner_big_word(textarea, self.mode == Mode::Visual); // `iW`: `ciW`/`diW`/`yiW`/`viW`
          },
          Input { key: Key::Char('"'), .. }
            if matches!(self.mode, Mode::Operator(_) | Mode::Visual)
              && matches!(self.pending, Input { key: Key::Char('i'), ctrl: false, .. }) =>
          {
            select_inner_quoted(textarea, '"', self.mode == Mode::Visual); // `i"`: `ci"`/`di"`/`yi"`/`vi"`
          },
          Input { key: Key::Char('w'), .. } => textarea.move_cursor(CursorMove::WordForward),
          Input { key: Key::Char('W'), ctrl: false, .. } => move_cursor_big_word_forward(textarea),
          Input { key: Key::Char('e'), ctrl: false, .. }
            if matches!(self.mode, Mode::Operator(_)) =>
          {
            textarea.move_cursor(CursorMove::WordForward) // `e` behaves like `w` in operator-pending mode
          },
          Input { key: Key::Char('e'), ctrl: false, .. } => {
            textarea.move_cursor(CursorMove::WordEnd)
          },
          Input { key: Key::Char('b'), ctrl: false, .. } => {
            textarea.move_cursor(CursorMove::WordBack)
          },
          Input { key: Key::Char('B'), ctrl: false, .. } => move_cursor_big_word_backward(textarea),
          Input { key: Key::Char('^'), .. } => textarea.move_cursor(CursorMove::Head),
          Input { key: Key::Char('0'), .. } => textarea.move_cursor(CursorMove::Head),
          Input { key: Key::Char('$'), .. } => textarea.move_cursor(CursorMove::End),
          Input { key: Key::Char('D'), .. } => {
            textarea.delete_line_by_end();
            return Transition::Mode(Mode::Normal);
          },
          Input { key: Key::Char('C'), .. } => {
            textarea.delete_line_by_end();
            textarea.cancel_selection();
            return Transition::Mode(Mode::Insert);
          },
          Input { key: Key::Char('p'), .. } => {
            #[cfg(feature = "arboard")]
            {
              Clipboard::new().map_or_else(
                |e| log::error!("{e:?}"),
                |mut clipboard| {
                  clipboard
                    .get_text()
                    .map_or_else(|e| log::error!("{e:?}"), |text| textarea.set_yank_text(text))
                },
              );
            }
            textarea.paste();
            return Transition::Mode(Mode::Normal);
          },
          Input { key: Key::Char('u'), ctrl: false, .. } => {
            textarea.undo();
            return Transition::Mode(Mode::Normal);
          },
          Input { key: Key::Char('r'), ctrl: true, .. } => {
            textarea.redo();
            return Transition::Mode(Mode::Normal);
          },
          Input { key: Key::Char('r'), ctrl: false, .. } => {
            return Transition::Mode(Mode::Replace);
          },
          Input { key: Key::Char('x'), ctrl: true, .. } if self.mode == Mode::Normal => {
            increment_number_under_cursor(textarea, -1);
          },
          Input { key: Key::Char('x'), .. } => {
            if !textarea.is_selecting() {
              textarea.start_selection();
            }
            if let Some(selection_range) = textarea.selection_range() {
              let selection_direction = get_selection_direction(selection_range, textarea.cursor());
              match selection_direction {
                SelectionDirection::Backward => {},
                _ => {
                  textarea.move_cursor(CursorMove::Forward); // Vim's forward text selection is inclusive
                },
              }
            }
            textarea.cut();
            self.send_copy_action_with_text(textarea.yank_text());
            return Transition::Mode(Mode::Normal);
          },
          Input { key: Key::Char('X'), .. } => {
            if self.mode == Mode::Visual {
              textarea.move_cursor(CursorMove::Head);
              textarea.start_selection();
              textarea.move_cursor(CursorMove::End);
            } else {
              textarea.start_selection();
              textarea.move_cursor(CursorMove::Back);
            }
            textarea.cut();
            self.send_copy_action_with_text(textarea.yank_text());
            return Transition::Mode(Mode::Normal);
          },
          Input { key: Key::Char('i'), ctrl: false, .. }
            if matches!(self.mode, Mode::Operator(_) | Mode::Visual) =>
          {
            // Wait for the text object that follows, e.g. `w` in `ciw`/`diw`/`yiw`/`viw`.
            return Transition::Pending(input);
          },
          Input { key: Key::Char('i'), .. } => {
            textarea.cancel_selection();
            return Transition::Mode(Mode::Insert);
          },
          Input { key: Key::Char('a'), ctrl: false, .. }
            if matches!(self.mode, Mode::Operator('d'))
              || matches!(self.mode, Mode::Operator('y')) =>
          {
            textarea.cancel_selection();
            textarea.move_cursor(CursorMove::Forward);
            textarea.move_cursor(CursorMove::WordBack);
            textarea.start_selection();
            return Transition::Nop;
          },
          Input { key: Key::Char('a'), ctrl: true, .. } if self.mode == Mode::Normal => {
            increment_number_under_cursor(textarea, 1);
          },
          Input { key: Key::Char('a'), .. } => {
            textarea.cancel_selection();
            textarea.move_cursor(CursorMove::Forward);
            return Transition::Mode(Mode::Insert);
          },
          Input { key: Key::Char('A'), .. } => {
            textarea.cancel_selection();
            textarea.move_cursor(CursorMove::End);
            return Transition::Mode(Mode::Insert);
          },
          Input { key: Key::Char('o'), .. } => {
            textarea.move_cursor(CursorMove::End);
            textarea.insert_newline();
            return Transition::Mode(Mode::Insert);
          },
          Input { key: Key::Char('O'), .. } => {
            textarea.move_cursor(CursorMove::Head);
            textarea.insert_newline();
            textarea.move_cursor(CursorMove::Up);
            return Transition::Mode(Mode::Insert);
          },
          Input { key: Key::Char('I'), .. } => {
            textarea.cancel_selection();
            textarea.move_cursor(CursorMove::Head);
            return Transition::Mode(Mode::Insert);
          },
          Input { key: Key::Char('e'), ctrl: true, .. } => textarea.scroll((1, 0)),
          Input { key: Key::Char('y'), ctrl: true, .. } => textarea.scroll((-1, 0)),
          Input { key: Key::Char('d'), ctrl: true, .. } => textarea.scroll(Scrolling::HalfPageDown),
          Input { key: Key::Char('u'), ctrl: true, .. } => textarea.scroll(Scrolling::HalfPageUp),
          Input { key: Key::Char('f'), ctrl: true, .. } | Input { key: Key::PageDown, .. } => {
            textarea.scroll(Scrolling::PageDown)
          },
          Input { key: Key::Char('b'), ctrl: true, .. } | Input { key: Key::PageUp, .. } => {
            textarea.scroll(Scrolling::PageUp)
          },
          Input { key: Key::Char('v'), ctrl: false, .. } if self.mode == Mode::Normal => {
            textarea.start_selection();
            return Transition::Mode(Mode::Visual);
          },
          Input { key: Key::Char('V'), ctrl: false, .. } if self.mode == Mode::Normal => {
            textarea.move_cursor(CursorMove::Head);
            textarea.start_selection();
            textarea.move_cursor(CursorMove::End);
            return Transition::Mode(Mode::Visual);
          },
          Input { key: Key::Esc, .. }
          | Input { key: Key::Char('c'), ctrl: true, .. }
          | Input { key: Key::Char('v'), ctrl: false, .. }
            if self.mode == Mode::Visual =>
          {
            textarea.cancel_selection();
            return Transition::Mode(Mode::Normal);
          },
          Input { key: Key::Char('g'), ctrl: false, .. }
            if matches!(self.pending, Input { key: Key::Char('g'), ctrl: false, .. }) =>
          {
            textarea.move_cursor(CursorMove::Top)
          },
          Input { key: Key::Char('G'), ctrl: false, .. } => {
            textarea.move_cursor(CursorMove::Bottom)
          },
          Input { key: Key::Char(c), ctrl: false, .. } if self.mode == Mode::Operator(c) => {
            // Handle yy, dd, cc. (This is not strictly the same behavior as Vim)
            textarea.move_cursor(CursorMove::Head);
            textarea.start_selection();
            let cursor = textarea.cursor();
            textarea.move_cursor(CursorMove::Down);
            if cursor == textarea.cursor() {
              textarea.move_cursor(CursorMove::End); // At the last line, move to end of the line instead
            }
          },
          Input { key: Key::Char(op @ ('y' | 'd' | 'c')), ctrl: false, .. }
            if self.mode == Mode::Normal =>
          {
            textarea.start_selection();
            return Transition::Mode(Mode::Operator(op));
          },
          Input { key: Key::Char('y'), ctrl: false, .. } if self.mode == Mode::Visual => {
            if let Some(selection_range) = textarea.selection_range() {
              let selection_direction = get_selection_direction(selection_range, textarea.cursor());
              match selection_direction {
                SelectionDirection::Backward => {},
                _ => {
                  textarea.move_cursor(CursorMove::Forward); // Vim's forward text selection is inclusive
                },
              }
            }
            textarea.copy();
            self.send_copy_action_with_text(textarea.yank_text());
            return Transition::Mode(Mode::Normal);
          },
          Input { key: Key::Char('d'), ctrl: false, .. } if self.mode == Mode::Visual => {
            if let Some(selection_range) = textarea.selection_range() {
              let selection_direction = get_selection_direction(selection_range, textarea.cursor());
              match selection_direction {
                SelectionDirection::Backward => {},
                _ => {
                  textarea.move_cursor(CursorMove::Forward); // Vim's forward text selection is inclusive
                },
              }
            }
            textarea.cut();
            return Transition::Mode(Mode::Normal);
          },
          Input { key: Key::Char('c'), ctrl: false, .. } if self.mode == Mode::Visual => {
            if let Some(selection_range) = textarea.selection_range() {
              let selection_direction = get_selection_direction(selection_range, textarea.cursor());
              match selection_direction {
                SelectionDirection::Backward => {},
                _ => {
                  textarea.move_cursor(CursorMove::Forward); // Vim's forward text selection is inclusive
                },
              }
            }
            textarea.cut();
            self.send_copy_action_with_text(textarea.yank_text());
            return Transition::Mode(Mode::Insert);
          },
          Input { key: Key::Char('S'), ctrl: false, .. } => {
            textarea.move_cursor(CursorMove::Head);
            textarea.start_selection();
            textarea.move_cursor(CursorMove::End);
            textarea.cut();
            self.send_copy_action_with_text(textarea.yank_text());
            return Transition::Mode(Mode::Insert);
          },
          Input { key: Key::Esc, .. } => {
            textarea.cancel_selection();
            return Transition::Mode(Mode::Normal);
          },
          input => return Transition::Pending(input),
        }

        // Handle the pending operator
        match self.mode {
          Mode::Operator('y') => {
            textarea.copy();
            self.send_copy_action_with_text(textarea.yank_text());
            Transition::Mode(Mode::Normal)
          },
          Mode::Operator('d') => {
            textarea.cut();
            Transition::Mode(Mode::Normal)
          },
          Mode::Operator('c') => {
            textarea.cut();
            self.send_copy_action_with_text(textarea.yank_text());
            Transition::Mode(Mode::Insert)
          },
          _ => Transition::Nop,
        }
      },
      Mode::Insert => {
        match input {
          Input { key: Key::Esc, .. } | Input { key: Key::Char('c'), ctrl: true, .. } => {
            Transition::Mode(Mode::Normal)
          },
          input => {
            textarea.input(input); // Use default key mappings in insert mode
            Transition::Mode(Mode::Insert)
          },
        }
      },
      Mode::Replace => match input {
        Input { key: Key::Esc, .. } | Input { key: Key::Char('c'), ctrl: true, .. } => {
          Transition::Mode(Mode::Normal)
        },
        input => {
          textarea.delete_str(1);
          textarea.input(input);
          Transition::Mode(Mode::Normal)
        },
      },
    }
  }

  fn send_copy_action_with_text(&self, text: String) {
    if let Some(sender) = &self.command_tx {
      sender.send(Action::CopyData(text)).map_or_else(|e| log::error!("{e:?}"), |_| {});
    }
  }
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum InnerWordKind {
  Space,
  Punct,
  Other,
}

impl InnerWordKind {
  fn of(c: char) -> Self {
    if c.is_whitespace() {
      Self::Space
    } else if c == '_' {
      Self::Other
    } else if c.is_ascii_punctuation() {
      Self::Punct
    } else {
      Self::Other
    }
  }
}

// Find the span (start, end) of the first decimal number on the line whose end touches or
// follows `start_col` (vim only searches forward on the current line, never wraps or looks
// backward), including an immediately-preceding minus sign.
fn find_number_span(chars: &[char], start_col: usize) -> Option<(usize, usize)> {
  let n = chars.len();
  let mut i = 0;
  while i < n {
    if chars[i].is_ascii_digit() {
      let digit_start = i;
      while i < n && chars[i].is_ascii_digit() {
        i += 1;
      }
      let start = if digit_start > 0 && chars[digit_start - 1] == '-' { digit_start - 1 } else { digit_start };
      if i > start_col {
        return Some((start, i));
      }
    } else {
      i += 1;
    }
  }
  None
}

// `Ctrl-A` / `Ctrl-X`: increment or decrement the next number on the line by `delta`, preserving
// zero-padded width (e.g. `007` -> `008`) the way Vim does.
fn increment_number_under_cursor(textarea: &mut TextArea, delta: i64) {
  let cursor = textarea.cursor();
  let chars: Vec<char> = textarea.lines()[cursor.0].chars().collect();
  let Some((start, end)) = find_number_span(&chars, cursor.1) else {
    return;
  };
  let text: String = chars[start..end].iter().collect();
  let Ok(value) = text.parse::<i64>() else {
    return;
  };
  let new_value = value.saturating_add(delta);
  let digit_start = if chars[start] == '-' { start + 1 } else { start };
  let width = end - digit_start;
  let has_leading_zero = width > 1 && chars[digit_start] == '0';
  let new_text =
    if has_leading_zero && new_value >= 0 { format!("{new_value:0width$}") } else { new_value.to_string() };
  let row = cursor.0;
  textarea.cancel_selection();
  textarea.move_cursor(CursorMove::Jump(row as u16, start as u16));
  textarea.delete_str(end - start);
  textarea.insert_str(&new_text);
  textarea.move_cursor(CursorMove::Back);
}

// Select the `iw` text object under the cursor: the run of same-kind characters (word,
// punctuation, or whitespace) touching the cursor, not crossing line boundaries.
fn select_inner_word(textarea: &mut TextArea, visual: bool) {
  let cursor = textarea.cursor();
  let chars: Vec<char> = textarea.lines()[cursor.0].chars().collect();
  if chars.is_empty() {
    return;
  }
  let col = cursor.1.min(chars.len() - 1);
  let kind = InnerWordKind::of(chars[col]);
  let mut start = col;
  while start > 0 && InnerWordKind::of(chars[start - 1]) == kind {
    start -= 1;
  }
  let mut end = col;
  while end + 1 < chars.len() && InnerWordKind::of(chars[end + 1]) == kind {
    end += 1;
  }
  let row = cursor.0;
  // Operator-pending mode wants an exclusive end (`end + 1`); Visual mode's y/d/c/x handlers
  // already nudge the cursor forward by one to account for inclusive selection, so land one
  // character earlier here to avoid double-counting.
  let target = if visual { end } else { end + 1 };
  textarea.cancel_selection();
  textarea.move_cursor(CursorMove::Jump(row as u16, start as u16));
  textarea.start_selection();
  textarea.move_cursor(CursorMove::Jump(row as u16, target as u16));
}

// Select the `iW` text object under the cursor: the run of whitespace or non-whitespace
// characters touching the cursor, not crossing line boundaries.
fn select_inner_big_word(textarea: &mut TextArea, visual: bool) {
  let cursor = textarea.cursor();
  let chars: Vec<char> = textarea.lines()[cursor.0].chars().collect();
  if chars.is_empty() {
    return;
  }
  let col = cursor.1.min(chars.len() - 1);
  let is_space = chars[col].is_whitespace();
  let mut start = col;
  while start > 0 && chars[start - 1].is_whitespace() == is_space {
    start -= 1;
  }
  let mut end = col;
  while end + 1 < chars.len() && chars[end + 1].is_whitespace() == is_space {
    end += 1;
  }
  let row = cursor.0;
  let target = if visual { end } else { end + 1 };
  textarea.cancel_selection();
  textarea.move_cursor(CursorMove::Jump(row as u16, start as u16));
  textarea.start_selection();
  textarea.move_cursor(CursorMove::Jump(row as u16, target as u16));
}

// Select the `i"` (or other quote char) text object: the text strictly between the nearest
// pair of quote characters on the current line, at or after the cursor. Does not cross lines
// and does not handle escaped quotes.
fn select_inner_quoted(textarea: &mut TextArea, quote: char, visual: bool) {
  let cursor = textarea.cursor();
  let chars: Vec<char> = textarea.lines()[cursor.0].chars().collect();
  let positions: Vec<usize> =
    chars.iter().enumerate().filter(|(_, c)| **c == quote).map(|(i, _)| i).collect();
  let col = cursor.1;
  let Some(pair) = positions.chunks_exact(2).find(|p| p[1] >= col) else {
    return;
  };
  let (start, end) = (pair[0], pair[1]);
  if end <= start + 1 {
    return; // empty quotes, nothing between them
  }
  let row = cursor.0;
  let target = if visual { end - 1 } else { end };
  textarea.cancel_selection();
  textarea.move_cursor(CursorMove::Jump(row as u16, (start + 1) as u16));
  textarea.start_selection();
  textarea.move_cursor(CursorMove::Jump(row as u16, target as u16));
}

// Unlike a (small) word, a WORD is only delimited by whitespace, e.g. `foo(a).bar` is one WORD.
fn find_big_word_start_forward(line: &str, start_col: usize) -> Option<usize> {
  let chars: Vec<char> = line.chars().collect();
  let n = chars.len();
  let mut i = start_col.min(n);
  while i < n && !chars[i].is_whitespace() {
    i += 1;
  }
  while i < n && chars[i].is_whitespace() {
    i += 1;
  }
  (i < n).then_some(i)
}

fn find_big_word_start_backward(line: &str, start_col: usize) -> Option<usize> {
  let chars: Vec<char> = line.chars().collect();
  let p = start_col.min(chars.len());
  if p == 0 {
    return None;
  }
  let mut i = p - 1;
  while i > 0 && chars[i].is_whitespace() {
    i -= 1;
  }
  if chars[i].is_whitespace() {
    return None;
  }
  while i > 0 && !chars[i - 1].is_whitespace() {
    i -= 1;
  }
  Some(i)
}

fn move_cursor_big_word_forward(textarea: &mut TextArea) {
  let cursor = textarea.cursor();
  let lines = textarea.lines();
  let target = if let Some(col) = find_big_word_start_forward(&lines[cursor.0], cursor.1) {
    (cursor.0, col)
  } else if cursor.0 + 1 < lines.len() {
    (cursor.0 + 1, 0)
  } else {
    (cursor.0, lines[cursor.0].chars().count())
  };
  textarea.move_cursor(CursorMove::Jump(target.0 as u16, target.1 as u16));
}

fn move_cursor_big_word_backward(textarea: &mut TextArea) {
  let cursor = textarea.cursor();
  let lines = textarea.lines();
  let target = if let Some(col) = find_big_word_start_backward(&lines[cursor.0], cursor.1) {
    (cursor.0, col)
  } else if cursor.0 > 0 {
    let row = cursor.0 - 1;
    (row, lines[row].chars().count())
  } else {
    (cursor.0, 0)
  };
  textarea.move_cursor(CursorMove::Jump(target.0 as u16, target.1 as u16));
}
