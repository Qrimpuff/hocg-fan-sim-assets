pub trait TrimOnce {
    fn trim_start_once(&self, pat: &str) -> &str;
    fn trim_end_once(&self, pat: &str) -> &str;
}

impl TrimOnce for str {
    fn trim_start_once(&self, pat: &str) -> &str {
        self.strip_prefix(pat).unwrap_or(self)
    }

    fn trim_end_once(&self, pat: &str) -> &str {
        self.strip_suffix(pat).unwrap_or(self)
    }
}

pub fn sanitize_filename(filename: &str) -> String {
    filename
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\' | '/' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

#[must_use]
pub fn clean_text(text: &str) -> String {
    // anything that needs to be done to have clean text goes here
    // I tried unicode-normalization but it messed up some characters
    text.trim().to_string()
}
