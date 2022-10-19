//! Module to deal with the OAuth 2.0 and OpenID Connect endpoints and flows.
//! See [new] for basic usage.
//!
//! NOTE: We attempt to keep this code separate from other PubHubs code
//! in the hope of turning into a library in the future.
//!
//! # Design choices
//!
//! We use the OAuth 2.0  **'code grant'**  flow (see RFC6749, section 4.1)
//! with the `form_post` `response_type` from "OAuth 2.0 Form Post Response Mode",
//! where the authorization code is passed to the `redirect_uri` via POST
//! instead of GET.  OAuth 2.0 clients (i.e. Hubs) must authenticate using
//! a `client_id` and `client_secret` (see section 2.3.1 of RFC6749.)
//!
//! We do not keep any **state**, and therefore cannot enforce the single use of
//! authorization codes (see RFC6749, section 4.1.2, under "code",)
//! but since the authorization code is passed via POST and can only
//! be used someone possessing the correct client credentials, the security
//! implications are minimal.
//!
//! Of OpenID Connect (see "OpenID Connect Core 1.0", OIDCC1.0) we do *not*
//! use the UserInfo endpoint, but instead pass an ID Token (OIDCC1.0, Section 2)
//! via OAuth's Token Endpoint (OIDCC1.0, 3.1.3.6 and RFC6749, Section 5.)
//! Accordingly, we do not return an `access_token` from the Token Endpoint
//! (even though it is REQUIRED by 5.1 of RFC6749.)
//!
//! To prevent having to look up `client_id`s in a database, we attach a
//! hmac to all `client_id`s that authenticates both the client's id, and
//! the `redirect_uri`, see [ClientId] for more info.

use aead::{Aead as _, AeadCore as _, KeyInit as _};
use base64ct::{Base64, Base64Url, Encoding as _};
use chacha20poly1305::XChaCha20Poly1305;
use serde::Deserialize;
use std::borrow::Cow;
use std::fmt::Write as _;
use std::str::FromStr as _;
use thiserror::Error; // this module is written like a library - don't use anyhow
                      // for errors returned to the user of the library
use anyhow::Context as _;
use hmac::Mac as _;
use http::Request as _;
use sha2::Digest as _;

/// Creates a new [Oidc] trait object that handles the OAuth 2.0 and
/// OpenID Connect endpoints to the extend that it can, passing
/// the remaining work/choices to the specified [Handler].
///
/// ```
/// use pubhubs::oidc::{self, ClientId, Oidc as _};
/// use pubhubs::oidc::http::{Method, ContentType};
/// use std::borrow::Cow;
/// use std::str::FromStr as _;
///
/// // Suppose the HTTP requests we receive look like this:
/// struct Request {
///     body : Vec<u8>,
///     method : Method,
///     query : String,
///     content_type : Option<ContentType>,
///     authorization : Option<String>,
/// }
///
/// // Then the first step is to have them implement the oidc::http::Request trait,
/// // so that oidc can deal with them.
/// impl<'s> oidc::http::Request<'s> for Request {
///     type Body = &'s [u8];
///
///     fn method(&'s self) -> Method { self.method }
///     fn query(&'s self) ->Cow<str>  { Cow::Borrowed(&self.query) }
///     fn body(&'s self) -> Self::Body { &self.body }
///     fn content_type(&'s self) -> Option<ContentType> { self.content_type }
///     fn authorization(&'s self) -> Option<Cow<str>> {
///         self.authorization.as_ref().map(|a| Cow::Borrowed(a.as_str()))
///     }
/// }
///
/// // We must also define a transformation from the oidc::http::Response enum
/// // to the type we use for HTTP responses.
/// #[derive(Debug)]
/// enum Response {
///     FromOidc(oidc::http::Response),
///     Own(String),
/// }
///
/// impl From<oidc::http::Response> for Response {
///     fn from(r : oidc::http::Response) -> Self { Response::FromOidc(r) }
/// }
///
/// // Some input from our end is required, which we provide via the `MyHandler` class.
/// struct MyHandler {}
///
/// impl oidc::Handler for MyHandler {
///     type Req = Request;
///     type Resp = Response;
///
///     fn handle_auth(&self, req : Self::Req, auth_request_handle : String) -> Self::Resp {
///         // This should return some page where a user can authenticate.
///         // When the user is authenticated, we use the `auth_request_handle` to
///         // to obtain an `auth_code` we have the user send back to the client.
///
///         // For this demonstration, we perform no authentication, but
///         // simply return the `auth_request_handle`.
///         Response::Own(auth_request_handle)
///     }
///
///     fn is_valid_client(&self, client_id : &ClientId, redirect_uri : &str) -> bool {
///         // This function is called to afford us the opportunity to ban
///         // certain `clients` and `redirect_uri`'s.
///         client_id.bare_id() != "banned" && redirect_uri != "https://example.com/banned"
///     }
/// }
///
/// let h = MyHandler{};
///
/// // If "some secret" changes, all `client_id`'s, `auth_code`'s, `auth_request_handle`,
/// // and client passwords become invalid.
/// let o = oidc::new::<MyHandler>(h, "some secret".as_bytes());
///
/// // Create new client credentials.
/// let client_creds = o.generate_client_credentials("some-client", "https://example.com");
///
/// // Simulate a user-agent that has been sent by a client to the authorization endpoint.
/// // This is the first step the client takes to obtain an `id_token` for this user-agent.
/// let resp = o.handle_auth(Request{
///     body: vec![],
///     method: Method::Get,
///     query:
///     format!("client_id={client_id}&redirect_uri=https://example.com&response_type=code&response_mode=form_post&state=state&nonce=nonce&scope=oidc", client_id = client_creds.client_id.as_ref()),
///     content_type: None,
///     authorization: None,
/// });
///
/// let mut auth_request_handle = String::new();
///
/// match resp {
///     Response::Own(s) => { auth_request_handle = s; }
///     Response::FromOidc(r) => { assert!(false, "did not expect {:?}", r); }
/// }
///
/// // Here the user should be authenticated by some appropriate process.
/// // Once we are satisfied, we use the `auth_request_handle` to create
/// // an `auth_code` to have the user POST back to the client.
/// //
/// // At that point we also already create the `id_token`, which is sealed inside the `auth_code`,
/// // and only extracted after a proper request of the client to the token endpoint.
/// let resp : oidc::http::Response  = o.grant_code(auth_request_handle,
///     |tcd : oidc::TokenCreationData| -> Result<String,()> {
///
///     assert_eq!(tcd.nonce, "nonce");
///     assert_eq!(ClientId::from_str(&tcd.client_id).unwrap().bare_id(), "some-client");
///
///     Ok("id_token".to_string())
///     // This is of course not a proper `id_token`.
///     // Don't forget to include the `tcd.nonce` and `tcd.client_id`
///     // in the `id_token`!
/// }).unwrap();
///
/// // The http response `resp` when server to the user-agent, makes them post the
/// // `auth_code` to `redirect_uri` of the client.
///
/// // The auth_code can be extracted from `resp`, as follows.
/// let mut auth_code = String::new();
///
/// match resp {
///     oidc::http::Response::Grant(oidc::redirect_uri::Response{
///         uri,
///         data: oidc::redirect_uri::ResponseData::CodeGrant{ code, state },
///     }) => {
///         assert_eq!(state, "state");
///         assert_eq!(uri, "https://example.com");
///         auth_code = code;
///     },
///     _ => { assert!(false, "unexpected response: {:?}", resp); }
/// }
///
/// // The client, upon receiving the `auth_code`, can use it to obtain the `id_token`:
/// let resp = o.handle_token(Request{
///     body : format!("grant_type=authorization_code&code={auth_code}&client_id={client_id}&redirect_uri={redirect_uri}",
///         auth_code = auth_code,
///         client_id = client_creds.client_id.as_ref(),
///         redirect_uri = "https://example.com",
///     ).as_bytes().to_vec(),
///     method : Method::Post,
///     query : String::new(),
///     content_type : Some(ContentType::UrlEncoded),
///     authorization : Some(client_creds.basic_auth())
/// });
///
/// match resp {
///     Response::FromOidc(oidc::http::Response::Token(oidc::http::TokenResponse::IdToken(id_token)))
///         => { assert_eq!(id_token, "id_token") },
///     _ => { assert!(false, "did not expect {:?}", resp) }
/// }
/// ```
pub fn new<H: Handler>(h: H, secret: impl AsRef<[u8]>) -> impl Oidc<H = H> {
    let secret = secret.as_ref();

    OidcImpl::<H> {
        handler: h,
        client_hmac_secret: derive_secret("client-hmac", secret),
        client_password_secret: derive_secret("client-password", secret),
        auth_code_secret: derive_secret("auth-code", secret),
        auth_request_handle_secret: derive_secret("auth-request-handle", secret),
    }
}

/// An Oidc instance, created by [new], handles
/// requests to OpenID connect and OAuth 2.0 endpoints,
/// and passes them on to the specified [Handler].
pub trait Oidc {
    type H: Handler;

    /// Handles the RFC6749 4.1.1 Authorization Request.
    ///
    /// The client asks us to authenticate the present user-agent, and after having done so
    /// have the user-agent POST an auth_code to the client's redirect_uri that can be used
    /// by the client to obtain an id_token from the Token Endpoint.
    fn handle_auth(&self, req: <Self::H as Handler>::Req) -> <Self::H as Handler>::Resp;

    /// Generates an auth_code for the given auth_request_handle (see [Handler::handle_auth]) that
    /// will have the Token Endpoint return the `id_token` created by the `id_token_creator`.
    ///
    /// Fails with Error::InvalidAuthRequestHandle if the auth_request_handle is invalid,
    /// and Response::IdTokenCreation when id_token_creator fails, but passes all
    /// other errors via the redirect_uri::Response::Error via the user-agent to the client.
    fn grant_code(
        &self,
        auth_request_handle: String,
        id_token_creator: impl FnOnce(TokenCreationData) -> Result<String, ()>,
    ) -> Result<http::Response, Error>;

    /// Handles the RFC6749 4.1.3 Access Token Request.
    ///
    /// The client retrieves the id_token of the user using the auth_code it got via
    /// the resource owner's user-agent.
    fn handle_token(&self, req: <Self::H as Handler>::Req) -> <Self::H as Handler>::Resp;

    /// Generates [ClientCredentials] from a `bare_id` and `redirect_uri`.
    fn generate_client_credentials(
        &self,
        bare_id: impl AsRef<str>,
        redirect_uri: impl AsRef<str>,
    ) -> ClientCredentials;
}

