use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Apply one SGR parameter list (`ESC[...m`) to `st`.
fn sgr(st: Style, params: &str) -> Style {
    let nums: Vec<u16> = if params.is_empty() {
        vec![0]
    } else {
        params.split([';', ':']).map(|p| p.parse().unwrap_or(0)).collect()
    };
    let mut st = st;
    let mut i = 0;
    while i < nums.len() {
        let n = nums[i];
        i += 1;
        match n {
            0 => st = Style::new(),
            1 => st = st.add_modifier(Modifier::BOLD),
            2 => st = st.add_modifier(Modifier::DIM),
            3 => st = st.add_modifier(Modifier::ITALIC),
            4 => st = st.add_modifier(Modifier::UNDERLINED),
            7 => st = st.add_modifier(Modifier::REVERSED),
            9 => st = st.add_modifier(Modifier::CROSSED_OUT),
            22 => st = st.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => st = st.remove_modifier(Modifier::ITALIC),
            24 => st = st.remove_modifier(Modifier::UNDERLINED),
            27 => st = st.remove_modifier(Modifier::REVERSED),
            29 => st = st.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => st = st.fg(Color::Indexed((n - 30) as u8)),
            39 => st = st.fg(Color::Reset),
            40..=47 => st = st.bg(Color::Indexed((n - 40) as u8)),
            49 => st = st.bg(Color::Reset),
            90..=97 => st = st.fg(Color::Indexed((n - 90 + 8) as u8)),
            100..=107 => st = st.bg(Color::Indexed((n - 100 + 8) as u8)),
            38 | 48 => {
                let color = match nums.get(i) {
                    Some(5) => nums.get(i + 1).map(|&c| (Color::Indexed(c as u8), 2)),
                    Some(2) => match nums.get(i + 1..i + 4) {
                        Some(&[r, g, b]) => Some((Color::Rgb(r as u8, g as u8, b as u8), 4)),
                        _ => None,
                    },
                    _ => None,
                };
                match color {
                    Some((c, used)) => {
                        st = if n == 38 { st.fg(c) } else { st.bg(c) };
                        i += used;
                    }
                    None => break,
                }
            }
            _ => {}
        }
    }
    st
}

/// Convert text containing ANSI SGR colour codes (from `tmux capture-pane -e`)
/// into styled lines. Other escape sequences are dropped.
pub fn lines(s: &str) -> Vec<Line<'static>> {
    let mut out = vec![];
    let mut spans: Vec<Span<'static>> = vec![];
    let mut buf = String::new();
    let mut st = Style::new();
    let mut chars = s.chars().peekable();

    let flush = |buf: &mut String, spans: &mut Vec<Span<'static>>, st: Style| {
        if !buf.is_empty() {
            spans.push(Span::styled(std::mem::take(buf), st));
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\n' => {
                flush(&mut buf, &mut spans, st);
                out.push(Line::from(std::mem::take(&mut spans)));
            }
            '\r' => {}
            '\t' => buf.push_str("    "),
            '\x1b' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    let mut params = String::new();
                    let mut fin = None;
                    for c in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            fin = Some(c);
                            break;
                        }
                        params.push(c);
                    }
                    if fin == Some('m') {
                        flush(&mut buf, &mut spans, st);
                        st = sgr(st, &params);
                    }
                }
                Some(']') => {
                    // OSC: skip to BEL or ST.
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\x07' || (c == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            c if c.is_control() => {}
            c => buf.push(c),
        }
    }
    flush(&mut buf, &mut spans, st);
    if !spans.is_empty() {
        out.push(Line::from(spans));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_and_resets() {
        let l = lines("a\x1b[31mred\x1b[0m b\n\x1b[38;2;1;2;3mx\x1b[m");
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].spans[1].content, "red");
        assert_eq!(l[0].spans[1].style.fg, Some(Color::Indexed(1)));
        assert_eq!(l[0].spans[2].style, Style::new());
        assert_eq!(l[1].spans[0].style.fg, Some(Color::Rgb(1, 2, 3)));
    }

    #[test]
    fn drops_other_escapes() {
        let l = lines("\x1b[2Khi\x1b]0;title\x07!");
        assert_eq!(l[0].spans[0].content, "hi!");
    }
}
