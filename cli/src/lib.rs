pub mod data;
pub mod holoburi;
pub mod holodelta;
pub mod images;
pub mod img_hash;
pub mod ogbajoj;
pub mod price_check;
pub mod products;
pub mod qna;
pub mod utils;

use std::{sync::OnceLock, time::Duration};

use clap::ValueEnum;
use reqwest::{
    Method, StatusCode,
    blocking::{Client, ClientBuilder},
};

pub const DEBUG: bool = false;

/// Warns that a source (or part of it) was skipped and its stored data kept. Besides the log, it
/// prints a GitHub Actions workflow command, so that it shows as an annotation on the run summary
/// (the run itself succeeds); outside of Actions it's just another line.
pub fn warn_skipped(message: &str) {
    eprintln!("Warning: {message}");
    println!("::warning::{}", workflow_command_value(message));
}

/// Escapes a workflow command value (one line).
fn workflow_command_value(message: &str) -> String {
    message
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// The start of a response body, for the logs (a whole HTML page is too much).
pub fn body_excerpt(body: &str) -> String {
    const MAX_CHARS: usize = 200;
    let body = body.trim();
    if body.is_empty() {
        return "empty body".into();
    }
    let excerpt: String = body.chars().take(MAX_CHARS).collect();
    let excerpt = excerpt.split_whitespace().collect::<Vec<_>>().join(" ");
    if body.chars().nth(MAX_CHARS).is_some() {
        format!("{excerpt:?}...")
    } else {
        format!("{excerpt:?}")
    }
}

const HTTP_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:144.0) Gecko/20100101 Firefox/144.0";

fn http_client_base() -> ClientBuilder {
    ClientBuilder::new()
        .user_agent(HTTP_USER_AGENT)
        .cookie_store(true)
        .timeout(Duration::from_secs(30))
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
        .max_retries_per_request(1)
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

fn deck_log_http_client() -> &'static Client {
    static DECKLOG_HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
    DECKLOG_HTTP_CLIENT.get_or_init(|| build_retry_client("decklog.bushiroad.com", true))
}

fn deck_log_en_http_client() -> &'static Client {
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

#[cfg(test)]
pub(crate) mod test_server {
    /// A local server answering each request with the next response, then closing.
    pub fn serve(responses: Vec<String>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buffer = [0; 4096];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        url
    }

    pub fn http(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_body_excerpt() {
        assert_eq!(body_excerpt(""), "empty body");
        assert_eq!(body_excerpt("  \n "), "empty body");
        assert_eq!(body_excerpt(" <p>a\n  b</p> "), "\"<p>a b</p>\"");
        let long = format!("<title>メンテナンス中</title>{}", "x".repeat(500));
        let excerpt = body_excerpt(&long);
        assert!(excerpt.starts_with("\"<title>メンテナンス中</title>xxx"));
        assert!(excerpt.ends_with("\"..."));
        assert!(excerpt.chars().count() < 210);
    }

    #[test]
    fn test_workflow_command_value() {
        assert_eq!(
            workflow_command_value("100% down\nfor now"),
            "100%25 down%0Afor now"
        );
    }
}