/// A [Handler] instance (passed to [new]) returns control to you
/// when needed (to authorize the resource owner, and so on.)
pub trait Handler {
    type Req: for<'s> http::Request<'s>;
    type Resp: From<http::Response>;

    /// The handle_auth method is called when the details passed to the authorization
    /// endpoint check out as far as this OIDC library is concerned, and
    /// the application can proceed to authenticate the user by sending
    /// the appropriate response (containing, for example, a page
    /// with an IRMA QR-code.)
    ///
    /// When the user has been authenticated, the handle can be passed to
    /// the grant_auth method of the Oidc instance.
    fn handle_auth(&self, req: Self::Req, auth_request_handle: String) -> Self::Resp;

    /// IsValidClient allows the handler to reject certain clients.
    ///
    /// At this point, the client_id and redirect_uri have already been verified
    /// using an HMAC.
    fn is_valid_client(&self, _client_id: &ClientId, _redirect_uri: &str) -> bool {
        true
    }
}

pub mod http {
    use super::*;

    /// Represents an HTTP request.  
    ///
    /// **Note 1:** A [hyper::Request] can be converted asynchronously to an [hyper_support::CompleteRequest]
    /// which has this [Request] trait
    /// via [hyper_support::CompleteRequest::from].
    ///
    /// **Note 2:**. We have added a lifetime parameter 's for &self to allow Body to be a reference with
    ///     &self's lifetime, e.g. `&'s[u8]` --- no longer needed when generic associated types,
    ///     see RFC1598, become stable.
    pub trait Request<'s> {
        type Body: std::io::Read;

