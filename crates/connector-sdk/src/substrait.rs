//! Substrait validation at the connector planning boundary.

mod extensions;
mod schema;
mod types;
mod validation;

use prost::Message;
use sha2::{Digest, Sha256};
use substrait::proto::Plan;
use substrait_validator::{Config, Validity};

use crate::{PlanningContext, RequestError, ValidatedCandidate, ValidationFingerprint};

pub(super) const SUBSTRAIT_VERSION: (u32, u32, u32) = (0, 69, 0);
pub(super) const LOGICAL_TYPE_NAME: &str = "logical";
pub(super) const COMPARISON_URI: &str =
    "https://github.com/substrait-io/substrait/blob/v0.69.0/extensions/functions_comparison.yaml";
pub(super) const BOOLEAN_URI: &str =
    "https://github.com/substrait-io/substrait/blob/v0.69.0/extensions/functions_boolean.yaml";
pub(super) const LOGICAL_URI: &str = "urn:scalardb:analytics:substrait:logical";
pub(super) const MAX_PLAN_BYTES: usize = 1024 * 1024;
pub(super) const MAX_SCHEMA_COMPLEXITY: usize = 4096;
pub(super) const MAX_OUTPUT_COLUMNS: usize = 1024;
pub(super) const MAX_OUTPUT_COMPLEXITY: usize = 16384;

pub(crate) fn validate_candidate(
    bytes: &[u8],
    context: &PlanningContext,
) -> Result<(ValidatedCandidate, ValidationFingerprint), RequestError> {
    if bytes.len() > MAX_PLAN_BYTES {
        return Err(RequestError::undeclared(format!(
            "serialized plans may not exceed {MAX_PLAN_BYTES} bytes"
        )));
    }
    let validation = substrait_validator::parse(bytes, &Config::default());
    if validation.check() == Validity::Invalid {
        let reason = validation
            .get_diagnostic()
            .map(|diagnostic| format!("{diagnostic:?}"))
            .unwrap_or_else(|| "the official Substrait validator rejected the plan".to_owned());
        return Err(RequestError::invalid(reason));
    }

    let plan = Plan::decode(bytes)
        .map_err(|error| RequestError::invalid(format!("cannot decode protobuf: {error}")))?;
    let candidate = validation::validate_plan(plan)?;
    let fingerprint = validation_fingerprint(bytes, context);
    Ok((candidate, fingerprint))
}

fn validation_fingerprint(bytes: &[u8], context: &PlanningContext) -> ValidationFingerprint {
    let mut digest = Sha256::new();
    digest.update(b"scalardb-analytics-connector/substrait-0.69.0\0");
    update_digest_field(&mut digest, bytes);
    for field in context.fingerprint_fields() {
        update_digest_field(&mut digest, field.as_bytes());
    }
    ValidationFingerprint::new(digest.finalize().into())
}

fn update_digest_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}
