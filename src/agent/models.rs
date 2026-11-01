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

#[derive(Deserialize)]
struct Recommended {
    #[serde(rename = "clinePass", default)]
    cline_pass: Vec<RecommendedModel>,
}

#[derive(Deserialize)]
struct RecommendedModel {
    id: String,
}

/// The `cline-pass/<model>` ids of a Cline membership, billed to its subscription
/// rather than to credits, from `GET <url>/ai/cline/recommended-models`.
pub async fn fetch_cline(url: &str, token: &str) -> Result<Vec<String>, String> {
    let url = format!("{}/ai/cline/recommended-models", url.trim_end_matches('/'));
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
    let list: Recommended = response.json().await.map_err(|e| e.to_string())?;
    if list.cline_pass.is_empty() {
        return Err("no clinePass model in answer".to_string());
    }
    let mut ids: Vec<String> = list.cline_pass.into_iter().map(|model| model.id).collect();
    ids.sort();
    Ok(ids)
}

#[cfg(test)]
mod cline_tests {
    use super::*;

    #[test]
    fn cline_answer_gives_the_plan_models_sorted() {
        let body = r#"{"clinePass":[
            {"id":"cline-pass/b"},
            {"id":"cline-pass/a","name":"A","description":"x","tags":[]}
        ]}"#;
        let list: Recommended = serde_json::from_str(body).expect("valid answer");
        let ids: Vec<String> = list.cline_pass.into_iter().map(|m| m.id).collect();
        assert_eq!(ids, ["cline-pass/b", "cline-pass/a"]);
    }

    #[test]
    fn cline_answer_without_plan_models_is_an_empty_list() {
        let list: Recommended = serde_json::from_str("{}").expect("valid answer");
        assert!(list.cline_pass.is_empty());
    }
}
