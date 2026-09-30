use rand::{rngs::OsRng, RngCore};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use subtle::ConstantTimeEq;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct Principal {
    pub key_id: Uuid,
    pub token_public_id: String,
    pub tenant_id: Uuid,
    pub scopes: Vec<String>,
    pub allowed_flow_ids: Option<Vec<Uuid>>,
    pub allowed_model_aliases: Option<Vec<String>>,
}

impl Principal {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }

    pub fn allows_flow(&self, flow_id: Uuid) -> bool {
        self.allowed_flow_ids
            .as_ref()
            .is_none_or(|ids| ids.contains(&flow_id))
    }

    pub fn allows_model(&self, alias: &str) -> bool {
        self.allowed_model_aliases
            .as_ref()
            .is_none_or(|aliases| aliases.iter().any(|a| a == alias))
    }
}

pub fn generate_key() -> (String, String, Vec<u8>) {
    let public_id = Uuid::new_v4().to_string();
    let mut secret = [0_u8; 32];
    OsRng.fill_bytes(&mut secret);
    let token = format!("cf_{public_id}.{}", hex::encode(secret));
    let digest = Sha256::digest(secret).to_vec();
    (token, public_id, digest)
}

fn parse_bearer(bearer: &str) -> Option<(&str, Vec<u8>)> {
    let token = bearer.strip_prefix("Bearer cf_")?;
    let (public_id, secret_hex) = token.split_once('.')?;
    if Uuid::parse_str(public_id).is_err() || secret_hex.len() != 64 {
        return None;
    }
    Some((public_id, hex::decode(secret_hex).ok()?))
}

pub async fn authenticate(pool: &PgPool, bearer: &str) -> Result<Option<Principal>, sqlx::Error> {
    let Some((public_id, secret)) = parse_bearer(bearer) else {
        return Ok(None);
    };
    let record = sqlx::query("SELECT id, tenant_id, secret_digest, scopes, allowed_flow_ids, allowed_model_aliases FROM api_keys WHERE public_id = $1 AND revoked_at IS NULL AND primary_token_revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now()) UNION ALL SELECT k.id, k.tenant_id, t.secret_digest, k.scopes, k.allowed_flow_ids, k.allowed_model_aliases FROM api_key_tokens t JOIN api_keys k ON k.id = t.key_id WHERE t.public_id = $1 AND t.revoked_at IS NULL AND k.revoked_at IS NULL AND (k.expires_at IS NULL OR k.expires_at > now()) LIMIT 1")
        .bind(public_id)
        .fetch_optional(pool)
        .await?;
    let Some(record) = record else {
        return Ok(None);
    };
    let stored: Vec<u8> = record.try_get("secret_digest")?;
    let actual = Sha256::digest(secret);
    if stored.len() != actual.len() || !bool::from(stored.ct_eq(actual.as_slice())) {
        return Ok(None);
    }
    Ok(Some(Principal {
        key_id: record.try_get("id")?,
        token_public_id: public_id.to_owned(),
        tenant_id: record.try_get("tenant_id")?,
        scopes: record.try_get("scopes")?,
        allowed_flow_ids: record.try_get("allowed_flow_ids")?,
        allowed_model_aliases: record.try_get("allowed_model_aliases")?,
    }))
}

#[cfg(test)]
mod tests {
    use super::{generate_key, parse_bearer};

    #[test]
    fn generated_and_rotated_tokens_use_a_bounded_bearer_format() {
        let (first, _, _) = generate_key();
        let (second, _, _) = generate_key();
        assert_ne!(first, second);
        assert_eq!(
            parse_bearer(&format!("Bearer {first}")).unwrap().1.len(),
            32
        );
        assert_eq!(
            parse_bearer(&format!("Bearer {second}")).unwrap().1.len(),
            32
        );
        for invalid in [
            "",
            "cf_not-a-bearer",
            "Bearer cf_bad.1234",
            "Bearer cf_00000000-0000-0000-0000-000000000000.deadbeef",
        ] {
            assert!(parse_bearer(invalid).is_none());
        }
        let oversized = format!(
            "Bearer cf_00000000-0000-0000-0000-000000000000.{}",
            "a".repeat(1_000_000)
        );
        assert!(parse_bearer(&oversized).is_none());
    }
}
