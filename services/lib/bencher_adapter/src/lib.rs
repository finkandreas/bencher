pub mod adapters;
pub mod error;
pub mod results;

pub use adapters::{json::AdapterJson, magic::AdapterMagic, rust::AdapterRust};
use bencher_json::project::report::JsonAdapter;
pub use error::AdapterError;
pub use results::{adapter_results::AdapterResults, AdapterResultsArray};

pub trait Adapter {
    fn convert(&self, input: &str) -> Result<AdapterResults, AdapterError> {
        Self::parse(input)
    }

    fn parse(input: &str) -> Result<AdapterResults, AdapterError>;
}

impl Adapter for JsonAdapter {
    fn convert(&self, input: &str) -> Result<AdapterResults, AdapterError> {
        match self {
            JsonAdapter::Magic => AdapterMagic::parse(input),
            JsonAdapter::Json => AdapterJson::parse(input),
            JsonAdapter::Rust => AdapterRust::parse(input),
        }
    }

    fn parse(input: &str) -> Result<AdapterResults, AdapterError> {
        AdapterMagic::parse(input)
    }
}
