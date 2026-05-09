<h1 align="center">🦀 Crustoxy - Route Claude Code to Any OpenAI-Compatible LLM</h1>

<div align="center">
    <a href="https://sonarcloud.io/summary/new_code?id=omidiyanto_crustoxy">
        <img src="https://sonarcloud.io/api/project_badges/measure?project=omidiyanto_crustoxy&metric=alert_status" alt="Quality Gate Status">
    </a>
    <br><br>
    <a href="https://github.com/omidiyanto/crustoxy/actions/workflows/ci.yaml">
        <img src="https://img.shields.io/github/actions/workflow/status/omidiyanto/crustoxy/ci.yaml?style=for-the-badge&logo=github&label=Build%20%26%20Test" alt="CI Status">
    </a>
    <a href="https://github.com/omidiyanto/crustoxy/releases">
        <img src="https://img.shields.io/github/v/release/omidiyanto/crustoxy?style=for-the-badge&logo=github&color=green" alt="Latest Release">
    </a>
    <br><br>
    <img src="https://img.shields.io/badge/Rust-red?style=for-the-badge&logo=rust&logoColor=#E57324" alt="Rust">
    <img src="https://img.shields.io/badge/docker-blue.svg?style=for-the-badge&logo=docker&logoColor=white" alt="Docker">
    <img src="https://img.shields.io/badge/Claude_Code-8A2BE2?style=for-the-badge&logo=anthropic&logoColor=white" alt="Claude">
    <img src="https://img.shields.io/github/license/omidiyanto/crustoxy?style=for-the-badge&color=blue" alt="License">
</div>
<br>

<div align="center">
    <img src="src/assets/logo.svg" alt="Crustoxy Logo">
    <h3 align="center"><i>A blazing fast and secure single-binary Rust proxy <br> empowering <a href="https://docs.anthropic.com/en/docs/agents-and-tools/claude-code/overview">Claude Code</a> with unlimited LLM models flexibility.</i></h3>
</div>

## **🤔 Why was Crustoxy created?**  
This project was built to unleash the extraordinary potential of *Claude Code*. Claude Code transcends traditional CLI coding agents due to its software architecture, which is designed as an enterprise-grade autonomous agent ecosystem rather than a simple terminal interface wrapper. Its core strength lies in agentic workflows that embed seamlessly into your local environment—capable of autonomously mapping repositories, executing terminal commands, running comprehensive test suites, and performing self-healing on errors. These functions are entirely driven by a proprietary system prompt meticulously crafted for context management optimization without demanding manual configuration.

Furthermore, this tool is fortified by a robust plugin ecosystem enabling smooth integration with various third-party services. It comes wrapped in enterprise-grade security and governance features such as anti-destructive guardrails, strict access management, and high-level privacy standards. This makes it an instant, secure, and infinitely more comprehensive plug-and-play solution for industrial scale when compared to rigid open-source alternatives.

Through **Crustoxy**, this proxy bridges Claude Code's capabilities to freely interact with 24+ different LLM providers (such as OpenAI, OpenRouter, Puter, Groq, DeepSeek, Google Gemini, Ollama, etc.), liberating it from the exclusivity constraints of the Anthropic API.

## 🎯 Core Features

