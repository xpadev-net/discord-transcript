use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hkdf::Hkdf;
use sha2::Sha256;
use std::fmt::{Display, Formatter};
use tokio_postgres::Client as PgClient;

use crate::infrastructure::sql::GET_GUILD_BOT_TOKEN_SQL;

pub const BOT_TOKEN_KEY_VERSION: &str = "v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedBotToken {
    pub ciphertext: String,
    pub nonce: String,
    pub key_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuildBotTokenMetadata {
    pub updated_at: Option<String>,
    pub last_validated_at: Option<String>,
    pub bot_user_id: Option<String>,
    pub bot_username: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredGuildBotToken {
    pub encrypted: EncryptedBotToken,
    pub metadata: GuildBotTokenMetadata,
}

#[derive(Clone)]
pub struct BotTokenCipher {
    cipher: Aes256Gcm,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotTokenCryptoError {
    MissingKey,
    UnsupportedKeyVersion(String),
    InvalidEncoding,
    EncryptFailed,
    DecryptFailed,
    KeyDerivationFailed,
    InvalidPlaintext,
}

impl Display for BotTokenCryptoError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingKey => f.write_str("missing guild bot token encryption key"),
            Self::UnsupportedKeyVersion(version) => {
                write!(f, "unsupported guild bot token key version: {version}")
            }
            Self::InvalidEncoding => f.write_str("invalid encrypted guild bot token encoding"),
            Self::EncryptFailed => f.write_str("failed to encrypt guild bot token"),
            Self::DecryptFailed => f.write_str("failed to decrypt guild bot token"),
            Self::KeyDerivationFailed => f.write_str("failed to derive guild bot token key"),
            Self::InvalidPlaintext => f.write_str("invalid guild bot token plaintext"),
        }
    }
}

impl std::error::Error for BotTokenCryptoError {}

#[derive(Debug)]
pub enum BotTokenResolveError {
    Database(String),
    MissingCipher,
    Crypto(BotTokenCryptoError),
}

impl Display for BotTokenResolveError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(err) => write!(f, "failed to load guild bot token: {err}"),
            Self::MissingCipher => {
                f.write_str("guild bot token is configured but encryption key is missing")
            }
            Self::Crypto(err) => write!(f, "failed to resolve guild bot token: {err}"),
        }
    }
}

impl std::error::Error for BotTokenResolveError {}

impl BotTokenCipher {
    pub fn new(key_material: &str) -> Result<Self, BotTokenCryptoError> {
        let key_material = key_material.trim();
        if key_material.is_empty() {
            return Err(BotTokenCryptoError::MissingKey);
        }
        let hk = Hkdf::<Sha256>::new(
            Some(b"discord-transcript:guild-bot-token-key:v1"),
            key_material.as_bytes(),
        );
        let mut key = [0u8; 32];
        hk.expand(b"guild-bot-token-aes-256-gcm", &mut key)
            .map_err(|_| BotTokenCryptoError::KeyDerivationFailed)?;
        let cipher =
            Aes256Gcm::new_from_slice(&key).map_err(|_| BotTokenCryptoError::MissingKey)?;
        Ok(Self { cipher })
    }

    pub fn encrypt_for_guild(
        &self,
        guild_id: &str,
        token: &str,
    ) -> Result<EncryptedBotToken, BotTokenCryptoError> {
        if token.trim().is_empty() {
            return Err(BotTokenCryptoError::InvalidPlaintext);
        }
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let nonce_bytes = nonce.as_slice();
        let ciphertext = self
            .cipher
            .encrypt(
                Nonce::from_slice(nonce_bytes),
                Payload {
                    msg: token.as_bytes(),
                    aad: guild_id.as_bytes(),
                },
            )
            .map_err(|_| BotTokenCryptoError::EncryptFailed)?;

        Ok(EncryptedBotToken {
            ciphertext: STANDARD.encode(ciphertext),
            nonce: STANDARD.encode(nonce_bytes),
            key_version: BOT_TOKEN_KEY_VERSION.to_owned(),
        })
    }

    pub fn decrypt_for_guild(
        &self,
        guild_id: &str,
        encrypted: &EncryptedBotToken,
    ) -> Result<String, BotTokenCryptoError> {
        if encrypted.key_version != BOT_TOKEN_KEY_VERSION {
            return Err(BotTokenCryptoError::UnsupportedKeyVersion(
                encrypted.key_version.clone(),
            ));
        }
        let nonce = STANDARD
            .decode(&encrypted.nonce)
            .map_err(|_| BotTokenCryptoError::InvalidEncoding)?;
        if nonce.len() != 12 {
            return Err(BotTokenCryptoError::InvalidEncoding);
        }
        let ciphertext = STANDARD
            .decode(&encrypted.ciphertext)
            .map_err(|_| BotTokenCryptoError::InvalidEncoding)?;
        let plaintext = self
            .cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: ciphertext.as_ref(),
                    aad: guild_id.as_bytes(),
                },
            )
            .map_err(|_| BotTokenCryptoError::DecryptFailed)?;
        String::from_utf8(plaintext).map_err(|_| BotTokenCryptoError::InvalidPlaintext)
    }
}

