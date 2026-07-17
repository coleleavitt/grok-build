use anyhow::{Context, bail};
use chrono::Utc;
use clap::{Args, Subcommand};
use std::io::Write;
use xai_grok_anthropic_auth::endpoints::AUTHORIZE_SCOPES;
use xai_grok_anthropic_auth::{
    Account, AccountData, AccountStore, AuthorizeRequest, OAuthClient, OAuthEndpoints, OAuthTokens,
    PkcePair, parse_redirect_code,
};

#[derive(Debug, Clone, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: AuthCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum AuthCommand {
    /// Manage Anthropic OAuth accounts
    Anthropic(AnthropicAuthArgs),
}

#[derive(Debug, Clone, Args)]
pub struct AnthropicAuthArgs {
    #[command(subcommand)]
    pub command: AnthropicAuthCommand,
}

#[derive(Debug, Clone, Subcommand)]
pub enum AnthropicAuthCommand {
    /// Sign in to an Anthropic account with browser OAuth
    Login(AnthropicLoginArgs),
    /// Show stored Anthropic accounts without secrets
    Status,
    /// Select the active Anthropic account
    Use {
        /// Account name to make active.
        name: String,
    },
    /// Remove an Anthropic account, defaulting to the active one
    Logout {
        /// Account name to remove.
        name: Option<String>,
    },
}

#[derive(Debug, Clone, Args)]
pub struct AnthropicLoginArgs {
    /// Stored account name. Defaults to the returned email/UUID when available.
    #[arg(long)]
    pub name: Option<String>,
    /// Manual callback value in the `code#state` form.
    #[arg(long, value_name = "CODE#STATE")]
    pub redirect: Option<String>,
}

pub async fn run_cli_auth(args: AuthArgs) -> anyhow::Result<()> {
    match args.command {
        AuthCommand::Anthropic(args) => run_cli_anthropic_auth(args).await,
    }
}

pub async fn run_cli_anthropic_auth(args: AnthropicAuthArgs) -> anyhow::Result<()> {
    match args.command {
        AnthropicAuthCommand::Login(login) => login_anthropic(login).await,
        AnthropicAuthCommand::Status => status_anthropic(),
        AnthropicAuthCommand::Use { name } => use_anthropic(&name),
        AnthropicAuthCommand::Logout { name } => logout_anthropic(name.as_deref()),
    }
}

async fn login_anthropic(args: AnthropicLoginArgs) -> anyhow::Result<()> {
    let endpoints = OAuthEndpoints::from_env();
    let pkce = PkcePair::generate();
    let state = xai_grok_anthropic_auth::pkce::generate_state();
    let authorize_url = AuthorizeRequest {
        endpoints: &endpoints,
        pkce: &pkce,
        state: &state,
        scopes: &AUTHORIZE_SCOPES,
    }
    .to_url()
    .context("failed to build Anthropic authorization URL")?;

    eprintln!("Open this Anthropic OAuth URL in your browser:\n");
    eprintln!("{authorize_url}\n");
    eprintln!("After approving access, paste the callback value shown by Anthropic.");
    eprintln!("It should look like: code#state");

    let pasted = match args.redirect {
        Some(redirect) => redirect,
        None => {
            eprint!("Anthropic callback code#state: ");
            std::io::stderr().flush().ok();
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .context("failed to read Anthropic OAuth callback")?;
            line
        }
    };
    let code = parse_redirect_code(&pasted, &state)
        .context("invalid Anthropic OAuth callback; expected code#state from this login attempt")?;
    let tokens = OAuthClient::new(endpoints)
        .exchange_code(&code, &pkce.verifier, &state)
        .await
        .context("Anthropic OAuth token exchange failed")?;
    let store = AccountStore::from_env();
    let account = upsert_oauth_account(&store, &tokens, args.name.as_deref())?;
    eprintln!(
        "Signed in to Anthropic as `{}`. Stored account data at {}.",
        account.name,
        store.path().display()
    );
    Ok(())
}

fn status_anthropic() -> anyhow::Result<()> {
    let store = AccountStore::from_env();
    let data = store
        .load()
        .context("failed to load Anthropic account store")?;
    println!("Anthropic account store: {}", store.path().display());
    if data.accounts.is_empty() {
        println!("No Anthropic accounts are stored.");
        return Ok(());
    }
    for (idx, account) in data.accounts.iter().enumerate() {
        let active = data.active_index.unwrap_or(0) == idx;
        println!(
            "{}{}",
            if active { "* " } else { "  " },
            account_status(account)
        );
    }
    Ok(())
}

fn use_anthropic(name: &str) -> anyhow::Result<()> {
    validate_account_name(name)?;
    let store = AccountStore::from_env();
    let mut selected = false;
    store
        .read_modify_write(|data| {
            selected = select_account(data, name, Utc::now().timestamp_millis());
        })
        .context("failed to update Anthropic account store")?;
    if !selected {
        bail!("Anthropic account `{name}` was not found");
    }
    eprintln!("Selected Anthropic account `{name}`.");
    Ok(())
}