- **Blazing Fast & Lightweight**: Written in pure Rust using `axum`, boasting near-zero proxy latency and an extremely minimal memory footprint perfect for long-running daemonized processes.
- **Anthropic ↔ OpenAI Compat API**: Automatically translates Anthropic's complex proprietary API requests (such as `messages`, `system`, `tools`, `thinking`) into standard, universally accepted OpenAI-compatible API requests. It then seamlessly streams the responses back using Anthropic's exact SSE (Server-Sent Events) formatting and event sequences.
- **Out-of-the-box 24+ Provider Support**: Natively integrates with 24 major LLM platforms (OpenRouter, DeepSeek, Puter, Groq, Ollama, etc.) by automatically defining base URLs and mapping provider-specific quirks, driven directly by your simple `.env` configuration.
- **Embedded Web UI Dashboard**: A beautifully designed "Nothing OS" style configuration panel built directly into the proxy. Manage your profiles, API keys, model mappings, and monitor live proxy health—all without restarting the server.
- **Smart Model Auto-Routing & Fallback**: Define multiple provider/model combinations per Claude tier (`opus`, `sonnet`, `haiku`). The internal router load-balances traffic across models (Round-Robin, Random, or Least-Errors) and seamlessly falls back to the next healthy model mid-stream if a provider goes down.
- **API Key Pooling & Rotation**: Avoid rate limits by attaching multiple API keys to a single provider. Keys are automatically cycled on each request. If a key hits a `429 Rate Limit`, it is temporarily placed on a smart cooldown and skipped until it recovers.
- **Smart 429 Rate Limit Deflection**:
  - Proactive algorithmic sliding window rate limiter that intelligently throttles concurrent bursts *before* provider limits are hit.
  - Reactive blocking with customizable exponential backoff and jitter retries when an HTTP `429` is eventually encountered.
- **RTK Token Optimization (System Prompt Compact)**: Automatically compacts Claude Code's notoriously large system prompts (often 4,000+ tokens) into a concise, factual RTK-style format (as low as 200–300 tokens) by extracting essential metadata (workspace, platform, OS) and discarding boilerplate. Saves significant token budget on every turn. Optional full override via `OVERRIDE_SYSTEM_PROMPT`.
- **Automated IP Rotation (Anti-WAF Shield)**: Actively communicates with a localized `warp-svc` daemon to automatically trigger `warp-cli` disconnection and registration renewal sequences, rotating your public Cloudflare WARP IPv4/IPv6 if all passive rate-limit retries fail to bypass IP-based blocks.
- **Zero-Latency Agentic Mocking**: Intercepts expensive internal Claude Code workspace telemetry calls (such as Quota probing, conversation title generation, and OS filepath constraint extraction) and mocks the responses instantly on the edge, bypassing wasteful API roundtrips and heavily saving token costs.
- **Advanced Think & Thought Tag Extraction**: Stateful stream parsing that intercepts inline deep-reasoning tags (`<think>...` or `<thought>...`) emitted by Open-Weights models on-the-fly, safely relocating their contents into pure, native Anthropic `thinking` blocks without interrupting the main text stream.
- **Heuristic Tool Parser Fallback**: A two-tiered safety net that statically heals structurally malformed/garbled JSON tool schemas, and dynamically detects raw text tool calls (e.g., `<function=Name><parameter=key>value</parameter>`) natively emitted by Open-Weights models. It parses their geometry on-the-fly and accurately converts them into valid Anthropic structured JSON tool call events.
- **Intelligent Auto-Retry Pipeline**: A self-healing SSE streaming architecture that detects tool-calling intent in plain text responses and automatically triggers an internal corrective retry, keeping the connection open and preventing Claude Code from stalling.
- **Synchronous Non-Streaming Fallback**: Graceful handling of standard `stream: false` requests, securely decoding raw text/tool calls back into Anthropic `MessagesResponse` format.
- **IDE Extension Compatibility**: Plug-and-play compatibility with both the official `Claude Code for VS Code` extension as well as the robust `Google Antigravity` IDE assistant workflow.

---

## 🚀 Quick Start

### 1. Prerequisites (For Native Setup)

Ensure you have **Rust** and **Cargo** installed globally. 
If you plan to use `ENABLE_IP_ROTATION=true` natively (without Docker), you **must** install Cloudflare WARP (`warp-cli`):