pub async fn load_guild_bot_token(
    db: &PgClient,
    guild_id: &str,
) -> Result<Option<StoredGuildBotToken>, tokio_postgres::Error> {
    let row = db.query_opt(GET_GUILD_BOT_TOKEN_SQL, &[&guild_id]).await?;
    Ok(row.map(|row| StoredGuildBotToken {
        encrypted: EncryptedBotToken {
            ciphertext: row.get("bot_token_ciphertext"),
            nonce: row.get("bot_token_nonce"),
            key_version: row.get("bot_token_key_version"),
        },
        metadata: GuildBotTokenMetadata {
            updated_at: row.get("bot_token_updated_at"),
            last_validated_at: row.get("bot_token_last_validated_at"),
            bot_user_id: row.get("bot_user_id"),
            bot_username: row.get("bot_username"),
        },
    }))
}

pub fn resolve_bot_token_from_record(
    guild_id: &str,
    global_bot_token: &str,
    stored: Option<&StoredGuildBotToken>,
    cipher: Option<&BotTokenCipher>,
) -> Result<String, BotTokenResolveError> {
    let Some(stored) = stored else {
        return Ok(global_bot_token.to_owned());
    };
    let cipher = cipher.ok_or(BotTokenResolveError::MissingCipher)?;
    cipher
        .decrypt_for_guild(guild_id, &stored.encrypted)
        .map_err(BotTokenResolveError::Crypto)
}

pub async fn resolve_effective_bot_token(
    db: &PgClient,
    guild_id: &str,
    global_bot_token: &str,
    cipher: Option<&BotTokenCipher>,
) -> Result<String, BotTokenResolveError> {
    let stored = load_guild_bot_token(db, guild_id)
        .await
        .map_err(|err| BotTokenResolveError::Database(err.to_string()))?;
    resolve_bot_token_from_record(guild_id, global_bot_token, stored.as_ref(), cipher)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(encrypted: EncryptedBotToken) -> StoredGuildBotToken {
        StoredGuildBotToken {
            encrypted,
            metadata: GuildBotTokenMetadata {
                updated_at: None,
                last_validated_at: None,
                bot_user_id: None,
                bot_username: None,
            },
        }
    }

    #[test]
    fn bot_token_cipher_round_trips_for_same_guild() {
        let cipher = BotTokenCipher::new("secret key material").expect("cipher");
        let encrypted = cipher
            .encrypt_for_guild("guild-1", "guild-token")
            .expect("encrypt");

        let decrypted = cipher
            .decrypt_for_guild("guild-1", &encrypted)
            .expect("decrypt");

        assert_eq!(decrypted, "guild-token");
        assert_ne!(encrypted.ciphertext, "guild-token");
    }

    #[test]
    fn bot_token_cipher_rejects_wrong_guild_aad() {
        let cipher = BotTokenCipher::new("secret key material").expect("cipher");
        let encrypted = cipher
            .encrypt_for_guild("guild-1", "guild-token")
            .expect("encrypt");

        let err = cipher
            .decrypt_for_guild("guild-2", &encrypted)
            .expect_err("wrong guild should fail");

        assert_eq!(err, BotTokenCryptoError::DecryptFailed);
    }

    #[test]
    fn bot_token_cipher_rejects_tampered_ciphertext() {
        let cipher = BotTokenCipher::new("secret key material").expect("cipher");
        let mut encrypted = cipher
            .encrypt_for_guild("guild-1", "guild-token")
            .expect("encrypt");
        encrypted.ciphertext.push('A');

        let err = cipher
            .decrypt_for_guild("guild-1", &encrypted)
            .expect_err("tampered ciphertext should fail");

        assert!(matches!(
            err,
            BotTokenCryptoError::InvalidEncoding | BotTokenCryptoError::DecryptFailed
        ));
    }

    #[test]
    fn resolver_falls_back_to_global_when_guild_token_absent() {
        let token =
            resolve_bot_token_from_record("guild-1", "global-token", None, None).expect("fallback");

        assert_eq!(token, "global-token");
    }

    #[test]
    fn resolver_requires_cipher_when_guild_token_exists() {
        let cipher = BotTokenCipher::new("secret key material").expect("cipher");
        let encrypted = cipher
            .encrypt_for_guild("guild-1", "guild-token")
            .expect("encrypt");

        let err = resolve_bot_token_from_record(
            "guild-1",
            "global-token",
            Some(&stored(encrypted)),
            None,
        )
        .expect_err("missing cipher should fail");

        assert!(matches!(err, BotTokenResolveError::MissingCipher));
    }

    #[test]
    fn resolver_prefers_decrypted_guild_token() {
        let cipher = BotTokenCipher::new("secret key material").expect("cipher");
        let encrypted = cipher
            .encrypt_for_guild("guild-1", "guild-token")
            .expect("encrypt");

        let token = resolve_bot_token_from_record(
            "guild-1",
            "global-token",
            Some(&stored(encrypted)),
            Some(&cipher),
        )
        .expect("guild token");

        assert_eq!(token, "guild-token");
    }
}
