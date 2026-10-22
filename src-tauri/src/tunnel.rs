//! tunnel.rs — SSH 隧道：本地端口转发，让 MySQL 连接经由跳板机访问内网
//!
//! 实现方式：连接 SSH 服务端后，在 127.0.0.1 随机端口监听，
//! 每个入站 TCP 连接打开一个 `direct-tcpip` 通道双向泵送数据。

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use russh::client::{self, Handle};
use russh::ChannelMsg;

use crate::error::AppError;
use crate::models::SshConfig;

/// 信任所有主机密钥（本地工具，避免首次连接交互）
struct SshHandler;

#[async_trait::async_trait]
impl client::Handler for SshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh_keys::key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

pub struct Tunnel {
    pub local_port: u16,
    shutdown: CancellationToken,
}

impl Tunnel {
    /// 建立 SSH 隧道；返回本地监听端口
    pub async fn start(
        cfg: &SshConfig,
        target_host: &str,
        target_port: u16,
    ) -> Result<Self, AppError> {
        if cfg.host.is_empty() || cfg.user.is_empty() {
            return Err(AppError::new("SSH_CONF", "SSH 主机或用户名未填写"));
        }

        let config = Arc::new(client::Config::default());
        let mut session: Handle<SshHandler> =
            client::connect(config, (cfg.host.as_str(), cfg.port), SshHandler)
                .await
                .map_err(|e| AppError::new("SSH_CONNECT", format!("SSH 连接失败：{e}")))?;

        // 认证
        let authed = match cfg.auth.as_str() {
            "key" => {
                let path = shellexpand(&cfg.secret);
                let key = russh_keys::load_secret_key(&path, None)
                    .map_err(|e| AppError::new("SSH_KEY", format!("私钥加载失败：{e}")))?;
                session
                    .authenticate_publickey(&cfg.user, Arc::new(key))
                    .await
                    .map_err(|e| AppError::new("SSH_AUTH", format!("SSH 认证失败：{e}")))?
            }
            _ => {
                session
                    .authenticate_password(&cfg.user, &cfg.secret)
                    .await
                    .map_err(|e| AppError::new("SSH_AUTH", format!("SSH 认证失败：{e}")))?
            }
        };
        if !authed {
            return Err(AppError::new(
                "SSH_AUTH",
                "SSH 认证失败：用户名 / 密码 / 私钥不正确",
            ));
        }

        // 本地随机端口
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| AppError::new("SSH_BIND", format!("本地端口监听失败：{e}")))?;
        let local_port = listener
            .local_addr()
            .map_err(|e| AppError::new("SSH_BIND", e.to_string()))?
            .port();

        let shutdown = CancellationToken::new();
        let token = shutdown.clone();
        let target_host = target_host.to_string();

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    accepted = listener.accept() => {
                        let Ok((mut tcp, _)) = accepted else { break };
                        let Ok(channel) = session
                            .channel_open_direct_tcpip(&target_host, target_port as u32, "127.0.0.1", 0)
                            .await
                        else {
                            let _ = tcp.shutdown().await;
                            continue;
                        };
                        let t = token.clone();
                        tokio::spawn(async move {
                            pump(tcp, channel, t).await;
                        });
                    }
                }
            }
        });

        Ok(Self {
            local_port,
            shutdown,
        })
    }

    pub fn stop(&self) {
        self.shutdown.cancel();
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// 双向泵送：本地 TCP <-> SSH 通道
async fn pump(mut tcp: TcpStream, mut channel: russh::Channel<russh::client::Msg>, token: CancellationToken) {
    let mut buf = vec![0u8; 8 * 1024];
    loop {
        tokio::select! {
            biased;
            _ = token.cancelled() => break,
            r = tcp.read(&mut buf) => match r {
                Ok(0) => {
                    let _ = channel.eof().await;
                    break;
                }
                Ok(n) => {
                    if channel.data(&buf[..n]).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            },
            msg = channel.wait() => match msg {
                Some(ChannelMsg::Data { ref data }) => {
                    if tcp.write_all(data).await.is_err() {
                        break;
                    }
                }
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            },
        }
    }
    let _ = channel.close().await;
    let _ = tcp.shutdown().await;
}

/// 展开 `~` 与环境变量（私钥路径输入习惯）
fn shellexpand(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix('~')) {
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            return std::path::Path::new(&home)
                .join(rest)
                .to_string_lossy()
                .into_owned();
        }
    }
    p.to_string()
}