**Ubuntu / Debian Installation:**
```bash
# Add cloudflare gpg key
curl -fsSL https://pkg.cloudflareclient.com/pubkey.gpg | sudo gpg --yes --dearmor --output /usr/share/keyrings/cloudflare-warp-archive-keyring.gpg
# Add repo
echo "deb [signed-by=/usr/share/keyrings/cloudflare-warp-archive-keyring.gpg] https://pkg.cloudflareclient.com/ $(lsb_release -cs) main" | sudo tee /etc/apt/sources.list.d/cloudflare-client.list
# Install
sudo apt-get update && sudo apt-get install cloudflare-warp
```

### 2. Clone & Configure
   ```bash
   git clone https://github.com/omidiyanto/crustoxy.git
   cd crustoxy
   cp .env.example .env
   ```
   *Note: `.env` is now strictly for deployment variables (`HOST`, `PORT`, `ANTHROPIC_AUTH_TOKEN`). All proxy configuration is done through the Web UI.*

3. **Build & Run Locally**
   ```bash
   cargo build --release
   ./target/release/crustoxy
   ```
   *The server will start on `http://127.0.0.1:8082`*.

4. **First-Time Setup (Web Dashboard)**
   Open your browser and navigate to:
   **`http://127.0.0.1:8082/ui/`**
   
   Configure your proxy via the **Nothing OS-styled dashboard**:
   - Add multiple API keys per provider (automatically pooled and rotated).
   - Configure model routing per Claude tier (`opus`, `sonnet`, `haiku`).
   - Toggle features like IP Rotation and RTK Prompt Compact.
   - Click "Save & Apply" to hot-reload the proxy instantly.

5. **Connect Claude Code via CLI**
   Set the API URL for your local Claude Code terminal session:
   ```bash
   export ANTHROPIC_AUTH_TOKEN="sk-ant-dummy"
   export ANTHROPIC_BASE_URL="http://127.0.0.1:8082"
   claude
   ```

   **Make it persistent in `~/.bashrc`:**
   To automatically apply these variables every time you open a terminal, append them to your `~/.bashrc` (or `~/.zshrc`):
   ```bash
   echo 'export ANTHROPIC_AUTH_TOKEN="sk-ant-dummy"' >> ~/.bashrc
   echo 'export ANTHROPIC_BASE_URL="http://127.0.0.1:8082"' >> ~/.bashrc
   source ~/.bashrc
   ```

5. **Connect via Claude Code VS Code Extension**
   Crustoxy is fully compatible with the official Claude Code VS Code extension. To configure it via the raw settings file:
   1. Open the Extensions tab in VS Code and search for **Claude Code for VS Code**.
   2. Click the gear (`⚙️`) icon on the extension page and select **Extension Settings**.
   3. Find **Claude Code: Environment Variables** and click the hyperlink **"Edit in settings.json"**.
   4. Map your proxy values by inserting the JSON array like this example:
      ```json
      "claudeCode.environmentVariables": [
          {
              "name": "ANTHROPIC_BASE_URL",
              "value": "http://127.0.0.1:8082"
          },
          {
              "name": "ANTHROPIC_AUTH_TOKEN",
              "value": "sk-ant-dummy"
          }
      ]
      ```
   5. Save the file and restart your IDE for the connection to apply.

---

## 🐳 Docker Deployment

The project includes a `docker-compose.yaml` to spin up the Rust binary on an ultra-slim Debian runtime pre-installed with `warp-cli` for automated IP rotation.

```bash
# 1. Edit .env and tweak docker-compose if necessary
# 2. Start the service
docker-compose up -d --build

# Note: For Kimi OAuth interactive login, attach to the container:
# docker attach crustoxy
docker-compose logs -f
```

---

## ⚙️ Configuration Parameters

You can fine-tune Crustoxy to fit your exact infrastructure requirements via the **Web UI Dashboard** (which saves to `~/.config/crustoxy/config.toml`). Below are the key configurations and what they govern:

