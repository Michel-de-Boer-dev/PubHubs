//! Endpoints provided by a hub
use serde::{Deserialize, Serialize};

use crate::api::*;
use crate::misc::serde_ext;

///
pub struct Info {}
impl EndpointDetails for Info {
    type RequestType = ();
    type ResponseType = InfoResp;

    const METHOD: http::Method = http::Method::GET;
    const PATH: &'static str = ""; // the base url contains the path
}

#[derive(Serialize, Deserialize, Debug)]
pub struct InfoResp {
    /// Key used by the hub to sign requests to the other hubs with
    pub verifying_key: serde_ext::B16<ed25519_dalek::VerifyingKey>,
}