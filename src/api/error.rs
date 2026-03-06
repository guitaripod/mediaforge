use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;
use tracing::error;
use utoipa::ToSchema;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug)]
pub struct AppError(anyhow::Error);

#[derive(Serialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        error!("Request error: {:?}", self.0);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": self.0.to_string() })),
        )
            .into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    #[test]
    fn app_error_produces_500_with_json() {
        let err = AppError(anyhow::anyhow!("something broke"));
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/json"
        );
    }

    #[test]
    fn app_error_from_rusqlite() {
        let err: AppError = rusqlite::Error::QueryReturnedNoRows.into();
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn app_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file gone");
        let err: AppError = io_err.into();
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
