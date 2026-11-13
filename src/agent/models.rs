#[derive(serde::Deserialize)]
struct ModelList {
    data: Vec<ModelEntry>,
}

#[derive(serde::Deserialize)]
struct ModelEntry {
    id: String,
}

#[derive(serde::Deserialize)]
struct Recommended {
    #[serde(rename = "clinePass", default)]
    cline_pass: Vec<RecommendedModel>,
}

#[derive(serde::Deserialize)]
struct RecommendedModel {
    id: String,
}

/// The model ids an endpoint offers, sorted: the `data[].id` of `GET <url>/models`, or
/// the `clinePass[].id` of a Cline membership, which lists its plan models elsewhere.
pub async fn fetch(
    url: &str,
    token: &str,
    provider: crate::endpoint::Provider,
) -> Result<Vec<String>, String> {
    let url = format!(
        "{}/{}",
        crate::util::http::base_url(url),
        provider.models_path()
    );
    let body = crate::util::http::get(&url, token).await?;
    ids(&body, provider)
}

/// The ids in the answer of `provider`'s listing, sorted.
fn ids(body: &str, provider: crate::endpoint::Provider) -> Result<Vec<String>, String> {
    use crate::endpoint::Provider;
    let mut ids: Vec<String> = match provider {
        Provider::Cline => {
            let list: Recommended = serde_json::from_str(body).map_err(|e| e.to_string())?;
            if list.cline_pass.is_empty() {
                return Err("no clinePass model in answer".to_string());
            }
            list.cline_pass.into_iter().map(|model| model.id).collect()
        }
        _ => {
            let list: ModelList = serde_json::from_str(body).map_err(|e| e.to_string())?;
            list.data.into_iter().map(|entry| entry.id).collect()
        }
    };
    ids.sort();
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_openai_answer_gives_its_model_ids_sorted() {
        use crate::endpoint::Provider;
        let body = r#"{"data":[{"id":"b"},{"id":"a"}]}"#;
        assert_eq!(
            ids(body, Provider::OpenAi),
            Ok(["a".to_string(), "b".to_string()].to_vec())
        );
    }

    #[test]
    fn cline_answer_gives_the_plan_models_sorted() {
        use crate::endpoint::Provider;
        let body = r#"{"clinePass":[
            {"id":"cline-pass/b"},
            {"id":"cline-pass/a","name":"A","description":"x","tags":[]}
        ]}"#;
        assert_eq!(
            ids(body, Provider::Cline),
            Ok(["cline-pass/a".to_string(), "cline-pass/b".to_string()].to_vec())
        );
    }

    #[test]
    fn cline_answer_without_plan_models_is_an_error() {
        use crate::endpoint::Provider;
        assert!(ids("{}", Provider::Cline).is_err());
    }
}
