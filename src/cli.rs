use crate::auth;
use crate::db::DbPool;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use clap::{Parser, Subcommand};
use rand::RngCore;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "albion_rest_api")]
#[command(about = "Albion REST API server and key management")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    #[command(about = "Start the API server")]
    Serve {
        #[arg(long)]
        config: PathBuf,
    },
    #[command(about = "Manage API keys")]
    Keys {
        #[arg(long)]
        config: PathBuf,
        #[command(subcommand)]
        command: KeysCommand,
    },
}

#[derive(Subcommand)]
pub enum KeysCommand {
    #[command(about = "Create a new API key")]
    Create {
        #[arg(long)]
        label: String,
        #[arg(long)]
        owner: String,
        #[arg(long, default_value_t = false)]
        admin: bool,
    },
    #[command(about = "List all API keys")]
    List,
    #[command(about = "Revoke an API key (set inactive)")]
    Revoke { key_id: String },
    #[command(about = "Delete an API key permanently")]
    Delete { key_id: String },
}

pub fn print_usage() {
    println!("Usage: albion_rest_api <command>");
    println!();
    println!("Commands:");
    println!("  serve    Start the API server");
    println!("  keys     Manage API keys");
    println!();
    println!("Run 'albion_rest_api <command> --help' for more information on a command.");
}

pub async fn handle_keys_command(
    command: KeysCommand,
    pool: DbPool,
) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        KeysCommand::Create {
            label,
            owner,
            admin,
        } => create_key(&pool, &label, &owner, admin).await,
        KeysCommand::List => list_keys(&pool).await,
        KeysCommand::Revoke { key_id } => revoke_key(&pool, &key_id).await,
        KeysCommand::Delete { key_id } => delete_key(&pool, &key_id).await,
    }
}

async fn create_key(
    pool: &DbPool,
    label: &str,
    owner: &str,
    admin: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let key_id = uuid::Uuid::new_v4().to_string();
    let mut secret_bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut secret_bytes);
    let secret = URL_SAFE_NO_PAD.encode(secret_bytes);

    let secret_hash =
        auth::hash_secret(&secret).map_err(|e| format!("failed to hash secret: {e}"))?;

    sqlx::query(
        "INSERT INTO api_keys (key_id, secret_hash, label, owner, is_admin) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&key_id)
    .bind(&secret_hash)
    .bind(label)
    .bind(owner)
    .bind(admin)
    .execute(pool)
    .await
    .map_err(|e| format!("failed to insert API key: {e}"))?;

    tracing::info!(key_id = %key_id, label = %label, owner = %owner, admin = %admin, "API key created");

    println!();
    println!("API key created successfully");
    println!();
    println!("Key ID:  {key_id}");
    println!("Secret:  {secret}");
    println!("Label:   {label}");
    println!("Owner:   {owner}");
    println!("Admin:   {admin}");
    println!();
    println!("IMPORTANT: Store the secret securely. It will not be shown again.");
    println!();

    Ok(())
}

