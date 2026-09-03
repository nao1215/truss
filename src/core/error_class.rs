//! The vocabulary of failure classes the adapters share.
//!
//! Every failure truss reports belongs to one class, and the class is what a caller
//! branches on. `docs/problems.md` is the canonical description of the classes: each
//! [`ErrorClass`] here is one section of that page, and [`ErrorClass::slug`] is that
//! section's anchor.
//!
//! The class is spelled three ways because the adapters speak three languages, and all
//! three spellings come from this one table: the HTTP server puts the slug in the RFC 9457
//! `type` URI, the Wasm package reports [`ErrorClass::camel_case_name`] as `kind`, and the
//! CLI prints the slug in parentheses after its message. Nothing here decides an HTTP status
//! or a CLI exit code; those are each adapter's own presentation of a shared class.

use super::TransformError;

/// A class of failure, named as `docs/problems.md` names it.
///
/// The transform classes are the [`TransformError`] variants, which is why
/// [`TransformError::class`] is an exhaustive match: a new variant cannot be added without
/// giving it a class. The remaining classes belong to the HTTP server, which fails in ways
/// the pipeline has no name for.
///
/// The table is the whole vocabulary, not the part any one build reaches: a Wasm-only build
/// constructs none of the server's classes, and a server-only build never asks for a
/// camelCase name. Gating the variants on features would split the single table this module
/// exists to be.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorClass {
    /// The transform options are not a request the pipeline can carry out.
    InvalidOptions,
    /// The input bytes were recognised but are not what was declared.
    InvalidInput,
    /// The input is a supported format but could not be decoded.
    DecodeFailed,
    /// The input is not a format truss decodes, or is one it decodes only in part.
    UnsupportedInputMediaType,
    /// The requested output format is not one truss encodes, or not one this input allows.
    UnsupportedOutputMediaType,
    /// The transform ran but the output could not be encoded.
    EncodeFailed,
    /// The request needs a codec or feature this build was compiled without.
    CapabilityMissing,
    /// The transform would exceed a pixel, size, or time limit of the pipeline.
    LimitExceeded,
    /// The request itself could not be understood: a malformed body, query, or header, or
    /// a command line the CLI cannot parse.
    InvalidRequest,
    /// The request's own media type, as opposed to the image's.
    UnsupportedMediaType,
    /// Credentials are missing or do not verify.
    Unauthorized,
    /// Credentials verified but do not allow this request.
    Forbidden,
    /// The source the request named does not exist.
    NotFound,
    /// The route exists and does not serve the method the request used.
    MethodNotAllowed,
    /// The `Accept` header allows none of the output formats truss can produce.
    NotAcceptable,
    /// The client stopped sending before the request was complete.
    RequestTimeout,
    /// The request or the source it named is larger than the server accepts.
    PayloadTooLarge,
    /// The input was read but exceeds what the server will process.
    UnprocessableEntity,
    /// The rate limit for the client's address was reached.
    TooManyRequests,
    /// A failure that is not the request's doing, such as a file truss could not read.
    InternalError,
    /// The request names something truss knows of but this build does not do.
    NotImplemented,
    /// A remote source or storage backend failed.
    BadGateway,
    /// truss is draining, saturated, or not ready.
    ServiceUnavailable,
    /// A remote source redirected more times than the configuration allows.
    LoopDetected,
}

impl ErrorClass {
    /// The class's name: the anchor of its section in `docs/problems.md`, and what the HTTP
    /// server's `type` URI ends with and the CLI prints in parentheses.
    #[allow(dead_code)]
    pub(crate) const fn slug(self) -> &'static str {
        match self {
            Self::InvalidOptions => "invalid-options",
            Self::InvalidInput => "invalid-input",
            Self::DecodeFailed => "decode-failed",
            Self::UnsupportedInputMediaType => "unsupported-input-media-type",
            Self::UnsupportedOutputMediaType => "unsupported-output-media-type",
            Self::EncodeFailed => "encode-failed",
            Self::CapabilityMissing => "capability-missing",
            Self::LimitExceeded => "limit-exceeded",
            Self::InvalidRequest => "invalid-request",
            Self::UnsupportedMediaType => "unsupported-media-type",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not-found",
            Self::MethodNotAllowed => "method-not-allowed",
            Self::NotAcceptable => "not-acceptable",
            Self::RequestTimeout => "request-timeout",
            Self::PayloadTooLarge => "payload-too-large",
            Self::UnprocessableEntity => "unprocessable-entity",
            Self::TooManyRequests => "too-many-requests",
            Self::InternalError => "internal-error",
            Self::NotImplemented => "not-implemented",
            Self::BadGateway => "bad-gateway",
            Self::ServiceUnavailable => "service-unavailable",
            Self::LoopDetected => "loop-detected",
        }
    }

    /// The camelCase spelling of [`slug`](Self::slug), which is what the Wasm package
    /// reports as `kind`.
    ///
    /// The two spellings are one name, and `camel_case_name_matches_slug` holds them to
    /// that: the test computes the camelCase form of every slug and compares. The table is
    /// written out rather than computed because the Wasm error payload needs a
    /// `&'static str`, and a case conversion cannot produce one.
    #[cfg(any(feature = "wasm", test))]
    pub(crate) const fn camel_case_name(self) -> &'static str {
        match self {
            Self::InvalidOptions => "invalidOptions",
            Self::InvalidInput => "invalidInput",
            Self::DecodeFailed => "decodeFailed",
            Self::UnsupportedInputMediaType => "unsupportedInputMediaType",
            Self::UnsupportedOutputMediaType => "unsupportedOutputMediaType",
            Self::EncodeFailed => "encodeFailed",
            Self::CapabilityMissing => "capabilityMissing",
            Self::LimitExceeded => "limitExceeded",
            Self::InvalidRequest => "invalidRequest",
            Self::UnsupportedMediaType => "unsupportedMediaType",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "notFound",
            Self::MethodNotAllowed => "methodNotAllowed",
            Self::NotAcceptable => "notAcceptable",
            Self::RequestTimeout => "requestTimeout",
            Self::PayloadTooLarge => "payloadTooLarge",
            Self::UnprocessableEntity => "unprocessableEntity",
            Self::TooManyRequests => "tooManyRequests",
            Self::InternalError => "internalError",
            Self::NotImplemented => "notImplemented",
            Self::BadGateway => "badGateway",
            Self::ServiceUnavailable => "serviceUnavailable",
            Self::LoopDetected => "loopDetected",
        }
    }

    /// Every class, in the order `docs/problems.md` lists them.
    ///
    /// Only the tests walk this, and they are the reason it exists: a class that gains a
    /// spelling, an exit code, or a status has to gain it for all of them at once.
    #[cfg(test)]
    pub(crate) const ALL: [Self; 24] = [
        Self::InvalidOptions,
        Self::InvalidInput,
        Self::DecodeFailed,
        Self::UnsupportedInputMediaType,
        Self::UnsupportedOutputMediaType,
        Self::EncodeFailed,
        Self::CapabilityMissing,
        Self::LimitExceeded,
        Self::InvalidRequest,
        Self::UnsupportedMediaType,
        Self::Unauthorized,
        Self::Forbidden,
        Self::NotFound,
        Self::MethodNotAllowed,
        Self::NotAcceptable,
        Self::RequestTimeout,
        Self::PayloadTooLarge,
        Self::UnprocessableEntity,
        Self::TooManyRequests,
        Self::InternalError,
        Self::NotImplemented,
        Self::BadGateway,
        Self::ServiceUnavailable,
        Self::LoopDetected,
    ];
}

