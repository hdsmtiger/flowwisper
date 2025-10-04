//! 观测性初始化脚手架。

use tracing_subscriber::{fmt, layer::SubscriberExt, EnvFilter, Registry};

pub fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_layer = fmt::layer().with_target(false);
    let subscriber = Registry::default().with(env_filter).with(fmt_layer);

    tracing::subscriber::set_global_default(subscriber).expect("failed to set global subscriber");
}
