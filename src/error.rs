use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Err(Box<dyn std::error::Error + Send + Sync + 'static>),
    #[error("{0}")]
    Msg(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    BinError(#[from] binrw::Error),
}
