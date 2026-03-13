use std::time::Duration;

use serde::Deserialize;

/// CORS configuration
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorsConfig {
    /// Allowed origins (wildcard "*" or explicit list)
    #[serde(default)]
    pub origins: AnyOrArray,
    /// Allowed HTTP methods (wildcard "*" or explicit list)
    #[serde(default)]
    pub methods: AnyOrArray,
    /// Allowed headers (wildcard "*" or explicit list)
    #[serde(default)]
    pub headers: AnyOrArray,
    /// Headers to expose to the browser
    #[serde(default)]
    pub expose_headers: Vec<String>,
    /// Allow credentials
    #[serde(default)]
    pub credentials: bool,
    /// Max age for preflight cache in seconds
    #[serde(default)]
    pub max_age: Option<u64>,
    /// Allow private network access (CORS-RFC1918)
    #[serde(default)]
    pub private_network: bool,
}

/// Either a wildcard "*" or explicit list of values
#[derive(Debug, Default, Clone)]
pub enum AnyOrArray {
    /// Match any value
    #[default]
    Any,
    /// Explicit list
    List(Vec<String>),
}

impl<'de> Deserialize<'de> for AnyOrArray {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de;

        struct AnyOrArrayVisitor;

        impl<'de> de::Visitor<'de> for AnyOrArrayVisitor {
            type Value = AnyOrArray;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("\"*\" or array of strings")
            }

            fn visit_str<E>(self, v: &str) -> Result<AnyOrArray, E>
            where
                E: de::Error,
            {
                if v == "*" {
                    Ok(AnyOrArray::Any)
                } else {
                    Ok(AnyOrArray::List(vec![v.to_string()]))
                }
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<AnyOrArray, A::Error>
            where
                A: de::SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while let Some(val) = seq.next_element::<String>()? {
                    if val == "*" {
                        return Ok(AnyOrArray::Any);
                    }
                    values.push(val);
                }
                Ok(AnyOrArray::List(values))
            }
        }

        deserializer.deserialize_any(AnyOrArrayVisitor)
    }
}

impl CorsConfig {
    /// Get max age as Duration
    pub fn max_age_duration(&self) -> Option<Duration> {
        self.max_age.map(Duration::from_secs)
    }
}
