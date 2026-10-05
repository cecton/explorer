use serde::Deserialize;
use std::time::Duration;

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

/// Model ids served by an OpenAI-compatible server, from `GET <url>/models`, sorted.
pub async fn fetch(url: &str, token: &str) -> Result<Vec<String>, String> {
    let url = format!("{}/models", url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("{url}: HTTP {}", response.status()));
    }
    let list: ModelList = response.json().await.map_err(|e| e.to_string())?;
    let mut ids: Vec<String> = list.data.into_iter().map(|entry| entry.id).collect();
    ids.sort();
    Ok(ids)
}
