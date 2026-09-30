use anyhow::{anyhow, Result};
use opentelemetry::{global, trace::TracerProvider as _};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{metrics::SdkMeterProvider, trace::SdkTracerProvider, Resource};
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

#[derive(Default)]
pub struct Telemetry {
    traces: Option<SdkTracerProvider>,
    metrics: Option<SdkMeterProvider>,
}

fn validate_endpoint(raw: &str) -> Result<()> {
    let url = reqwest::Url::parse(raw)?;
    let secure = url.scheme() == "https"
        || (url.scheme() == "http"
            && crate::config::allows_local_http(url.host_str(), "otel-collector"));
    if !secure
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(anyhow!(
            "OTLP endpoint must be HTTPS or local HTTP without credentials, path, or query"
        ));
    }
    Ok(())
}

pub fn init(service_name: &'static str) -> Result<Telemetry> {
    let endpoint = match std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        Ok(value) if !value.is_empty() => value,
        _ => {
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .init();
            return Ok(Telemetry::default());
        }
    };
    validate_endpoint(&endpoint)?;
    let resource = Resource::builder().with_service_name(service_name).build();
    let span_exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&endpoint)
        .build()?;
    let traces = SdkTracerProvider::builder()
        .with_batch_exporter(span_exporter)
        .with_resource(resource.clone())
        .build();
    let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .with_endpoint(&endpoint)
        .build()?;
    let metrics = SdkMeterProvider::builder()
        .with_periodic_exporter(metric_exporter)
        .with_resource(resource)
        .build();
    global::set_meter_provider(metrics.clone());
    let tracer = traces.tracer(service_name);
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .init();
    Ok(Telemetry {
        traces: Some(traces),
        metrics: Some(metrics),
    })
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        if let Some(metrics) = &self.metrics {
            let _ = metrics.shutdown();
        }
        if let Some(traces) = &self.traces {
            let _ = traces.shutdown();
        }
    }
}

pub fn record_run_created() {
    global::meter("chatflow")
        .u64_counter("chatflow.runs.created")
        .build()
        .add(1, &[]);
}

pub fn record_run_claimed() {
    global::meter("chatflow")
        .u64_counter("chatflow.runs.claimed")
        .build()
        .add(1, &[]);
}

#[cfg(test)]
mod tests {
    use super::validate_endpoint;

    #[test]
    fn exporter_endpoint_requires_secure_transport_or_loopback() {
        assert!(validate_endpoint("http://127.0.0.1:14317").is_ok());
        assert!(validate_endpoint("https://collector.example:4317").is_ok());
        for invalid in [
            "http://collector.example:4317",
            "https://user:pass@collector.example:4317",
            "https://collector.example:4317/custom",
            "https://collector.example:4317?token=x",
        ] {
            assert!(validate_endpoint(invalid).is_err(), "{invalid}");
        }
    }
}
