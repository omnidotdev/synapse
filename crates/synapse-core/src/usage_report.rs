//! Reporting a served request's usage to the caller's usage dashboard
//!
//! Every billed modality (completions, embeddings, speech, images) reports
//! what it served through one request extension, so each reaches synapse-api
//! the same way. synapse-api's per-product daily totals are what Aether's
//! reconciliation compares its charges against, so a modality that does not
//! report reads as served spend with no record behind it

use std::fmt;
use std::sync::Arc;

/// Kind of request served, named like the reference type of its Aether charge
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modality {
    /// Chat completion or message
    Completion,
    /// Text embeddings
    Embedding,
    /// Speech-to-text
    Transcription,
    /// Text-to-speech
    Speech,
    /// Image generation
    ImageGeneration,
    /// Image edit from reference images
    ImageEdit,
}

impl Modality {
    /// Wire name, identical to the charge's Aether reference type
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completion => "completion",
            Self::Embedding => "embedding",
            Self::Transcription => "transcription",
            Self::Speech => "speech",
            Self::ImageGeneration => "image_generation",
            Self::ImageEdit => "image_edit",
        }
    }
}

impl fmt::Display for Modality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What one served request used and was charged
#[derive(Debug, Clone, PartialEq)]
pub struct ServedUsage {
    /// Kind of request served
    pub modality: Modality,
    /// Provider that served it
    pub provider: String,
    /// Model that served it
    pub model: String,
    /// Input units (tokens; audio bytes for speech-to-text, characters for
    /// text-to-speech, images for image generation)
    pub input_tokens: u32,
    /// Output tokens
    pub output_tokens: u32,
    /// Retail USD charged for it, at the ledger's $0.0001 precision exactly
    /// like the charge (0 when nothing was charged, e.g. BYOK)
    pub charged_usd: f64,
}

/// Where a request reports its served usage
///
/// The auth layer installs one per authenticated request, bound to the key it
/// resolved; a request without one (unauthenticated, or reporting disabled)
/// reports nothing
#[derive(Clone)]
pub struct UsageReportSink(Arc<dyn Fn(ServedUsage) + Send + Sync>);

impl UsageReportSink {
    /// A sink handing each report to `report`
    pub fn new(report: impl Fn(ServedUsage) + Send + Sync + 'static) -> Self {
        Self(Arc::new(report))
    }

    /// Report one served request
    pub fn report(&self, usage: ServedUsage) {
        (self.0)(usage);
    }
}

impl fmt::Debug for UsageReportSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UsageReportSink")
    }
}

/// Report a served request through the sink installed on its extensions, if any
pub fn report_served_usage(extensions: &http::Extensions, usage: ServedUsage) {
    if let Some(sink) = extensions.get::<UsageReportSink>() {
        sink.report(usage);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn usage() -> ServedUsage {
        ServedUsage {
            modality: Modality::Embedding,
            provider: "openai".to_owned(),
            model: "text-embedding-3-small".to_owned(),
            input_tokens: 40,
            output_tokens: 0,
            charged_usd: 0.0001,
        }
    }

    #[test]
    fn a_request_reports_through_its_sink() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        let mut extensions = http::Extensions::new();
        extensions.insert(UsageReportSink::new(move |u| {
            sink_seen.lock().unwrap().push(u);
        }));

        report_served_usage(&extensions, usage());

        let seen = std::mem::take(&mut *seen.lock().unwrap());
        assert_eq!(seen, vec![usage()]);
    }

    #[test]
    fn a_request_without_a_sink_reports_nothing() {
        report_served_usage(&http::Extensions::new(), usage());
    }

    #[test]
    fn modalities_are_named_like_their_charge_reference_types() {
        let names: Vec<_> = [
            Modality::Completion,
            Modality::Embedding,
            Modality::Transcription,
            Modality::Speech,
            Modality::ImageGeneration,
            Modality::ImageEdit,
        ]
        .iter()
        .map(|m| m.as_str())
        .collect();
        assert_eq!(
            names,
            [
                "completion",
                "embedding",
                "transcription",
                "speech",
                "image_generation",
                "image_edit"
            ]
        );
    }
}