fn logout_anthropic(name: Option<&str>) -> anyhow::Result<()> {
    let store = AccountStore::from_env();
    let mut removed = None;
    store
        .read_modify_write(|data| {
            removed = remove_account(data, name);
        })
        .context("failed to update Anthropic account store")?;
    let Some(removed) = removed else {
        match name {
            Some(name) => bail!("Anthropic account `{name}` was not found"),
            None => bail!("No Anthropic accounts are stored"),
        }
    };
    eprintln!("Removed Anthropic account `{removed}`.");
    Ok(())
}

fn select_account(data: &mut AccountData, name: &str, now_ms: i64) -> bool {
    if let Some(idx) = data.accounts.iter().position(|a| a.name == name) {
        data.active_index = Some(idx);
        data.accounts[idx].last_used = now_ms;
        true
    } else {
        false
    }
}

fn remove_account(data: &mut AccountData, name: Option<&str>) -> Option<String> {
    let idx = match name {
        Some(name) => data.accounts.iter().position(|a| a.name == name),
        None => {
            let active_idx = data.active_index.unwrap_or(0);
            data.accounts
                .get(active_idx)
                .map(|_| active_idx)
                .or_else(|| (!data.accounts.is_empty()).then_some(0))
        }
    }?;
    let account = data.accounts.remove(idx);
    if data.accounts.is_empty() {
        data.active_index = None;
    } else if let Some(active) = data.active_index {
        let next_active = if active > idx { active - 1 } else { active };
        data.active_index = Some(next_active.min(data.accounts.len() - 1));
    }
    Some(account.name)
}

fn upsert_oauth_account(
    store: &AccountStore,
    tokens: &OAuthTokens,
    requested_name: Option<&str>,
) -> anyhow::Result<Account> {
    if let Some(name) = requested_name {
        validate_account_name(name)?;
    }
    let now_ms = Utc::now().timestamp_millis();
    let mut saved = None;
    store
        .read_modify_write(|data| {
            let idx = find_matching_account(data, tokens, requested_name);
            match idx {
                Some(idx) => {
                    let account = &mut data.accounts[idx];
                    if let Some(name) = requested_name {
                        account.name = name.to_owned();
                    }
                    apply_tokens(account, tokens, now_ms);
                    data.active_index = Some(idx);
                    saved = Some(account.clone());
                }
                None => {
                    let name =
                        unique_account_name(data, preferred_account_name(tokens, requested_name));
                    let mut account = Account::new(name, tokens.refresh.clone());
                    apply_tokens(&mut account, tokens, now_ms);
                    data.accounts.push(account.clone());
                    data.active_index = Some(data.accounts.len() - 1);
                    saved = Some(account);
                }
            }
        })
        .context("failed to persist Anthropic account")?;
    saved.context("Anthropic account was not persisted")
}

fn find_matching_account(
    data: &AccountData,
    tokens: &OAuthTokens,
    requested_name: Option<&str>,
) -> Option<usize> {
    if let Some(name) = requested_name {
        if let Some(idx) = data.accounts.iter().position(|a| a.name == name) {
            return Some(idx);
        }
    }
    if let Some(uuid) = tokens.account.as_ref().map(|a| a.uuid.as_str()) {
        if let Some(idx) = data
            .accounts
            .iter()
            .position(|a| a.uuid.as_deref() == Some(uuid))
        {
            return Some(idx);
        }
    }
    if let Some(email) = tokens
        .account
        .as_ref()
        .and_then(|a| a.email_address.as_deref())
    {
        return data
            .accounts
            .iter()
            .position(|a| a.email.as_deref() == Some(email) || a.name == email);
    }
    None
}

fn preferred_account_name(tokens: &OAuthTokens, requested_name: Option<&str>) -> String {
    requested_name
        .map(str::to_owned)
        .or_else(|| {
            tokens
                .account
                .as_ref()
                .and_then(|a| a.email_address.clone())
        })
        .or_else(|| tokens.account.as_ref().map(|a| a.uuid.clone()))
        .unwrap_or_else(|| "anthropic".to_owned())
}

fn unique_account_name(data: &AccountData, base: String) -> String {
    if data.find(&base).is_none() {
        return base;
    }
    for n in 2.. {
        let candidate = format!("{base}-{n}");
        if data.find(&candidate).is_none() {
            return candidate;
        }
    }
    unreachable!()
}

fn apply_tokens(account: &mut Account, tokens: &OAuthTokens, now_ms: i64) {
    account.refresh_token = tokens.refresh.clone();
    account.access_token = Some(tokens.access.clone());
    account.expires_at = Some(tokens.expires_at);
    account.scopes = Some(tokens.scopes.clone());
    if let Some(token_account) = &tokens.account {
        account.uuid = Some(token_account.uuid.clone());
        if token_account.email_address.is_some() {
            account.email = token_account.email_address.clone();
        }
    }
    if let Some(organization) = &tokens.organization {
        account.organization_uuid = Some(organization.uuid.clone());
    }
    if account.added_at == 0 {
        account.added_at = now_ms;
    }
    account.last_used = now_ms;
    account.enabled = Some(true);
    account.disabled_reason = None;
    account.refresh_failure_count = None;
    account.last_auth_error = None;
}

