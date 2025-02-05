//! Details on the constellation of PubHubs servers

use crate::misc::serde_ext;
use crate::servers;

/// Public details on the constellation of PubHubs servers.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Constellation {
    pub transcryptor_jwt_key: serde_ext::B16<ed25519_dalek::VerifyingKey>,
    pub transcryptor_url: url::Url,
    pub phc_jwt_key: serde_ext::B16<ed25519_dalek::VerifyingKey>,
    pub phc_url: url::Url,
    pub auths_jwt_key: serde_ext::B16<ed25519_dalek::VerifyingKey>,
    pub auths_url: url::Url,
}

impl Constellation {
    /// Returns the url of the named server
    pub fn url(&self, name: servers::Name) -> &url::Url {
        match name {
            servers::Name::PubhubsCentral => &self.phc_url,
            servers::Name::Transcryptor => &self.transcryptor_url,
            servers::Name::AuthenticationServer => &self.auths_url,
        }
    }
}