async fn list_keys(pool: &DbPool) -> Result<(), Box<dyn std::error::Error>> {
    let rows = sqlx::query_as::<_, auth::ApiKeyRow>(
        "SELECT id, key_id, secret_hash, label, owner, active, is_admin, created_at, updated_at \
         FROM api_keys ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await
    .map_err(|e| format!("failed to query API keys: {e}"))?;

    if rows.is_empty() {
        println!("No API keys found");
        return Ok(());
    }

    println!();
    println!(
        "{:<38} {:<20} {:<30} {:<8} {:<8} {:<20} {:<20}",
        "KEY_ID", "LABEL", "OWNER", "ACTIVE", "ADMIN", "CREATED_AT", "UPDATED_AT"
    );
    println!("{}", "-".repeat(144));

    for row in &rows {
        println!(
            "{:<38} {:<20} {:<30} {:<8} {:<8} {:<20} {:<20}",
            row.key_id,
            row.label,
            row.owner,
            row.active,
            row.is_admin,
            row.created_at,
            row.updated_at
        );
    }
    println!();

    Ok(())
}

async fn revoke_key(pool: &DbPool, key_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let result = sqlx::query("UPDATE api_keys SET active = 0 WHERE key_id = ?")
        .bind(key_id)
        .execute(pool)
        .await
        .map_err(|e| format!("failed to revoke API key: {e}"))?;

    if result.rows_affected() == 0 {
        return Err(format!("API key {key_id} not found").into());
    }

    tracing::info!(key_id = %key_id, "API key revoked");
    println!("API key {key_id} revoked successfully");
    Ok(())
}

async fn delete_key(pool: &DbPool, key_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    let result = sqlx::query("DELETE FROM api_keys WHERE key_id = ?")
        .bind(key_id)
        .execute(pool)
        .await
        .map_err(|e| format!("failed to delete API key: {e}"))?;

    if result.rows_affected() == 0 {
        return Err(format!("API key {key_id} not found").into());
    }

    tracing::info!(key_id = %key_id, "API key deleted");
    println!("API key {key_id} deleted successfully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use argon2::PasswordHash;

    async fn test_pool() -> DbPool {
        let id = uuid::Uuid::new_v4();
        crate::db::init(&format!("sqlite:file:{id}?mode=memory&cache=shared"), 5)
            .await
            .expect("test database init")
    }

    async fn seed_key(pool: &DbPool) -> String {
        let key_id = uuid::Uuid::new_v4().to_string();
        let hash = auth::hash_secret("test-secret").expect("hash");
        sqlx::query("INSERT INTO api_keys (key_id, secret_hash, label, owner) VALUES (?, ?, ?, ?)")
            .bind(&key_id)
            .bind(&hash)
            .bind("test-label")
            .bind("test-owner")
            .execute(pool)
            .await
            .expect("seed key");
        key_id
    }

    #[test]
    fn test_cli_requires_subcommand() {
        let cli = Cli::try_parse_from(["app"]).expect("parse");
        assert!(cli.command.is_none());
    }

    #[test]
    fn test_keys_requires_config_flag() {
        let result = Cli::try_parse_from(["app", "keys", "list"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_keys_parses_config_flag() {
        let cli = Cli::try_parse_from(["app", "keys", "--config", "/path/to/config.toml", "list"])
            .expect("parse");
        match cli.command {
            Some(Command::Keys { config, .. }) => {
                assert_eq!(config, PathBuf::from("/path/to/config.toml"));
            }
            _ => panic!("expected Keys command"),
        }
    }

    #[tokio::test]
    async fn test_create_key_inserts_row() {
        let pool = test_pool().await;

        handle_keys_command(
            KeysCommand::Create {
                label: "partner-x".into(),
                owner: "contact@example.com".into(),
                admin: false,
            },
            pool.clone(),
        )
        .await
        .expect("create key");

        let row = sqlx::query_as::<_, auth::ApiKeyRow>(
            "SELECT id, key_id, secret_hash, label, owner, active, is_admin, created_at, updated_at \
             FROM api_keys",
        )
        .fetch_one(&pool)
        .await
        .expect("fetch row");

        assert_eq!(row.label, "partner-x");
        assert_eq!(row.owner, "contact@example.com");
        assert!(row.active);
        assert!(!row.is_admin);
        assert!(PasswordHash::new(&row.secret_hash).is_ok());
    }

    #[tokio::test]
    async fn test_list_keys_empty() {
        let pool = test_pool().await;
        let result = handle_keys_command(KeysCommand::List, pool).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_list_keys_returns_all() {
        let pool = test_pool().await;
        seed_key(&pool).await;
        seed_key(&pool).await;

        let result = handle_keys_command(KeysCommand::List, pool).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_revoke_key_sets_inactive() {
        let pool = test_pool().await;
        let key_id = seed_key(&pool).await;

        handle_keys_command(
            KeysCommand::Revoke {
                key_id: key_id.clone(),
            },
            pool.clone(),
        )
        .await
        .expect("revoke key");

        let active: bool = sqlx::query_scalar("SELECT active FROM api_keys WHERE key_id = ?")
            .bind(&key_id)
            .fetch_one(&pool)
            .await
            .expect("fetch active");
        assert!(!active);
    }

    #[tokio::test]
    async fn test_revoke_nonexistent_key() {
        let pool = test_pool().await;
        let result = handle_keys_command(
            KeysCommand::Revoke {
                key_id: "nonexistent".into(),
            },
            pool,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_delete_key_removes_row() {
        let pool = test_pool().await;
        let key_id = seed_key(&pool).await;

        handle_keys_command(
            KeysCommand::Delete {
                key_id: key_id.clone(),
            },
            pool.clone(),
        )
        .await
        .expect("delete key");

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_keys WHERE key_id = ?")
            .bind(&key_id)
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_delete_nonexistent_key() {
        let pool = test_pool().await;
        let result = handle_keys_command(
            KeysCommand::Delete {
                key_id: "nonexistent".into(),
            },
            pool,
        )
        .await;
        assert!(result.is_err());
    }
}
