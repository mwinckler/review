use ratatui::text::{Line, Span, Text};

/// Sanitize remote text before parsing, keeping source newlines and tabs intact.
pub(crate) fn render(body: &str, width: u16) -> Text<'static> {
    let mut safe = String::new();
    for line in body.lines() {
        if (line.starts_with("<!-- review:") || line.starts_with("<!-- review-batch:"))
            && line.ends_with(" -->")
        {
            continue;
        }
        for ch in line.chars() {
            if ch.is_control() && ch != '\t' {
                safe.extend(ch.escape_default());
            } else {
                safe.push(ch);
            }
        }
        safe.push('\n');
    }
    let options = tui_markdown::Options::default().table_width(width.max(1));
    let text = tui_markdown::from_str_with_options(&safe, &options);
    Text {
        lines: text
            .lines
            .into_iter()
            .map(|line| Line {
                spans: line
                    .spans
                    .into_iter()
                    .map(|span| Span::styled(span.content.into_owned(), span.style))
                    .collect(),
                style: line.style,
                alignment: line.alignment,
            })
            .collect(),
        style: text.style,
        alignment: text.alignment,
    }
}
