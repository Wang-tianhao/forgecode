use std::sync::Arc;

use forge_app::domain::{
    ChatCompletionMessage, Context, Model, ModelId, ProviderResponse, ResultStream,
};
use forge_app::{EnvironmentInfra, HttpInfra};
use forge_domain::{ChatRepository, Provider, ProviderId};
use forge_infra::CacacheStorage;
use tokio::task::AbortHandle;
use url::Url;

use crate::provider::anthropic::AnthropicResponseRepository;
use crate::provider::bedrock::BedrockResponseRepository;
use crate::provider::google::GoogleResponseRepository;
use crate::provider::openai::OpenAIResponseRepository;
use crate::provider::openai_responses::OpenAIResponsesResponseRepository;
use crate::provider::opencode::OpenCodeZenResponseRepository;

/// Repository responsible for routing chat requests to the appropriate provider
/// implementation based on the provider's response type.
pub struct ForgeChatRepository<F> {
    router: Arc<ProviderRouter<F>>,
    model_cache: Arc<CacacheStorage>,
    catalog_cache: Arc<CacacheStorage>,
    bg_refresh: BgRefresh,
}

impl<F: EnvironmentInfra<Config = forge_config::ForgeConfig> + HttpInfra> ForgeChatRepository<F> {
    /// Creates a new ForgeChatRepository with the given infrastructure.
    ///
    /// # Arguments
    ///
    /// * `infra` - Infrastructure providing environment and HTTP capabilities
    pub fn new(infra: Arc<F>) -> Self {
        let env = infra.get_environment();
        let config = infra.get_config().unwrap_or_default();
        let model_cache_ttl_secs = config.model_cache_ttl_secs;

        let openai_repo = OpenAIResponseRepository::new(infra.clone());
        let codex_repo = OpenAIResponsesResponseRepository::new(infra.clone());
        let anthropic_repo = AnthropicResponseRepository::new(infra.clone());
        let bedrock_repo =
            BedrockResponseRepository::new(Arc::new(config.retry.unwrap_or_default()));
        let google_repo = GoogleResponseRepository::new(infra.clone());
        let opencode_zen_repo = OpenCodeZenResponseRepository::new(infra.clone());

        let model_cache = Arc::new(CacacheStorage::new(
            env.cache_dir().join("model_cache"),
            Some(model_cache_ttl_secs as u128),
        ));
        // Keep the last successful public catalog available during outages.
        let catalog_cache = Arc::new(CacacheStorage::new(
            env.cache_dir().join("model_catalog_cache"),
            None,
        ));

        Self {
            router: Arc::new(ProviderRouter {
                openai_repo,
                codex_repo,
                anthropic_repo,
                bedrock_repo,
                google_repo,
                opencode_zen_repo,
            }),
            model_cache,
            catalog_cache,
            bg_refresh: BgRefresh::default(),
        }
    }
}

