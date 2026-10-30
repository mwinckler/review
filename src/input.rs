use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub(crate) fn insert(text: &mut String, cursor: &mut usize, value: &str) {
    let value: String = value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .collect();
    text.insert_str(*cursor, &value);
    *cursor += value.len();
}

pub(crate) fn multiline(text: &mut String, cursor: &mut usize, key: KeyEvent) {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        if key.code == KeyCode::Char('u') && key.modifiers.contains(KeyModifiers::CONTROL) {
            text.clear();
            *cursor = 0;
        }
        return;
    }
    if edit(text, cursor, key.code) {
        return;
    }
    match key.code {
        KeyCode::Up | KeyCode::Down => {
            let start = text[..*cursor]
                .rfind('\n')
                .map(|index| index + 1)
                .unwrap_or(0);
            let column = text[start..*cursor].chars().count();
            let target = if key.code == KeyCode::Up {
                start
                    .checked_sub(1)
                    .map(|end| text[..end].rfind('\n').map(|index| index + 1).unwrap_or(0))
            } else {
                text[*cursor..]
                    .find('\n')
                    .map(|offset| *cursor + offset + 1)
            };
            if let Some(target) = target {
                *cursor = target
                    + text[target..]
                        .split('\n')
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(column)
                        .map(char::len_utf8)
                        .sum::<usize>();
            }
        }
        KeyCode::Enter => insert(text, cursor, "\n"),
        KeyCode::Tab => insert(text, cursor, "\t"),
        KeyCode::Char(ch) => insert(text, cursor, &ch.to_string()),
        _ => {}
    }
}

pub(crate) fn edit(text: &mut String, cursor: &mut usize, code: KeyCode) -> bool {
    let previous = text[..*cursor]
        .char_indices()
        .next_back()
        .map(|(index, _)| index)
        .unwrap_or(0);
    let next = text[*cursor..]
        .chars()
        .next()
        .map(|ch| *cursor + ch.len_utf8())
        .unwrap_or(*cursor);
    match code {
        KeyCode::Left => *cursor = previous,
        KeyCode::Right => *cursor = next,
        KeyCode::Home => {
            *cursor = text[..*cursor]
                .rfind('\n')
                .map(|index| index + 1)
                .unwrap_or(0);
        }
        KeyCode::End => {
            *cursor = text[*cursor..]
                .find('\n')
                .map(|index| index + *cursor)
                .unwrap_or(text.len());
        }
        KeyCode::Backspace => {
            text.drain(previous..*cursor);
            *cursor = previous;
        }
        KeyCode::Delete => {
            text.drain(*cursor..next);
        }
        _ => return false,
    }
    true
}
