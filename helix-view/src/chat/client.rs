use std::{process::Stdio, sync::Arc};

use anyhow::bail;
use arc_swap::ArcSwapOption;
use futures_util::Future;
use helix_loader::config_dir;
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::ChildStdin,
};

use super::chat_state::Message;

#[derive(Serialize)]
struct ChatRequest {
    n: u32,
    top_p: u32,
    stream: bool,
    temperature: f32,
    model: String,
    messages: Vec<Message>,
}

struct ChatClientData {
    endpoint: String,
    token: String,
}

#[derive(Clone, Default)]
pub struct ChatClient {
    data: Arc<ArcSwapOption<ChatClientData>>,
}

impl ChatClientData {
    pub async fn from_config() -> Result<Self, anyhow::Error> {
        #[derive(Deserialize)]
        struct GithubAppConfig {
            oauth_token: String,
        }

        #[derive(Deserialize)]
        struct GithubConfig {
            #[serde(flatten)]
            apps: std::collections::HashMap<String, GithubAppConfig>,
        }

        #[derive(Deserialize)]
        struct GithubTokenRespEndpoints {
            api: String,
        }

        #[derive(Deserialize)]
        struct GithubTokenResp {
            endpoints: GithubTokenRespEndpoints,
            token: String,
        }

        let mut path = config_dir();
        path.pop();
        path.push("github-copilot/apps.json");
        let config_str = tokio::fs::read_to_string(&path).await?;
        let config: GithubConfig = serde_json::de::from_str(&config_str)?;

        let app_config = config
            .apps
            .values()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No apps found in configuration"))?;
        let oauth_token = &app_config.oauth_token;

        let child = tokio::process::Command::new("curl")
            .arg("https://api.github.com/copilot_internal/v2/token")
            .arg("-H")
            .arg(format!("Authorization: Token {}", oauth_token))
            .output()
            .await?;

        if !child.status.success() {
            bail!("could not fetch token from github {:?}", child.stdout);
        }

        let token_resp: GithubTokenResp = serde_json::from_slice(&child.stdout)
            .map_err(|e| anyhow::anyhow!("Failed to parse token response: {:?}", e))?;

        let endpoint = token_resp.endpoints.api;
        let token = token_resp.token;

        Ok(Self { endpoint, token })
    }
}

impl ChatClient {
    async fn get_or_init(&self) -> Arc<ChatClientData> {
        if self.data.load().is_none() {
            self.data
                .store(Some(Arc::new(ChatClientData::from_config().await.unwrap())))
        }
        self.data.load_full().unwrap()
    }

    pub async fn send_chat<F: Future<Output = bool>>(
        &self,
        message: &[Message],
        mut callback: impl FnMut(String) -> F,
    ) {
        let config = self.get_or_init().await;
        log::info!(
            "start callback endpoint={} token={}",
            config.endpoint,
            config.token,
        );
        let mut child = tokio::process::Command::new("curl")
            .arg("--request")
            .arg("POST")
            .arg("--silent")
            .arg(format!("{}/chat/completions", &config.endpoint))
            .arg("-H")
            .arg(format!("Authorization: Bearer {}", config.token))
            .arg("-H")
            .arg("x-ms-useragent: Helix/0.1.0")
            .arg("-H")
            .arg("x-ms-user-agent: Helix/0.1.0")
            .arg("-H")
            .arg("Copilot-Integration-Id: vscode-chat")
            .arg("-H")
            .arg("Content-Type: application/json")
            .arg("--data")
            .arg("@-")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();

        log::info!("spawned curl {:?}", child.id());

        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut err_lines = BufReader::new(child.stderr.take().unwrap()).lines();
        let mut inp_lines = child.stdin.take().unwrap();

        log::info!("sending msgs curl: {message:?}");

        let request = ChatRequest {
            n: 1,
            top_p: 1,
            stream: true,
            temperature: 0.1,
            // model: "gpt-3.5-turbo".to_owned(),
            model: "gpt-4o-2024-08-06".to_owned(),
            messages: message.to_owned(),
        };
        let history = serde_json::to_vec(&request).unwrap();
        let send_result = async move {
            inp_lines.write_all(&history).await?;
            inp_lines.shutdown().await?; // Ensure stdin is properly closed
            drop::<ChildStdin>(inp_lines);
            log::info!("finish curl send");
            Ok(())
        };

        let recv_result = async {
            while let Some(line) = lines.next_line().await? {
                // log::info!("curl raw result {:?}", line);
                if line == "data: [DONE]" {
                    break;
                }
                let line = line.strip_prefix("data:").unwrap_or(&line).trim();
                if !line.is_empty() {
                    // log::info!("curl processed result {:?}", line);
                    match serde_json::from_str::<serde_json::Value>(line) {
                        Ok(parsed) => {
                            if let Some(content) = parsed["choices"]
                                .get(0)
                                .and_then(|choice| choice["delta"].get("content"))
                            {
                                if let Some(content_str) = content.as_str() {
                                    if !callback(content_str.to_owned()).await {
                                        break;
                                    }
                                    // if send.send(content_str.to_string()).await.is_err() {
                                    //     break;
                                    // }
                                    // request_redraw();
                                }
                            }
                        }
                        Err(parse_err) => {
                            log::error!(
                                "copilot response is not valid json: {line:?} {parse_err:?}"
                            );
                        }
                    }
                }
            }
            Ok::<(), tokio::io::Error>(())
        };

        let recv_stderr = async {
            while let Some(line) = err_lines.next_line().await? {
                log::error!("curl err {:?}", line);
            }
            Ok::<(), tokio::io::Error>(())
        };

        // Ensure all tasks are awaited properly and handle errors
        let _ = tokio::try_join!(send_result, recv_result, recv_stderr).map_err(|e| {
            log::error!("Error during curl execution: {:?}", e);
        });

        log::info!("wating for curl");
        // Ensure the process is cleaned up properly
        match child.wait().await {
            Err(e) => {
                log::error!("Error waiting for curl process: {:?}", e);
            }
            Ok(exit_status) => {
                if !exit_status.success() {
                    log::error!("curl error code: {:?}", exit_status);
                }
            }
        };
    }
}