        fn method(&'s self) -> Method;
        fn query(&'s self) -> Cow<str>;
        fn body(&'s self) -> Self::Body;
        fn content_type(&'s self) -> Option<ContentType>;
        fn authorization(&'s self) -> Option<Cow<str>>;
    }

    /// Enumerates the Http methods used here.
    #[non_exhaustive]
    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    pub enum Method {
        Get,
        Post,
        Other,
    }

    /// Enumerates the content-types used here.
    #[non_exhaustive]
    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    pub enum ContentType {
        /// application/x-www-form-urlencoded
        UrlEncoded,
        /// application/json
        Json,
        Other,
    }

    /// [Response] enumerates the possible HTTP responses generated by an
    /// [Oidc] instance.  [`From<Response>`] is implemented for
    /// [`hyper::Response<hyper::Body>`].
    #[derive(Debug, PartialEq, Eq)]
    pub enum Response {
        /// returned by [Oidc::handle_auth]
        Auth(AuthResponse),

        /// returned by [Oidc::handle_token]
        Token(TokenResponse),

        /// returned by [Oidc::grant_code]
        Grant(redirect_uri::Response),
    }

    /// [AuthResponse] enumerates the possible HTTP responses generated by
    /// [Oidc::handle_auth].
    #[derive(Debug, PartialEq, Eq)]
    pub enum AuthResponse {
        Error(S52Error),
        /// Make the user-agent POST to this URI.
        FormPost(redirect_uri::Response),
    }

    /// [TokenResponse] enumerates the possible HTTP responses generated by
    /// [Oidc::handle_token].
    #[derive(Debug, PartialEq, Eq)]
    pub enum TokenResponse {
        Error(S52Error),
        IdToken(String),
    }

    impl From<AuthResponse> for Response {
        fn from(e: AuthResponse) -> Self {
            Response::Auth(e)
        }
    }

    impl From<TokenResponse> for Response {
        fn from(e: TokenResponse) -> Self {
            Response::Token(e)
        }
    }

    impl From<S52Error> for AuthResponse {
        fn from(e: S52Error) -> Self {
            AuthResponse::Error(e)
        }
    }

    impl From<S52Error> for TokenResponse {
        fn from(e: S52Error) -> Self {
            TokenResponse::Error(e)
        }
    }

    impl From<redirect_uri::Response> for AuthResponse {
        fn from(fp: redirect_uri::Response) -> Self {
            AuthResponse::FormPost(fp)
        }
    }

    impl Response {
        pub fn status(&self) -> u16 {
            match self {
                Response::Auth(AuthResponse::Error(e))
                | Response::Token(TokenResponse::Error(e)) => e.status(),
                _ => 200,
            }
        }

        pub fn headers(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
            // headers is an array of pairs ("Header-Name", f),
            // where f(self) returns Some("header value") or None, depending on whether
            // Header-Name is to be included.
            //
            // If rust gets the "yield" keyword, this awkward business can be avoided.
            type HeaderValueCreator = fn(&Response) -> Option<&'static str>;

            const HEADERS: [(&str, HeaderValueCreator); 4] = [
                ("Content-Type", |s| match s {
                    Response::Auth(AuthResponse::FormPost(_)) | Response::Grant(_) => {
                        Some("text/html;charset=UTF-8")
                    }
                    Response::Auth(AuthResponse::Error(_)) => Some("plain/text;charset=UTF-8"),
                    Response::Token(_) => Some("application/json;charset=UTF-8"),
                }),
                ("Cache-Control", |_| Some("no-store")),
                // RFC6749 demands the "Pragma: no-cache" header too,
                // but "Pragma" has been deprecated, so we ignore this demand.
                ("WWW-Authenticate", |s| match s.status() {
                    401 => Some("Basic"),
                    _ => None,
                }),
                // "frame-ancestors none" addresses RFC6749, 10.13
                ("Content-Security-Policy", |_| Some("frame-ancestors none;")),
            ];

            HEADERS
                .into_iter()
                .filter_map(|(name, gen)| gen(self).map(|value| (name, value)))
        }

        /// Returns the HTTP body associated with this response.
        ///
        /// The body is returned as a String (instead of, say a [std::io::Read]
        /// or
        /// [futures::stream::Stream](https://docs.rs/futures/latest/futures/stream/trait.Stream.html), because the body is generally small
        pub fn into_body(self) -> String {
            match self {
                Response::Auth(AuthResponse::Error(e)) => {
                    format!("Oops! something went wrong - sorry about that.\n\nWe can't tell for sure who sent you here, but it might have been a fool's errand. \n\nIf you think it isn't, please contact the website that sent you here, and provide them the following information.\n\n{}\n\n{}", e.error(), e.error_description())
                }
                Response::Auth(AuthResponse::FormPost(rur)) | Response::Grant(rur) => {
                    let mut inputs = String::new();

                    rur.data.walk_fields(|field_name: &str, field_value: &str| {
                        writeln!(
                            inputs,
                            "<input type=\"hidden\" name=\"{name}\" value=\"{value}\">",
                            name = html::escape(field_name),
                            value = html::escape(field_value)
                        )
                        .unwrap()
                    });

                    format!(
                        r#"<html>
                            <head><title>Form redirection...</title></head>
                            <body onload="javascript:document.forms[0].submit()">
                                <form method="post" action="{redirect_uri}">
                                    <input type="hidden">
                                    {inputs}
                                    <input type="submit" value="Click here to proceed">
                                </form>
                            </body>
                        </html>"#,
                        redirect_uri = html::escape(&rur.uri),
                        inputs = inputs,
                    )
                }
                Response::Token(TokenResponse::Error(e)) => {
                    #[derive(serde::Serialize)]
                    struct Resp<'a> {
                        error: &'a str,
                        error_description: &'a str,
                    }

                    serde_json::to_string(&Resp {
                        error: e.error(),
                        error_description: e.error_description(),
                    })
                    .expect("did not think this serialization could fail")
                }
                Response::Token(TokenResponse::IdToken(t)) => {
                    #[derive(serde::Serialize)]
                    struct Resp<'a> {
                        access_token: &'a str,
                        token_type: &'a str,
                        id_token: &'a str,
                    }

                    serde_json::to_string(&Resp {
                        access_token: "we provide only an id_token, no access token",
                        token_type: "absent",
                        id_token: &t,
                    })
                    .expect("did not think this serialization could fail")
                }
            }
        }
    }

    /// Enumerates the errors returned by token endpoint, see Section 5.2 of RFC6749.
    /// These are also reused by the authorization endpoint when it's not safe to notify the
    /// client via the redirect_uri, for example, when the authenticity of the redirect_uri could
    /// not be established.  
    ///
    /// For more details on each error, see the "error_description" method.
    #[derive(Debug, PartialEq, Eq)]
    pub enum S52Error {
        UnsupportedMethod,
        MalformedQuery,
        MalformedClientId,
        MalformedRedirectUri,
        InvalidClientMAC,
        UnsupportedResponseMode,
        MalformedRequestBody,
        UnsupportedContentType,
        InvalidAuthCode,
        UnsupportedGrantType,
        MissingClientCredentials,
        MalformedClientCredentials,
        InvalidClientCredentials,
    }

    /// RFC 6749 Section 5.2 error codes
    #[doc(hidden)]
    enum S52EC {
        InvalidRequest,
        InvalidClient,
        InvalidGrant,
        UnsupportedGrantType,

        // The following two error codes are not presently used.
        #[allow(dead_code)]
        UnauthorizedClient,
        #[allow(dead_code)]
        InvalidScope,
    }

    impl S52EC {
        fn to_static_str(&self) -> &'static str {
            match self {
                S52EC::InvalidRequest => "invalid_request",
                S52EC::InvalidClient => "invalid_client",
                S52EC::InvalidGrant => "invalid_grant",
                S52EC::UnauthorizedClient => "unauthorized_client",
                S52EC::UnsupportedGrantType => "unsupported_grant_type",
                S52EC::InvalidScope => "invalid_scope",
            }
        }
    }

    impl S52Error {
        fn status(&self) -> u16 {
            match self.error() {
                "unauthorized_client" => 401, // unauthorized
                _ => 400,                     // bad request
            }
        }

        fn error(&self) -> &'static str {
            self.error_code().to_static_str()
        }

        /// Returns the associates RFC6749-Section-5.2 error code
        fn error_code(&self) -> S52EC {
            match self {
                S52Error::UnsupportedMethod
                | S52Error::MalformedQuery
                | S52Error::MalformedRedirectUri
                | S52Error::UnsupportedResponseMode
                | S52Error::MalformedRequestBody
                // MalformedClientId and InvalidClientMAC are not given the InvalidClient
                // error code, because they are returned by the authorization endpoint too,
                // where the client itself does not authorize itself directly - we do not want
                // the user to get a password prompt due to the WWW-Authenticate header.
                | S52Error::MalformedClientId
                | S52Error::InvalidClientMAC
                | S52Error::UnsupportedContentType => S52EC::InvalidRequest,

                S52Error::InvalidAuthCode => S52EC::InvalidGrant,

                S52Error::MissingClientCredentials
                | S52Error::MalformedClientCredentials
                | S52Error::InvalidClientCredentials => S52EC::InvalidClient,

                S52Error::UnsupportedGrantType => S52EC::UnsupportedGrantType,
            }
        }

        fn error_description(&self) -> &'static str {
            match self {
        S52Error::UnsupportedMethod => "Invalid HTTP method - GET must be used for the authorization endpoint, and POST for the token endpoint",
        S52Error::MalformedQuery => "The query string could not be parsed, contained unknown fields, or lacked required fields such as client_id, response_type or redirect_uri.",
        S52Error::MalformedClientId => "The client_id contained invalid characters, or did not contain a tilde ('~').",
        S52Error::MalformedRedirectUri => "The redirect_uri could not be parsed, contained a fragment (which is prohibited) or did not use the 'https' scheme.",
        S52Error::InvalidClientMAC => "The combination of client_id and redirect_uri was not authenticated by the MAC inside the client_id.",
        S52Error::UnsupportedResponseMode => "Unsupported response_mode; only 'form_post' is supported.",
        S52Error::MalformedRequestBody => "The request body could not be parsed, contained unknown fields, or lacked required fields.",
        S52Error::UnsupportedContentType => "Unsupported Content-Type; only 'application/x-www-form-urlencoded' is supported",
        S52Error::InvalidAuthCode => "Invalid authorization code.",
        S52Error::UnsupportedGrantType => "Unsupported grant_type; only 'authorization_code' is supported.",
        S52Error::MissingClientCredentials => "Missing 'Authorization' HTTP header.",
        S52Error::MalformedClientCredentials => "Malformed 'Authorization: Basic ...' header.",
        S52Error::InvalidClientCredentials => "Invalid client_id or password.",
            }
        }
    }

    /// support for hyper
    pub mod hyper_support {
        use super::*;

        impl From<&hyper::Method> for Method {
            fn from(hm: &hyper::Method) -> Method {
                match *hm {
                    hyper::Method::GET => Method::Get,
                    hyper::Method::POST => Method::Post,
                    _ => Method::Other,
                }
            }
        }

        /// Represents a small http request whose body has been read completely into memory.
        pub struct CompleteRequest<Body: hyper::body::HttpBody + Unpin> {
            underlying: hyper::Request<Body>,
            body: hyper::body::Bytes,
        }

        impl<Body: hyper::body::HttpBody + Unpin> CompleteRequest<Body> {
            /// Reads the body of the given http request into memory provided
            /// that its content-length does not exceed the provided `max_body_size`.
            pub async fn from(
                mut req: hyper::http::Request<Body>,
                max_body_size: u64,
            ) -> Result<Option<Self>, Body::Error> {
                let body = req.body();

                // check body size
                match body.size_hint().upper() {
                    None => return Ok(None),
                    Some(s) => {
                        if s > max_body_size {
                            return Ok(None);
                        }
                    }
                }

                Ok(Some(CompleteRequest {
                    body: hyper::body::to_bytes(req.body_mut()).await?,
                    underlying: req,
                }))
            }

            /// Retrieves the first value of the given header converted to [&str].
            /// Returns [None] when no such header exists, or when its first value
            /// contains an invalid or opaque character, see [tests::valid_header_characters].
            #[doc(hidden)]
            fn header(&self, name: impl hyper::header::AsHeaderName) -> Option<&str> {
                self.underlying.headers().get(name)?.to_str().ok()
            }
        }

        impl<'s, Body: hyper::body::HttpBody + Unpin> Request<'s> for CompleteRequest<Body> {
            type Body = &'s [u8];

            fn method(&'s self) -> Method {
                self.underlying.method().into()
            }

            fn query(&'s self) -> Cow<str> {
                match self.underlying.uri().query() {
                    Some(q) => Cow::Borrowed(q),
                    None => Cow::Owned("".to_string()),
                }
            }

            fn body(&'s self) -> Self::Body {
                &self.body
            }

            fn content_type(&'s self) -> Option<ContentType> {
                let ct = self.header(hyper::header::CONTENT_TYPE)?;

                if ct.starts_with("application/x-www-form-urlencoded") {
                    return Some(ContentType::UrlEncoded);
                }
                if ct.starts_with("application/json") {
                    return Some(ContentType::Json);
                }
                Some(ContentType::Other)
            }

            fn authorization(&'s self) -> Option<Cow<str>> {
                Some(Cow::Borrowed(self.header(hyper::header::AUTHORIZATION)?))
            }
        }

        impl From<Response> for hyper::Response<hyper::Body> {
            fn from(r: Response) -> hyper::Response<hyper::Body> {
                let mut builder = hyper::Response::builder().status(r.status());

                for (header_name, header_value) in r.headers() {
                    builder = builder.header(header_name, header_value);
                }

                let result = builder.body(r.into_body().into());

                match result {
                    Ok(resp) => resp,
                    Err(_) => hyper::Response::builder()
                        .status(500)
                        .body("failed to build HTTP response".into())
                        .unwrap(), // should not fail with these arguments
                }
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            /// This test clarifies which characters may appear in a [hyper::header::HeaderValue].
            fn valid_header_characters() {
                // According to RFC7230 (see 'field-content' in section 3.2),
                // header values may contain the following bytes:
                //
                //  - 9, a tab ('\t');
                //  - 32, a space (' ');
                //  - 33 ('!') to 126 ('~') inclusive, that is, any visible ASCII character (VCHAR);
                //  - 128 to 255, so-called 'opaque characters'.
                //
                // The opaque characters are allowed only for historic reasons, and should not be
                // used, and may even be invalid unicode.
                //
                // Whence HeaderValue::to_str() rejects any opaque characters.
                //
                // Note: while RFC7230 includes "obs-fold" in the definition of "field-value",
                //       its use is banned except within a 'message/http' (yes, 'http') -
                //       a MIME type which has not caught on it seems.
                for byte in 0..=255 {
                    let hv = hyper::header::HeaderValue::from_bytes([byte].as_slice());
                    assert_eq!(hv.is_ok(), byte >= 32 && byte != 127 || byte == 9);
                    if let Ok(hv) = hv {
                        let ts = hv.to_str();
                        assert_eq!(ts.is_ok(), byte < 127);
                    }
                }
            }

            #[tokio::test]
            async fn complete_request() {
                // bodies that are too large are rejected
                assert!(CompleteRequest::from(
                    hyper::Request::builder().body("asd".to_string()).unwrap(),
                    2,
                )
                .await
                .unwrap()
                .is_none());

                // test method
                for (ms, m) in vec![
                    ("GET", Method::Get),
                    ("POST", Method::Post),
                    ("PATCH", Method::Other),
                ] {
                    let req = CompleteRequest::from(
                        hyper::Request::builder()
                            .method(ms)
                            .body("asd".to_string())
                            .unwrap(),
                        4,
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    assert_eq!(req.method(), m);
                }

                // test query
                for (u, q) in vec![
                    ("https://no-query.com", ""),
                    ("http://query.com?", ""),
                    ("https://query.com?some-query#fragment", "some-query"),
                ] {
                    let req = CompleteRequest::from(
                        hyper::Request::builder()
                            .uri(u)
                            .body("asd".to_string())
                            .unwrap(),
                        4,
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    assert_eq!(req.query(), q);
                }

                // empty body
                let req = CompleteRequest::from(
                    hyper::Request::builder()
                        .body(hyper::body::Body::empty())
                        .unwrap(),
                    4,
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(req.body(), "".as_bytes());

                // string body
                let req = CompleteRequest::from(
                    hyper::Request::builder().body("asd".to_string()).unwrap(),
                    4,
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(req.body(), "asd".as_bytes());

                // content-type
                for (cts, ct) in vec![
                    (
                        Some(b"application/json".as_slice()),
                        Some(ContentType::Json),
                    ),
                    (
                        Some(b"application/x-www-form-urlencoded".as_slice()),
                        Some(ContentType::UrlEncoded),
                    ),
                    (Some(b"text/plain".as_slice()), Some(ContentType::Other)),
                    (None, None),
                    (Some(b"\t".as_slice()), Some(ContentType::Other)),
                    (Some(b"\xff".as_slice()), None), // 'opaque' character
                ] {
                    let mut rb = hyper::Request::builder();

                    if let Some(cts) = cts {
                        rb = rb.header("Content-Type", cts);
                    }

                    let req = CompleteRequest::from(rb.body("asd".to_string()).unwrap(), 4)
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(req.content_type(), ct);
                }

                // authorization
                for auth in vec![None, Some("Blaat")] {
                    let mut rb = hyper::Request::builder();

                    if let Some(auth) = auth {
                        rb = rb.header("Authorization", auth);
                    }

                    let req = CompleteRequest::from(rb.body("asd".to_string()).unwrap(), 4)
                        .await
                        .unwrap()
                        .unwrap();

                    assert_eq!(
                        req.authorization().map(|a| a.into_owned()),
                        auth.map(|a| a.to_string())
                    );
                }

                // example
                let req = CompleteRequest::from(
                    hyper::Request::builder()
                        .method("POST")
                        .header("Content-Type", "application/json")
                        .uri("https://example.com/?query#fragment")
                        .body("asd".to_string())
                        .unwrap(),
                    3,
                )
                .await
                .unwrap()
                .unwrap();

                assert_eq!(req.method(), Method::Post);
                assert_eq!(req.query(), "query");
                assert_eq!(req.body(), "asd".as_bytes());
                assert_eq!(req.content_type(), Some(ContentType::Json));
                assert_eq!(req.authorization(), None);
            }
        }
    }
}

pub mod redirect_uri {

    /// Represents the response of the [super::Oidc] to the client of having the
    /// user-agent POST the [ResponseData] to the specified uri.
    #[derive(Debug, PartialEq, Eq)]
    pub struct Response {
        pub uri: String,
        pub data: ResponseData,
    }

    /// Represents data passed to the client by POSTing it to its `redirect_uri`.
    #[derive(Debug, PartialEq, Eq)]
    pub enum ResponseData {
        CodeGrant { code: String, state: String },
        Error { error: Error, state: Option<String> },
    }

    /// Represents an error to be passed to a client's `redirect_uri`.
    #[derive(Debug, PartialEq, Eq)]
    #[allow(clippy::enum_variant_names)] // ServerError refers to OAuth's server_error
    pub enum Error {
        UnsupportedResponseType,
        UnsupportedParameter(String),
        InvalidState,
        InvalidNonce,
        InvalidScope,
        UnauthorizedClient,
        ServerError,
    }

    impl ResponseData {
        /// Enumerates then fields that are to be POSTed to the [Response::uri], by calling
        ///   `cb(field_name, field_value)`
        /// for each field.
        ///
        /// NB. the `field_name` and `field_value` are not (yet) encoded for embedding in HTML.
        pub fn walk_fields(&self, mut cb: impl FnMut(&str, &str)) {
            match self {
                ResponseData::CodeGrant { code, state } => {
                    cb("code", code);
                    cb("state", state);
                }
                ResponseData::Error { error, state } => {
                    cb("error", error.error());
                    if let Some(desc) = error.error_description() {
                        cb("error_description", &desc);
                    }
                    if let Some(state) = state {
                        cb("state", state);
                    }
                }
            }
        }
    }

    impl Error {
        fn error(&self) -> &'static str {
            match self {
                Self::UnsupportedResponseType => "unsupported_response_type",
                Self::UnsupportedParameter(_) => "invalid_request",
                Self::InvalidState => "invalid_request",
                Self::InvalidNonce => "invalid_request",
                Self::InvalidScope => "invalid_scope",
                Self::UnauthorizedClient => "unauthorized_client",
                Self::ServerError => "server_error",
            }
        }

        fn error_description(&self) -> Option<String> {
            match self {
                Self::UnsupportedResponseType => Some("only 'code' response_type is supported".to_string()),
                Self::UnsupportedParameter(param) => Some(format!("parameter '{param}' is not supported")) ,
                Self::InvalidState => Some("'state' parameter must be set, non-empty and printable ascii".to_string()),
                Self::InvalidNonce => Some("'nonce' parameter must be set, non-empty and printable ascii".to_string()),
                Self::InvalidScope => Some("'scope' parameter must be set, include 'oidc', and may contain only printable ascii characters excluding '\"' and '\\'".to_string()),
                Self::UnauthorizedClient => None,
                Self::ServerError => Some("internal server error".to_string()),
            }
        }
    }
}

/// Represents login details for a client when contacting the token endpoint,
/// see [Oidc::handle_token].
///
/// Can be created using [Oidc::generate_client_credentials].
#[derive(PartialEq, Eq, Debug, Clone, PartialOrd, Hash)]
pub struct ClientCredentials {
    pub client_id: ClientId,
    pub password: String,
}

impl ClientCredentials {
    /// Returns the credentials wrapped in an "Authorization" header value.
    ///
    /// ```
    /// use pubhubs::oidc;
    /// use std::str::FromStr as _;
    ///
    /// assert_eq!(oidc::ClientCredentials{
    ///     client_id: oidc::ClientId::from_str("some-client~mac").unwrap(),
    ///     password: "password".to_string()
    /// }.basic_auth(), "Basic c29tZS1jbGllbnR+bWFjOnBhc3N3b3Jk".to_string())
    /// ```
    pub fn basic_auth(&self) -> String {
        basic_auth::Credentials {
            userid: self.client_id.as_ref().to_string(),
            password: self.password.clone(),
        }
        .to_string()
    }
}

/// Wraps a [String] holding a client's identifier of the form
/// `<bare_id>~<mac>`, where `bare_id` is arbitrary (e.g. `test_hub`)
/// and `mac` is a message authentication code that binds the `bare_id`
/// to a `redirect_uri` using a secret derived from the secret
/// passed to the [Oidc] via [new].
#[derive(PartialEq, Eq, Debug, Clone, PartialOrd, Hash)]
pub struct ClientId {
    data: String,
    tilde_pos: usize,
}

impl std::convert::TryFrom<String> for ClientId {
    type Error = Error;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let pos = s.rfind('~');
        if pos.is_none() {
            return Err(Error::MalformedClientId);
        }

        if !is_printable_ascii(s.chars()) {
            return Err(Error::MalformedClientId);
        }

        Ok(ClientId {
            tilde_pos: pos.unwrap(), // not none, see above
            data: s,
        })
    }
}

impl std::str::FromStr for ClientId {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s.to_owned())
    }
}

