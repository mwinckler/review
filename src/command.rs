pub(crate) fn expand(argument: &str, replacements: &[(&str, &str)]) -> String {
    argument
        .split_inclusive('}')
        .map(|part| {
            if let Some((token, value)) =
                replacements.iter().find(|(token, _)| part.ends_with(token))
            {
                format!("{}{value}", &part[..part.len() - token.len()])
            } else {
                part.to_owned()
            }
        })
        .collect()
}