#[async_trait::async_trait]
impl<F: EnvironmentInfra<Config = forge_config::ForgeConfig> + HttpInfra + Sync> ChatRepository
    for ForgeChatRepository<F>
{
    async fn chat(
        &self,
        model_id: &ModelId,
        context: Context,
        provider: Provider<Url>,
    ) -> ResultStream<ChatCompletionMessage, anyhow::Error> {
        self.router.chat(model_id, context, provider).await
    }

    async fn models(&self, provider: Provider<Url>) -> anyhow::Result<Vec<Model>> {
        use forge_app::KVStore;

        let is_catalog = super::catalog::is_zai_catalog(&provider);
        // Do not reuse an old bundled list or a custom provider's cached models.
        let cache_key = if is_catalog {
            format!("models.dev:{}", provider.id)
        } else {
            format!("models:{}", provider.id)
        };

        if let Ok(Some(cached)) = self
            .model_cache
            .cache_get::<_, Vec<Model>>(&cache_key)
            .await
        {
            tracing::debug!(provider_id = %provider.id, "returning cached models; refreshing in background");

            // Spawn a background task to refresh the disk cache. The abort
            // handle is stored so the task is cancelled if the service is dropped.
            let cache = self.model_cache.clone();
            let catalog_cache = self.catalog_cache.clone();
            let router = self.router.clone();
            let key = cache_key;
            let handle = tokio::spawn(async move {
                match router.models(provider).await {
                    Ok(models) => {
                        if is_catalog && let Err(err) = catalog_cache.cache_set(&key, &models).await
                        {
                            tracing::warn!(error = %err, "failed to retain last successful catalog");
                        }
                        if let Err(err) = cache.cache_set(&key, &models).await {
                            tracing::warn!(error = %err, "background refresh: failed to cache model list");
                        }
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "background refresh: failed to fetch models");
                    }
                }
            });
            self.bg_refresh.register(handle.abort_handle());

            return Ok(cached);
        }

        let models = match self.router.models(provider).await {
            Ok(models) => models,
            Err(error) if is_catalog => {
                if let Ok(Some(cached)) = self.catalog_cache.cache_get(&cache_key).await {
                    tracing::warn!(%error, "catalog unavailable; using last successful catalog");
                    return Ok(cached);
                }
                tracing::warn!(%error, "catalog unavailable; using bundled Coding Plan models");
                // Never cache the fallback: retry the catalog on the next request.
                return Ok(super::provider_repo::bundled_zai_models());
            }
            Err(error) => return Err(error),
        };

        if is_catalog && let Err(err) = self.catalog_cache.cache_set(&cache_key, &models).await {
            tracing::warn!(error = %err, "failed to retain last successful catalog");
        }
        if let Err(err) = self.model_cache.cache_set(&cache_key, &models).await {
            tracing::warn!(error = %err, "failed to cache model list");
        }

        Ok(models)
    }
}

/// Routes chat and model requests to the correct provider backend.
struct ProviderRouter<F> {
    openai_repo: OpenAIResponseRepository<F>,
    codex_repo: OpenAIResponsesResponseRepository<F>,
    anthropic_repo: AnthropicResponseRepository<F>,
    bedrock_repo: BedrockResponseRepository,
    google_repo: GoogleResponseRepository<F>,
    opencode_zen_repo: OpenCodeZenResponseRepository<F>,
}

impl<F: HttpInfra + EnvironmentInfra<Config = forge_config::ForgeConfig> + Sync> ProviderRouter<F> {
    async fn chat(
        &self,
        model_id: &ModelId,
        context: Context,
        provider: Provider<Url>,
    ) -> ResultStream<ChatCompletionMessage, anyhow::Error> {
        match provider.response {
            Some(ProviderResponse::OpenAI) => {
                // Check if model is a Codex model
                if model_id.as_str().contains("gpt-5")
                    && (provider.id == ProviderId::OPENAI
                        || provider.id == ProviderId::GITHUB_COPILOT
                        || provider.id == ProviderId::CODEX)
                {
                    self.codex_repo.chat(model_id, context, provider).await
                } else if provider.id == ProviderId::CODEX
                    || (provider.id == ProviderId::GITHUB_COPILOT
                        && model_id.as_str() == "grok-4.6")
                {
                    // All Codex models and Copilot's Grok 4.6 require the Responses API.
                    self.codex_repo.chat(model_id, context, provider).await
                } else {
                    self.openai_repo.chat(model_id, context, provider).await
                }
            }
            Some(ProviderResponse::OpenAIResponses) => {
                self.codex_repo.chat(model_id, context, provider).await
            }
            Some(ProviderResponse::Anthropic) => {
                self.anthropic_repo.chat(model_id, context, provider).await
            }
            Some(ProviderResponse::Bedrock) => {
                self.bedrock_repo.chat(model_id, context, provider).await
            }
            Some(ProviderResponse::Google) => {
                self.google_repo.chat(model_id, context, provider).await
            }
            Some(ProviderResponse::OpenCode) => {
                self.opencode_zen_repo
                    .chat(model_id, context, provider)
                    .await
            }
            None => Err(anyhow::anyhow!(
                "Provider response type not configured for provider: {}",
                provider.id
            )),
        }
    }

