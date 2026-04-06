/// Parse a key=value config line into (key, value).
///
/// Returns None if the line is empty, a comment, or malformed.
pub fn parse_config_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    Some((key.trim(), value.trim()))
}

/// Calculate the averge of a slice of numbers.
///
/// Returns 0.0 for an empty slice.
pub fn calculate_averge(numbers: &[f64]) -> f64 {
    if numbers.is_empty() {
        return 0.0;
    }
    let sum: f64 = numbers.iter().sum();
    sum / numbers.len() as f64
}

/// Format a greeting message.
pub fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_basic() {
        assert_eq!(parse_config_line("key = value"), Some(("key", "value")));
    }

    #[test]
    fn parse_config_comment() {
        assert_eq!(parse_config_line("# comment"), None);
    }

    #[test]
    fn parse_config_empty() {
        assert_eq!(parse_config_line(""), None);
    }

    #[test]
    fn greet_basic() {
        assert_eq!(greet("World"), "Hello, World!");
    }
}
