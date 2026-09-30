use std::{env, net::SocketAddr};

use thiserror::Error;

pub fn allows_local_http(host: Option<&str>, docker_host: &str) -> bool {
    allows_local_http_with_opt_in(
        host,
        docker_host,
        env::var("CHATFLOW_LOCAL_DOCKER_HTTP").as_deref() == Ok("true"),
    )
}

fn allows_local_http_with_opt_in(
    host: Option<&str>,
    docker_host: &str,
    docker_opt_in: bool,
) -> bool {
    matches!(host, Some("localhost" | "127.0.0.1" | "::1"))
        || (docker_opt_in && host == Some(docker_host))
}

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub api_bind: SocketAddr,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("DATABASE_URL is required")]
    MissingDatabaseUrl,
    #[error("API_BIND must be a socket address: {0}")]
    InvalidApiBind(#[from] std::net::AddrParseError),
    #[error("WORKER_CONCURRENCY must be an integer between 1 and 64")]
    InvalidWorkerConcurrency,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkerSettings {
    pub concurrency: usize,
}

impl WorkerSettings {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::parse(env::var("WORKER_CONCURRENCY").ok().as_deref())
    }

    pub fn parse(raw: Option<&str>) -> Result<Self, ConfigError> {
        let concurrency = match raw {
            None => 4,
            Some(value) => value
                .parse()
                .map_err(|_| ConfigError::InvalidWorkerConcurrency)?,
        };
        if !(1..=64).contains(&concurrency) {
            return Err(ConfigError::InvalidWorkerConcurrency);
        }
        Ok(Self { concurrency })
    }
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let database_url = env::var("DATABASE_URL").map_err(|_| ConfigError::MissingDatabaseUrl)?;
        let api_bind = env::var("API_BIND")
            .unwrap_or_else(|_| "127.0.0.1:3000".to_owned())
            .parse()?;
        Ok(Self {
            database_url,
            api_bind,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{allows_local_http_with_opt_in, WorkerSettings};

    #[test]
    fn local_docker_http_opt_in_allows_only_the_named_service() {
        assert!(allows_local_http_with_opt_in(
            Some("127.0.0.1"),
            "vault",
            false
        ));
        assert!(!allows_local_http_with_opt_in(
            Some("vault"),
            "vault",
            false
        ));
        assert!(allows_local_http_with_opt_in(Some("vault"), "vault", true));
        assert!(allows_local_http_with_opt_in(
            Some("ragflow-cpu"),
            "ragflow-cpu",
            true
        ));
        assert!(!allows_local_http_with_opt_in(
            Some("ragflow-cpu"),
            "ragflow-cpu",
            false
        ));
        assert!(!allows_local_http_with_opt_in(
            Some("other-service"),
            "vault",
            true
        ));
    }

    #[test]
    fn worker_concurrency_is_bounded() {
        assert_eq!(WorkerSettings::parse(None).unwrap().concurrency, 4);
        assert_eq!(WorkerSettings::parse(Some("8")).unwrap().concurrency, 8);
        for invalid in ["0", "65", "-1", "abc"] {
            assert!(WorkerSettings::parse(Some(invalid)).is_err());
        }
    }
}
