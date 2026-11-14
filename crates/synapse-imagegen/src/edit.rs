//! Image edit requests (`POST /v1/images/edits`), parsed from multipart form
//! data and forwarded upstream part for part

use axum::body::Bytes;

use crate::error::{ImageGenError, Result};

/// Most input images one edit accepts
pub const MAX_IMAGES: usize = 16;

/// Most images one edit produces
const MAX_N: u32 = 10;

/// Accepted `quality` values
const QUALITIES: [&str; 4] = ["low", "medium", "high", "auto"];

/// Accepted `input_fidelity` values
const INPUT_FIDELITIES: [&str; 2] = ["high", "low"];

/// One part of the multipart form, kept byte for byte so it is forwarded
/// upstream unchanged
#[derive(Debug, Clone)]
pub struct FormPart {
    /// Field name (e.g. `image[]`, `prompt`)
    pub name: String,
    /// File name, for file parts
    pub file_name: Option<String>,
    /// Content type, when the client sent one
    pub content_type: Option<String>,
    /// Raw part body
    pub data: Bytes,
}

/// A validated image edit request
#[derive(Debug, Clone)]
pub struct ImageEditRequest {
    /// Model identifier as sent (e.g. `openai/gpt-image-1.5`)
    pub model: String,
    /// Edit instructions
    pub prompt: String,
    /// Number of images to produce
    pub n: u32,
    /// Requested quality, if any
    pub quality: Option<String>,
    /// Requested size, if any
    pub size: Option<String>,
    /// Requested input fidelity, if any
    pub input_fidelity: Option<String>,
    /// Number of input images (`image[]` or `image` parts)
    pub image_count: usize,
    /// Whether a mask image was sent
    pub has_mask: bool,
    /// Every form part in the order received
    pub parts: Vec<FormPart>,
}

/// Whether a field carries an input image
fn is_image_field(name: &str) -> bool {
    name == "image" || name == "image[]"
}

/// Read a text field as UTF-8
fn text(part: &FormPart) -> Result<String> {
    String::from_utf8(part.data.to_vec())
        .map(|value| value.trim().to_owned())
        .map_err(|_| ImageGenError::InvalidRequest(format!("field '{}' must be UTF-8 text", part.name)))
}

/// Check an optional field against its accepted values
fn check_choice(field: &str, value: Option<&str>, accepted: &[&str]) -> Result<()> {
    match value {
        Some(value) if !accepted.contains(&value) => Err(ImageGenError::InvalidRequest(format!(
            "'{field}' must be one of {}, got '{value}'",
            accepted.join(", ")
        ))),
        _ => Ok(()),
    }
}

