pub mod data;
pub mod holodelta;
pub mod images;
pub mod ogbajoj;
pub mod price_check;
pub mod qna;
pub mod utils;

use std::{sync::OnceLock, time::Duration};

use clap::ValueEnum;
use reqwest::{
    Method, StatusCode,
    blocking::{Client, ClientBuilder},
};

pub const DEBUG: bool = false;

const HTTP_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:144.0) Gecko/20100101 Firefox/144.0";

fn http_client_base() -> ClientBuilder {
    ClientBuilder::new()
        .user_agent(HTTP_USER_AGENT)
        .cookie_store(true)
        .timeout(Duration::from_secs(70))
}

fn build_retry_client(host: &'static str, retry_post: bool) -> Client {
    http_client_base()
        .retry(build_retry_policy(host, retry_post))
        .build()
        .unwrap()
}

fn should_retry_transient_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn build_retry_policy(host: &'static str, retry_post: bool) -> reqwest::retry::Builder {
    reqwest::retry::for_host(host)
        .max_retries_per_request(3)
        .classify_fn(move |req_rep| match (req_rep.method(), req_rep.status()) {
            (&Method::GET | &Method::HEAD, Some(status))
                if should_retry_transient_status(status) =>
            {
                req_rep.retryable()
            }
            (&Method::GET | &Method::HEAD, None) if req_rep.error().is_some() => {
                req_rep.retryable()
            }
            (&Method::POST, Some(status))
                if retry_post && should_retry_transient_status(status) =>
            {
                req_rep.retryable()
            }
            (&Method::POST, None) if retry_post && req_rep.error().is_some() => req_rep.retryable(),
            _ => req_rep.success(),
        })
}

fn http_client() -> &'static Client {
    static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
    HTTP_CLIENT.get_or_init(|| http_client_base().build().unwrap())
}

fn decklog_http_client() -> &'static Client {
    static DECKLOG_HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
    DECKLOG_HTTP_CLIENT.get_or_init(|| build_retry_client("decklog.bushiroad.com", true))
}

fn decklog_en_http_client() -> &'static Client {
    static DECKLOG_EN_HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
    DECKLOG_EN_HTTP_CLIENT.get_or_init(|| build_retry_client("decklog-en.bushiroad.com", true))
}

fn google_docs_http_client() -> &'static Client {
    static GOOGLE_DOCS_HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
    GOOGLE_DOCS_HTTP_CLIENT.get_or_init(|| build_retry_client("docs.google.com", false))
}

fn google_sheets_api_http_client() -> &'static Client {
    static GOOGLE_SHEETS_API_HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
    GOOGLE_SHEETS_API_HTTP_CLIENT.get_or_init(|| build_retry_client("sheets.googleapis.com", false))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Language {
    All,
    Japanese,
    English,
}

impl From<Language> for hocg_fan_sim_assets_model::Language {
    fn from(value: Language) -> Self {
        match value {
            Language::All => panic!(
                "Language::All is not a valid language for hocg_fan_sim_assets_model::Language"
            ),
            Language::Japanese => hocg_fan_sim_assets_model::Language::Japanese,
            Language::English => hocg_fan_sim_assets_model::Language::English,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PriceCheckMode {
    /// Use the first urls found
    Quick,
    /// Compare images to find the best match
    Images,
}
