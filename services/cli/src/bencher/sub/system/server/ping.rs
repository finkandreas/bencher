use std::convert::TryFrom;

use async_trait::async_trait;

use crate::{
    bencher::{backend::Backend, sub::SubCmd},
    cli::system::server::CliPing,
    CliError,
};

#[derive(Debug, Clone)]
pub struct Ping {
    pub backend: Backend,
}

impl TryFrom<CliPing> for Ping {
    type Error = CliError;

    fn try_from(ping: CliPing) -> Result<Self, Self::Error> {
        let CliPing { backend } = ping;
        Ok(Self {
            backend: backend.try_into()?,
        })
    }
}

#[async_trait]
impl SubCmd for Ping {
    async fn exec(&self) -> Result<(), CliError> {
        self.backend
            .send_with(
                |client| async move { client.server_ping_get().send().await },
                true,
            )
            .await?;
        Ok(())
    }
}