impl TransformError {
    /// The class this failure belongs to.
    ///
    /// This is the join between the pipeline's errors and the shared vocabulary: the HTTP
    /// `type`, the Wasm `kind`, and the slug the CLI prints all come from here, so the three
    /// adapters cannot describe one failure two ways.
    pub(crate) const fn class(&self) -> ErrorClass {
        match self {
            Self::InvalidOptions(_) => ErrorClass::InvalidOptions,
            Self::InvalidInput(_) => ErrorClass::InvalidInput,
            Self::DecodeFailed(_) => ErrorClass::DecodeFailed,
            Self::UnsupportedInputMediaType(_) => ErrorClass::UnsupportedInputMediaType,
            Self::UnsupportedOutputMediaType(_) => ErrorClass::UnsupportedOutputMediaType,
            Self::EncodeFailed(_) => ErrorClass::EncodeFailed,
            Self::CapabilityMissing(_) => ErrorClass::CapabilityMissing,
            Self::LimitExceeded(_) => ErrorClass::LimitExceeded,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ErrorClass, TransformError};
    use crate::MediaType;

    /// Turns `unsupported-input-media-type` into `unsupportedInputMediaType`.
    fn camel_case(slug: &str) -> String {
        let mut out = String::with_capacity(slug.len());
        let mut capitalize = false;
        for ch in slug.chars() {
            if ch == '-' {
                capitalize = true;
            } else if capitalize {
                out.extend(ch.to_uppercase());
                capitalize = false;
            } else {
                out.push(ch);
            }
        }
        out
    }

    #[test]
    fn camel_case_name_matches_slug() {
        for class in ErrorClass::ALL {
            assert_eq!(
                class.camel_case_name(),
                camel_case(class.slug()),
                "{class:?} spells its slug and its camelCase name differently"
            );
        }
    }

    #[test]
    fn slugs_are_unique_and_kebab_case() {
        let mut seen: Vec<&str> = Vec::new();
        for class in ErrorClass::ALL {
            let slug = class.slug();
            assert!(!seen.contains(&slug), "{slug} is used by two classes");
            assert!(
                slug.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "{slug} is not kebab case"
            );
            seen.push(slug);
        }
    }

    /// The eight classes a transform can fail with, with the slug `docs/problems.md`
    /// documents for each. Written out rather than derived, so a variant that changes
    /// class has to change this table too.
    #[test]
    fn transform_errors_carry_their_documented_class() {
        let cases: [(TransformError, &str); 8] = [
            (
                TransformError::InvalidOptions("x".into()),
                "invalid-options",
            ),
            (TransformError::InvalidInput("x".into()), "invalid-input"),
            (TransformError::DecodeFailed("x".into()), "decode-failed"),
            (
                TransformError::UnsupportedInputMediaType("x".into()),
                "unsupported-input-media-type",
            ),
            (
                TransformError::UnsupportedOutputMediaType(MediaType::Gif),
                "unsupported-output-media-type",
            ),
            (TransformError::EncodeFailed("x".into()), "encode-failed"),
            (
                TransformError::CapabilityMissing("x".into()),
                "capability-missing",
            ),
            (TransformError::LimitExceeded("x".into()), "limit-exceeded"),
        ];
        for (error, slug) in cases {
            assert_eq!(error.class().slug(), slug, "{error:?}");
        }
    }
}
