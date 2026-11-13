use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ApiError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("already exists: {0}")]
    AlreadyExists(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("unauthenticated: {0}")]
    Unauthenticated(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("internal: {0}")]
    Internal(String),
}

pub type Result<T, E = ApiError> = std::result::Result<T, E>;

impl From<ApiError> for tonic::Status {
    fn from(e: ApiError) -> Self {
        use tonic::Status;
        match e {
            ApiError::NotFound(m) => Status::not_found(m),
            ApiError::AlreadyExists(m) => Status::already_exists(m),
            ApiError::Conflict(m) => Status::aborted(m),
            ApiError::Invalid(m) => Status::invalid_argument(m),
            ApiError::Unauthenticated(m) => Status::unauthenticated(m),
            ApiError::Forbidden(m) => Status::permission_denied(m),
            ApiError::Internal(m) => Status::internal(m),
        }
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        ApiError::Invalid(e.to_string())
    }
}

impl From<mongodb::error::Error> for ApiError {
    fn from(e: mongodb::error::Error) -> Self {
        ApiError::Internal(format!("mongodb: {e}"))
    }
}

impl From<bson::ser::Error> for ApiError {
    fn from(e: bson::ser::Error) -> Self {
        ApiError::Internal(format!("bson: {e}"))
    }
}
