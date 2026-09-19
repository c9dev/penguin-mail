#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("corrupt value in {column}: {value}")]
    Corrupt { column: &'static str, value: String },
    #[error("the database worker stopped")]
    Closed,
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;
