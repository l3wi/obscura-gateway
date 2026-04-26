mod config;
mod cookies;
mod db;
mod gateway;
mod install;
mod models;
mod server;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use config::{AppConfig, AppPaths, ProxyPolicyConfig, rewrite_config_file};
use gateway::Gateway;
use reqwest::multipart;
use serde::Serialize;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

use crate::cookies::{CookieFormat, parse_cookies};
use crate::db::Database;
use crate::install::require_obscura;
use crate::models::{
    CreateProfileRequest, CreateSessionRequest, DumpFormat, DumpSessionRequest,
    EvaluateSessionRequest, NavigateSessionRequest, ProfileIdentity, ProfileMode,
    UpdateProfileRequest, ViewportConfig,
};
use crate::server::{AppState, app};

#[derive(Parser)]
#[command(name = "obscura-gateway")]
#[command(about = "Obscura gateway control plane and CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Setup,
    Run,
    Config(ConfigCommand),
    Session(SessionCommand),
    Profile(ProfileCommand),
    Cookies(CookiesCommand),
    Grant(GrantCommand),
    Artifacts(ArtifactsCommand),
    Events(EventsCommand),
    Quotas,
}

#[derive(Args)]
struct ConfigCommand {
    #[command(subcommand)]
    command: ConfigSubcommand,
}

#[derive(Subcommand)]
enum ConfigSubcommand {
    SetServerUrl {
        value: String,
    },
    SetApiKey {
        value: String,
    },
    SetObscuraBin {
        value: PathBuf,
    },
    SetDefaultProxyPolicy {
        value: String,
    },
    UpsertProxyPolicy {
        name: String,
        scheme: String,
        host: String,
        port: u16,
        #[arg(long)]
        username: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        country: Option<String>,
        #[arg(long)]
        city: Option<String>,
    },
    DeleteProxyPolicy {
        name: String,
    },
    Show,
}

#[derive(Args)]
struct SessionCommand {
    #[command(subcommand)]
    command: SessionSubcommand,
}

#[derive(Subcommand)]
enum SessionSubcommand {
    Create {
        #[arg(long)]
        tenant_id: Option<String>,
        #[arg(long)]
        profile: Option<String>,
        #[arg(long, default_value = "read_only")]
        profile_mode: String,
        #[arg(long = "allowed-domain")]
        allowed_domains: Vec<String>,
        #[arg(long = "denied-domain")]
        denied_domains: Vec<String>,
        #[arg(long)]
        proxy_policy: Option<String>,
    },
    List,
    Show {
        id: String,
    },
    Navigate {
        id: String,
        url: String,
        #[arg(long, default_value = "load")]
        wait_until: String,
        #[arg(long)]
        timeout: Option<u64>,
    },
    Eval {
        id: String,
        expression: String,
    },
    Dump {
        id: String,
        #[arg(long)]
        format: String,
    },
    Close {
        id: String,
    },
}

#[derive(Args)]
struct ProfileCommand {
    #[command(subcommand)]
    command: ProfileSubcommand,
}

#[derive(Subcommand)]
enum ProfileSubcommand {
    Create {
        name: String,
        #[arg(long)]
        description: String,
        #[arg(long)]
        user_agent: Option<String>,
        #[arg(long)]
        accept_language: Option<String>,
        #[arg(long)]
        timezone: Option<String>,
        #[arg(long)]
        viewport_width: Option<u32>,
        #[arg(long)]
        viewport_height: Option<u32>,
        #[arg(long)]
        screen_width: Option<u32>,
        #[arg(long)]
        screen_height: Option<u32>,
        #[arg(long)]
        proxy_affinity: Option<String>,
    },
    List,
    Show {
        id: String,
    },
    Update {
        id: String,
        #[arg(long)]
        description: String,
        #[arg(long)]
        user_agent: Option<String>,
        #[arg(long)]
        accept_language: Option<String>,
        #[arg(long)]
        timezone: Option<String>,
        #[arg(long)]
        viewport_width: Option<u32>,
        #[arg(long)]
        viewport_height: Option<u32>,
        #[arg(long)]
        screen_width: Option<u32>,
        #[arg(long)]
        screen_height: Option<u32>,
        #[arg(long)]
        proxy_affinity: Option<String>,
    },
    Delete {
        id: String,
    },
}

