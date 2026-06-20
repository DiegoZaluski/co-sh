//! Provider registry — known LLM backends, their base URLs, default models,
//! and API key environment variable names.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    OpenAICompatible,
}

#[derive(Debug)]
pub(crate) struct ProviderConfig {
    pub name: &'static str,
    pub family: Family,
    pub base_url: &'static str,
    pub default_model: &'static str,
    pub needs_extra_headers: bool,
}

const PROVIDERS: &[(&str, ProviderConfig)] = &[
    (
        "openai",
        ProviderConfig {
            name: "openai",
            family: Family::OpenAICompatible,
            base_url: "https://api.openai.com/v1",
            default_model: "gpt-4o-mini",
            needs_extra_headers: false,
        },
    ),
    (
        "groq",
        ProviderConfig {
            name: "groq",
            family: Family::OpenAICompatible,
            base_url: "https://api.groq.com/openai/v1",
            default_model: "llama-3.3-70b-versatile",
            needs_extra_headers: false,
        },
    ),
    (
        "mistral",
        ProviderConfig {
            name: "mistral",
            family: Family::OpenAICompatible,
            base_url: "https://api.mistral.ai/v1",
            default_model: "mistral-small-latest",
            needs_extra_headers: false,
        },
    ),
    (
        "together",
        ProviderConfig {
            name: "together",
            family: Family::OpenAICompatible,
            base_url: "https://api.together.xyz/v1",
            default_model: "meta-llama/Llama-3.1-8B-Instruct-Turbo",
            needs_extra_headers: false,
        },
    ),
    (
        "openrouter",
        ProviderConfig {
            name: "openrouter",
            family: Family::OpenAICompatible,
            base_url: "https://openrouter.ai/api/v1",
            default_model: "qwen/qwen-2.5-72b-instruct",
            needs_extra_headers: true,
        },
    ),
    (
        "xai",
        ProviderConfig {
            name: "xai",
            family: Family::OpenAICompatible,
            base_url: "https://api.x.ai/v1",
            default_model: "grok-2-1212",
            needs_extra_headers: false,
        },
    ),
    (
        "deepseek",
        ProviderConfig {
            name: "deepseek",
            family: Family::OpenAICompatible,
            base_url: "https://api.deepseek.com/v1",
            default_model: "deepseek-chat",
            needs_extra_headers: false,
        },
    ),
    (
        "perplexity",
        ProviderConfig {
            name: "perplexity",
            family: Family::OpenAICompatible,
            base_url: "https://api.perplexity.ai",
            default_model: "llama-3.1-sonar-small-128k-chat",
            needs_extra_headers: false,
        },
    ),
    (
        "fireworks",
        ProviderConfig {
            name: "fireworks",
            family: Family::OpenAICompatible,
            base_url: "https://api.fireworks.ai/inference/v1",
            default_model: "qwen2.5-72b-instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "cohere",
        ProviderConfig {
            name: "cohere",
            family: Family::OpenAICompatible,
            base_url: "https://api.cohere.com/compatibility/v1",
            default_model: "command-r-plus",
            needs_extra_headers: false,
        },
    ),
    (
        "huggingface",
        ProviderConfig {
            name: "huggingface",
            family: Family::OpenAICompatible,
            base_url: "https://router.huggingface.co/v1",
            default_model: "meta-llama/Llama-3.3-70B-Instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "sambanova",
        ProviderConfig {
            name: "sambanova",
            family: Family::OpenAICompatible,
            base_url: "https://api.sambanova.ai/v1",
            default_model: "Meta-Llama-3.1-8B-Instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "poe",
        ProviderConfig {
            name: "poe",
            family: Family::OpenAICompatible,
            base_url: "https://api.poe.com/v1",
            default_model: "GPT-4o",
            needs_extra_headers: false,
        },
    ),
    (
        "cerebras",
        ProviderConfig {
            name: "cerebras",
            family: Family::OpenAICompatible,
            base_url: "https://api.cerebras.ai/v1",
            default_model: "llama-3.1-8b-chat-completion",
            needs_extra_headers: false,
        },
    ),
    (
        "nvidia",
        ProviderConfig {
            name: "nvidia",
            family: Family::OpenAICompatible,
            base_url: "https://integrate.api.nvidia.com/v1",
            default_model: "meta/llama-3.1-8b-instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "anyscale",
        ProviderConfig {
            name: "anyscale",
            family: Family::OpenAICompatible,
            base_url: "https://api.endpoints.anyscale.com/v1",
            default_model: "meta-llama/Llama-3.1-8B-Instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "vercel",
        ProviderConfig {
            name: "vercel",
            family: Family::OpenAICompatible,
            base_url: "https://ai-gateway.vercel.sh/v1",
            default_model: "meta-llama/Llama-3.1-8B-Instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "cloudflare",
        ProviderConfig {
            name: "cloudflare",
            family: Family::OpenAICompatible,
            base_url: "https://gateway.ai.cloudflare.com/v1/_/cloudflare/workers-ai/openai",
            default_model: "@cf/meta/llama-3.1-8b-instruct",
            needs_extra_headers: false,
        },
    ),
    (
        "azure",
        ProviderConfig {
            name: "azure",
            family: Family::OpenAICompatible,
            base_url: "",
            default_model: "gpt-4o",
            needs_extra_headers: false,
        },
    ),
    (
        "ollama",
        ProviderConfig {
            name: "ollama",
            family: Family::OpenAICompatible,
            base_url: "http://localhost:11434/v1",
            default_model: "llama3.3",
            needs_extra_headers: false,
        },
    ),
    (
        "lmstudio",
        ProviderConfig {
            name: "lmstudio",
            family: Family::OpenAICompatible,
            base_url: "http://localhost:1234/v1",
            default_model: "llama3.3",
            needs_extra_headers: false,
        },
    ),
    (
        "vllm",
        ProviderConfig {
            name: "vllm",
            family: Family::OpenAICompatible,
            base_url: "http://localhost:8000/v1",
            default_model: "llama3.3",
            needs_extra_headers: false,
        },
    ),
    (
        "llamacpp",
        ProviderConfig {
            name: "llamacpp",
            family: Family::OpenAICompatible,
            base_url: "http://localhost:8080/v1",
            default_model: "llama3.3",
            needs_extra_headers: false,
        },
    ),
];

const API_KEY_ENVS: &[(&str, &str)] = &[
    ("openai", "OPENAI_API_KEY"),
    ("mistral", "MISTRAL_API_KEY"),
    ("groq", "GROQ_API_KEY"),
    ("cerebras", "CEREBRAS_API_KEY"),
    ("openrouter", "OPENROUTER_API_KEY"),
    ("together", "TOGETHER_API_KEY"),
    ("huggingface", "HUGGINGFACE_API_KEY"),
    ("nvidia", "NVIDIA_API_KEY"),
    ("github", "GITHUB_TOKEN"),
    ("cloudflare", "CLOUDFLARE_API_KEY"),
    ("fireworks", "FIREWORKS_API_KEY"),
    ("ollama", "OLLAMA_API_KEY"),
    ("xai", "XAI_API_KEY"),
    ("deepseek", "DEEPSEEK_API_KEY"),
    ("perplexity", "PERPLEXITY_API_KEY"),
    ("cohere", "COHERE_API_KEY"),
    ("sambanova", "SAMBANOVA_API_KEY"),
    ("poe", "POE_API_KEY"),
    ("anyscale", "ANYSCALE_API_KEY"),
    ("vercel", "VERCEL_API_KEY"),
    ("azure", "AZURE_OPENAI_KEY"),
];

pub(crate) fn get_provider(name: &str) -> Option<&'static ProviderConfig> {
    PROVIDERS
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, config)| config)
}

pub(crate) fn get_api_key(provider: &str) -> Option<String> {
    let env_var = API_KEY_ENVS
        .iter()
        .find(|(key, _)| *key == provider)
        .map_or("OPENAI_API_KEY", |(_, var)| *var);
    std::env::var(env_var).ok()
}