impl AsRef<str> for ClientId {
    fn as_ref(&self) -> &str {
        &self.data
    }
}

impl From<ClientId> for String {
    fn from(c: ClientId) -> String {
        c.data
    }
}

impl ClientId {
    pub fn bare_id(&self) -> &str {
        &self.data[..self.tilde_pos]
    }

    fn mac(&self) -> &str {
        &self.data[self.tilde_pos + 1..]
    }

    /// check_mac true only if the client id contains an HMAC
    /// that authenticates the client_id and redirect_uri.
    ///
    /// WARNING:  
    ///
    ///  - Not checking the redirect_uri makes your server an open
    ///    redirector, see CWE-601, and e.g. CVE-2020-26877.
    ///  
    ///  - If this function performs some expensive operation, like querying
    ///    a database, this opens the door to a DOS attack.
    ///
    fn check_mac(&self, secret: &[u8], redirect_uri: &str) -> bool {
        let mac = Base64Url::decode_vec(self.mac());
        if mac.is_err() {
            return false;
        }
        let mac = mac.unwrap();
        Self::compute_mac(self.bare_id(), secret, redirect_uri)
            .verify_slice(&mac)
            .is_ok()
    }

    /// Given the client's `bare_id`, the hmac `secret` and the `redirect_uri`,
    /// computes the associated hmac, returned as [hmac::Mac].
    fn compute_mac(bare_id: &str, secret: &[u8], redirect_uri: &str) -> impl hmac::Mac {
        <hmac::Hmac<sha2::Sha256> as hmac::Mac>::new_from_slice(secret)
            // currently, new_from_slice never returns an error
            .expect("expected no error from 'Hmac::new_from_slice'")
            .chain_update(bare_id)
            .chain_update(b"\0")
            .chain_update(redirect_uri)
    }

    /// Generates a new client id including the hmac from the `bare_id`,
    /// the `redirect_uri` and the hmac `secret`.
    fn new(bare_id: &str, secret: &[u8], redirect_uri: &str) -> ClientId {
        let mac = Base64Url::encode_string(
            &Self::compute_mac(bare_id, secret, redirect_uri)
                .finalize()
                .into_bytes(),
        );

        let mut result = String::with_capacity(bare_id.len() + mac.len() + 1);
        result.push_str(bare_id);
        result.push('~');
        result.push_str(&mac);
        ClientId {
            data: result,
            tilde_pos: bare_id.len(),
        }
    }

    fn password_mac(client_id: &str, secret: &[u8]) -> impl hmac::Mac {
        <hmac::Hmac<sha2::Sha256> as hmac::Mac>::new_from_slice(secret)
            // currently, new_from_slice never returns an error
            .expect("expected no error from 'Hmac::new_from_slice'")
            .chain_update(client_id)
    }

    /// Computes the password associated with the given `client_id`,
    /// which is the urlsafe base64 encoding of a sha256-hmac
    /// of `client_id`.
    ///
    /// Note:  to check a password, use [ClientId::check_password] instead, which employs
    /// constant time equality to prevent timing attacks.
    fn password(client_id: impl AsRef<str>, secret: impl AsRef<[u8]>) -> String {
        Base64Url::encode_string(
            &Self::password_mac(client_id.as_ref(), secret.as_ref())
                .finalize()
                .into_bytes(),
        )
    }

    fn check_password(
        client_id: impl AsRef<str>,
        secret: impl AsRef<[u8]>,
        password: impl AsRef<str>,
    ) -> bool {
        let pw = Base64Url::decode_vec(password.as_ref());
        if pw.is_err() {
            return false;
        }
        let pw = pw.unwrap();

        Self::password_mac(client_id.as_ref(), secret.as_ref())
            .verify_slice(&pw)
            .is_ok()
    }
}

/// Error encapsulates all errors returned by this module.
#[derive(Error, Debug, Clone, PartialEq, Eq, PartialOrd)]
pub enum Error {
    #[error("invalid/corrupted client id")]
    MalformedClientId,

    #[error("invalid/corrupted auth_request_handle")]
    InvalidAuthRequestHandle,

    #[error("invalid/corrupted auth_code")]
    InvalidAuthCode,

    #[error("invalid scope: illegal character or extra space (' ')")]
    InvalidScope,

    #[error("failed to create id_token")]
    IdTokenCreation,
}

/// OAuth 2.0's RFC6749 calls this "*VSCHAR" in its Appendix A.
#[doc(hidden)]
fn is_printable_ascii(characters: impl IntoIterator<Item = char>) -> bool {
    characters
        .into_iter()
        .all(|c: char| matches!(c, '\x20'..='\x7e'))
}

/// Convenience function that parses the given scope into scope tokens, see RFC6749 sections 3.3
/// and A.4.  The scope tokens are returned in sorted order.
/// Returns Error::InvalidScope when the scope is invalid.
pub fn parse_scope(scope: &str) -> Result<Vec<&str>, Error> {
    let mut res = Vec::<&str>::new();

    for token in scope.split(' ') {
        if token.is_empty() {
            return Err(Error::InvalidScope);
        }

        for c in token.chars() {
            match c {
                '\x21' | '\x23'..='\x5b' | '\x5d'..='\x7e' => continue,
                _ => return Err(Error::InvalidScope), // invalid character
            }
        }

        res.push(token);
    }

    res.sort();

    Ok(res)
}

/// Data extracted from an `auth_request_handle` to be used to create an `id_token`,
/// see [Oidc::grant_code].
#[derive(PartialEq, Eq, Debug)]
pub struct TokenCreationData {
    /// must be included in the `id_token` (as the `nonce` field)
    pub nonce: String,

    /// must be included in the `id_token` as the `aud` field
    pub client_id: String,

    /// need not be included in the `id_token`, but may determine the contents of the `id_token`
    pub scope: String,
}

#[doc(hidden)]
struct OidcImpl<H: Handler> {
    handler: H,
    client_hmac_secret: Secret,
    client_password_secret: Secret,
    auth_code_secret: Secret,
    auth_request_handle_secret: Secret,
}

/// Represents the query arguments passed to the authorization endpoint,
/// see RFC6749, section 4.1.1.
#[derive(Deserialize, Debug)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
#[doc(hidden)]
struct AuthQuery {
    response_type: String,
    client_id: String,
    redirect_uri: String,

