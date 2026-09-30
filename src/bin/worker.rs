use std::{sync::Arc, time::Duration};

use chatflow::{
    config::{Config, WorkerSettings},
    provider::RouterProvider,
    retention::{self, RetentionSettings},
    runtime,
};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _telemetry = chatflow::telemetry::init("chatflow-worker")?;
    let config = Config::from_env()?;
    let settings = WorkerSettings::from_env()?;
    let retention_settings = RetentionSettings::from_env()?;
    let pool = PgPoolOptions::new()
        .max_connections((settings.concurrency * 2 + 2).min(32) as u32)
        .connect(&config.database_url)
        .await?;
    sqlx::migrate!().run(&pool).await?;
    let owner = format!(
        "{}-{}",
        std::env::var("HOSTNAME").unwrap_or_else(|_| "worker".into()),
        Uuid::new_v4()
    );
    let provider = Arc::new(RouterProvider::from_env(pool.clone())?);
    let permits = Arc::new(tokio::sync::Semaphore::new(settings.concurrency));
    let retention_pool = pool.clone();
    let retention_task = tokio::spawn(async move {
        loop {
            let delay = match retention::sweep(&retention_pool).await {
                Ok(report) => {
                    if report.runs > 0 || report.messages > 0 {
                        tracing::info!(
                            runs = report.runs,
                            messages = report.messages,
                            "retention sweep"
                        );
                    }
                    if report.has_more() {
                        Duration::from_millis(250)
                    } else {
                        retention_settings.sweep_interval
                    }
                }
                Err(error) => {
                    tracing::error!(%error, "retention sweep failed");
                    retention_settings.sweep_interval
                }
            };
            tokio::time::sleep(delay).await;
        }
    });
    let mut tasks = tokio::task::JoinSet::new();
    tracing::info!(owner, concurrency = settings.concurrency, "worker ready");
    loop {
        while let Some(result) = tasks.try_join_next() {
            if let Err(error) = result {
                tracing::error!(%error, "worker task panicked");
            }
        }
        let permit = tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            permit = permits.clone().acquire_owned() => permit?,
        };
        let Some(job) = runtime::claim(&pool, &owner).await? else {
            drop(permit);
            if runtime::reap_exhausted(&pool).await? {
                continue;
            }
            tokio::select! {
                _ = tokio::signal::ctrl_c() => break,
                _ = tokio::time::sleep(Duration::from_millis(250)) => continue,
            }
        };
        chatflow::telemetry::record_run_claimed();
        let task_pool = pool.clone();
        let task_provider = provider.clone();
        let task_owner = owner.clone();
        tasks.spawn(async move {
            let heartbeat_pool = task_pool.clone();
            let heartbeat_job = job.clone();
            let heartbeat_owner = task_owner.clone();
            let heartbeat = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    if !runtime::heartbeat(&heartbeat_pool, &heartbeat_job, &heartbeat_owner)
                        .await
                        .unwrap_or(false)
                    {
                        break;
                    }
                }
            });
            if let Err(error) =
                runtime::execute(&task_pool, task_provider, job.clone(), &task_owner).await
            {
                tracing::error!(run_id = %job.run_id, error = %error, "run execution failed");
            }
            heartbeat.abort();
            drop(permit);
        });
    }
    while let Some(result) = tasks.join_next().await {
        if let Err(error) = result {
            tracing::error!(%error, "worker task panicked");
        }
    }
    retention_task.abort();
    Ok(())
}
