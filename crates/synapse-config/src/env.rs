use std::sync::OnceLock;

use regex::Regex;

/// Expand `{{ env.VAR }}` placeholders in a raw TOML string
///
/// Supports an optional default value via `{{ env.VAR | default("fallback") }}`.
/// When a default is provided and the variable is unset, the default is used
/// instead of returning an error.
///
/// This replaces `DynamicString<T>` by operating on the raw config text
/// before deserialization, so config structs use plain String/SecretString.
/// Lines starting with `#` (TOML comments) are passed through unchanged.
pub fn expand_env(input: &str) -> Result<String, String> {
    fn re() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        // Matches `{{ env.VAR }}` and `{{ env.VAR | default("fallback") }}`
        // Also handles TOML-escaped quotes: `{{ env.VAR | default(\"fallback\") }}`
        // Group 1: the key (e.g. `env.VAR_NAME`)
        // Group 2: optional default value inside default("...")
        RE.get_or_init(|| {
            Regex::new(r#"\{\{\s*([a-zA-Z0-9_.]+)\s*(?:\|\s*default\(\\?"([^"\\]*)\\?"\))?\s*\}\}"#)
                .expect("must be valid regex")
        })
    }

    let mut output = String::with_capacity(input.len());

    for (i, line) in input.lines().enumerate() {
        if i > 0 {
            output.push('\n');
        }

        // Skip expansion for comment lines
        if line.trim_start().starts_with('#') {
            output.push_str(line);
            continue;
        }

        let mut result = String::with_capacity(line.len());
        let mut last_end = 0;

        for captures in re().captures_iter(line) {
            let overall = captures.get(0).unwrap();
            let key = captures.get(1).unwrap().as_str();
            let default_value = captures.get(2).map(|m| m.as_str());

            result.push_str(&line[last_end..overall.start()]);

            let mut parts = key.split('.');
            match (parts.next(), parts.next(), parts.next()) {
                (Some("env"), Some(var_name), None) => match std::env::var(var_name) {
                    Ok(value) => result.push_str(&value),
                    Err(_) => match default_value {
                        Some(default) => result.push_str(default),
                        None => {
                            return Err(format!("environment variable not found: `{var_name}`"));
                        }
                    },
                },
                _ => {
                    return Err(format!("only variables scoped with 'env.' are supported: `{key}`"));
                }
            }

            last_end = overall.end();
        }

        result.push_str(&line[last_end..]);
        output.push_str(&result);
    }

    // Preserve trailing newline if present
    if input.ends_with('\n') {
        output.push('\n');
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_placeholders() {
        let input = "key = \"value\"";
        assert_eq!(expand_env(input).unwrap(), input);
    }

    #[test]
    fn single_env_var() {
        temp_env::with_var("TEST_VAR", Some("hello"), || {
            let result = expand_env("key = \"{{ env.TEST_VAR }}\"").unwrap();
            assert_eq!(result, "key = \"hello\"");
        });
    }

    #[test]
    fn multiple_env_vars() {
        let vars = [("FOO", Some("foo")), ("BAR", Some("bar"))];
        temp_env::with_vars(vars, || {
            let result = expand_env("a = \"{{ env.FOO }}\"\nb = \"{{ env.BAR }}\"").unwrap();
            assert_eq!(result, "a = \"foo\"\nb = \"bar\"");
        });
    }

    #[test]
    fn missing_env_var() {
        temp_env::with_var_unset("MISSING_VAR", || {
            let err = expand_env("key = \"{{ env.MISSING_VAR }}\"").unwrap_err();
            assert!(err.contains("MISSING_VAR"));
        });
    }

    #[test]
    fn unsupported_scope() {
        let err = expand_env("key = \"{{ foo.BAR }}\"").unwrap_err();
        assert!(err.contains("only variables scoped with 'env.'"));
    }

    #[test]
    fn commented_lines_skip_expansion() {
        temp_env::with_var_unset("MISSING_VAR", || {
            let input = "# key = \"{{ env.MISSING_VAR }}\"";
            let result = expand_env(input).unwrap();
            assert_eq!(result, input);
        });
    }

    #[test]
    fn indented_comment_skips_expansion() {
        temp_env::with_var_unset("MISSING_VAR", || {
            let input = "  # key = \"{{ env.MISSING_VAR }}\"";
            let result = expand_env(input).unwrap();
            assert_eq!(result, input);
        });
    }

    #[test]
    fn mixed_comments_and_values() {
        let vars = [("REAL_VAR", Some("value"))];
        temp_env::with_vars(vars, || {
            temp_env::with_var_unset("COMMENTED_VAR", || {
                let input = "# secret = \"{{ env.COMMENTED_VAR }}\"\nkey = \"{{ env.REAL_VAR }}\"";
                let result = expand_env(input).unwrap();
                assert_eq!(result, "# secret = \"{{ env.COMMENTED_VAR }}\"\nkey = \"value\"");
            });
        });
    }

    #[test]
    fn default_used_when_var_missing() {
        temp_env::with_var_unset("OPTIONAL_VAR", || {
            let result = expand_env("key = \"{{ env.OPTIONAL_VAR | default(\"\") }}\"").unwrap();
            assert_eq!(result, "key = \"\"");
        });
    }

    #[test]
    fn default_not_used_when_var_present() {
        temp_env::with_var("OPTIONAL_VAR", Some("actual"), || {
            let result = expand_env("key = \"{{ env.OPTIONAL_VAR | default(\"fallback\") }}\"").unwrap();
            assert_eq!(result, "key = \"actual\"");
        });
    }

    #[test]
    fn default_with_nonempty_fallback() {
        temp_env::with_var_unset("MISSING_VAR", || {
            let result = expand_env("key = \"{{ env.MISSING_VAR | default(\"fallback\") }}\"").unwrap();
            assert_eq!(result, "key = \"fallback\"");
        });
    }

    #[test]
    fn default_with_toml_escaped_quotes() {
        temp_env::with_var_unset("OTEL_ENDPOINT", || {
            let result =
                expand_env(r#"endpoint = "{{ env.OTEL_ENDPOINT | default(\"http://otel-collector:4317\") }}""#)
                    .unwrap();
            assert_eq!(result, r#"endpoint = "http://otel-collector:4317""#);
        });
    }

    #[test]
    fn default_with_toml_escaped_quotes_var_present() {
        temp_env::with_var("OTEL_ENDPOINT", Some("http://custom:4317"), || {
            let result =
                expand_env(r#"endpoint = "{{ env.OTEL_ENDPOINT | default(\"http://otel-collector:4317\") }}""#)
                    .unwrap();
            assert_eq!(result, r#"endpoint = "http://custom:4317""#);
        });
    }

    #[test]
    fn missing_var_without_default_still_errors() {
        temp_env::with_var_unset("REQUIRED_VAR", || {
            let err = expand_env("key = \"{{ env.REQUIRED_VAR }}\"").unwrap_err();
            assert!(err.contains("REQUIRED_VAR"));
        });
    }
}