    /// See "OAuth 2.0 Form Post Response Mode".
    #[serde(default)]
    response_mode: Option<String>,

    #[serde(default)]
    scope: Option<String>,

    #[serde(default)]
    state: Option<String>,

    /// See OIDCC1.0, Section 3.1.2.1
    #[serde(default)]
    nonce: Option<String>,

    // The following parameters from OIDCC1.0, 3.1.2.1 are not supported,
    // and included only to give a better error message.
    display: Option<String>,
    prompt: Option<String>,
    max_age: Option<String>,
    ui_locales: Option<String>,
    id_token_hint: Option<String>,
    login_hint: Option<String>,
    acr_values: Option<String>,
}

/// Represents the parameters POSTed to the token endpoint,
/// see RFC6749, section 4.1.3.
#[derive(Deserialize, Debug, Clone)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
#[doc(hidden)]
struct TokenQuery {
    grant_type: String,
    code: String,
    client_id: String,
    redirect_uri: String,
}

/// Represents the fields POSTed to redirect_uri
/// by us, and should thus not already be used in the redirect_uri
/// query itself (in case the POST and GET parameters are merged.)
#[derive(Deserialize, Default, PartialEq, Eq)]
#[doc(hidden)]
struct RedirectUriSpecialFields {
    code: Option<String>,
    state: Option<String>,
    nonce: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
    error_uri: Option<String>,
}

impl RedirectUriSpecialFields {
    fn empty(&self) -> bool {
        *self == Self::default()
    }
}

impl<H: Handler> Oidc for OidcImpl<H> {
    type H = H;

    fn handle_auth(&self, req: H::Req) -> H::Resp {
        macro_rules! http_error {
            ($param:tt) => {
                H::Resp::from(http::AuthResponse::from(http::S52Error::$param).into())
            };
        }

        if req.method() != http::Method::Get {
            return http_error!(UnsupportedMethod);
        }

        // parse query
        let query = serde_urlencoded::from_str::<AuthQuery>(req.query().as_ref());
        if query.is_err() {
            return http_error!(MalformedQuery);
        }
        let query = query.unwrap();

        // parse client_id
        let client_id: Result<ClientId, Error> = str::parse(&query.client_id);
        if client_id.is_err() {
            return http_error!(MalformedClientId);
        }
        let client_id = client_id.unwrap();

        // check MAC in client_id
        if !client_id.check_mac(&self.client_hmac_secret, &query.redirect_uri) {
            return http_error!(InvalidClientMAC);
        }

        // check redirect_uri
        let parsed_redirect_uri = url::Url::parse(&query.redirect_uri);
        if parsed_redirect_uri.is_err() {
            return http_error!(MalformedRedirectUri);
        }
        let parsed_redirect_uri = parsed_redirect_uri.unwrap();

        if parsed_redirect_uri.scheme() != "https" || parsed_redirect_uri.fragment().is_some() {
            return http_error!(MalformedRedirectUri);
        }

        // check that the query part of the redirect_uri is valid urlencoded
        // and does not contain any parameters we'd use
        if let Some(ruq) = parsed_redirect_uri.query() {
            let ruq: Result<RedirectUriSpecialFields, _> = serde_urlencoded::from_str(ruq);
            if ruq.is_err() {
                return http_error!(MalformedRedirectUri);
            }
            let ruq = ruq.unwrap();

            if !ruq.empty() {
                return http_error!(MalformedRedirectUri);
            }
        }

        // check response_mode
        if query.response_mode != Some("form_post".to_string()) {
            return http_error!(UnsupportedResponseMode);
        }

        // NOTE: from here on we can post our errors to the client
        // by redirecting the user-agent.

        let err_resp = |error_type: redirect_uri::Error| -> H::Resp {
            H::Resp::from(http::Response::Auth(http::AuthResponse::FormPost(
                redirect_uri::Response {
                    uri: query.redirect_uri.clone(),
                    data: redirect_uri::ResponseData::Error {
                        error: error_type,
                        state: query.state.clone(),
                    },
                },
            )))
        };

        // check response_type
        if query.response_type != "code" {
            return err_resp(redirect_uri::Error::UnsupportedResponseType);
        }

        // check state
        if !is_valid_state(&query.state) {
            return err_resp(redirect_uri::Error::InvalidState);
        }

        macro_rules! check_is_none {
            ($param:tt) => {
                if !query.$param.is_none() {
                    return err_resp(redirect_uri::Error::UnsupportedParameter(String::from(
                        stringify!($param),
                    )));
                }
            };
        }

        check_is_none!(display);
        check_is_none!(prompt);
        check_is_none!(max_age);
        check_is_none!(ui_locales);
        check_is_none!(id_token_hint);
        check_is_none!(login_hint);
        check_is_none!(acr_values);

        // check nonce - OIDCC1.0 3.1.2.{1,2} do not explicitly impose
        // specific restrictions for the nonce, so we'll treat the nonce
        // the same as state
        if !is_valid_state(&query.nonce) {
            return err_resp(redirect_uri::Error::InvalidNonce);
        }

        // check scope - must include 'openid' per 3.1.2.1 of OIDCC1.0
        if query.scope == None {
            return err_resp(redirect_uri::Error::InvalidScope);
        }

        let scope = parse_scope(query.scope.as_ref().unwrap());
        if scope.is_err() {
            return err_resp(redirect_uri::Error::InvalidScope);
        }

        let scope = scope.unwrap();
        if scope.binary_search_by(|x| "oidc".cmp(x)).is_err() {
            return err_resp(redirect_uri::Error::InvalidScope);
        }

        // let handler check that the client is (still) authorized
        if !self
            .handler
            .is_valid_client(&client_id, &query.redirect_uri)
        {
            return err_resp(redirect_uri::Error::UnauthorizedClient);
        }

        // Okay, everything seems to be in order;  hand over control
        // to the handler.
        match (AuthRequestData {
            state: query
                .state
                .clone()
                .expect("is_valid_state to have ensured state is not none"),
            nonce: query
                .nonce
                .expect("is_valid_state to have checked nonce is not none"),
            redirect_uri: query.redirect_uri.clone(),
            scope: query
                .scope
                .expect("parse_scope to have checked scope is not none"),
            client_id: query.client_id,
        }
        .to_handle(&self.auth_request_handle_secret))
        {
            Ok(handle) => self.handler.handle_auth(req, handle),
            Err(err) => {
                log::error!("failed to create auth_request_handle: {}", err);
                err_resp(redirect_uri::Error::ServerError)
            }
        }
    }

    fn grant_code(
        &self,
        auth_request_handle: String,
        id_token_creator: impl FnOnce(TokenCreationData) -> Result<String, ()>,
    ) -> Result<http::Response, Error> {
        let data =
            AuthRequestData::from_handle(auth_request_handle, &self.auth_request_handle_secret)?;

        let id_token = id_token_creator(TokenCreationData {
            nonce: data.nonce,
            client_id: data.client_id.clone(),
            scope: data.scope,
        })
        .map_err(|_| Error::IdTokenCreation)?;

        let code = AuthCodeData { id_token }.to_code(&self.auth_code_secret, data.client_id);

        if let Err(err) = code {
            log::error!("failed to create auth_code: {}", err);

            return Ok(http::Response::Grant(redirect_uri::Response {
                uri: data.redirect_uri,
                data: redirect_uri::ResponseData::Error {
                    error: redirect_uri::Error::ServerError,
                    state: Some(data.state),
                },
            }));
        }

        let code = code.unwrap();

        Ok(http::Response::Grant(redirect_uri::Response {
            uri: data.redirect_uri,
            data: redirect_uri::ResponseData::CodeGrant {
                state: data.state,
                code,
            },
        }))
    }

    fn handle_token(&self, req: H::Req) -> H::Resp {
        macro_rules! http_error {
            ($param:tt) => {
                H::Resp::from(http::TokenResponse::from(http::S52Error::$param).into())
            };
        }

        if req.method() != http::Method::Post {
            return http_error!(UnsupportedMethod);
        }

        if req.content_type() != Some(http::ContentType::UrlEncoded) {
            return http_error!(UnsupportedContentType);
        }

        // parse body
        let query: Result<TokenQuery, _> = serde_urlencoded::from_reader(req.body());
        if query.is_err() {
            return http_error!(MalformedRequestBody);
        }
        let query = query.unwrap();

        // check grant_type
        if query.grant_type != "authorization_code" {
            return http_error!(UnsupportedGrantType);
        }

        // check credentials
        let auth = req.authorization();
        if auth.is_none() {
            return http_error!(MissingClientCredentials);
        }
        let auth = auth.unwrap();

        let creds = basic_auth::Credentials::from_str(&auth);
        if creds.is_err() {
            return http_error!(MalformedClientCredentials);
        }
        let creds = creds.unwrap();

        if creds.userid != query.client_id {
            return http_error!(InvalidClientCredentials);
        }

        if !ClientId::check_password(creds.userid, self.client_password_secret, creds.password) {
            return http_error!(InvalidClientCredentials);
        }

        let acd = AuthCodeData::from_code(query.code, &self.auth_code_secret, &query.client_id);
        if acd.is_err() {
            return http_error!(InvalidAuthCode);
        }
        let acd = acd.unwrap();

        // parse client_id
        let client_id: Result<ClientId, Error> = str::parse(&query.client_id);
        if client_id.is_err() {
            // should not happen, though, as client_id was already checked by the auth endpoint
            return http_error!(MalformedClientId);
        }
        let client_id = client_id.unwrap();

        // check the redirect_uri is correct
        if !client_id.check_mac(&self.client_hmac_secret, &query.redirect_uri) {
            return http_error!(InvalidClientMAC);
        }

        // NB.  We do not need to check the redirect uri, as it has already been
        //      checked by the auth endpoint.

        H::Resp::from(http::TokenResponse::IdToken(acd.id_token).into())
    }

    fn generate_client_credentials(
        &self,
        bare_id: impl AsRef<str>,
        redirect_uri: impl AsRef<str>,
    ) -> ClientCredentials {
        let client_id = ClientId::new(
            bare_id.as_ref(),
            &self.client_hmac_secret,
            redirect_uri.as_ref(),
        );
        ClientCredentials {
            password: ClientId::password(client_id.as_ref(), self.client_password_secret),
            client_id,
        }
    }
}

/// Holds the data sealed in an `auth_request_handle`.
#[doc(hidden)]
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Debug)]
struct AuthRequestData {
    state: String,
    nonce: String,
    redirect_uri: String,
    scope: String,
    client_id: String,
}