    async fn models(&self, provider: Provider<Url>) -> anyhow::Result<Vec<Model>> {
        match provider.response {
            Some(ProviderResponse::OpenAI) => self.openai_repo.models(provider).await,
            Some(ProviderResponse::OpenAIResponses) => self.codex_repo.models(provider).await,
            Some(ProviderResponse::Anthropic) => self.anthropic_repo.models(provider).await,
            Some(ProviderResponse::Bedrock) => self.bedrock_repo.models(provider).await,
            Some(ProviderResponse::Google) => self.google_repo.models(provider).await,
            Some(ProviderResponse::OpenCode) => self.opencode_zen_repo.models(provider).await,
            None => Err(anyhow::anyhow!(
                "Provider response type not configured for provider: {}",
                provider.id
            )),
        }
    }
}

/// Tracks abort handles for background tasks and cancels them on drop.
#[derive(Default)]
struct BgRefresh(std::sync::Mutex<Vec<AbortHandle>>);

impl BgRefresh {
    /// Registers an abort handle to be cancelled when this guard is dropped.
    fn register(&self, handle: AbortHandle) {
        if let Ok(mut handles) = self.0.lock() {
            handles.push(handle);
        }
    }
}

impl Drop for BgRefresh {
    fn drop(&mut self) {
        if let Ok(mut handles) = self.0.lock() {
            for handle in handles.drain(..) {
                handle.abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use bytes::Bytes;
    use fake::{Fake, Faker};
    use forge_app::KVStore;
    use forge_domain::{ConfigOperation, Environment, ModelSource};
    use forge_eventsource::EventSource;
    use pretty_assertions::assert_eq;
    use reqwest::header::HeaderMap;
    use serde_json::json;

    use super::*;

    struct FixtureInfra {
        directory: tempfile::TempDir,
        server_url: String,
    }

    impl EnvironmentInfra for FixtureInfra {
        type Config = forge_config::ForgeConfig;

        fn get_env_var(&self, _: &str) -> Option<String> {
            None
        }

        fn get_env_vars(&self) -> BTreeMap<String, String> {
            BTreeMap::new()
        }

        fn get_environment(&self) -> Environment {
            let mut env: Environment = Faker.fake();
            env.base_path = self.directory.path().to_owned();
            env
        }

        fn get_config(&self) -> anyhow::Result<Self::Config> {
            Ok(Self::Config::default())
        }

        async fn update_environment(&self, _: Vec<ConfigOperation>) -> anyhow::Result<()> {
            unreachable!()
        }
    }

    #[async_trait::async_trait]
    impl HttpInfra for FixtureInfra {
        async fn http_get(
            &self,
            url: &Url,
            headers: Option<HeaderMap>,
        ) -> anyhow::Result<reqwest::Response> {
            assert_eq!(url.as_str(), super::super::catalog::URL);
            assert_eq!(headers, None);
            Ok(reqwest::get(&self.server_url).await?)
        }

        async fn http_post(
            &self,
            _: &Url,
            _: Option<HeaderMap>,
            _: Bytes,
        ) -> anyhow::Result<reqwest::Response> {
            unreachable!()
        }

        async fn http_delete(&self, _: &Url) -> anyhow::Result<reqwest::Response> {
            unreachable!()
        }

        async fn http_eventsource(
            &self,
            _: &Url,
            _: Option<HeaderMap>,
            _: Bytes,
        ) -> anyhow::Result<EventSource> {
            unreachable!()
        }
    }

    fn fixture(server: &mockito::ServerGuard) -> ForgeChatRepository<FixtureInfra> {
        ForgeChatRepository::new(Arc::new(FixtureInfra {
            directory: tempfile::tempdir().unwrap(),
            server_url: server.url(),
        }))
    }

    fn provider() -> Provider<Url> {
        Provider {
            id: ProviderId::ZAI_CODING,
            provider_type: Default::default(),
            response: Some(ProviderResponse::OpenAI),
            url: Url::parse("https://api.z.ai/api/coding/paas/v4/chat/completions").unwrap(),
            models: Some(ModelSource::Url(
                Url::parse(super::super::catalog::URL).unwrap(),
            )),
            auth_methods: vec![forge_domain::AuthMethod::ApiKey],
            url_params: vec![],
            credential: Some(forge_domain::AuthCredential::new_api_key(
                ProviderId::ZAI_CODING,
                forge_domain::ApiKey::from("secret-must-not-leak".to_owned()),
            )),
            custom_headers: Some(std::collections::HashMap::from([(
                "x-private-header".to_owned(),
                "private-value".to_owned(),
            )])),
        }
    }

    async fn wait_for_refresh(fixture: &ForgeChatRepository<FixtureInfra>) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fixture
                    .bg_refresh
                    .0
                    .lock()
                    .unwrap()
                    .iter()
                    .all(AbortHandle::is_finished)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn catalog_failure_uses_uncached_bundle_then_recovers() {
        let mut server = mockito::Server::new_async().await;
        let fixture = fixture(&server);
        let failed = server
            .mock("GET", "/")
            .with_status(503)
            .create_async()
            .await;
        let actual = fixture.models(provider()).await.unwrap();
        let expected = super::super::provider_repo::bundled_zai_models();
        assert_eq!(actual, expected);
        assert!(!actual.is_empty());
        let actual: Option<Vec<Model>> = fixture
            .catalog_cache
            .cache_get(&format!("models.dev:{}", ProviderId::ZAI_CODING))
            .await
            .unwrap();
        assert_eq!(actual, None);
        failed.assert_async().await;
        failed.remove_async().await;

        let response = json!({"zai-coding-plan": {"models": {"new-model": {
            "id": "new-model", "name": "New model", "tool_call": true,
            "reasoning": true, "limit": {"context": 1000000},
            "modalities": {"input": ["text"]}
        }}}});
        let success = server
            .mock("GET", "/")
            .with_status(200)
            .with_body(response.to_string())
            .create_async()
            .await;
        let actual = fixture.models(provider()).await.unwrap();
        let expected = vec![
            Model::new("new-model")
                .name("New model".to_owned())
                .tools_supported(true)
                .supports_reasoning(true)
                .context_length(1000000),
        ];
        assert_eq!(actual, expected);
        success.assert_async().await;
        let actual: Vec<Model> = fixture
            .catalog_cache
            .cache_get(&format!("models.dev:{}", ProviderId::ZAI_CODING))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn invalid_refresh_preserves_last_successful_catalog() {
        // Exercise both a background refresh and a foreground cache miss.
        for fresh in [true, false] {
            let mut server = mockito::Server::new_async().await;
            let fixture = fixture(&server);
            let expected = vec![Model::new("last-known-good")];
            let key = format!("models.dev:{}", ProviderId::ZAI_CODING);
            fixture
                .catalog_cache
                .cache_set(&key, &expected)
                .await
                .unwrap();
            if fresh {
                fixture
                    .model_cache
                    .cache_set(&key, &expected)
                    .await
                    .unwrap();
            }
            let mock = server
                .mock("GET", "/")
                .with_status(200)
                .with_body(r#"{"zai-coding-plan":{"models":{}}}"#)
                .create_async()
                .await;

            let actual = fixture.models(provider()).await.unwrap();
            assert_eq!(actual, expected);
            wait_for_refresh(&fixture).await;
            let actual: Vec<Model> = fixture
                .catalog_cache
                .cache_get(&key)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(actual, expected);
            mock.assert_async().await;
        }
    }

    #[tokio::test]
    async fn explicit_hardcoded_override_does_not_fetch_catalog() {
        let server = mockito::Server::new_async().await;
        let fixture = fixture(&server);
        let expected = vec![Model::new("custom")];
        let mut provider = provider();
        provider.models = Some(ModelSource::Hardcoded(expected.clone()));
        let actual = fixture.models(provider).await.unwrap();
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    #[ignore = "requires network access to models.dev"]
    async fn live_coding_plan_catalog() {
        let fixture = FixtureInfra {
            directory: tempfile::tempdir().unwrap(),
            server_url: super::super::catalog::URL.to_owned(),
        };
        let actual = super::super::catalog::fetch_models(&fixture).await.unwrap();
        assert!(!actual.is_empty());
        assert!(actual.iter().all(|model| model.context_length.is_some()));
        println!(
            "Catalog model IDs: {:?}",
            actual.iter().map(|m| m.id.as_str()).collect::<Vec<_>>()
        );
    }
}
