use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};
use forge_app::HttpInfra;
use forge_domain::{InputModality, Model, ModelSource, Provider, ProviderId};
use serde::Deserialize;
use url::Url;

/// Public catalog endpoint; never send provider credentials to this host.
pub(super) const URL: &str = "https://models.dev/api.json";

/// Whether this provider uses the built-in Coding Plan catalog.
pub(super) fn is_zai_catalog(provider: &Provider<Url>) -> bool {
    provider.id == ProviderId::ZAI_CODING
        && matches!(&provider.models, Some(ModelSource::Url(url)) if url.as_str() == URL)
}

#[derive(Deserialize)]
struct Catalog {
    #[serde(rename = "zai-coding-plan")]
    provider: CatalogProvider,
}

#[derive(Deserialize)]
struct CatalogProvider {
    models: BTreeMap<String, CatalogModel>,
}

#[derive(Deserialize)]
struct CatalogModel {
    id: String,
    name: String,
    description: Option<String>,
    tool_call: bool,
    reasoning: bool,
    limit: Limits,
    modalities: Modalities,
}

#[derive(Deserialize)]
struct Limits {
    context: u64,
}

#[derive(Deserialize)]
struct Modalities {
    input: Vec<String>,
}

fn parse_models(body: &str) -> Result<Vec<Model>> {
    let catalog: Catalog = serde_json::from_str(body).context("Invalid models.dev catalog")?;
    anyhow::ensure!(
        !catalog.provider.models.is_empty(),
        "Empty Coding Plan catalog"
    );
    catalog
        .provider
        .models
        .into_iter()
        .map(|(id, entry)| {
            anyhow::ensure!(!id.is_empty() && id == entry.id, "Invalid catalog model ID");
            let input_modalities = entry
                .modalities
                .input
                .into_iter()
                .filter_map(|modality| match modality.as_str() {
                    "text" => Some(InputModality::Text),
                    "image" => Some(InputModality::Image),
                    // Forge cannot send video or PDF content blocks.
                    _ => None,
                })
                .collect();
            let mut model = Model::new(id)
                .name(entry.name)
                .context_length(entry.limit.context)
                .tools_supported(entry.tool_call)
                .supports_reasoning(entry.reasoning)
                .input_modalities(input_modalities);
            model.description = entry.description;
            // tool_call does not imply support for parallel tool calls.
            Ok(model)
        })
        .collect()
}

/// Fetches Coding Plan metadata without provider authentication headers.
///
/// # Errors
/// Returns an error on timeout, HTTP failure, or an invalid/empty catalog.
pub(super) async fn fetch_models(http: &impl HttpInfra) -> Result<Vec<Model>> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let response = http.http_get(&Url::parse(URL)?, None).await?;
        let body = response.error_for_status()?.text().await?;
        parse_models(&body)
    })
    .await
    .context("Timed out fetching models.dev catalog")?
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::*;

    fn fixture() -> serde_json::Value {
        json!({
            "zai": {"models": {"wrong-provider": {}}},
            "zai-coding-plan": {"models": {
                "glm-new": {
                    "id": "glm-new", "name": "New GLM", "description": "New release",
                    "tool_call": true, "reasoning": false,
                    "limit": {"context": 1000000, "output": 131072},
                    "modalities": {"input": ["text", "image", "video", "pdf"]}
                },
                "glm-basic": {
                    "id": "glm-basic", "name": "Basic GLM",
                    "tool_call": false, "reasoning": true,
                    "limit": {"context": 204800},
                    "modalities": {"input": ["text"]}
                }
            }}
        })
    }

    #[test]
    fn maps_only_coding_plan_models_and_supported_capabilities() {
        let fixture = fixture();
        let actual = parse_models(&fixture.to_string()).unwrap();
        let expected = vec![
            Model::new("glm-basic")
                .name("Basic GLM".to_owned())
                .tools_supported(false)
                .supports_reasoning(true)
                .context_length(204800),
            Model::new("glm-new")
                .name("New GLM".to_owned())
                .description("New release".to_owned())
                .tools_supported(true)
                .supports_reasoning(false)
                .context_length(1000000)
                .input_modalities(vec![InputModality::Text, InputModality::Image]),
        ];
        assert_eq!(actual, expected);
    }

    #[test]
    fn rejects_unusable_catalogs() {
        let mut mismatch = fixture();
        mismatch["zai-coding-plan"]["models"]["glm-new"]["id"] = json!("wrong");
        let fixtures = vec![
            "not json".to_owned(),
            json!({"zai": {"models": {}}}).to_string(),
            json!({"zai-coding-plan": {"models": {}}}).to_string(),
            json!({"zai-coding-plan": {"models": {"broken": {"id": "broken"}}}}).to_string(),
            mismatch.to_string(),
        ];
        for fixture in fixtures {
            let actual = parse_models(&fixture);
            assert!(actual.is_err());
        }
    }
}