impl AuthRequestData {
    #[doc(hidden)]
    fn to_handle(&self, key: &chacha20poly1305::Key) -> anyhow::Result<String> {
        seal(&self, key, b"")
    }

    #[doc(hidden)]
    fn from_handle(handle: impl AsRef<str>, key: &chacha20poly1305::Key) -> Result<Self, Error> {
        unseal(handle, key, b"").map_err(|_| Error::InvalidAuthRequestHandle)
    }
}

/// Holds the data sealed in an `auth_code`.
#[doc(hidden)]
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Debug, Clone)]
struct AuthCodeData {
    id_token: String,
}

impl AuthCodeData {
    #[doc(hidden)]
    fn to_code(
        &self,
        key: &chacha20poly1305::Key,
        client_id: impl AsRef<str>,
    ) -> anyhow::Result<String> {
        seal(&self, key, client_id.as_ref().as_bytes())
    }

    #[doc(hidden)]
    fn from_code(
        code: impl AsRef<str>,
        key: &chacha20poly1305::Key,
        client_id: impl AsRef<str>,
    ) -> Result<Self, Error> {
        unseal(code, key, client_id.as_ref().as_bytes()).map_err(|_| Error::InvalidAuthCode)
    }
}

/// Singleton failure type for internal use
#[doc(hidden)]
#[derive(Debug, Error, PartialEq, Eq)]
enum Failure {
    #[error("failure")]
    Failure,
}

/// Encodes and encrypts the given obj with additional associated data (or b"" if none)
/// and returns it as urlsafe base64 string.  Use [unseal] to revert.
#[doc(hidden)]
fn seal<T: serde::Serialize>(
    obj: &T,
    key: &chacha20poly1305::Key,
    aad: impl AsRef<[u8]>,
) -> anyhow::Result<String> {
    let plaintext = rmp_serde::to_vec(obj).context("serializing")?;

    let nonce = XChaCha20Poly1305::generate_nonce(&mut aead::OsRng);
    let ciphertext = XChaCha20Poly1305::new(key)
        .encrypt(
            &nonce,
            aead::Payload {
                msg: plaintext.as_slice(),
                aad: aad.as_ref(),
            },
        )
        .map_err(|e| anyhow::anyhow!(e))
        .context("encrypting")?;

    let mut buf = Vec::with_capacity(nonce.len() + ciphertext.len());
    buf.extend_from_slice(&nonce);
    buf.extend_from_slice(&ciphertext);

    Ok(Base64Url::encode_string(&buf))
}

/// Reverse of the [seal] operation.
#[doc(hidden)]
fn unseal<T: serde::de::DeserializeOwned>(
    envelope: impl AsRef<str>,
    key: &chacha20poly1305::Key,
    aad: impl AsRef<[u8]>,
) -> Result<T, Failure> {
    let buf = Base64Url::decode_vec(envelope.as_ref()).map_err(|_| Failure::Failure)?;

    #[allow(dead_code)] // buf[..NONCE_LEN] is not considered usage - a bug?
    const NONCE_LEN: usize = chacha20poly1305::XNonce::LENGTH;

    if buf.len() < NONCE_LEN {
        return Err(Failure::Failure);
    }

    let plaintext = XChaCha20Poly1305::new(key)
        .decrypt(
            (&buf[..NONCE_LEN]).into(),
            aead::Payload {
                msg: &buf[NONCE_LEN..],
                aad: aad.as_ref(),
            },
        )
        .map_err(|_| Failure::Failure)?;

    rmp_serde::from_slice(&plaintext).map_err(|_| Failure::Failure)
}

/// trait to extract the length from a GenericArray
#[doc(hidden)]
trait GenericArrayExt {
    const LENGTH: usize;
}

impl<T, U: generic_array::ArrayLength<T>> GenericArrayExt for generic_array::GenericArray<T, U> {
    const LENGTH: usize = <U as typenum::marker_traits::Unsigned>::USIZE;
}

/// Type to hold secrets for internal use- basically an [u8, 32], i.e., an 'u256'
#[doc(hidden)]
type Secret = generic_array::GenericArray<u8, typenum::consts::U32>;

#[doc(hidden)]
fn derive_secret(concerns: &str, secret: &[u8]) -> Secret {
    sha2::Sha256::new()
        .chain_update(concerns.as_bytes())
        .chain_update(b"\0")
        .chain_update(secret)
        .finalize()
}

#[doc(hidden)]
fn is_valid_state(s: &Option<String>) -> bool {
    if s.is_none() {
        return false;
    }

    let s: &String = s.as_ref().unwrap();

    // see A.5 of RFC6749
    if !is_printable_ascii(s.chars()) {
        return false;
    }
    if s.is_empty() {
        return false;
    }
    true
}

/// Module for parsing Basic authorization headers such as:
///
///   Authorization: Basic czZCaGRSa3F0Mzo3RmpmcDBaQnIxS3REUmJuZlZkbUl3
///
/// See section 2 of RFC2617.
#[doc(hidden)]
mod basic_auth {
    use super::*;

    #[derive(Debug, PartialEq, Eq, Clone)]
    pub(crate) struct Credentials {
        pub userid: String,
        pub password: String,
    }

    #[derive(Error, Debug, Clone, PartialEq, Eq)]
    pub(crate) enum Error {
        #[error("does not start with 'Basic'")]
        MissingBasic,

        #[error("whitespace missing between Basic and remainder")]
        MissingWhitespace,

        #[error("invalid base64")]
        InvalidBase64,

        #[error("invalid utf8")]
        InvalidUtf8,

        #[error("missing ':' between userid and password")]
        MissingColon,
    }

    impl std::str::FromStr for Credentials {
        type Err = Error;

        fn from_str(s: &str) -> Result<Self, Error> {
            const BASIC: &str = "Basic";

            let s = s.trim_start(); // remove whitespace from start
            if !s.starts_with(BASIC) {
                return Err(Error::MissingBasic);
            }

            let s = &s[BASIC.len()..];

            // check that the remainder starts with some whitespace
            if !s
                .chars()
                .next()
                .ok_or(Error::MissingWhitespace)?
                .is_whitespace()
            {
                return Err(Error::MissingWhitespace);
            }

            // remove whitespace
            let s = s.trim();

            // decode base64
            let s = Base64::decode_vec(s).map_err(|_| Error::InvalidBase64)?;
            let s = std::str::from_utf8(&s).map_err(|_| Error::InvalidUtf8)?;

            // userid are not allowed to contain an ':'
            let pos = s.find(':').ok_or(Error::MissingColon)?;

            Ok(Credentials {
                userid: s[..pos].to_owned(),
                password: s[pos + 1..].to_owned(),
            })
        }
    }

    impl std::fmt::Display for Credentials {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "Basic {}",
                Base64::encode_string(format!("{}:{}", self.userid, self.password).as_bytes())
            )
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn from_str() {
            assert_eq!(
                Credentials::from_str(" Basic  czZCaGRSa3F0Mzo3RmpmcDBaQnIxS3REUmJuZlZkbUl3 "),
                Ok(Credentials {
                    userid: "s6BhdRkqt3".to_owned(),
                    password: "7Fjfp0ZBr1KtDRbnfVdmIw".to_owned(),
                }),
            );
        }

        #[test]
        fn to_string() {
            let c = Credentials {
                userid: "user".to_string(),
                password: "pw".to_string(),
            };

            assert_eq!(c, Credentials::from_str(&c.to_string()).unwrap());
        }
    }
}

pub mod html {
    use super::*;

