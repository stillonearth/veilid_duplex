use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub struct BevyVeilidError {
    pub details: String,
}

impl BevyVeilidError {
    pub fn new(msg: &str) -> BevyVeilidError {
        BevyVeilidError {
            details: msg.to_string(),
        }
    }
}

impl fmt::Display for BevyVeilidError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.details)
    }
}

impl Error for BevyVeilidError {
    fn description(&self) -> &str {
        &self.details
    }
}