impl ImageEditRequest {
    /// Validate the parts of an image edit form
    ///
    /// # Errors
    ///
    /// Returns `InvalidRequest` when a required field is missing, the image
    /// count is outside 1 to 16, a choice field has an unsupported value, or
    /// streaming is requested
    pub fn from_parts(parts: Vec<FormPart>) -> Result<Self> {
        let mut model = None;
        let mut prompt = None;
        let mut n = 1;
        let mut quality = None;
        let mut size = None;
        let mut input_fidelity = None;
        let mut image_count = 0;
        let mut has_mask = false;

        for part in &parts {
            match part.name.as_str() {
                name if is_image_field(name) => image_count += 1,
                "mask" => has_mask = true,
                "model" => model = Some(text(part)?),
                "prompt" => prompt = Some(text(part)?),
                "quality" => quality = Some(text(part)?),
                "size" => size = Some(text(part)?),
                "input_fidelity" => input_fidelity = Some(text(part)?),
                "n" => {
                    let value = text(part)?;
                    n = value
                        .parse::<u32>()
                        .ok()
                        .filter(|n| (1..=MAX_N).contains(n))
                        .ok_or_else(|| {
                            ImageGenError::InvalidRequest(format!("'n' must be an integer from 1 to {MAX_N}"))
                        })?;
                }
                // The response is read as one JSON body, so a streamed
                // response could be neither returned nor billed
                "stream" if text(part)? == "true" => {
                    return Err(ImageGenError::InvalidRequest(
                        "streaming image edits are not supported".to_owned(),
                    ));
                }
                _ => {}
            }
        }

        let model = model
            .filter(|m| !m.is_empty())
            .ok_or_else(|| ImageGenError::InvalidRequest("missing required field 'model'".to_owned()))?;
        let prompt = prompt
            .filter(|p| !p.is_empty())
            .ok_or_else(|| ImageGenError::InvalidRequest("missing required field 'prompt'".to_owned()))?;
        if !(1..=MAX_IMAGES).contains(&image_count) {
            return Err(ImageGenError::InvalidRequest(format!(
                "'image[]' must carry 1 to {MAX_IMAGES} images, got {image_count}"
            )));
        }
        check_choice("quality", quality.as_deref(), &QUALITIES)?;
        check_choice("input_fidelity", input_fidelity.as_deref(), &INPUT_FIDELITIES)?;

        Ok(Self {
            model,
            prompt,
            n,
            quality,
            size,
            input_fidelity,
            image_count,
            has_mask,
            parts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, value: &str) -> FormPart {
        FormPart {
            name: name.to_owned(),
            file_name: None,
            content_type: None,
            data: Bytes::from(value.to_owned()),
        }
    }

    fn image(name: &str) -> FormPart {
        FormPart {
            name: name.to_owned(),
            file_name: Some("ref.png".to_owned()),
            content_type: Some("image/png".to_owned()),
            data: Bytes::from_static(b"\x89PNG"),
        }
    }

    fn base() -> Vec<FormPart> {
        vec![
            field("model", "openai/gpt-image-1.5"),
            field("prompt", "a sticker"),
            image("image[]"),
        ]
    }

    #[test]
    fn parses_fields_and_counts_images() {
        let mut parts = base();
        parts.extend([
            image("image[]"),
            image("image"),
            image("mask"),
            field("quality", "medium"),
            field("size", "1024x1024"),
            field("n", "2"),
            field("input_fidelity", "high"),
            field("background", "transparent"),
        ]);

        let request = ImageEditRequest::from_parts(parts).unwrap();

        assert_eq!(request.model, "openai/gpt-image-1.5");
        assert_eq!(request.prompt, "a sticker");
        assert_eq!(request.image_count, 3);
        assert!(request.has_mask);
        assert_eq!(request.n, 2);
        assert_eq!(request.quality.as_deref(), Some("medium"));
        assert_eq!(request.size.as_deref(), Some("1024x1024"));
        assert_eq!(request.input_fidelity.as_deref(), Some("high"));
        // Unknown fields are kept for forwarding
        assert_eq!(request.parts.len(), 11);
    }

    #[test]
    fn defaults_to_one_image() {
        let request = ImageEditRequest::from_parts(base()).unwrap();
        assert_eq!(request.n, 1);
        assert!(!request.has_mask);
    }

    #[test]
    fn rejects_missing_model_prompt_or_images() {
        let without = |name: &str| base().into_iter().filter(|p| p.name != name).collect::<Vec<_>>();

        for name in ["model", "prompt", "image[]"] {
            assert!(
                matches!(
                    ImageEditRequest::from_parts(without(name)),
                    Err(ImageGenError::InvalidRequest(_))
                ),
                "missing {name} must be rejected"
            );
        }
    }

    #[test]
    fn rejects_more_than_sixteen_images() {
        let mut parts = base();
        parts.extend((0..MAX_IMAGES).map(|_| image("image[]")));
        assert!(ImageEditRequest::from_parts(parts).is_err());
    }

    #[test]
    fn rejects_unsupported_choices_and_streaming() {
        for (name, value) in [
            ("quality", "hd"),
            ("input_fidelity", "medium"),
            ("n", "0"),
            ("n", "11"),
            ("n", "two"),
            ("stream", "true"),
        ] {
            let mut parts = base();
            parts.push(field(name, value));
            assert!(
                ImageEditRequest::from_parts(parts).is_err(),
                "{name}={value} must be rejected"
            );
        }
    }
}