    /// [escape] replaces the characters '<', '>', '&', '\'', and '"' with character references.
    ///
    /// There is a crate that does this, html_escape, but it's soo easy, it's worth doing to
    /// avoid the additional dependency.
    ///
    /// ```
    /// use pubhubs::oidc::html::escape;
    ///
    /// let s: &str = "no special characters";
    /// assert!(std::ptr::eq(escape(s).as_ref(), s));
    /// assert_eq!(escape("<>&\"'"), "&lt;&gt;&amp;&quot;&#27;");
    /// assert_eq!(
    ///     escape("and < now > with & some \" text ' in between"),
    ///     "and &lt; now &gt; with &amp; some &quot; text &#27; in between"
    /// );
    /// ```
    pub fn escape(s: &str) -> Cow<str> {
        let mut it = s
            .match_indices(['<', '>', '&', '\'', '"'].as_slice())
            .peekable();

        // no special characters, so no need for allocation
        if it.peek().is_none() {
            return Cow::Borrowed(s);
        }

        let mut result = String::with_capacity(s.len() + s.len() / 10); // 10% more

        let last_idx = it.fold(0, |previous_idx: usize, p: (usize, &str)| {
            let (idx, m) = p;

            result.push_str(&s[previous_idx..idx]);
            result.push_str(match m.chars().next().unwrap() {
                '<' => "&lt;",
                '>' => "&gt;",
                '&' => "&amp;",
                '"' => "&quot;",
                '\'' => "&#27;",
                _ => panic!("did not search for this character"),
            });

            idx + 1
        });

        result.push_str(&s[last_idx..]);

        Cow::Owned(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_id_from_str() {
        for r in vec![
            ClientId::from_str("asd"), // no tilde
            ClientId::from_str("~\0"), // invalid character
        ] {
            assert_eq!(r, Err(Error::MalformedClientId));
        }

        let c = ClientId::from_str("foo~bar").expect("expected no error");
        assert_eq!(
            c,
            ClientId {
                data: "foo~bar".to_owned(),
                tilde_pos: 3,
            },
        );

        assert_eq!(c.bare_id(), "foo");
        assert_eq!(c.mac(), "bar");

        // corner cases
        let c = ClientId::from_str("~").expect("expected no error");

        assert_eq!(c.bare_id(), "");
        assert_eq!(c.mac(), "");

        let c = ClientId::from_str("~~").expect("expected no error");

        assert_eq!(c.bare_id(), "~");
        assert_eq!(c.mac(), "");
    }

    #[test]
    fn client_id_mac() {
        // !/usr/bin/env python3
        // import hmac, base64, hashlib
        // a = hmac.new(b"secret", msg=b"foo\0uri",digestmod=hashlib.sha256).digest()
        // print(base64.urlsafe_b64encode(a))
        let cs = "foo~xMSH1zzz7OzoQbIUBkS2i-HTg__7XI4Z0t31WiIfkMU=";

        let id = "foo";
        let secret = "secret".as_bytes();
        let uri = "uri";

        assert_eq!(cs, ClientId::new(id, secret, uri).as_ref());

        let c = ClientId::from_str(cs).expect("expected no error");

        assert!(c.check_mac(secret, uri));
        assert!(!c.check_mac("5ecret".as_bytes(), uri));
        assert!(!c.check_mac(secret, "ur1"));

        // a prefix of the correct mac is not enough:
        assert!(
            !ClientId::from_str("foo~xMSH1zzz7OzoQbIUBkS2i-HTg__7XI4Z0t31WiIfkM")
                .expect("expected no error")
                .check_mac(secret, uri)
        );

        // invalid base 64 causes the mac to be invalid
        let c = ClientId::from_str("foo~not base 64!").expect("expected no error");
        assert!(!c.check_mac(secret, uri));
    }

    #[test]
    fn client_id_password() {
        // !/usr/bin/env python3
        // import hmac, base64, hashlib
        // a = hmac.new(b"secret", msg=b"foo",digestmod=hashlib.sha256).digest()
        // print(base64.urlsafe_b64encode(a))
        let pw = "dzukRpPHVT1u4g9h6l0nV6mk9KRNKEGuTpW1LkzWLbQ=";
        assert_eq!(pw, ClientId::password("foo", "secret"));
        assert!(ClientId::check_password("foo", "secret", &pw));
        assert_ne!(pw, ClientId::password("foo1", "secret"));
        assert_ne!(pw, ClientId::password("foo", "secret1"));
        assert!(!ClientId::check_password("foo1", "secret", &pw));
        assert!(!ClientId::check_password("foo", "secret1", &pw));
    }

    /// http::Request implementation used for testing
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct MockHttpRequest {
        query: String,
        method: http::Method,
        body: String,
        content_type: Option<http::ContentType>,
        authorization: Option<String>,
    }

    impl<'s> http::Request<'s> for MockHttpRequest {
        type Body = &'s [u8];

        fn query(&'s self) -> Cow<str> {
            Cow::Borrowed(&self.query)
        }

        fn method(&'s self) -> http::Method {
            self.method
        }

        fn body(&'s self) -> &[u8] {
            self.body.as_bytes()
        }

        fn content_type(&'s self) -> Option<http::ContentType> {
            self.content_type
        }

        fn authorization(&'s self) -> Option<Cow<str>> {
            match self.authorization {
                None => None,
                Some(ref s) => Some(Cow::Borrowed(s.as_str())),
            }
        }
    }

    /// http::Response used for testing
    #[derive(Debug, PartialEq, Eq)]
    enum MockHttpResponse {
        FromOidc(http::Response),
        HandleAuthPage(String),
    }

    impl From<http::Response> for MockHttpResponse {
        fn from(r: http::Response) -> Self {
            MockHttpResponse::FromOidc(r)
        }
    }

    /// Handler used for testing
    struct MockHandler {}

    impl Handler for MockHandler {
        type Req = MockHttpRequest;
        type Resp = MockHttpResponse;

        fn handle_auth(
            &self,
            _req: MockHttpRequest,
            auth_request_handle: String,
        ) -> MockHttpResponse {
            MockHttpResponse::HandleAuthPage(auth_request_handle)
        }

        fn is_valid_client(&self, client_id: &ClientId, _redirect_uri: &str) -> bool {
            !client_id.bare_id().starts_with("invalid_client")
        }
    }

    #[test]
    fn handle_auth() {
        let secret = "secret".as_bytes();
        let client_hmac_secret = super::derive_secret("client-hmac", secret);
        let auth_request_handle_secret = super::derive_secret("auth-request-handle", secret);

        let oidc = new(MockHandler {}, secret);

        let handle_auth = |query: &str| {
            oidc.handle_auth(MockHttpRequest {
                query: query.to_owned(),
                authorization: None,
                content_type: None,
                body: "".to_string(),
                method: http::Method::Get,
            })
        };

        let query1 = "response_type=code&redirect_uri=uri&client_id=foo";

        macro_rules! http_error {
            ($param:tt) => {
                MockHttpResponse::FromOidc(http::AuthResponse::from(http::S52Error::$param).into())
            };
        }

        for query in vec![
            "",
            "unknown_field=123",
            &(query1.to_owned() + "&response_type=code"),
        ] {
            assert_eq!(handle_auth(query), http_error!(MalformedQuery));
        }

        assert_eq!(handle_auth(query1), http_error!(MalformedClientId));

        assert_eq!(
            handle_auth("response_type=code&redirect_uri=uri&client_id=foo~"),
            http_error!(InvalidClientMAC)
        );

        let create_query = |bare_client_id: &str,
                            redirect_uri: &str,
                            response_mode: Option<&str>,
                            scope: Option<&str>,
                            state: Option<&str>,
                            nonce: Option<&str>,
                            response_type: &str| {
            serde_urlencoded::to_string(AuthQuery {
                response_type: response_type.to_string(),
                client_id: ClientId::new(bare_client_id, &client_hmac_secret, redirect_uri).into(),
                redirect_uri: redirect_uri.to_string(),
                response_mode: response_mode.map(|a| a.to_string()),
                scope: scope.map(|a| a.to_string()),
                state: state.map(|a| a.to_string()),
                nonce: nonce.map(|a| a.to_string()),
                display: None,
                prompt: None,
                max_age: None,
                ui_locales: None,
                id_token_hint: None,
                login_hint: None,
                acr_values: None,
            })
            .unwrap()
        };

        let create_query_from_uri = |uri: &str| {
            create_query(
                "foo",
                uri,
                Some("form_post"),
                Some("oidc"),
                Some("state"),
                Some("nonce"),
                "code",
            )
        };

        for uri in vec![
            "",
            "http://no-https.com",
            "https://example.com/#with-fragment",
            "https://valid.com/?error=using-special-field",
            "https://valid.com/?error_description=using-special-field",
            "https://valid.com/?error_uri=using-special-field",
            "https://valid.com/?code=using-special-field",
            "https://valid.com/?state=using-special-field",
        ] {
            assert_eq!(
                handle_auth(&create_query_from_uri(uri)),
                http_error!(MalformedRedirectUri)
            );
        }

        if let MockHttpResponse::HandleAuthPage(h) = handle_auth(&create_query_from_uri(
            "https://valid.com?valid_parameter=something",
        )) {
            let ard = AuthRequestData::from_handle(h, &auth_request_handle_secret).unwrap();

            assert_eq!(ard.state, "state");
            assert_eq!(ard.nonce, "nonce");
            assert_eq!(
                ard.redirect_uri,
                "https://valid.com?valid_parameter=something"
            );
            assert_eq!(ard.scope, "oidc");
            assert_eq!(ClientId::from_str(&ard.client_id).unwrap().bare_id(), "foo");
        } else {
            assert!(false);
        }

        for param in vec![
            "display",
            "prompt",
            "max_age",
            "ui_locales",
            "id_token_hint",
            "login_hint",
            "acr_values",
        ] {
            assert_eq!(
                handle_auth(
                    &(create_query_from_uri("https://valid.com?valid_parameter=something",)
                        + "&"
                        + param
                        + "=something")
                ),
                MockHttpResponse::FromOidc(http::Response::Auth(http::AuthResponse::FormPost(
                    redirect_uri::Response {
                        uri: "https://valid.com?valid_parameter=something".to_string(),
                        data: redirect_uri::ResponseData::Error {
                            error: redirect_uri::Error::UnsupportedParameter(param.to_string()),
                            state: Some("state".to_string()),
                        },
                    }
                )))
            );
        }

        // only response_mode="form_post" is accepted
        for rm in vec![None, Some("query"), Some("fragment"), Some("web_message")] {
            assert_eq!(
                handle_auth(&create_query(
                    "foo",
                    "https://valid.com?valid_parameter=something",
                    rm,
                    Some("oidc"),
                    Some("state"),
                    Some("nonce"),
                    "code",
                )),
                http_error!(UnsupportedResponseMode)
            );
        }

        for rt in vec!["", "id_token", "token"] {
            assert_eq!(
                handle_auth(&create_query(
                    "foo",
                    "https://valid.com?valid_parameter=something",
                    Some("form_post"),
                    Some("oidc"),
                    Some("state"),
                    Some("nonce"),
                    rt,
                )),
                MockHttpResponse::FromOidc(
                    http::AuthResponse::FormPost(redirect_uri::Response {
                        uri: "https://valid.com?valid_parameter=something".to_string(),
                        data: redirect_uri::ResponseData::Error {
                            error: redirect_uri::Error::UnsupportedResponseType,
                            state: Some("state".to_string()),
                        },
                    })
                    .into()
                )
            );
        }

        for s in vec![None, Some(""), Some("\0")] {
            assert_eq!(
                handle_auth(&create_query(
                    "foo",
                    "https://valid.com?valid_parameter=something",
                    Some("form_post"),
                    Some("oidc"),
                    s,
                    Some("nonce"),
                    "code",
                )),
                MockHttpResponse::FromOidc(
                    http::AuthResponse::FormPost(redirect_uri::Response {
                        uri: "https://valid.com?valid_parameter=something".to_string(),
                        data: redirect_uri::ResponseData::Error {
                            error: redirect_uri::Error::InvalidState,
                            state: s.map(|s| s.to_string())
                        },
                    })
                    .into()
                )
            );
        }

        for n in vec![None, Some(""), Some("\0")] {
            assert_eq!(
                handle_auth(&create_query(
                    "foo",
                    "https://valid.com?valid_parameter=something",
                    Some("form_post"),
                    Some("oidc"),
                    Some("state"),
                    n,
                    "code",
                )),
                MockHttpResponse::FromOidc(
                    http::AuthResponse::FormPost(redirect_uri::Response {
                        uri: "https://valid.com?valid_parameter=something".to_string(),
                        data: redirect_uri::ResponseData::Error {
                            error: redirect_uri::Error::InvalidNonce,
                            state: Some("state".to_string())
                        },
                    })
                    .into()
                )
            );
        }

        for s in vec![
            None,
            Some(""),
            Some("invalid character \0"),
            Some("another invalid character '"),
            Some("no-oidc"),
        ] {
            assert_eq!(
                handle_auth(&create_query(
                    "foo",
                    "https://valid.com?valid_parameter=something",
                    Some("form_post"),
                    s,
                    Some("state"),
                    Some("nonce"),
                    "code",
                )),
                MockHttpResponse::FromOidc(
                    http::AuthResponse::FormPost(redirect_uri::Response {
                        uri: "https://valid.com?valid_parameter=something".to_string(),
                        data: redirect_uri::ResponseData::Error {
                            error: redirect_uri::Error::InvalidScope,
                            state: Some("state".to_string()),
                        },
                    })
                    .into()
                )
            );
        }

        // NB: our mock handler rejects clients with id starting with invalid_client
        assert_eq!(
            handle_auth(&create_query(
                "invalid_client",
                "https://valid.com?valid_parameter=something",
                Some("form_post"),
                Some("oidc"),
                Some("state"),
                Some("nonce"),
                "code",
            )),
            MockHttpResponse::FromOidc(
                http::AuthResponse::FormPost(redirect_uri::Response {
                    uri: "https://valid.com?valid_parameter=something".to_string(),
                    data: redirect_uri::ResponseData::Error {
                        error: redirect_uri::Error::UnauthorizedClient,
                        state: Some("state".to_string()),
                    },
                })
                .into()
            )
        );
    }

    #[test]
    fn chacha20poly1305_lengths() {
        assert_eq!(chacha20poly1305::Key::LENGTH, 32);
        assert_eq!(chacha20poly1305::XNonce::LENGTH, 24);
        assert_eq!(chacha20poly1305::Tag::LENGTH, 16);
    }

    #[test]
    fn auth_request_data() {
        let key = XChaCha20Poly1305::generate_key(&mut aead::OsRng);
        let data = AuthRequestData {
            state: "state".to_string(),
            nonce: "nonce".to_string(),
            redirect_uri: "http://example.com".to_string(),
            scope: "scope".to_string(),
            client_id: "foo".to_string(),
        };

        let handle = data.to_handle(&key).unwrap();
        assert_eq!(Ok(data), AuthRequestData::from_handle(&handle, &key));
        assert_eq!(
            Err(Error::InvalidAuthRequestHandle),
            AuthRequestData::from_handle(
                &handle,
                &XChaCha20Poly1305::generate_key(&mut aead::OsRng),
            )
        );
        assert_eq!(
            Err(Error::InvalidAuthRequestHandle),
            AuthRequestData::from_handle("", &key)
        );
        assert_eq!(
            Err(Error::InvalidAuthRequestHandle),
            AuthRequestData::from_handle("not base64", &key)
        );
    }

    #[test]
    fn derive_secret() {
        assert_eq!(
            Base64Url::encode_string(&super::derive_secret("sauce", "secret".as_bytes())),
            // #!/usr/bin/env python3
            // import hashlib, base64
            // base64.urlsafe_b64encode( hashlib.sha256(b"sauce\0secret").digest())
            "Elu83iqLSCgBQYov_V5HPye-s_cKYc7IifxDrUMv57g="
        );
    }

    #[test]
    fn grant_code() {
        let secret = "secret".as_bytes();
        let auth_code_secret = super::derive_secret("auth-code", secret);
        let auth_request_handle_secret = super::derive_secret("auth-request-handle", secret);

        let oidc = new(MockHandler {}, secret);

        // invalid_auth_handle results in error
        assert_eq!(
            oidc.grant_code("".to_string(), |_| Ok("".to_string())),
            Err(Error::InvalidAuthRequestHandle)
        );

        let handle = AuthRequestData {
            state: "state".to_string(),
            nonce: "nonce".to_string(),
            redirect_uri: "uri".to_string(),
            scope: "scope".to_string(),
            client_id: "foo".to_string(),
        }
        .to_handle(&auth_request_handle_secret)
        .unwrap();

        // error in creation of id_token result in error
        assert_eq!(
            oidc.grant_code(handle.clone(), |_| Err(())),
            Err(Error::IdTokenCreation)
        );

        // correct inputs lead to the correct outputs
        if let Ok(http::Response::Grant(redirect_uri::Response {
            uri,
            data: redirect_uri::ResponseData::CodeGrant { code, state },
        })) = oidc.grant_code(handle, |tcd| {
            assert_eq!(
                tcd,
                TokenCreationData {
                    nonce: "nonce".to_string(),
                    client_id: "foo".to_string(),
                    scope: "scope".to_string(),
                },
            );

            Ok("id_token".to_string())
        }) {
            assert_eq!(uri, "uri".to_string());
            assert_eq!(state, "state".to_string());

            let acd = AuthCodeData::from_code(code.clone(), &auth_code_secret, "foo").unwrap();

            assert_eq!(
                acd,
                AuthCodeData {
                    id_token: "id_token".to_string(),
                },
            );

            // cannot decode auth_code with other client_id
            assert_eq!(
                AuthCodeData::from_code(code, &auth_code_secret, "bar"),
                Err(Error::InvalidAuthCode)
            );
        } else {
            assert!(false);
        }
    }

    #[test]
    fn handle_token() {
        const SECRET: &[u8] = "secret".as_bytes();

        #[derive(Clone)]
        struct S {
            auth_code_secret: Secret,
            client_hmac_secret: Secret,
            client_password_secret: Secret,
            redirect_uri: String,
            client_bare_id: String,
            client_id: Option<ClientId>,
            acd: AuthCodeData,
            credentials: basic_auth::Credentials,
            query: TokenQuery,
            req: MockHttpRequest,
        }

        impl S {
            fn set_client_id(&mut self) {
                self.client_id = Some(ClientId::new(
                    &self.client_bare_id,
                    &self.client_hmac_secret,
                    &self.redirect_uri,
                ));
            }

            fn set_credentials(&mut self) {
                self.credentials = basic_auth::Credentials {
                    userid: self.client_id.as_ref().unwrap().as_ref().to_owned(),
                    password: ClientId::password(
                        self.client_id.as_ref().unwrap().as_ref(),
                        &self.client_password_secret,
                    ),
                }
            }

            fn set_query(&mut self) {
                self.query.code = self
                    .acd
                    .to_code(&self.auth_code_secret, &self.client_id.as_ref().unwrap())
                    .unwrap();
                self.query.client_id = self.client_id.as_ref().unwrap().as_ref().to_owned();
                self.query.redirect_uri = self.redirect_uri.clone();
            }

            fn set_request(&mut self) {
                self.req.authorization = Some(self.credentials.to_string());
                self.req.body = serde_urlencoded::to_string(&self.query).unwrap();
            }

            fn handle_token(&self, oidc: &impl Oidc<H = MockHandler>) -> http::Response {
                match oidc.handle_token(self.req.clone()) {
                    MockHttpResponse::FromOidc(result) => result,
                    _ => panic!("expected FromOidc"),
                }
            }
        }

        let oidc = new(MockHandler {}, SECRET);

        let mut s = S {
            auth_code_secret: super::derive_secret("auth-code", SECRET),
            client_hmac_secret: super::derive_secret("client-hmac", SECRET),
            client_password_secret: super::derive_secret("client-password", SECRET),
            redirect_uri: "https://example.com".to_string(),
            client_bare_id: "foo".to_string(),
            client_id: None, // set by set_client_id
            acd: AuthCodeData {
                id_token: "id_token".to_string(),
            },
            credentials: basic_auth::Credentials {
                userid: "".to_string(),   // set by set_credentials
                password: "".to_string(), // idem
            },
            query: TokenQuery {
                grant_type: "authorization_code".to_string(),
                code: "".to_string(),         // set by set_query
                client_id: "".to_string(),    // idem
                redirect_uri: "".to_string(), // idem
            },
            req: MockHttpRequest {
                query: "".to_string(),
                authorization: None, // set by set_request
                content_type: Some(http::ContentType::UrlEncoded),
                body: "".to_string(), // set by set_request
                method: http::Method::Post,
            },
        };

        s.set_client_id();
        s.set_credentials();
        s.set_query();
        s.set_request();

        macro_rules! err {
            ($param:tt) => {
                http::TokenResponse::Error(http::S52Error::$param.into()).into()
            };
        }

        // first test the happy flow
        assert_eq!(
            s.handle_token(&oidc),
            http::TokenResponse::IdToken("id_token".to_string()).into()
        );

        // wrong method
        {
            let mut s = s.clone();
            s.req.method = http::Method::Get;
            assert_eq!(s.handle_token(&oidc), err!(UnsupportedMethod))
        }

        // wrong content type
        {
            let mut s = s.clone();
            s.req.content_type = None;
            assert_eq!(s.handle_token(&oidc), err!(UnsupportedContentType))
        }

        // invalid body
        {
            let mut s = s.clone();
            s.req.body = "".to_string();
            assert_eq!(s.handle_token(&oidc), err!(MalformedRequestBody))
        }

        // invalid grant type
        {
            let mut s = s.clone();
            s.query.grant_type = "invalid".to_string();
            s.set_request();
            assert_eq!(s.handle_token(&oidc), err!(UnsupportedGrantType))
        }

        // authorization problems
        {
            // missing authorization
            let mut s = s.clone();
            s.req.authorization = None;
            assert_eq!(s.handle_token(&oidc), err!(MissingClientCredentials));

            // wrong userid
            s.client_bare_id = "not_foo".to_string();
            s.set_client_id();
            s.set_credentials();
            // We don't do "s.set_query();" so query still holds "foo~..." as client_id.
            s.set_request();
            assert_eq!(s.handle_token(&oidc), err!(InvalidClientCredentials));
        }

        {
            // wrong password
            let mut s = s.clone();
            s.credentials.password = "gibberish".to_string();
            s.set_request();
            assert_eq!(s.handle_token(&oidc), err!(InvalidClientCredentials));
        }

        {
            // invalid client mac
            let mut s = s.clone();

            s.client_id = Some(ClientId::from_str("some~thing invalid").unwrap());
            s.set_credentials();
            s.set_query();
            s.set_request();
            assert_eq!(s.handle_token(&oidc), err!(InvalidClientMAC));
        }

        {
            // auth code signed by wrong key
            let mut s = s.clone();

            s.auth_code_secret = Secret::default();
            s.set_query();
            s.set_request();
            assert_eq!(s.handle_token(&oidc), err!(InvalidAuthCode));
        }

        {
            // auth code destined for other client
            let mut s = s.clone();

            let old_client_id = s.query.client_id.clone();

            s.client_bare_id = "not foo".to_string();
            s.set_client_id();
            s.set_query();
            s.query.client_id = old_client_id;
            s.set_request();

            assert_eq!(s.handle_token(&oidc), err!(InvalidAuthCode));
        }

        {
            // invalid redirect_uri
            let mut s = s.clone();

            s.query.redirect_uri = "something invalid".to_string();
            s.set_request();

            assert_eq!(s.handle_token(&oidc), err!(InvalidClientMAC));
        }
    }
}
