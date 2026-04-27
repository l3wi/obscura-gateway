use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};

use crate::models::{CdpGrantRecord, GatewayEvent, ProfileRecord, SessionRecord, SessionState};

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let conn =
            Connection::open(path).with_context(|| format!("failed to open {}", path.display()))?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        conn.execute_batch(
            r#"
            create table if not exists profiles (
                profile_id text primary key,
                name text not null unique,
                description text not null,
                identity text not null default '{}',
                cookie_urls text not null,
                cookie_count integer not null default 0,
                created_at text not null,
                updated_at text not null,
                last_used_at text
            );

            create table if not exists sessions (
                session_id text primary key,
                tenant_id text,
                profile_id text,
                profile_mode text not null default '\"read_only\"',
                state text not null,
                created_at text not null,
                updated_at text not null,
                idle_deadline text not null,
                absolute_deadline text not null,
                cdp_ws_url text,
                child_pid integer,
                stealth integer not null,
                proxy_policy text not null,
                allowed_domains text not null,
                denied_domains text not null,
                close_reason text
            );

            create table if not exists cdp_grants (
                grant_id text primary key,
                session_id text not null,
                token text not null unique,
                expires_at text not null,
                used_at text
            );

            create table if not exists session_events (
                event_id text primary key,
                session_id text not null,
                kind text not null,
                message text not null,
                created_at text not null
            );
            "#,
        )?;
        if let Err(err) = conn.execute(
            "alter table sessions add column profile_mode text not null default '\"read_only\"'",
            [],
        ) {
            let duplicate = matches!(
                err,
                rusqlite::Error::SqliteFailure(_, Some(ref msg)) if msg.contains("duplicate column name")
            );
            if !duplicate {
                return Err(err.into());
            }
        }
        if let Err(err) = conn.execute(
            "alter table profiles add column identity text not null default '{}'",
            [],
        ) {
            let duplicate = matches!(
                err,
                rusqlite::Error::SqliteFailure(_, Some(ref msg)) if msg.contains("duplicate column name")
            );
            if !duplicate {
                return Err(err.into());
            }
        }
        Ok(())
    }

    pub fn insert_profile(&self, profile: &ProfileRecord) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        conn.execute(
            "insert into profiles (profile_id, name, description, identity, cookie_urls, cookie_count, created_at, updated_at, last_used_at)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                profile.profile_id,
                profile.name,
                profile.description,
                serde_json::to_string(&profile.identity)?,
                serde_json::to_string(&profile.cookie_urls)?,
                profile.cookie_count as i64,
                profile.created_at.to_rfc3339(),
                profile.updated_at.to_rfc3339(),
                profile.last_used_at.map(|v| v.to_rfc3339()),
            ],
        )?;
        Ok(())
    }

    pub fn update_profile_metadata(
        &self,
        profile_id: &str,
        description: &str,
        identity: &crate::models::ProfileIdentity,
        cookie_urls: &[String],
        cookie_count: usize,
        last_used_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        conn.execute(
            "update profiles
             set description = ?2, identity = ?3, cookie_urls = ?4, cookie_count = ?5, updated_at = ?6, last_used_at = coalesce(?7, last_used_at)
             where profile_id = ?1",
            params![
                profile_id,
                description,
                serde_json::to_string(identity)?,
                serde_json::to_string(cookie_urls)?,
                cookie_count as i64,
                Utc::now().to_rfc3339(),
                last_used_at.map(|v| v.to_rfc3339()),
            ],
        )?;
        Ok(())
    }

    pub fn touch_profile_last_used(&self, profile_id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        conn.execute(
            "update profiles set last_used_at = ?2, updated_at = ?2 where profile_id = ?1",
            params![profile_id, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn list_profiles(&self) -> Result<Vec<ProfileRecord>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        let mut stmt = conn.prepare(
            "select profile_id, name, description, identity, cookie_urls, cookie_count, created_at, updated_at, last_used_at
             from profiles order by updated_at desc",
        )?;
        let rows = stmt.query_map([], map_profile)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get_profile(&self, profile_id: &str) -> Result<ProfileRecord> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        conn.query_row(
            "select profile_id, name, description, identity, cookie_urls, cookie_count, created_at, updated_at, last_used_at
             from profiles where profile_id = ?1",
            [profile_id],
            map_profile,
        )
        .optional()?
        .ok_or_else(|| anyhow!("profile not found"))
    }

    pub fn profiles_count(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        let count: i64 = conn.query_row("select count(*) from profiles", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    pub fn delete_profile(&self, profile_id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        conn.execute("delete from profiles where profile_id = ?1", [profile_id])?;
        Ok(())
    }

    pub fn insert_session(&self, session: &SessionRecord) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        conn.execute(
            "insert into sessions (session_id, tenant_id, profile_id, profile_mode, state, created_at, updated_at, idle_deadline, absolute_deadline, cdp_ws_url, child_pid, stealth, proxy_policy, allowed_domains, denied_domains, close_reason)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                session.session_id,
                session.tenant_id,
                session.profile_id,
                serde_json::to_string(&session.profile_mode)?,
                serde_json::to_string(&session.state)?,
                session.created_at.to_rfc3339(),
                session.updated_at.to_rfc3339(),
                session.idle_deadline.to_rfc3339(),
                session.absolute_deadline.to_rfc3339(),
                session.cdp_ws_url,
                session.child_pid.map(|v| v as i64),
                session.stealth as i64,
                session.proxy_policy,
                serde_json::to_string(&session.allowed_domains)?,
                serde_json::to_string(&session.denied_domains)?,
                session.close_reason,
            ],
        )?;
        Ok(())
    }

    pub fn update_session_state(
        &self,
        session_id: &str,
        state: SessionState,
        child_pid: Option<u32>,
        cdp_ws_url: Option<&str>,
        close_reason: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        conn.execute(
            "update sessions
             set state = ?2, child_pid = ?3, cdp_ws_url = coalesce(?4, cdp_ws_url), close_reason = coalesce(?5, close_reason), updated_at = ?6
             where session_id = ?1",
            params![
                session_id,
                serde_json::to_string(&state)?,
                child_pid.map(|v| v as i64),
                cdp_ws_url,
                close_reason,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionRecord>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        let mut stmt = conn.prepare(
            "select session_id, tenant_id, profile_id, profile_mode, state, created_at, updated_at, idle_deadline, absolute_deadline, cdp_ws_url, child_pid, stealth, proxy_policy, allowed_domains, denied_domains, close_reason
             from sessions order by created_at desc",
        )?;
        let rows = stmt.query_map([], map_session)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn total_sessions_count(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        let count: i64 = conn.query_row("select count(*) from sessions", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    pub fn active_sessions_count(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        let count: i64 = conn.query_row(
            "select count(*) from sessions where state in ('\"provisioning\"','\"ready\"','\"attached\"','\"idle\"')",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn mark_active_sessions_failed(&self, reason: &str) -> Result<usize> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        let updated = conn.execute(
            "update sessions
             set state = '\"failed\"',
                 child_pid = null,
                 cdp_ws_url = null,
                 close_reason = ?1,
                 updated_at = ?2
             where state in ('\"provisioning\"','\"ready\"','\"attached\"','\"idle\"','\"closing\"')",
            params![reason, Utc::now().to_rfc3339()],
        )?;
        Ok(updated)
    }

    pub fn get_session(&self, session_id: &str) -> Result<SessionRecord> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        conn.query_row(
            "select session_id, tenant_id, profile_id, profile_mode, state, created_at, updated_at, idle_deadline, absolute_deadline, cdp_ws_url, child_pid, stealth, proxy_policy, allowed_domains, denied_domains, close_reason
             from sessions where session_id = ?1",
            [session_id],
            map_session,
        )
        .optional()?
        .ok_or_else(|| anyhow!("session not found"))
    }

    pub fn insert_grant(&self, grant: &CdpGrantRecord) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        conn.execute(
            "insert into cdp_grants (grant_id, session_id, token, expires_at, used_at) values (?1, ?2, ?3, ?4, ?5)",
            params![
                grant.grant_id,
                grant.session_id,
                grant.token,
                grant.expires_at.to_rfc3339(),
                grant.used_at.map(|v| v.to_rfc3339()),
            ],
        )?;
        Ok(())
    }

    pub fn active_sessions_for_profile(&self, profile_id: &str) -> Result<Vec<SessionRecord>> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        let mut stmt = conn.prepare(
            "select session_id, tenant_id, profile_id, profile_mode, state, created_at, updated_at, idle_deadline, absolute_deadline, cdp_ws_url, child_pid, stealth, proxy_policy, allowed_domains, denied_domains, close_reason
             from sessions
             where profile_id = ?1 and state in ('\"provisioning\"','\"ready\"','\"attached\"','\"idle\"','\"closing\"')
             order by created_at asc",
        )?;
        let rows = stmt.query_map([profile_id], map_session)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn use_grant(&self, token: &str, expected_session_id: &str) -> Result<CdpGrantRecord> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        let grant = conn
            .query_row(
                "select grant_id, session_id, token, expires_at, used_at from cdp_grants where token = ?1",
                [token],
                map_grant,
            )
            .optional()?
            .ok_or_else(|| anyhow!("grant not found"))?;
        if grant.session_id != expected_session_id {
            bail!("grant does not belong to requested session");
        }
        if grant.used_at.is_some() {
            bail!("grant already used");
        }
        if grant.expires_at < Utc::now() {
            bail!("grant expired");
        }
        conn.execute(
            "update cdp_grants set used_at = ?2 where grant_id = ?1",
            params![grant.grant_id, Utc::now().to_rfc3339()],
        )?;
        Ok(grant)
    }

    pub fn insert_event(&self, event: &GatewayEvent) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| anyhow!("db mutex poisoned"))?;
        conn.execute(
            "insert into session_events (event_id, session_id, kind, message, created_at) values (?1, ?2, ?3, ?4, ?5)",
            params![
                event.event_id,
                event.session_id,
                event.kind,
                event.message,
                event.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }
}

fn map_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProfileRecord> {
    Ok(ProfileRecord {
        profile_id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        identity: serde_json::from_str(&row.get::<_, String>(3)?).unwrap_or_default(),
        cookie_urls: serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
        cookie_count: row.get::<_, i64>(5)? as usize,
        created_at: parse_dt(&row.get::<_, String>(6)?)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?,
        updated_at: parse_dt(&row.get::<_, String>(7)?)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?,
        last_used_at: row
            .get::<_, Option<String>>(8)?
            .map(|v| parse_dt(&v))
            .transpose()
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?,
    })
}

fn map_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
    let profile_mode_raw: String = row.get(3)?;
    let state_raw: String = row.get(4)?;
    Ok(SessionRecord {
        session_id: row.get(0)?,
        tenant_id: row.get(1)?,
        profile_id: row.get(2)?,
        profile_mode: serde_json::from_str(&profile_mode_raw)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?,
        state: serde_json::from_str(&state_raw)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?,
        created_at: parse_dt(&row.get::<_, String>(5)?)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?,
        updated_at: parse_dt(&row.get::<_, String>(6)?)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?,
        idle_deadline: parse_dt(&row.get::<_, String>(7)?)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?,
        absolute_deadline: parse_dt(&row.get::<_, String>(8)?)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?,
        cdp_ws_url: row.get(9)?,
        child_pid: row.get::<_, Option<i64>>(10)?.map(|v| v as u32),
        stealth: row.get::<_, i64>(11)? != 0,
        proxy_policy: row.get(12)?,
        allowed_domains: serde_json::from_str(&row.get::<_, String>(13)?).unwrap_or_default(),
        denied_domains: serde_json::from_str(&row.get::<_, String>(14)?).unwrap_or_default(),
        close_reason: row.get(15)?,
    })
}

fn map_grant(row: &rusqlite::Row<'_>) -> rusqlite::Result<CdpGrantRecord> {
    Ok(CdpGrantRecord {
        grant_id: row.get(0)?,
        session_id: row.get(1)?,
        token: row.get(2)?,
        expires_at: parse_dt(&row.get::<_, String>(3)?)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?,
        used_at: row
            .get::<_, Option<String>>(4)?
            .map(|v| parse_dt(&v))
            .transpose()
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?,
    })
}

fn parse_dt(input: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(input)?.with_timezone(&Utc))
}
