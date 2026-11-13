use std::time::Duration;

/// The body of `GET url` with the bearer `token`, or why it failed. Every request
/// outside a model call reads its answer through here, so the time a hung endpoint may
/// take is one number.
pub async fn get(url: &str, token: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("{url}: HTTP {}", response.status()));
    }
    response.text().await.map_err(|e| e.to_string())
}

/// `url` without its trailing slash, so that a path joins onto it either way.
pub fn base_url(url: &str) -> &str {
    url.trim_end_matches('/')
}
