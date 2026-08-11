//! One-time canonicalization of a provider request.
//!
//! A retry must send the exact same bytes that were hashed and recorded in the
//! episode envelope.  This borrowed wrapper keeps the original request for
//! the engine while caching its bounded canonical body and footprint.

use bytes::Bytes;
use krw_agent_protocol::ContentHash;

use crate::{MessagesRequest, ProviderRequestFootprint, WireError};

#[derive(Debug, Clone)]
pub struct PreparedMessagesRequest<'a> {
    request: &'a MessagesRequest,
    canonical_bytes: Bytes,
    request_hash: ContentHash,
    footprint: ProviderRequestFootprint,
}

impl<'a> PreparedMessagesRequest<'a> {
    pub fn new(request: &'a MessagesRequest) -> Result<Self, WireError> {
        request.validate()?;
        let canonical_bytes = Bytes::from(serde_jcs::to_vec(request)?);
        let request_hash = ContentHash::sha256(canonical_bytes.as_ref());
        let footprint = ProviderRequestFootprint {
            canonical_bytes: canonical_bytes.len(),
            input_tokens_upper_bound: u64::try_from(canonical_bytes.len())
                .map_err(|_| WireError::RequestFootprintOverflow)?,
        };
        Ok(Self {
            request,
            canonical_bytes,
            request_hash,
            footprint,
        })
    }

    pub fn request(&self) -> &'a MessagesRequest {
        self.request
    }

    pub fn canonical_bytes(&self) -> &Bytes {
        &self.canonical_bytes
    }

    pub fn request_hash(&self) -> &ContentHash {
        &self.request_hash
    }

    pub fn footprint(&self) -> ProviderRequestFootprint {
        self.footprint
    }
}