#[derive(Args)]
struct CookiesCommand {
    #[command(subcommand)]
    command: CookiesSubcommand,
}

#[derive(Subcommand)]
enum CookiesSubcommand {
    Import {
        #[arg(long)]
        profile: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, default_value = "auto")]
        format: String,
    },
    Export {
        #[arg(long)]
        profile: String,
        #[arg(long, default_value = "json")]
        format: String,
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

#[derive(Args)]
struct GrantCommand {
    #[command(subcommand)]
    command: GrantSubcommand,
}

#[derive(Subcommand)]
enum GrantSubcommand {
    Cdp { id: String },
}

#[derive(Args)]
struct ArtifactsCommand {
    #[command(subcommand)]
    command: ArtifactsSubcommand,
}

#[derive(Subcommand)]
enum ArtifactsSubcommand {
    List { id: String },
}

#[derive(Args)]
struct EventsCommand {
    #[command(subcommand)]
    command: EventsSubcommand,
}

#[derive(Subcommand)]
enum EventsSubcommand {
    Tail { id: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("obscura_gateway=info".parse()?),
        )
        .init();
    let cli = Cli::parse();
    let paths = AppPaths::discover()?;

    match cli.command {
        Commands::Setup => {
            paths.ensure_writable()?;
            let config = AppConfig::load_or_create(&paths)?;
            config.validate_paths(&paths)?;
            let obscura_bin = require_obscura(&config)?;
            println!(
                "verified {} and found obscura at {}",
                paths.root.display(),
                obscura_bin.display()
            );
        }
        Commands::Run => {
            paths.ensure_writable()?;
            let config = AppConfig::load_or_create(&paths)?;
            config.validate_paths(&paths)?;
            require_obscura(&config)?;
            let db = Database::open(&paths.database_file)?;
            let gateway = Arc::new(Gateway::new(paths.clone(), config.clone(), db));
            let listener = TcpListener::bind(&config.listen_addr).await?;
            let state = AppState { gateway };
            axum::serve(listener, app(state)).await?;
        }
        command => {
            paths.ensure_all()?;
            let config = AppConfig::load_or_create(&paths)?;
            handle_remote_command(command, &paths, &config).await?;
        }
    }
    Ok(())
}

async fn handle_remote_command(
    command: Commands,
    paths: &AppPaths,
    config: &AppConfig,
) -> Result<()> {
    let client = reqwest::Client::builder().build()?;
    match command {
        Commands::Config(cmd) => match cmd.command {
            ConfigSubcommand::SetServerUrl { value } => {
                let updated = rewrite_config_file(paths, |cfg| cfg.set_server_url(value.clone()))?;
                print_json(&updated);
            }
            ConfigSubcommand::SetApiKey { value } => {
                let updated = rewrite_config_file(paths, |cfg| cfg.set_api_key(value.clone()))?;
                print_json(&updated);
            }
            ConfigSubcommand::SetObscuraBin { value } => {
                let updated = rewrite_config_file(paths, |cfg| cfg.set_obscura_bin(value.clone()))?;
                print_json(&updated);
            }
            ConfigSubcommand::SetDefaultProxyPolicy { value } => {
                let updated =
                    rewrite_config_file(paths, |cfg| cfg.set_default_proxy_policy(value.clone()))?;
                print_json(&updated);
            }
            ConfigSubcommand::UpsertProxyPolicy {
                name,
                scheme,
                host,
                port,
                username,
                password,
                country,
                city,
            } => {
                let updated = rewrite_config_file(paths, |cfg| {
                    cfg.upsert_proxy_policy(
                        name.clone(),
                        ProxyPolicyConfig {
                            scheme: scheme.clone(),
                            host: host.clone(),
                            port,
                            username: username.clone(),
                            password: password.clone(),
                            country: country.clone(),
                            city: city.clone(),
                        },
                    )
                })?;
                print_json(&updated);
            }
            ConfigSubcommand::DeleteProxyPolicy { name } => {
                let mut updated = AppConfig::load_or_create(paths)?;
                updated.delete_proxy_policy(&name)?;
                updated.save(paths)?;
                print_json(&updated);
            }
            ConfigSubcommand::Show => print_json(config),
        },
        Commands::Session(cmd) => match cmd.command {
            SessionSubcommand::Create {
                tenant_id,
                profile,
                profile_mode,
                allowed_domains,
                denied_domains,
                proxy_policy,
            } => {
                let response = client
                    .post(format!("{}/v1/sessions", config.server_url))
                    .bearer_auth(&config.api_key)
                    .json(&CreateSessionRequest {
                        tenant_id,
                        profile_id: profile,
                        profile_mode: Some(parse_profile_mode(&profile_mode)?),
                        allowed_domains,
                        denied_domains,
                        proxy_policy,
                    })
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
                println!("{response}");
            }
            SessionSubcommand::List => get_and_print(&client, config, "/v1/sessions").await?,
            SessionSubcommand::Show { id } => {
                get_and_print(&client, config, &format!("/v1/sessions/{id}")).await?
            }
            SessionSubcommand::Navigate {
                id,
                url,
                wait_until,
                timeout,
            } => {
                let response = client
                    .post(format!(
                        "{}/v1/sessions/{id}/actions/navigate",
                        config.server_url
                    ))
                    .bearer_auth(&config.api_key)
                    .json(&NavigateSessionRequest {
                        url,
                        wait_until,
                        timeout_secs: timeout,
                    })
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
                println!("{response}");
            }
            SessionSubcommand::Eval { id, expression } => {
                let response = client
                    .post(format!(
                        "{}/v1/sessions/{id}/actions/eval",
                        config.server_url
                    ))
                    .bearer_auth(&config.api_key)
                    .json(&EvaluateSessionRequest { expression })
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
                println!("{response}");
            }
            SessionSubcommand::Dump { id, format } => {
                let body = client
                    .post(format!(
                        "{}/v1/sessions/{id}/actions/dump",
                        config.server_url
                    ))
                    .bearer_auth(&config.api_key)
                    .json(&DumpSessionRequest {
                        format: parse_dump_format(&format)?,
                    })
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
                println!("{body}");
            }
            SessionSubcommand::Close { id } => {
                let response = client
                    .delete(format!("{}/v1/sessions/{id}", config.server_url))
                    .bearer_auth(&config.api_key)
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
                println!("{response}");
            }
        },
        Commands::Profile(cmd) => match cmd.command {
            ProfileSubcommand::Create {
                name,
                description,
                user_agent,
                accept_language,
                timezone,
                viewport_width,
                viewport_height,
                screen_width,
                screen_height,
                proxy_affinity,
            } => {
                let response = client
                    .post(format!("{}/v1/profiles", config.server_url))
                    .bearer_auth(&config.api_key)
                    .json(&CreateProfileRequest {
                        name,
                        description,
                        identity: build_profile_identity(
                            user_agent,
                            accept_language,
                            timezone,
                            viewport_width,
                            viewport_height,
                            screen_width,
                            screen_height,
                            proxy_affinity,
                        )?,
                    })
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
                println!("{response}");
            }
            ProfileSubcommand::List => get_and_print(&client, config, "/v1/profiles").await?,
            ProfileSubcommand::Show { id } => {
                get_and_print(&client, config, &format!("/v1/profiles/{id}")).await?
            }
            ProfileSubcommand::Update {
                id,
                description,
                user_agent,
                accept_language,
                timezone,
                viewport_width,
                viewport_height,
                screen_width,
                screen_height,
                proxy_affinity,
            } => {
                let response = client
                    .patch(format!("{}/v1/profiles/{id}", config.server_url))
                    .bearer_auth(&config.api_key)
                    .json(&UpdateProfileRequest {
                        description,
                        identity: build_optional_profile_identity(
                            user_agent,
                            accept_language,
                            timezone,
                            viewport_width,
                            viewport_height,
                            screen_width,
                            screen_height,
                            proxy_affinity,
                        )?,
                    })
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
                println!("{response}");
            }
            ProfileSubcommand::Delete { id } => {
                client
                    .delete(format!("{}/v1/profiles/{id}", config.server_url))
                    .bearer_auth(&config.api_key)
                    .send()
                    .await?
                    .error_for_status()?;
                println!("deleted {id}");
            }
        },
        Commands::Cookies(cmd) => match cmd.command {
            CookiesSubcommand::Import {
                profile,
                file,
                format,
            } => {
                let raw = fs::read_to_string(&file)
                    .with_context(|| format!("failed to read {}", file.display()))?;
                let parsed = parse_cookies(&raw, parse_cookie_format(&format)?)
                    .context("failed to parse cookie input")?;
                if parsed.is_empty() {
                    bail!("no cookies found in import");
                }
                let form = multipart::Form::new().part(
                    "file",
                    multipart::Part::text(raw).file_name(
                        file.file_name()
                            .and_then(|v| v.to_str())
                            .unwrap_or("cookies.txt")
                            .to_string(),
                    ),
                );
                let response = client
                    .post(format!(
                        "{}/v1/profiles/{profile}/cookies:import",
                        config.server_url
                    ))
                    .bearer_auth(&config.api_key)
                    .multipart(form)
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
                println!("{response}");
            }
            CookiesSubcommand::Export {
                profile,
                format,
                output,
            } => {
                let body = client
                    .get(format!(
                        "{}/v1/profiles/{profile}/cookies:export?format={format}",
                        config.server_url
                    ))
                    .bearer_auth(&config.api_key)
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
                if let Some(output) = output {
                    fs::write(&output, body.as_bytes())?;
                    println!("{}", output.display());
                } else if format == "netscape" {
                    println!("{body}");
                } else {
                    println!("{body}");
                }
            }
        },
        Commands::Grant(cmd) => match cmd.command {
            GrantSubcommand::Cdp { id } => {
                let response = client
                    .post(format!("{}/v1/sessions/{id}/grants/cdp", config.server_url))
                    .bearer_auth(&config.api_key)
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
                println!("{response}");
            }
        },
        Commands::Artifacts(cmd) => match cmd.command {
            ArtifactsSubcommand::List { id } => {
                get_and_print(&client, config, &format!("/v1/sessions/{id}/artifacts")).await?
            }
        },
        Commands::Events(cmd) => match cmd.command {
            EventsSubcommand::Tail { id } => {
                let response = client
                    .get(format!("{}/v1/sessions/{id}/events", config.server_url))
                    .bearer_auth(&config.api_key)
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await?;
                print!("{response}");
            }
        },
        Commands::Quotas => get_and_print(&client, config, "/v1/quotas").await?,
        Commands::Setup | Commands::Run => {}
    }
    Ok(())
}

async fn get_and_print(client: &reqwest::Client, config: &AppConfig, path: &str) -> Result<()> {
    let response = client
        .get(format!("{}{}", config.server_url, path))
        .bearer_auth(&config.api_key)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    println!("{response}");
    Ok(())
}

fn print_json<T: Serialize>(value: &T) {
    println!("{}", serde_json::to_string_pretty(value).unwrap());
}

fn parse_cookie_format(value: &str) -> Result<CookieFormat> {
    match value {
        "auto" => Ok(CookieFormat::Auto),
        "json" => Ok(CookieFormat::Json),
        "netscape" => Ok(CookieFormat::Netscape),
        other => bail!("unsupported cookie format: {other}"),
    }
}

fn parse_dump_format(value: &str) -> Result<DumpFormat> {
    match value {
        "html" => Ok(DumpFormat::Html),
        "text" => Ok(DumpFormat::Text),
        "links" => Ok(DumpFormat::Links),
        other => bail!("unsupported dump format: {other}"),
    }
}

fn parse_profile_mode(value: &str) -> Result<ProfileMode> {
    match value {
        "read_only" => Ok(ProfileMode::ReadOnly),
        "read_write" => Ok(ProfileMode::ReadWrite),
        other => bail!("unsupported profile mode: {other}"),
    }
}

fn build_profile_identity(
    user_agent: Option<String>,
    accept_language: Option<String>,
    timezone: Option<String>,
    viewport_width: Option<u32>,
    viewport_height: Option<u32>,
    screen_width: Option<u32>,
    screen_height: Option<u32>,
    proxy_affinity: Option<String>,
) -> Result<ProfileIdentity> {
    let viewport = match (viewport_width, viewport_height) {
        (Some(width), Some(height)) => Some(ViewportConfig {
            width,
            height,
            screen_width,
            screen_height,
        }),
        (None, None) => None,
        _ => bail!("viewport_width and viewport_height must be provided together"),
    };

    if screen_width.is_some() ^ screen_height.is_some() {
        bail!("screen_width and screen_height must be provided together");
    }

    Ok(ProfileIdentity {
        user_agent,
        accept_language,
        timezone,
        viewport,
        proxy_affinity,
    })
}

fn build_optional_profile_identity(
    user_agent: Option<String>,
    accept_language: Option<String>,
    timezone: Option<String>,
    viewport_width: Option<u32>,
    viewport_height: Option<u32>,
    screen_width: Option<u32>,
    screen_height: Option<u32>,
    proxy_affinity: Option<String>,
) -> Result<Option<ProfileIdentity>> {
    if user_agent.is_none()
        && accept_language.is_none()
        && timezone.is_none()
        && viewport_width.is_none()
        && viewport_height.is_none()
        && screen_width.is_none()
        && screen_height.is_none()
        && proxy_affinity.is_none()
    {
        return Ok(None);
    }
    Ok(Some(build_profile_identity(
        user_agent,
        accept_language,
        timezone,
        viewport_width,
        viewport_height,
        screen_width,
        screen_height,
        proxy_affinity,
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cookies::export_netscape;
    use tempfile::tempdir;

    #[test]
    fn config_paths_are_under_root() {
        let dir = tempdir().unwrap();
        let paths = AppPaths::from_root(dir.path().join(".obscura-gateway"));
        paths.ensure_all().unwrap();
        let config = AppConfig::default_for_paths(&paths);
        config.save(&paths).unwrap();
        assert!(paths.config_file.exists());
        assert!(paths.cookies_dir.exists());
        assert!(
            paths
                .database_file
                .parent()
                .unwrap()
                .ends_with(".obscura-gateway")
        );
    }

    #[test]
    fn cookie_format_parser_accepts_expected_values() {
        assert!(matches!(
            parse_cookie_format("auto").unwrap(),
            CookieFormat::Auto
        ));
        assert!(parse_cookie_format("bad").is_err());
    }

    #[test]
    fn dump_format_parser_accepts_expected_values() {
        assert!(matches!(
            parse_dump_format("html").unwrap(),
            DumpFormat::Html
        ));
        assert!(parse_dump_format("bad").is_err());
    }

    #[test]
    fn profile_mode_parser_accepts_expected_values() {
        assert!(matches!(
            parse_profile_mode("read_only").unwrap(),
            ProfileMode::ReadOnly
        ));
        assert!(parse_profile_mode("bad").is_err());
    }

    #[test]
    fn profile_identity_builder_requires_complete_viewport() {
        assert!(
            build_profile_identity(None, None, None, Some(100), None, None, None, None).is_err()
        );
    }

    #[test]
    fn netscape_export_contains_cookie_name() {
        let output = export_netscape(&[crate::models::StoredCookie {
            name: "sid".into(),
            value: "abc".into(),
            domain: ".example.com".into(),
            path: "/".into(),
            secure: true,
            http_only: false,
            expires: None,
        }]);
        assert!(output.contains("sid"));
    }
}
