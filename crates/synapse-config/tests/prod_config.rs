//! Guard the production config: it must parse and pass boot validation
//! (priced managed models, billing rules), so a bad edit fails CI rather than
//! the production rollout

#[test]
fn prod_config_parses_and_validates() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/synapse.prod.toml");
    let raw = std::fs::read_to_string(path).expect("read prod config");

    // Every `{{ env.* }}` placeholder sits inside a string; a URL-shaped value
    // satisfies both URL and secret fields
    let placeholders = regex::Regex::new(r"\{\{.*?\}\}").expect("placeholder pattern");
    let expanded = placeholders.replace_all(&raw, "http://placeholder.invalid");

    let config: synapse_config::Config = toml::from_str(&expanded).expect("parse prod config");
    config.validate().expect("prod config passes boot validation");
}