### 1. Server Configuration
- `HOST` *(default: `0.0.0.0`)*: The network interface the proxy runs on.
- `PORT` *(default: `8082`)*: The port the proxy listens on.
- `ANTHROPIC_AUTH_TOKEN`: Optional. Defines an arbitrary static Bearer token used to secure Crustoxy. If filled, Claude Code CLI must use the matching value in its `ANTHROPIC_AUTH_TOKEN` environment variable. Leave blank for no auth.

### 2. Model Mapping
Claude Code inherently delegates tasks between `opus`, `sonnet`, and `haiku` models implicitly. Crustoxy redirects these to the models of your choosing:
- `MODEL_OPUS` / `MODEL_SONNET` / `MODEL_HAIKU`: Format using `provider_id/model_id` (e.g., `groq/llama3-8b-8192`).
- `MODEL`: The fallback unified model router if a specific subset isn't defined.

### 3. Routing & Load Balancing Strategies
When configuring multiple models per tier or multiple API keys per provider (separated by newlines in the Web UI), Crustoxy uses your chosen routing strategy to distribute the load:
- **Round Robin** (`round_robin`): Iterates sequentially through the list of available keys or models. Ensures an even and predictable distribution of requests.
- **Least Errors** (`least_errors`): Dynamically tracks failure rates and selects the key or model with the lowest historical error count, maximizing overall reliability.
- **Random** (`random`): Randomly selects an available key or model for each request.

*(Note: If a key hits its rate limit or consecutive error threshold, it is automatically placed on a temporary cooldown before rejoining the active pool).*

### 4. Rate Limiting & Concurrency
Crustoxy employs algorithmic Sliding Window limits to prevent your account from hitting provider throttles too aggressively.
- `PROVIDER_RATE_LIMIT` *(default: `40`)*: The amount of requests allowed during the window.
- `PROVIDER_RATE_WINDOW` *(default: `60`)*: The timeframe in seconds where the rate limit applies.
- `PROVIDER_MAX_CONCURRENCY` *(default: `5`)*: Hard caps how many simultaneous HTTP requests can be inflight to the provider. Any excess requests will cleanly wait in queue.

### 5. HTTP Settings
- `HTTP_READ_TIMEOUT` *(default: `300`)*: Max time in seconds to keep a stream connection alive while waiting for inference tokens. High values are recommended for deep reasoning models.
- `HTTP_CONNECT_TIMEOUT` *(default: `10`)*: Max time in seconds allowed to establish the initial HTTP handshake with a provider.

### 6. IP Rotation
- `ENABLE_IP_ROTATION` *(default: `true`)*: If set to true, seamlessly communicates with `warp-cli` to switch IP allocations when a provider enforces persistent IP-based `429` blocks. (Requires Cloudflare WARP daemon).

### 7. RTK System Prompt Optimization
- `ENABLE_RTK` *(default: `true`)*: When enabled, Claude Code's massive default system prompt (4,000–8,000 tokens) is automatically compacted into a concise RTK-style factual summary (200–300 tokens). Essential metadata (workspace path, OS platform, OS version) is preserved; repetitive instructional boilerplate is stripped.
- `OVERRIDE_SYSTEM_PROMPT`: Leave blank to use RTK-compacted prompt. Set to any text string to fully replace the system prompt sent to the provider, bypassing both the original and the RTK-compacted version.

### 8. Optimizations & Safety Nets
- `ENABLE_NETWORK_PROBE_MOCK` / `ENABLE_TITLE_GENERATION_SKIP` / `ENABLE_SUGGESTION_MODE_SKIP` / `ENABLE_FILEPATH_EXTRACTION_MOCK`: Set to `true` to intercept internal telemetry and UI-aesthetic requests heavily spammed by Claude Code. Crustoxy mocks perfect responses instantly, slashing your API token costs heavily.
- `ENABLE_TOOL_RETRY` *(default: `true`)*: Activates the active Auto-Retry Pipeline. When set to true, if a model writes sentences indicating it wants to use a tool (e.g. "Let me run a command") but fails to actually output the structured tool JSON, Crustoxy will silently push the context back and force the model to retry.
- `TOOL_RETRY_MAX` *(default: `2`)*: The maximum amount of times Crustoxy is allowed to automatically retry the provider per single user prompt.

