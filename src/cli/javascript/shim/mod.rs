use std::sync::Arc;

use crate::HttpIO;

use super::sync_v8::SyncV8;

mod console;
pub mod fetch;
pub async fn init(v8: &SyncV8, http: Arc<dyn HttpIO>) -> anyhow::Result<()> {
    console::init(v8).await?;
    fetch::init(v8, http).await?;
    Ok(())
}