fn account_status(account: &Account) -> String {
    let mut parts = vec![account.name.clone()];
    if let Some(email) = &account.email {
        if email != &account.name {
            parts.push(format!("email={email}"));
        }
    }
    if let Some(plan) = account.plan {
        parts.push(format!("plan={plan:?}"));
    }
    parts.push(format!("enabled={}", account.is_enabled()));
    parts.push(if account.access_token.is_some() {
        "oauth=ready".to_owned()
    } else {
        "oauth=needs-refresh".to_owned()
    });
    if let Some(error) = &account.last_auth_error {
        parts.push(format!("last_error={error}"));
    }
    parts.join(" ")
}

fn validate_account_name(name: &str) -> anyhow::Result<()> {
    if name.trim().is_empty() {
        bail!("Anthropic account name cannot be empty");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use xai_grok_anthropic_auth::token::{TokenAccount, TokenOrganization};
    use xai_grok_anthropic_auth::{AccessToken, RefreshToken};

    fn tokens(email: Option<&str>, uuid: &str) -> OAuthTokens {
        OAuthTokens {
            access: AccessToken::new("sk-ant-oat01-abcdefghijklmnopqrstuvwxyz012345"),
            refresh: RefreshToken::new("sk-ant-ort01-abcdefghijklmnopqrstuvwxyz012345"),
            expires_at: Utc::now() + Duration::hours(1),
            scopes: vec!["user:inference".to_owned()],
            account: Some(TokenAccount {
                uuid: uuid.to_owned(),
                email_address: email.map(str::to_owned),
            }),
            organization: Some(TokenOrganization {
                uuid: "org_123".to_owned(),
            }),
        }
    }

    #[test]
    fn login_upserts_by_email_and_marks_active() {
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::at(dir.path().join("anthropic-accounts.json"));
        let first =
            upsert_oauth_account(&store, &tokens(Some("a@example.com"), "acct_1"), None).unwrap();
        assert_eq!(first.name, "a@example.com");
        let second =
            upsert_oauth_account(&store, &tokens(Some("a@example.com"), "acct_1"), None).unwrap();
        assert_eq!(second.name, "a@example.com");
        let data = store.load().unwrap();
        assert_eq!(data.accounts.len(), 1);
        assert_eq!(data.active().unwrap().name, "a@example.com");
    }

    #[test]
    fn login_uses_unique_default_name_when_no_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::at(dir.path().join("anthropic-accounts.json"));
        let mut first = tokens(None, "acct_1");
        first.account = None;
        let mut second = tokens(None, "acct_2");
        second.account = None;
        assert_eq!(
            upsert_oauth_account(&store, &first, None).unwrap().name,
            "anthropic"
        );
        assert_eq!(
            upsert_oauth_account(&store, &second, None).unwrap().name,
            "anthropic-2"
        );
    }

    #[test]
    fn logout_refuses_to_wipe_last_account() {
        let dir = tempfile::tempdir().unwrap();
        let store = AccountStore::at(dir.path().join("anthropic-accounts.json"));
        upsert_oauth_account(&store, &tokens(Some("a@example.com"), "acct_1"), None).unwrap();
        let result = store.read_modify_write(|data| {
            data.accounts.clear();
            data.active_index = None;
        });
        assert!(result.is_err());
    }

    #[test]
    fn use_selects_account_and_updates_last_used() {
        let now_ms = 12345;
        let mut data = AccountData::default();
        data.accounts
            .push(Account::new("a", RefreshToken::new("sk-ant-ort01-a")));
        data.accounts
            .push(Account::new("b", RefreshToken::new("sk-ant-ort01-b")));
        assert!(select_account(&mut data, "b", now_ms));
        assert_eq!(data.active_index, Some(1));
        assert_eq!(data.accounts[1].last_used, now_ms);
        assert!(!select_account(&mut data, "missing", now_ms));
    }

    #[test]
    fn logout_removes_named_account_and_rebases_active_index() {
        let mut data = AccountData::default();
        data.accounts
            .push(Account::new("a", RefreshToken::new("sk-ant-ort01-a")));
        data.accounts
            .push(Account::new("b", RefreshToken::new("sk-ant-ort01-b")));
        data.accounts
            .push(Account::new("c", RefreshToken::new("sk-ant-ort01-c")));
        data.active_index = Some(2);
        assert_eq!(remove_account(&mut data, Some("b")).as_deref(), Some("b"));
        assert_eq!(data.active_index, Some(1));
        assert_eq!(data.active().unwrap().name, "c");
    }

    #[test]
    fn logout_defaults_to_active_account_with_first_account_fallback() {
        let mut data = AccountData::default();
        data.accounts
            .push(Account::new("a", RefreshToken::new("sk-ant-ort01-a")));
        data.accounts
            .push(Account::new("b", RefreshToken::new("sk-ant-ort01-b")));
        data.active_index = Some(99);
        assert_eq!(remove_account(&mut data, None).as_deref(), Some("a"));
        assert_eq!(data.active_index, Some(0));
        assert_eq!(data.active().unwrap().name, "b");
    }
}
