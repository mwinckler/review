use crossterm::event::KeyCode;

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