---

## Supported Built-in Providers

No need to figure out endpoint definitions. Just enter your API keys into the Web UI for any of the below. Multiple keys are supported per provider for automatic load balancing.

| Provider | Built-in Base URL |
| :--- | :--- |
| **Puter.com** | `https://api.puter.com/drivers/call` |
| **OpenAI** | `https://api.openai.com/v1` |
| **OpenRouter** | `https://openrouter.ai/api/v1` |
| **Groq** | `https://api.groq.com/openai/v1` |
| **DeepSeek** | `https://api.deepseek.com/v1` |
| **Google Gemini** | `https://generativelanguage.googleapis.com/v1beta/openai` |
| **Together AI** | `https://api.together.xyz/v1` |
| **Hugging Face** | `https://router.huggingface.co/v1` |
| **Mistral AI** | `https://api.mistral.ai/v1` |
| **Perplexity** | `https://api.perplexity.ai` |
| **Fireworks AI** | `https://api.fireworks.ai/inference/v1` |
| **DeepInfra** | `https://api.deepinfra.com/v1/openai` |
| **Ollama** | `http://localhost:11434/v1` |
| **Kimi OAuth** | `https://api.kimi.com/coding/v1` |
| **SumoPod** | `https://ai.sumopod.com/v1` |
| **Cloudflare AI** | `https://api.cloudflare.com/client/v4/accounts` |
| *...and 10+ more local/cloud services!* | |

*If you need to use a custom provider, simply override the Base URL in the Web UI Settings tab.*

---

## 🔄 WARP IP Rotation Mode

When `ENABLE_IP_ROTATION=true` in `.env`, the router will actively communicate with a local Cloudflare WARP daemon. 
If an API provider throws a `429 Too Many Requests` error and all internal exponential retries fail, it triggers a thread-safe native sequence to:
1. `warp-cli disconnect`
2. `warp-cli registration delete`
3. `warp-cli registration new`
4. `warp-cli connect`

This essentially rotates the outgoing IPv4/IPv6 without breaking the proxy pipeline, seamlessly bypassing IP-based rate limiting configurations set by providers.

> [!WARNING]
> **Limitations:** This IP Rotation **does not guarantee 100% success**. Cloudflare WARP uses a globally shared pool of public IPs. Frequently, these WARP IP ranges are flagged or outright blocked by various Cloud Providers and Web Application Firewalls (WAF) due to suspected *scraping bot* activity.
> 
> **Why is this feature still important?** Even though it isn't a *silver bullet*, this passive IP rotation mechanism fundamentally **extends your Session duration significantly**. Rather than having *Claude Code* permanently halt upon hitting its first *rate limit*, this feature gives the proxy a chance to "breathe" with a refreshed identity. It minimizes downtime during long, automated task executions and saves you from having to manually restart the agent.

---

## 🤝 How To Contribute
We highly encourage contributions to Crustoxy to make the routing more scalable or add optimizations to new providers. Here is how you can contribute:

1. **Fork the Repository**: Start by forking the project on GitHub and cloning it to your local development environment.
2. **Create a Feature Branch**: Branch off from `main` (e.g., `git checkout -b feature/add-new-provider`).
3. **Write Clear Code**: Ensure any new features are thoroughly documented and follow the existing architecture in `src/`.
4. **Run CI/CD Checks Locally**: Before submitting your request, please ensure your changes pass our structural guidelines:
   - Format the code: `cargo fmt`
   - Run the linter: `cargo clippy -- -Dwarnings`
   - Pass existing unit tests: `cargo test`
5. **Submit a Pull Request**: Push your branch to GitHub and open a detailed Pull Request explaining your changes and optimizations.

---
